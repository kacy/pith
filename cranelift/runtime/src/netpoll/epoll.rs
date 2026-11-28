//! Linux `epoll` / `eventfd` syscall wrapper and event harvest loop.

use super::registry::{
    collect_expired, take_ready, Deadline, DeadlineTarget, FdReg, Inner, Interest, Waiter, CLOSED,
    EVENTFD_TOKEN, FD_EVENTS, PENDING,
};
use super::{lock_inner, poller, Poller, WAIT_NONE};
use crate::concurrency::green;
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering as AtomicOrdering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

impl Poller {
    pub(super) fn new() -> Self {
        // SAFETY: plain libc calls with valid flags; we check every return value.
        let epfd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if epfd < 0 {
            runtime_fatal!("epoll_create1 failed: {}", std::io::Error::last_os_error())
        }
        let eventfd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if eventfd < 0 {
            runtime_fatal!("eventfd failed: {}", std::io::Error::last_os_error())
        }

        // register the eventfd level-triggered with no oneshot, so a notify
        // wakes an `epoll_wait` until the worker drains it.
        let mut ev = libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: EVENTFD_TOKEN,
        };
        // SAFETY: epfd and eventfd are both valid fds we just created; `ev` is a
        // properly initialized epoll_event living on this stack for the call.
        let rc = unsafe { libc::epoll_ctl(epfd, libc::EPOLL_CTL_ADD, eventfd, &mut ev) };
        if rc != 0 {
            runtime_fatal!("epoll_ctl ADD eventfd failed: {}", std::io::Error::last_os_error())
        }

        Poller {
            epfd,
            eventfd,
            notified: AtomicBool::new(false),
            mode: AtomicU8::new(WAIT_NONE),
            park: Mutex::new(()),
            park_cv: Condvar::new(),
            busy: AtomicBool::new(false),
            inner: Mutex::new(Inner::new()),
        }
    }
}

/// register a task's wait on `(fd, interest)` under `inner`, or report that it
/// need not wait. returns false when an edge in that direction arrived while
/// nobody waited: the bit is consumed and the caller retries its syscall
/// instead of parking. otherwise the waiter and its deadline are recorded and
/// the caller parks.
///
/// the first wait on an fd adds it to the epoll set. an `EEXIST` there means
/// the kernel still holds an entry for this fd that the registry lost track
/// of, so it is rewritten with a `MOD`. any other failure leaves the fd out of
/// `fds`, so the next wait tries again; this one then ends only by its
/// deadline or a close.
pub(super) enum RegisterOutcome {
    /// A buffered edge already satisfied this wait; caller retries immediately.
    Ready,
    /// Waiter registered with sequence number `seq` and outcome cell; caller must park.
    Parked(u64, Arc<AtomicI32>),
    /// `epoll_ctl` rejected the fd (e.g. EBADF/EPERM/ENOMEM); caller must NOT park.
    Failed,
}

pub(super) fn register_io(
    poller: &Poller,
    inner: &mut Inner,
    fd: RawFd,
    interest: Interest,
    deadline: Option<Instant>,
    task: usize,
) -> RegisterOutcome {
    match inner.fds.get_mut(&fd) {
        Some(reg) => {
            let bit = interest.epoll_bit();
            if reg.ready & bit != 0 {
                reg.ready &= !bit;
                return RegisterOutcome::Ready;
            }
        }
        None => {
            let mut err = epoll_apply(poller.epfd, libc::EPOLL_CTL_ADD, fd);
            if err == libc::EEXIST {
                err = epoll_apply(poller.epfd, libc::EPOLL_CTL_MOD, fd);
            }
            if err != 0 {
                return RegisterOutcome::Failed;
            }
            inner.fds.insert(fd, FdReg { ready: 0 });
        }
    }
    let outcome = Arc::new(AtomicI32::new(PENDING));
    let seq = inner.take_seq();
    inner.push_waiter(
        fd,
        interest,
        Waiter {
            task,
            seq,
            outcome: outcome.clone(),
        },
    );
    if let Some(at) = deadline {
        inner.push_deadline(Deadline {
            at,
            seq,
            target: DeadlineTarget::Io { fd, interest },
        });
    }
    RegisterOutcome::Parked(seq, outcome)
}

