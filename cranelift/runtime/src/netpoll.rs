//! epoll reactor (netpoller) for the green-thread backend.
//!
//! under `PITH_GREEN` a small pool of worker OS threads runs many pith tasks as
//! stackful coroutines (see `concurrency::green`). every blocking coordination op
//! — channels, mutex, waitgroup, semaphore, await — already yields the coroutine
//! back to its worker instead of parking the worker OS thread. socket I/O was the
//! last hard park: a green task that read from a socket with no data ready blocked
//! its whole worker on `poll`, starving every other task pinned to that worker.
//!
//! this module closes that gap. one shared epoll instance plus one dedicated
//! reactor OS thread watch every fd a green task is waiting on. when a socket
//! would block, the task registers its (fd, interest) here and suspends its
//! coroutine; the worker runs other tasks. when epoll reports the fd ready the
//! reactor does NOT resume the coroutine — it only calls `green::wake(task)`,
//! which re-enqueues the task onto its owner worker's pinned queue. that composes
//! with green's pinning discipline for free: the coroutine is only ever resumed on
//! the thread that first ran it, and the reactor never touches a coroutine's
//! stack.
//!
//! ## epoll mode: level-triggered + oneshot
//!
//! each fd is armed level-triggered with `EPOLLONESHOT`, so a fire auto-disarms
//! the fd (it stays in the interest set but reports nothing more until re-armed).
//! level-triggered closes the classic race where readiness arrives between a
//! task's `EWOULDBLOCK` and its registration: the ADD itself re-reports the
//! already-pending readiness, so the task is woken immediately rather than
//! sleeping forever. a spurious wake is always safe — the task just re-runs its
//! syscall, which is the real source of truth.
//!
//! ## the waiter registry
//!
//! a single `Mutex<Inner>` guards everything the reactor and the workers share:
//!
//! - `waiters`: for each `(fd, interest)`, the list of tasks blocked on it.
//!   multiple tasks may wait on one fd — e.g. several accept loops on a listener —
//!   so on readiness the reactor wakes *all* waiters for that (fd, interest) and
//!   clears the list; each retries its syscall, one wins, the losers re-block.
//! - `fds`: for each fd currently in the epoll set, the event mask presently
//!   armed. arming is always recomputed from the waiter lists (`reconcile_fd`):
//!   the first registrant on an fd does `EPOLL_CTL_ADD`, a change in the desired
//!   interest set (or re-arming after a oneshot fire) does `EPOLL_CTL_MOD`, and
//!   the last waiter leaving does `EPOLL_CTL_DEL`. keeping epoll state a pure
//!   function of the waiter lists is what makes the arm/disarm bookkeeping easy to
//!   reason about.
//! - `deadlines`: a min-heap of timestamped entries for waits that carry a
//!   finite timeout. the reactor sets each `epoll_wait` timeout to the nearest
//!   deadline and, on expiry, wakes the task with a timeout result. an entry
//!   targets either an fd wait or a pure timer.
//! - `timers`: tasks sleeping with no fd at all (`sleep_task`, behind
//!   `pith_sleep`). a sleep is a deadline wait minus the fd: same heap, same
//!   sweep, nothing armed in epoll.
//!
//! ## the eventfd nudge
//!
//! an eventfd is registered in epoll (level-triggered, no oneshot). readiness
//! registrations need no nudge — a live `epoll_ctl` ADD is honored by an in-flight
//! `epoll_wait`. only a *nearer deadline* needs one: if a worker registers a
//! timeout sooner than the reactor's current sleep, it writes the eventfd so the
//! reactor wakes and recomputes its timeout. (the eventfd doubles as a clean
//! shutdown handle, though the pool currently lives for the whole process.)
//!
//! ## lock discipline
//!
//! there is exactly one lock here: `Inner`'s mutex. it is never held across
//! `epoll_wait` (which blocks) and never across `green::wake` (which takes green's
//! own slab/queue locks). a worker registering a waiter takes this lock, arms
//! epoll under it, drops it, then parks. the reactor takes it only to drain fired
//! events and expired deadlines into a local list, drops it, then wakes. so this
//! lock never nests inside green's locks nor they inside it — no cross-module lock
//! cycle is possible.

use crate::concurrency::green;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicI32, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::time::{Duration, Instant};

/// which readiness a task waits for on an fd. one fd can carry a read waiter and
/// a write waiter at once (both registered, epoll armed for `EPOLLIN|EPOLLOUT`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Interest {
    Read,
    Write,
}

impl Interest {
    /// the epoll event bit this interest arms.
    fn epoll_bit(self) -> u32 {
        match self {
            Interest::Read => libc::EPOLLIN as u32,
            Interest::Write => libc::EPOLLOUT as u32,
        }
    }
}

// outcome of a wait, shared between the waiting task and the reactor via an
// `AtomicI32`. it starts `PENDING`; whichever reactor path handles the waiter
// first flips it to a terminal value with a compare-exchange, so a readiness and
// a timeout racing on the same waiter cannot both "win".
const PENDING: i32 = 0;
const READY: i32 = 1;
const TIMED_OUT: i32 = 2;
/// the fd was closed out from under a still-parked waiter (see `on_close`). the
/// woken task reports it as an I/O error so its read/write unwinds cleanly
/// instead of hanging or retrying a syscall on a dead fd.
const CLOSED: i32 = 3;

/// the epoll token used for the eventfd. real fds are stored as their own value
/// in `epoll_event.u64`; `u64::MAX` can never collide with a real fd.
const EVENTFD_TOKEN: u64 = u64::MAX;

/// one blocked task waiting on an (fd, interest).
struct Waiter {
    /// green slab id, passed straight to `green::wake`.
    task: usize,
    /// unique registration id, used to match a heap deadline entry back to the
    /// exact waiter (a stale heap entry whose waiter already fired finds nothing).
    seq: u64,
    /// shared result cell; the reactor writes `READY` or `TIMED_OUT` here before
    /// waking the task, which reads it back after `park_current` returns.
    outcome: Arc<AtomicI32>,
}

/// what a heap entry resolves when it expires: an fd wait that carried a
/// timeout, or a pure timer with no fd at all (a green task sleeping). the two
/// are distinct variants — rather than a sentinel fd smuggled through the io
/// shape — so the fd-arming code (`reconcile_fd`, `desired_mask`, `on_close`)
/// can never be handed a value that is not a real fd.
enum DeadlineTarget {
    Io { fd: RawFd, interest: Interest },
    Timer,
}

/// a finite-timeout registration, ordered by instant for the min-heap.
struct Deadline {
    at: Instant,
    seq: u64,
    target: DeadlineTarget,
}

// order deadlines by time (then seq) so a `BinaryHeap<Reverse<Deadline>>` pops the
// soonest first.
impl PartialEq for Deadline {
    fn eq(&self, other: &Self) -> bool {
        self.at == other.at && self.seq == other.seq
    }
}
impl Eq for Deadline {}
impl Ord for Deadline {
    fn cmp(&self, other: &Self) -> Ordering {
        self.at.cmp(&other.at).then(self.seq.cmp(&other.seq))
    }
}
impl PartialOrd for Deadline {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// the arm state of one fd in the epoll set. an fd is present in `Inner::fds`
/// exactly while it is in the epoll interest set. `armed` is the event mask epoll
/// currently reports on; it drops to 0 when a oneshot fire disarms the fd, which
/// tells `reconcile_fd` it must re-`MOD` to arm any remaining interest.
struct FdReg {
    armed: u32,
}

/// everything the workers and the reactor thread share, behind one mutex.
struct Inner {
    waiters: HashMap<(RawFd, Interest), Vec<Waiter>>,
    fds: HashMap<RawFd, FdReg>,
    deadlines: BinaryHeap<std::cmp::Reverse<Deadline>>,
    /// sleeping tasks, keyed by the seq of their heap entry. a timer has no
    /// (fd, interest) to key on, and giving it its own map keeps it out of
    /// `waiters` — where everything is fd-shaped — entirely.
    timers: HashMap<u64, Waiter>,
    next_seq: u64,
}

impl Inner {
    fn new() -> Self {
        Inner {
            waiters: HashMap::new(),
            fds: HashMap::new(),
            deadlines: BinaryHeap::new(),
            timers: HashMap::new(),
            next_seq: 0,
        }
    }