/// run one `epoll_ctl` op and return its errno, or 0 on success. an `ADD` or
/// `MOD` registers `FD_EVENTS`; `DEL` passes no event. the caller decides which
/// errors matter: a `DEL` on an already-closed fd races `pith_tcp_close`
/// (ENOENT/EBADF, expected), and a failed `ADD` leaves the fd unregistered.
pub(super) fn epoll_apply(epfd: RawFd, op: libc::c_int, fd: RawFd) -> i32 {
    let mut ev = libc::epoll_event {
        events: FD_EVENTS,
        u64: fd as u64,
    };
    let ev_ptr = if op == libc::EPOLL_CTL_DEL {
        std::ptr::null_mut()
    } else {
        &mut ev as *mut libc::epoll_event
    };
    // SAFETY: `epfd` is a poller's epoll fd; `fd` is a socket fd owned by pith
    // for the duration of the wait; `ev` (when used) is a valid event on this
    // stack. DEL passes a null event as the kernel allows.
    let rc = unsafe { libc::epoll_ctl(epfd, op, fd, ev_ptr) };
    if rc == 0 {
        0
    } else {
        std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
    }
}
/// run worker `index`'s poller once: wait up to `timeout` (zero polls without
/// blocking), and wake the tasks whose fds are ready or whose deadlines
/// expired. returns whether anything happened (a readiness, an expired
/// deadline or a notify) as opposed to the wait running out. never resumes a
/// coroutine.
///
/// the wait is capped at the poller's nearest deadline, so a parked worker
/// wakes in time to expire it.
pub(super) fn poll(index: usize, timeout: Duration) -> bool {
    let poller = poller(index);
    // a modest batch; more events than this in one wait just roll to the next.
    const MAX_EVENTS: usize = 64;
    let mut events = [libc::epoll_event { events: 0, u64: 0 }; MAX_EVENTS];

    let timeout_ms = next_timeout_ms(poller, timeout);
    // SAFETY: `epfd` is valid for the life of the process; the events buffer
    // is `MAX_EVENTS` long and lives on this stack across the call.
    let n = unsafe {
        libc::epoll_wait(
            poller.epfd,
            events.as_mut_ptr(),
            MAX_EVENTS as libc::c_int,
            timeout_ms,
        )
    };
    // EINTR (or any transient error) reads as an empty wait.
    let n = n.max(0) as usize;
    // the worker is awake from here on and looks at its queues once this
    // returns, so a `notify` now only needs to set the flag. that includes the
    // one each wake below sends this worker for its own tasks, which used to
    // cost an eventfd write and read per wake on a lightly loaded worker.
    poller.mode.store(WAIT_NONE, AtomicOrdering::SeqCst);

    // collect every task to wake into a local list, then wake them AFTER
    // dropping the registry lock: `green::wake` takes green's own locks, and
    // keeping our lock out of that nest preserves the "netpoll lock never
    // holds across a green wake" discipline.
    let mut to_wake: Vec<usize> = Vec::new();
    let mut notified = false;
    {
        let mut inner = lock_inner(poller);
        for ev in events.iter().take(n) {
            let token = ev.u64;
            let bits = ev.events;
            if token == EVENTFD_TOKEN {
                drain_eventfd(poller.eventfd);
                notified = true;
                continue;
            }
            take_event(&mut inner, token as RawFd, bits, &mut to_wake);
        }

        // service expired deadlines in the same lock hold.
        if !inner.deadlines.is_empty() {
            collect_expired(&mut inner, Instant::now(), &mut to_wake);
        }
        inner.publish(&poller.busy);
    }

    let woke = !to_wake.is_empty();
    for task in to_wake {
        green::wake(task);
    }
    woke || notified
}
/// the `epoll_wait` timeout for one poll: `cap`, or less if a deadline is due
/// sooner (0 if one is already due).
pub(super) fn next_timeout_ms(poller: &Poller, cap: Duration) -> libc::c_int {
    let inner = lock_inner(poller);
    let wait = match inner.deadlines.peek() {
        None => cap,
        Some(top) => cap.min(top.0.at.saturating_duration_since(Instant::now())),
    };
    if wait.is_zero() {
        return 0;
    }
    // round up so we never wake a hair early and re-sleep.
    let ms = wait.as_millis() + 1;
    ms.min(libc::c_int::MAX as u128) as libc::c_int
}

/// handle one epoll event for `fd`: wake the waiters of each direction it
/// reports, or, for a direction nobody waits on, keep its readiness bit for the
/// next wait. an error or hangup counts as both directions, so the pending
/// syscall can observe the real error. an event for an fd this poller no longer
/// tracks wakes nobody and is dropped.
pub(super) fn take_event(inner: &mut Inner, fd: RawFd, bits: u32, to_wake: &mut Vec<usize>) {
    let err = bits & (libc::EPOLLERR as u32 | libc::EPOLLHUP as u32) != 0;
    for interest in [Interest::Read, Interest::Write] {
        let fired = match interest {
            Interest::Read => bits & (libc::EPOLLIN as u32 | libc::EPOLLRDHUP as u32) != 0,
            Interest::Write => bits & libc::EPOLLOUT as u32 != 0,
        };
        if !(fired || err) || take_ready(inner, fd, interest, to_wake) {
            continue;
        }
        if let Some(reg) = inner.fds.get_mut(&fd) {
            reg.ready |= interest.epoll_bit();
        }
    }
}
/// drain the eventfd's counter so its level-triggered readiness clears. the value
/// is irrelevant: it is a pure wakeup.
pub(super) fn drain_eventfd(eventfd: RawFd) {
    let mut buf: u64 = 0;
    // SAFETY: reading 8 bytes from our own nonblocking eventfd into a local; a
    // single read fully drains an eventfd counter, and EAGAIN is fine.
    unsafe {
        libc::read(
            eventfd,
            &mut buf as *mut u64 as *mut libc::c_void,
            std::mem::size_of::<u64>(),
        );
    }
}
/// `on_close` for one poller: resolve its waiters on `fd` as closed, queue
/// them to wake, and take `fd` out of its epoll set.
pub(super) fn close_in(poller: &Poller, fd: RawFd, to_wake: &mut Vec<usize>) {
    let mut inner = lock_inner(poller);
    for interest in [Interest::Read, Interest::Write] {
        if let Some(waiters) = inner.waiters.remove(&(fd, interest)) {
            inner.waiting -= waiters.len();
            for w in waiters {
                w.resolve(CLOSED, to_wake);
            }
        }
    }
    if inner.fds.remove(&fd).is_some() {
        // fd was in the epoll set; remove it. errors (already gone) ignored.
        epoll_apply(poller.epfd, libc::EPOLL_CTL_DEL, fd);
    }
    inner.publish(&poller.busy);
    // any deadline heap entries for this fd are now stale; they match no
    // waiter and are dropped when they surface (same as a fired waiter's).
}