    /// the event mask an fd should currently be armed for: the union of the
    /// interests that still have at least one waiter.
    fn desired_mask(&self, fd: RawFd) -> u32 {
        let mut mask = 0;
        for interest in [Interest::Read, Interest::Write] {
            if self
                .waiters
                .get(&(fd, interest))
                .is_some_and(|v| !v.is_empty())
            {
                mask |= interest.epoll_bit();
            }
        }
        mask
    }
}

/// the whole reactor: the epoll fd, the nudge eventfd, and the shared registry.
struct Reactor {
    epfd: RawFd,
    eventfd: RawFd,
    inner: Mutex<Inner>,
}

/// built lazily on the first green socket wait so a program that never blocks on
/// a socket under the green backend pays nothing (and starts no reactor thread).
static REACTOR: OnceLock<Reactor> = OnceLock::new();

fn lock_inner(reactor: &Reactor) -> std::sync::MutexGuard<'_, Inner> {
    reactor.inner.lock().unwrap_or_else(|p| p.into_inner())
}

/// build (once) the epoll instance and eventfd. does NOT start the reactor thread
/// — that is a separate `Once` (`ensure_started`) so the constructor stays
/// non-reentrant, mirroring `green::scheduler` / `ensure_workers_started`.
fn reactor() -> &'static Reactor {
    REACTOR.get_or_init(|| {
        // SAFETY: plain libc calls with valid flags; we check every return value.
        let epfd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        assert!(epfd >= 0, "epoll_create1 failed");
        let eventfd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        assert!(eventfd >= 0, "eventfd failed");

        // register the eventfd level-triggered with no oneshot, so any nudge
        // always wakes an in-flight epoll_wait until we drain it.
        let mut ev = libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: EVENTFD_TOKEN,
        };
        // SAFETY: epfd and eventfd are both valid fds we just created; `ev` is a
        // properly initialized epoll_event living on this stack for the call.
        let rc = unsafe { libc::epoll_ctl(epfd, libc::EPOLL_CTL_ADD, eventfd, &mut ev) };
        assert!(rc == 0, "epoll_ctl ADD eventfd failed");

        Reactor {
            epfd,
            eventfd,
            inner: Mutex::new(Inner::new()),
        }
    })
}

/// start the reactor thread exactly once. called from the first `wait_io`, by
/// which point `reactor()` is initialized.
fn ensure_started() {
    static STARTED: Once = Once::new();
    STARTED.call_once(|| {
        reactor();
        std::thread::Builder::new()
            .name("pith-netpoll".to_string())
            .spawn(reactor_loop)
            .expect("spawn netpoll reactor");
    });
}

/// arm/re-arm/disarm epoll for `fd` so its armed mask matches the current waiter
/// lists. called under `inner` after any change to `fd`'s waiters (register,
/// fire, timeout, close). keeping epoll state a pure function of the waiter lists
/// is the whole arm/disarm story:
///
/// - no waiters, fd in the set  -> `EPOLL_CTL_DEL`, forget the fd.
/// - waiters, fd not in the set -> `EPOLL_CTL_ADD` with `EPOLLONESHOT | desired`.
/// - waiters, fd in the set, mask changed (a new interest, or re-arming after a
///   oneshot fire dropped `armed` to 0) -> `EPOLL_CTL_MOD`.
/// - waiters, fd in the set, mask unchanged -> nothing (a second waiter on an
///   already-armed interest just rode along on the existing arm).
fn reconcile_fd(reactor: &Reactor, inner: &mut Inner, fd: RawFd) {
    let desired = inner.desired_mask(fd);
    match inner.fds.get_mut(&fd) {
        None => {
            if desired != 0 {
                epoll_apply(reactor.epfd, libc::EPOLL_CTL_ADD, fd, desired);
                inner.fds.insert(fd, FdReg { armed: desired });
            }
        }
        Some(reg) => {
            if desired == 0 {
                epoll_apply(reactor.epfd, libc::EPOLL_CTL_DEL, fd, 0);
                inner.fds.remove(&fd);
            } else if reg.armed != desired {
                epoll_apply(reactor.epfd, libc::EPOLL_CTL_MOD, fd, desired);
                reg.armed = desired;
            }
        }
    }
}

/// run one `epoll_ctl` op. `DEL` ignores its mask. errors are swallowed: a `DEL`
/// on an already-closed fd races `pith_tcp_close` (ENOENT/EBADF, expected), and
/// an `ADD`/`MOD` failure just means the task falls back to a spurious retry when
/// no wake ever comes — never a crash.
fn epoll_apply(epfd: RawFd, op: libc::c_int, fd: RawFd, mask: u32) {
    let mut ev = libc::epoll_event {
        // oneshot on every real fd: a fire disarms it until we re-arm, so two
        // readiness events never queue up for the same wait.
        events: mask | libc::EPOLLONESHOT as u32,
        u64: fd as u64,
    };
    let ev_ptr = if op == libc::EPOLL_CTL_DEL {
        std::ptr::null_mut()
    } else {
        &mut ev as *mut libc::epoll_event
    };
    // SAFETY: `epfd` is the reactor's epoll fd; `fd` is a socket fd owned by pith
    // for the duration of the wait; `ev` (when used) is a valid event on this
    // stack. DEL passes a null event as the kernel allows.
    unsafe {
        libc::epoll_ctl(epfd, op, fd, ev_ptr);
    }
}

/// nudge the reactor awake so it recomputes its `epoll_wait` timeout — used only
/// when a worker registers a deadline nearer than the reactor's current sleep.
fn nudge(reactor: &Reactor) {
    let one: u64 = 1;
    // SAFETY: writing 8 bytes to our own eventfd; EAGAIN (counter saturated) is
    // fine — the reactor will still see a pending readiness and wake.
    unsafe {
        libc::write(
            reactor.eventfd,
            &one as *const u64 as *const libc::c_void,
            std::mem::size_of::<u64>(),
        );
    }
}

/// block the current green task until `fd` is ready for `read`/write, it times
/// out, or an error is detected. returns the tri-state readiness contract:
/// `1` ready, `0` timed out, `-1` error. must be called only from inside a green
/// task (the caller checks `green::current_task`); `task` is that task's slab id.
///
/// `timeout_ms < 0` means wait forever (no deadline registered).
pub(crate) fn wait_io(fd: RawFd, read: bool, timeout_ms: i64, task: usize) -> i64 {
    ensure_started();
    let reactor = reactor();
    let interest = if read { Interest::Read } else { Interest::Write };
    let outcome = Arc::new(AtomicI32::new(PENDING));

    let deadline = if timeout_ms < 0 {
        None
    } else {
        Some(Instant::now() + Duration::from_millis(timeout_ms as u64))
    };

    // register under the lock: allocate a seq, push the waiter, record any
    // deadline, and arm epoll. decide whether we need to nudge the reactor
    // (only if our deadline is sooner than whatever it is currently sleeping on).
    let should_nudge = {
        let mut inner = lock_inner(reactor);
        let seq = inner.next_seq;
        inner.next_seq += 1;

        let nudge = match deadline {
            Some(d) => inner.deadlines.peek().map_or(true, |top| d < top.0.at),
            None => false,
        };

        inner
            .waiters
            .entry((fd, interest))
            .or_default()
            .push(Waiter {
                task,
                seq,
                outcome: outcome.clone(),
            });
        if let Some(at) = deadline {
            inner.deadlines.push(std::cmp::Reverse(Deadline {
                at,
                seq,
                target: DeadlineTarget::Io { fd, interest },
            }));
        }
        reconcile_fd(reactor, &mut inner, fd);
        nudge
    };
    if should_nudge {
        nudge(reactor);
    }

    // suspend the coroutine back to the worker. `green::wake` (from the reactor)
    // re-enqueues us; even if that wake lands before we finish suspending,
    // green's `wake_pending` flag re-enqueues us from the park path, so it is
    // never lost. thus when `park_current` returns, `outcome` has been set.
    green::park_current();

    match outcome.load(AtomicOrdering::Acquire) {
        READY => 1,
        TIMED_OUT => 0,
        // the fd was closed under us while we were parked: report an error so the
        // caller's read/write returns an error and the task unwinds, rather than
        // retrying a syscall on a dead (or worse, recycled) fd.
        CLOSED => -1,
        // a wake with no terminal outcome should not happen; treat it as ready so
        // the caller re-runs its syscall (the source of truth) rather than
        // mistaking it for a timeout.
        _ => 1,
    }
}

/// park the current green task for `ms` milliseconds without holding its worker.
/// this is `wait_io` minus the fd: register a deadline, nudge the reactor if it
/// is now the nearest one, and suspend; the deadline sweep wakes us. must be
/// called only from inside a green task (the caller checks
/// `green::current_task`); `task` is that task's slab id.
///
/// a zero or negative duration returns immediately — there is nothing to wait
/// for, so nothing is registered.
pub(crate) fn sleep_task(ms: i64, task: usize) {
    if ms <= 0 {
        return;
    }
    ensure_started();
    let reactor = reactor();
    let outcome = Arc::new(AtomicI32::new(PENDING));
    let at = Instant::now() + Duration::from_millis(ms as u64);

    let should_nudge = register_timer(reactor, at, task, &outcome);
    if should_nudge {
        nudge(reactor);
    }

    // park until the sweep marks us done. unlike `wait_io` — where a spurious
    // wake is safe because the task re-runs its syscall — a sleep has no
    // syscall to act as the source of truth, so an early resume must go back
    // to sleep: our timer entry is still registered and the sweep will wake us
    // again. once the outcome is terminal the entry is gone and we return.
    while outcome.load(AtomicOrdering::Acquire) == PENDING {
        green::park_current();
    }
}

/// the registration half of `sleep_task`, split out so it can be exercised
/// without a live coroutine to suspend. pushes a timer deadline and its waiter
/// under the lock; returns whether the reactor must be nudged (only when this
/// deadline is nearer than whatever it is currently sleeping on).
fn register_timer(reactor: &Reactor, at: Instant, task: usize, outcome: &Arc<AtomicI32>) -> bool {
    let mut inner = lock_inner(reactor);
    let seq = inner.next_seq;
    inner.next_seq += 1;

    let nudge = inner.deadlines.peek().map_or(true, |top| at < top.0.at);

    inner.timers.insert(
        seq,
        Waiter {
            task,
            seq,
            outcome: outcome.clone(),
        },
    );
    inner.deadlines.push(std::cmp::Reverse(Deadline {
        at,
        seq,
        target: DeadlineTarget::Timer,
    }));
    nudge
}

/// the reactor thread: wait on epoll, wake the tasks whose fds are ready or whose
/// deadlines expired, and re-arm what is left. never resumes a coroutine.
fn reactor_loop() {
    let reactor = reactor();
    // a modest batch; more events than this in one wait just roll to the next.
    const MAX_EVENTS: usize = 64;
    let mut events: Vec<libc::epoll_event> = (0..MAX_EVENTS)
        .map(|_| libc::epoll_event { events: 0, u64: 0 })
        .collect();

    loop {
        // choose the sleep bound: the nearest deadline, or block indefinitely.
        let timeout_ms = next_timeout_ms(reactor);

        // SAFETY: `epfd` is valid for the life of the process; the events buffer
        // is `MAX_EVENTS` long and lives on this stack across the call.
        let n = unsafe {
            libc::epoll_wait(
                reactor.epfd,
                events.as_mut_ptr(),
                MAX_EVENTS as libc::c_int,
                timeout_ms,
            )
        };
        if n < 0 {
            // EINTR (or any transient error): just loop and wait again.
            continue;
        }

        // collect every task to wake into a local list, then wake them AFTER
        // dropping the registry lock — `green::wake` takes green's own locks, and
        // keeping our lock out of that nest preserves the "netpoll lock never
        // holds across a green wake" discipline.
        let mut to_wake: Vec<usize> = Vec::new();
        {
            let mut inner = lock_inner(reactor);
            for ev in events.iter().take(n as usize) {
                let token = ev.u64;
                let bits = ev.events;
                if token == EVENTFD_TOKEN {
                    drain_eventfd(reactor.eventfd);
                    continue;
                }
                let fd = token as RawFd;

                // the oneshot fire disarmed this fd in the kernel; record that so
                // reconcile_fd re-arms any interest that still has waiters.
                if let Some(reg) = inner.fds.get_mut(&fd) {
                    reg.armed = 0;
                }

                // an error/hangup wakes both directions so the pending syscall can
                // observe the real error. otherwise wake the direction that fired.
                let err = bits & (libc::EPOLLERR as u32 | libc::EPOLLHUP as u32);
                if bits & (libc::EPOLLIN as u32 | libc::EPOLLRDHUP as u32) != 0 || err != 0 {
                    take_ready(&mut inner, fd, Interest::Read, &mut to_wake);
                }
                if bits & (libc::EPOLLOUT as u32) != 0 || err != 0 {
                    take_ready(&mut inner, fd, Interest::Write, &mut to_wake);
                }
                reconcile_fd(reactor, &mut inner, fd);
            }

            // service expired deadlines in the same lock hold.
            collect_expired(reactor, &mut inner, Instant::now(), &mut to_wake);
        }

        for task in to_wake {
            green::wake(task);
        }
    }
}

/// the `epoll_wait` timeout for the next loop: milliseconds until the nearest
/// deadline (0 if already due), or `-1` to block until an fd event or a nudge.
fn next_timeout_ms(reactor: &Reactor) -> libc::c_int {
    let inner = lock_inner(reactor);
    match inner.deadlines.peek() {
        None => -1,
        Some(top) => {
            let now = Instant::now();
            if top.0.at <= now {
                0
            } else {
                let ms = (top.0.at - now).as_millis();
                if ms > libc::c_int::MAX as u128 {
                    libc::c_int::MAX
                } else {
                    // round up so we never wake a hair early and re-sleep.
                    ms as libc::c_int + 1
                }
            }
        }
    }
}

/// take all waiters for `(fd, interest)`, mark each ready, and queue it to wake.
/// clearing the list disarms this interest until a still-blocked task re-registers
/// (level-triggering means a genuinely-ready fd re-reports on the next arm).
fn take_ready(inner: &mut Inner, fd: RawFd, interest: Interest, to_wake: &mut Vec<usize>) {
    if let Some(waiters) = inner.waiters.remove(&(fd, interest)) {
        for w in waiters {
            // flip PENDING->READY; if a timeout already claimed it, leave it be.
            if w
                .outcome
                .compare_exchange(
                    PENDING,
                    READY,
                    AtomicOrdering::AcqRel,
                    AtomicOrdering::Acquire,
                )
                .is_ok()
            {
                to_wake.push(w.task);
            }
        }
    }
}

/// pop every deadline at or before `now`, mark the matching still-pending waiter
/// timed out, remove it, and queue it to wake. a stale heap entry (its waiter
/// already fired ready and was removed) simply matches nothing. a timer entry
/// has no fd, so it wakes its sleeper directly and never touches epoll.
fn collect_expired(reactor: &Reactor, inner: &mut Inner, now: Instant, to_wake: &mut Vec<usize>) {
    // track which fds we touch so we can re-arm/disarm them once at the end.
    let mut touched: Vec<RawFd> = Vec::new();
    while let Some(top) = inner.deadlines.peek() {
        if top.0.at > now {
            break;
        }
        let Deadline { seq, target, .. } = inner.deadlines.pop().unwrap().0;

        match target {
            DeadlineTarget::Timer => {
                // a sleep expiring is its success case; the same PENDING->
                // terminal compare-exchange keeps the resolve-once discipline
                // even though nothing else currently races for a timer.
                if let Some(w) = inner.timers.remove(&seq) {
                    if w
                        .outcome
                        .compare_exchange(
                            PENDING,
                            TIMED_OUT,
                            AtomicOrdering::AcqRel,
                            AtomicOrdering::Acquire,
                        )
                        .is_ok()
                    {
                        to_wake.push(w.task);
                    }
                }
            }
            DeadlineTarget::Io { fd, interest } => {
                if let Some(waiters) = inner.waiters.get_mut(&(fd, interest)) {
                    if let Some(pos) = waiters.iter().position(|w| w.seq == seq) {
                        let w = waiters.remove(pos);
                        if w
                            .outcome
                            .compare_exchange(
                                PENDING,
                                TIMED_OUT,
                                AtomicOrdering::AcqRel,
                                AtomicOrdering::Acquire,
                            )
                            .is_ok()
                        {
                            to_wake.push(w.task);
                        }
                        if waiters.is_empty() {
                            inner.waiters.remove(&(fd, interest));
                        }
                        if !touched.contains(&fd) {
                            touched.push(fd);
                        }
                    }
                }
            }
        }
    }
    for fd in touched {
        reconcile_fd(reactor, inner, fd);
    }
}

/// drain the eventfd's counter so its level-triggered readiness clears. the value
/// is irrelevant — it is a pure wakeup.
fn drain_eventfd(eventfd: RawFd) {
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

/// clean up the waiter registry for `fd` when it is closed, and WAKE any task
/// still parked on it with a closed/error outcome. epoll auto-removes a closed fd,
/// but a still-open dup could keep it alive, so we `EPOLL_CTL_DEL` explicitly
/// (ignoring ENOENT) too.
///
/// waking the parked waiters is the point: the common server teardown pattern is
/// one task closing a connection another task is blocked reading. dropping the
/// reader's registration silently (the old behavior) left its coroutine parked
/// forever — leaking its stack and slab slot and hanging anyone awaiting it. now
/// each parked waiter's outcome is flipped to `CLOSED` and the task is woken, so
/// its `wait_io` returns an error, its read/write unwinds, and its resources are
/// reclaimed. a `compare_exchange` from `PENDING` means a readiness or timeout
/// that already claimed the waiter still wins — we only close out a truly-pending
/// wait, never double-resolve one.
pub(crate) fn on_close(fd: RawFd) {
    // only touch the reactor if it was ever built; a process that never waited on
    // a socket has no registry to clean.
    let Some(reactor) = REACTOR.get() else {
        return;
    };

    // collect the tasks to wake, then wake them AFTER dropping the registry lock:
    // `green::wake` takes green's own slab/queue locks, and this lock must never
    // nest inside them (the module's lock discipline).
    let mut to_wake: Vec<usize> = Vec::new();
    {
        let mut inner = lock_inner(reactor);
        for interest in [Interest::Read, Interest::Write] {
            if let Some(waiters) = inner.waiters.remove(&(fd, interest)) {
                for w in waiters {
                    if w
                        .outcome
                        .compare_exchange(
                            PENDING,
                            CLOSED,
                            AtomicOrdering::AcqRel,
                            AtomicOrdering::Acquire,
                        )
                        .is_ok()
                    {
                        to_wake.push(w.task);
                    }
                }
            }
        }
        if inner.fds.remove(&fd).is_some() {
            // fd was in the epoll set; remove it. errors (already gone) ignored.
            epoll_apply(reactor.epfd, libc::EPOLL_CTL_DEL, fd, 0);
        }
        // any deadline heap entries for this fd are now stale; they match no
        // waiter and are dropped when they surface (same as a fired waiter's).
    }

    for task in to_wake {
        green::wake(task);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_ordering_is_soonest_first() {
        let now = Instant::now();
        let mut heap: BinaryHeap<std::cmp::Reverse<Deadline>> = BinaryHeap::new();
        heap.push(std::cmp::Reverse(Deadline {
            at: now + Duration::from_millis(50),
            seq: 1,
            target: DeadlineTarget::Io {
                fd: 3,
                interest: Interest::Read,
            },
        }));
        heap.push(std::cmp::Reverse(Deadline {
            at: now + Duration::from_millis(10),
            seq: 2,
            target: DeadlineTarget::Timer,
        }));
        // the nearer deadline (10ms) must come out first.
        assert_eq!(heap.pop().unwrap().0.seq, 2);
        assert_eq!(heap.pop().unwrap().0.seq, 1);
    }

    #[test]
    fn desired_mask_unions_interests() {
        let mut inner = Inner::new();
        assert_eq!(inner.desired_mask(7), 0);
        inner
            .waiters
            .entry((7, Interest::Read))
            .or_default()
            .push(Waiter {
                task: 0,
                seq: 0,
                outcome: Arc::new(AtomicI32::new(PENDING)),
            });
        assert_eq!(inner.desired_mask(7), libc::EPOLLIN as u32);
        inner
            .waiters
            .entry((7, Interest::Write))
            .or_default()
            .push(Waiter {
                task: 1,
                seq: 1,
                outcome: Arc::new(AtomicI32::new(PENDING)),
            });
        assert_eq!(
            inner.desired_mask(7),
            libc::EPOLLIN as u32 | libc::EPOLLOUT as u32
        );
    }

    // close-during-wait: a task blocked reading an fd that another task closes
    // must be woken with a CLOSED outcome (so its wait_io returns an error and the
    // task unwinds and reclaims its stack/slot), not silently dropped and parked
    // forever. we register a waiter by hand and drive on_close directly, since
    // wait_io itself needs a live coroutine to suspend into. the fd is a bogus
    // value that no real socket test uses, so the epoll DEL just fails harmlessly.
    #[test]
    fn on_close_wakes_parked_waiter_with_closed_outcome() {
        let reactor = reactor();
        let fd: RawFd = 424_242;
        let outcome = Arc::new(AtomicI32::new(PENDING));
        {
            let mut inner = lock_inner(reactor);
            let seq = inner.next_seq;
            inner.next_seq += 1;
            inner
                .waiters
                .entry((fd, Interest::Read))
                .or_default()
                .push(Waiter {
                    // a slab id far beyond any real task: green::wake finds no such
                    // slot and no-ops, which is all we need to exercise on_close.
                    task: usize::MAX,
                    seq,
                    outcome: outcome.clone(),
                });
            inner.fds.insert(fd, FdReg { armed: libc::EPOLLIN as u32 });
        }

        on_close(fd);

        // the waiter's outcome was flipped to CLOSED (wait_io maps that to -1), and
        // the registry no longer holds the waiter or the fd.
        assert_eq!(outcome.load(AtomicOrdering::Acquire), CLOSED);
        let inner = lock_inner(reactor);
        assert!(inner.waiters.get(&(fd, Interest::Read)).is_none());
        assert!(inner.fds.get(&fd).is_none());
    }

    // the two timer tests share the one process-global registry, so they must
    // not interleave with each other (cargo runs tests concurrently).
    static TIMER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // a timer registration must land in the timers map and the deadline heap,
    // and — once the sweep runs past it — resolve to TIMED_OUT with the entry
    // gone and no fd touched. driven by hand (register_timer + collect_expired)
    // because a real sleep_task needs a live coroutine to suspend into.
    #[test]
    fn timer_registers_expires_and_wakes() {
        let _guard = TIMER_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let reactor = reactor();
        let outcome = Arc::new(AtomicI32::new(PENDING));
        let at = Instant::now() + Duration::from_millis(5);
        register_timer(reactor, at, usize::MAX, &outcome);

        // identify our entry by its shared outcome cell, not by map emptiness:
        // the registry is global and other tests may hold entries of their own.
        let ours = |inner: &Inner| {
            inner
                .timers
                .values()
                .any(|w| Arc::ptr_eq(&w.outcome, &outcome))
        };

        let mut to_wake = Vec::new();
        {
            let mut inner = lock_inner(reactor);
            assert!(ours(&inner));

            // not due yet: the sweep leaves it alone.
            collect_expired(reactor, &mut inner, at - Duration::from_millis(1), &mut to_wake);
            assert!(to_wake.is_empty());
            assert_eq!(outcome.load(AtomicOrdering::Acquire), PENDING);

            // due: it resolves to TIMED_OUT, queues the task, and cleans up.
            collect_expired(reactor, &mut inner, at, &mut to_wake);
            assert!(!ours(&inner));
        }
        assert_eq!(to_wake, vec![usize::MAX]);
        assert_eq!(outcome.load(AtomicOrdering::Acquire), TIMED_OUT);
    }

    // the nudge decision: a timer nearer than the current top asks for one, a
    // farther timer rides on the reactor's existing sleep. asserted relative to
    // a timer of our own, since the global registry may hold other entries.
    #[test]
    fn timer_nudges_only_when_nearest() {
        let _guard = TIMER_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let reactor = reactor();
        let far = Instant::now() + Duration::from_secs(600);
        let outcome = Arc::new(AtomicI32::new(PENDING));
        register_timer(reactor, far, usize::MAX, &outcome);
        assert!(register_timer(reactor, far - Duration::from_secs(60), usize::MAX, &outcome));
        assert!(!register_timer(reactor, far + Duration::from_secs(60), usize::MAX, &outcome));
        // the far-future entries left behind are inert: nothing sweeps them
        // until long after every test in this process has finished, and their
        // task id (usize::MAX) makes any eventual wake a no-op.
    }

    // on_close must only close out a *pending* wait: a waiter a readiness or
    // timeout already claimed keeps its terminal outcome, so a close racing a
    // fire never double-resolves the wait.
    #[test]
    fn on_close_leaves_already_resolved_waiter() {
        let reactor = reactor();
        let fd: RawFd = 424_243;
        let outcome = Arc::new(AtomicI32::new(READY));
        {
            let mut inner = lock_inner(reactor);
            let seq = inner.next_seq;
            inner.next_seq += 1;
            inner
                .waiters
                .entry((fd, Interest::Write))
                .or_default()
                .push(Waiter {
                    task: usize::MAX,
                    seq,
                    outcome: outcome.clone(),
                });
        }

        on_close(fd);

        // still READY: on_close's compare_exchange only fires on a PENDING waiter.
        assert_eq!(outcome.load(AtomicOrdering::Acquire), READY);
    }
}
