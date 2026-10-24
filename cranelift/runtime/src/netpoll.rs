//! epoll netpoller for the green-thread backend.
//!
//! under `PITH_GREEN` a small pool of worker OS threads runs many pith tasks as
//! stackful coroutines (see `concurrency::green`). every blocking coordination op
//! — channels, mutex, waitgroup, semaphore, await — already yields the coroutine
//! back to its worker instead of parking the worker OS thread. socket I/O was the
//! last hard park: a green task that read from a socket with no data ready blocked
//! its whole worker on `poll`, starving every other task pinned to that worker.
//!
//! this module closes that gap. every green worker owns a poller: its own epoll
//! instance, eventfd and waiter registry. when a socket would block, the task
//! registers its (fd, interest) with the poller of the worker it is running on
//! and suspends its coroutine; the worker runs other tasks. the worker polls its
//! own epoll when it runs out of work, and parks *in* `epoll_wait`, so readiness
//! on one of its sockets wakes it directly. polling never resumes a coroutine:
//! it only calls `green::wake(task)`, which re-enqueues the task onto its owner
//! worker's pinned queue, so green's pinning discipline holds unchanged.
//!
//! this replaced one shared epoll instance, one registry lock and one dedicated
//! reactor thread. every wait from every worker took that lock and made its
//! `epoll_ctl` call under it, and every readiness crossed from the reactor
//! thread to a worker. with 32 workers the reactor could barely get its own lock
//! back to deliver events, and the workers spent most of their cpu in the
//! kernel. a per-worker poller is the thread-per-core shape seastar and glommio
//! use for i/o: the lock is only ever contended by a close from another thread,
//! and a readiness costs no cross-thread hop.
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
//! each poller's `Mutex<Inner>` guards everything its worker and a closing thread
//! share:
//!
//! - `waiters`: for each `(fd, interest)`, the list of tasks blocked on it.
//!   multiple tasks may wait on one fd — e.g. several accept loops on a listener —
//!   so on readiness the poller wakes *all* waiters for that (fd, interest) and
//!   clears the list; each retries its syscall, one wins, the losers re-block.
//!   tasks on different workers that wait on one fd register it in each of their
//!   pollers; epoll allows one fd in several instances.
//! - `fds`: for each fd currently in the epoll set, the event mask presently
//!   armed. arming is always recomputed from the waiter lists (`reconcile_fd`):
//!   the first registrant on an fd does `EPOLL_CTL_ADD`, and a change in the
//!   desired interest set (or re-arming after a oneshot fire) does
//!   `EPOLL_CTL_MOD`. an fd stays in the set, disarmed, once its last waiter
//!   leaves, so a connection that waits once per request pays one `MOD` per
//!   wait instead of an `ADD` and a `DEL`; only `on_close` removes it. keeping
//!   the armed mask a function of the waiter lists is what makes the
//!   arm/disarm bookkeeping easy to reason about.
//! - `deadlines`: a min-heap of timestamped entries for waits that carry a
//!   finite timeout. a parking worker caps its `epoll_wait` timeout at the nearest
//!   deadline and, on expiry, wakes the task with a timeout result. an entry
//!   targets either an fd wait or a pure timer.
//! - `timers`: tasks sleeping with no fd at all (`sleep_task`, behind
//!   `pith_sleep`). a sleep is a deadline wait minus the fd: same heap, same
//!   sweep, nothing armed in epoll.
//!
//! ## the eventfd notify
//!
//! each poller's eventfd is registered in its epoll (level-triggered, no
//! oneshot). it is how the scheduler wakes a parked worker for new work (see
//! `notify`): the write makes the worker's `epoll_wait` return even if it lands
//! before the wait starts. a deadline never needs one, because only the worker
//! itself registers on its poller, and it is running a task when it does. its
//! next park reads the new deadline.
//!
//! ## lock discipline
//!
//! the only lock here is each poller's `Inner` mutex. it is never held across
//! `epoll_wait` (which blocks) and never across `green::wake` (which takes green's
//! own slab/queue locks). a worker registering a waiter takes its poller's lock,
//! arms epoll under it, drops it, then parks. a poll takes it only to drain fired
//! events and expired deadlines into a local list, drops it, then wakes. so this
//! lock never nests inside green's locks nor they inside it — no cross-module lock
//! cycle is possible.

use crate::concurrency::green;
use std::cmp::Ordering;
use crate::int_hash::IntMap;
use std::collections::BinaryHeap;
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering as AtomicOrdering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
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

// outcome of a wait, shared between the waiting task and the poller via an
// `AtomicI32`. it starts `PENDING`; whichever poller path handles the waiter
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
    /// shared result cell; the poller writes `READY` or `TIMED_OUT` here before
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
/// while it is in the epoll interest set. `armed` is the event mask epoll
/// currently reports on; it drops to 0 when a oneshot fire disarms the fd, which
/// tells `reconcile_fd` it must re-`MOD` to arm any remaining interest.
struct FdReg {
    armed: u32,
}

/// one poller's registry, behind its mutex. the maps are keyed by fd numbers
/// and sequence numbers, so they use the runtime's integer hasher; siphash
/// would run inside the lock on every park and wake.
struct Inner {
    waiters: IntMap<(RawFd, Interest), Vec<Waiter>>,
    fds: IntMap<RawFd, FdReg>,
    /// how many entries of `fds` are armed. a disarmed fd cannot report
    /// anything, so only an armed one gives a poll something to find.
    armed_fds: usize,
    deadlines: BinaryHeap<std::cmp::Reverse<Deadline>>,
    /// sleeping tasks, keyed by the seq of their heap entry. a timer has no
    /// (fd, interest) to key on, and giving it its own map keeps it out of
    /// `waiters` — where everything is fd-shaped — entirely.
    timers: IntMap<u64, Waiter>,
    next_seq: u64,
    /// the heap size at which the next push first drops the entries whose
    /// waits have ended. see `push_deadline`.
    compact_at: usize,
}

/// the smallest heap `push_deadline` compacts: below it the stale entries
/// cost less than the scan that would drop them.
const DEADLINE_COMPACT_MIN: usize = 1024;

impl Inner {
    fn new() -> Self {
        Inner {
            waiters: IntMap::default(),
            fds: IntMap::default(),
            armed_fds: 0,
            deadlines: BinaryHeap::new(),
            timers: IntMap::default(),
            next_seq: 0,
            compact_at: DEADLINE_COMPACT_MIN,
        }
    }

    /// whether a deadline entry still has a wait to time out. a wait that
    /// fired ready, or was closed, left its entry behind in the heap.
    fn deadline_is_live(&self, deadline: &Deadline) -> bool {
        match deadline.target {
            DeadlineTarget::Timer => self.timers.contains_key(&deadline.seq),
            DeadlineTarget::Io { fd, interest } => self
                .waiters
                .get(&(fd, interest))
                .is_some_and(|waiters| waiters.iter().any(|w| w.seq == deadline.seq)),
        }
    }

    /// add a deadline to the heap. a wait that ends before its deadline
    /// leaves its entry in the heap until the deadline passes, so a server
    /// that gives every read a 5 s timeout kept five seconds of requests
    /// there: hundreds of megabytes across the workers, and a pop that walked
    /// a heap far larger than the cache. so once the heap reaches
    /// `compact_at`, the push first drops the stale entries and sets the next
    /// threshold at twice what is left, which keeps the heap within a small
    /// multiple of the live waits for a constant amortized cost per push.
    fn push_deadline(&mut self, deadline: Deadline) {
        if self.deadlines.len() >= self.compact_at {
            let entries = std::mem::take(&mut self.deadlines).into_vec();
            let live: Vec<_> = entries
                .into_iter()
                .filter(|entry| self.deadline_is_live(&entry.0))
                .collect();
            self.deadlines = BinaryHeap::from(live);
            self.compact_at = (2 * self.deadlines.len()).max(DEADLINE_COMPACT_MIN);
        }
        self.deadlines.push(std::cmp::Reverse(deadline));
    }

    /// record in `busy` whether this registry has anything a poll could find.
    /// called at the end of every locked change.
    fn publish(&self, busy: &AtomicBool) {
        let any = self.armed_fds > 0 || !self.deadlines.is_empty() || !self.timers.is_empty();
        busy.store(any, AtomicOrdering::SeqCst);
    }

    /// record that `fd` is in the epoll set armed for `mask` (0: disarmed).
    fn set_armed(&mut self, fd: RawFd, mask: u32) {
        let was = self.fds.insert(fd, FdReg { armed: mask }).map_or(0, |r| r.armed);
        self.armed_fds = self.armed_fds + usize::from(mask != 0) - usize::from(was != 0);
    }

    /// forget `fd`; returns whether it was in the epoll set.
    fn forget_fd(&mut self, fd: RawFd) -> bool {
        match self.fds.remove(&fd) {
            Some(reg) => {
                self.armed_fds -= usize::from(reg.armed != 0);
                true
            }
            None => false,
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

/// one worker's poller: its epoll fd, the eventfd that wakes it, and its
/// registry.
struct Poller {
    epfd: RawFd,
    eventfd: RawFd,
    /// a `notify` the worker has not consumed yet. it outlives a notify that
    /// lands before the worker starts waiting, so none is lost, and a burst of
    /// notifies for one parked worker costs one wake.
    notified: AtomicBool,
    /// how the worker is waiting, so `notify` knows how to wake it: not at
    /// all, on `park_cv`, or in `epoll_wait`.
    mode: AtomicU8,
    /// the futex park, for a worker with nothing registered with epoll. a
    /// condvar wake is cheaper than an eventfd write and an epoll wakeup, and
    /// the channel fan-out benchmark, which crosses workers on every message
    /// and does no i/o, ran about a third slower with every park in epoll.
    park: Mutex<()>,
    park_cv: Condvar,
    /// the registry holds an fd or a deadline, so a poll can find something.
    /// set under the lock after every change (`Inner::publish`). an idle worker
    /// checks it before polling, so a worker with no sockets and no sleepers
    /// pays no syscall and no lock on its way to park.
    busy: AtomicBool,
    inner: Mutex<Inner>,
}

/// `Poller::mode` values.
const WAIT_NONE: u8 = 0;
const WAIT_FUTEX: u8 = 1;
const WAIT_EPOLL: u8 = 2;

/// one poller per green worker, built with the worker pool, indexed like it.
static POLLERS: OnceLock<Vec<Poller>> = OnceLock::new();

fn lock_inner(poller: &Poller) -> std::sync::MutexGuard<'_, Inner> {
    poller.inner.lock().unwrap_or_else(|p| p.into_inner())
}

fn pollers() -> &'static [Poller] {
    POLLERS.get_or_init(|| (0..green::pool_size()).map(|_| Poller::new()).collect())
}

/// the poller of worker `index`.
fn poller(index: usize) -> &'static Poller {
    &pollers()[index]
}

impl Poller {
    fn new() -> Self {
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

/// arm epoll for `fd` so it reports what the current waiter lists want. called
/// under `inner` after any change to `fd`'s waiters (register, fire, timeout).
///
/// - waiters, fd not in the set -> `EPOLL_CTL_ADD` with `EPOLLONESHOT | desired`.
/// - waiters, fd in the set, mask changed (a new interest, or re-arming after a
///   oneshot fire dropped `armed` to 0) -> `EPOLL_CTL_MOD`.
/// - waiters, fd in the set, mask unchanged -> nothing (a second waiter on an
///   already-armed interest just rode along on the existing arm).
/// - no waiters -> nothing. a fired fd is already disarmed, and one whose last
///   waiter timed out may fire once more with nobody to wake, which costs a
///   spurious poll and is cheaper than a `DEL` now and an `ADD` on the next
///   wait.
///
/// an fd closed without `on_close` leaves a stale entry here, and the kernel
/// has already dropped it from the set. if its number comes back as a new fd,
/// the `MOD` fails with `ENOENT` and is retried as an `ADD`; the reverse case
/// retries an `ADD` that fails with `EEXIST` as a `MOD`.
fn reconcile_fd(poller: &Poller, inner: &mut Inner, fd: RawFd) {
    let desired = inner.desired_mask(fd);
    if desired == 0 {
        return;
    }
    match inner.fds.get(&fd).map(|reg| reg.armed) {
        None => {
            if epoll_apply(poller.epfd, libc::EPOLL_CTL_ADD, fd, desired) == libc::EEXIST {
                epoll_apply(poller.epfd, libc::EPOLL_CTL_MOD, fd, desired);
            }
        }
        Some(armed) if armed != desired => {
            if epoll_apply(poller.epfd, libc::EPOLL_CTL_MOD, fd, desired) == libc::ENOENT {
                epoll_apply(poller.epfd, libc::EPOLL_CTL_ADD, fd, desired);
            }
        }
        Some(_) => return,
    }
    inner.set_armed(fd, desired);
}

/// run one `epoll_ctl` op and return its errno, or 0 on success. `DEL` ignores
/// its mask. the caller decides which errors matter: a `DEL` on an
/// already-closed fd races `pith_tcp_close` (ENOENT/EBADF, expected), and an
/// `ADD`/`MOD` failure just means the task falls back to a spurious retry when
/// no wake ever comes — never a crash.
fn epoll_apply(epfd: RawFd, op: libc::c_int, fd: RawFd, mask: u32) -> i32 {
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

/// wake worker `index` out of `idle_wait`. a no-op when a notify is already
/// pending, so a burst of wakes for one parked worker costs one syscall.
///
/// the flag is stored before the wait mode is read, and `idle_wait` stores the
/// mode before it reads the flag, so either this sees the worker waiting and
/// wakes it, or the worker sees the flag and does not wait.
pub(crate) fn notify(index: usize) {
    let poller = poller(index);
    if poller.notified.swap(true, AtomicOrdering::SeqCst) {
        return;
    }
    match poller.mode.load(AtomicOrdering::SeqCst) {
        WAIT_FUTEX => {
            // taking the lock orders this against the worker's check-then-wait.
            let _g = poller.park.lock().unwrap_or_else(|p| p.into_inner());
            poller.park_cv.notify_one();
        }
        WAIT_EPOLL => {
            let one: u64 = 1;
            // SAFETY: writing 8 bytes to our own eventfd; EAGAIN (counter
            // saturated) is fine — the worker will still see a pending
            // readiness and wake.
            unsafe {
                libc::write(
                    poller.eventfd,
                    &one as *const u64 as *const libc::c_void,
                    std::mem::size_of::<u64>(),
                );
            }
        }
        _ => {}
    }
}

/// block the current green task until `fd` is ready for `read`/write, it times
/// out, or an error is detected. returns the tri-state readiness contract:
/// `1` ready, `0` timed out, `-1` error. must be called only from inside a green
/// task (the caller checks `green::current_task`); `task` is that task's slab id.
///
/// `timeout_ms < 0` means wait forever (no deadline registered).
///
/// `live` says whether the handle behind `fd` is still open. it is asked once
/// the waiter is registered and before the task parks, which closes the last
/// gap in the close teardown: a close that lands before the registration runs
/// `on_close` on an empty list and would leave this task parked with nobody to
/// wake it. a closer marks the handle dead before it calls `on_close`, and the
/// waiter registers before it asks, so one of the two always sees the other —
/// either `on_close` finds the waiter, or the waiter finds the handle dead and
/// resolves itself here. a raw fd, which nothing closes underneath, answers
/// true.
pub(crate) fn wait_io(
    fd: RawFd,
    read: bool,
    timeout_ms: i64,
    task: usize,
    live: &dyn Fn() -> bool,
) -> i64 {
    let interest = if read { Interest::Read } else { Interest::Write };
    // a green task is always on a worker, so this is the poller of the worker
    // the task is running on: the one that will poll for it.
    let Some(index) = green::worker_index() else {
        return -1;
    };
    let poller = poller(index);
    let outcome = Arc::new(AtomicI32::new(PENDING));

    let deadline = if timeout_ms < 0 {
        None
    } else {
        Some(Instant::now() + Duration::from_millis(timeout_ms as u64))
    };

    // register under the lock: allocate a seq, push the waiter, record any
    // deadline, and arm epoll. no notify is needed for the deadline: this
    // worker is the one that polls this registry, and it reads the nearest
    // deadline when it next parks.
    {
        let mut inner = lock_inner(poller);
        let seq = inner.next_seq;
        inner.next_seq += 1;

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
            inner.push_deadline(Deadline {
                at,
                seq,
                target: DeadlineTarget::Io { fd, interest },
            });
        }
        reconcile_fd(poller, &mut inner, fd);
        inner.publish(&poller.busy);
    }
    if !live() {
        // every waiter on this fd waits on the same dead handle — the number
        // cannot have moved on while this call holds it — so the ordinary close
        // teardown resolves them all, this one included; the wake it queues
        // for us is caught by the park below.
        on_close(fd);
    }

    // suspend the coroutine back to the worker. `green::wake` (from a poll)
    // re-enqueues us; even if that wake lands before we finish suspending,
    // green's `wake_pending` flag re-enqueues us from the park path, so it is
    // never lost. thus when `park_current` returns, `outcome` has been set.
    green::park_current(task);

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
/// this is `wait_io` minus the fd: register a deadline on this worker's poller
/// and suspend; the deadline sweep wakes us. must be
/// called only from inside a green task (the caller checks
/// `green::current_task`); `task` is that task's slab id.
///
/// a zero or negative duration returns immediately — there is nothing to wait
/// for, so nothing is registered.
pub(crate) fn sleep_task(ms: i64, task: usize) {
    if ms <= 0 {
        return;
    }
    let Some(index) = green::worker_index() else {
        std::thread::sleep(Duration::from_millis(ms as u64));
        return;
    };
    let outcome = Arc::new(AtomicI32::new(PENDING));
    let at = Instant::now() + Duration::from_millis(ms as u64);
    register_timer(poller(index), at, task, &outcome);

    // park until the sweep marks us done. unlike `wait_io` — where a spurious
    // wake is safe because the task re-runs its syscall — a sleep has no
    // syscall to act as the source of truth, so an early resume must go back
    // to sleep: our timer entry is still registered and the sweep will wake us
    // again. once the outcome is terminal the entry is gone and we return.
    while outcome.load(AtomicOrdering::Acquire) == PENDING {
        green::park_current(task);
    }
}

/// the registration half of `sleep_task`, split out so it can be exercised
/// without a live coroutine to suspend. pushes a timer deadline and its waiter
/// under the lock.
fn register_timer(poller: &Poller, at: Instant, task: usize, outcome: &Arc<AtomicI32>) {
    let mut inner = lock_inner(poller);
    let seq = inner.next_seq;
    inner.next_seq += 1;

    inner.timers.insert(
        seq,
        Waiter {
            task,
            seq,
            outcome: outcome.clone(),
        },
    );
    inner.push_deadline(Deadline {
        at,
        seq,
        target: DeadlineTarget::Timer,
    });
    inner.publish(&poller.busy);
}

/// run worker `index`'s poller once: wait up to `timeout` (zero polls without
/// blocking), wake the tasks whose fds are ready or whose deadlines expired,
/// and re-arm what is left. returns whether anything happened — a readiness,
/// an expired deadline or a notify — as opposed to the wait running out.
/// never resumes a coroutine.
///
/// the wait is capped at the poller's nearest deadline, so a parked worker
/// wakes in time to expire it.
fn poll(index: usize, timeout: Duration) -> bool {
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

    // collect every task to wake into a local list, then wake them AFTER
    // dropping the registry lock — `green::wake` takes green's own locks, and
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
            let fd = token as RawFd;

            // the oneshot fire disarmed this fd in the kernel; record that so
            // reconcile_fd re-arms any interest that still has waiters.
            if inner.fds.contains_key(&fd) {
                inner.set_armed(fd, 0);
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
            reconcile_fd(poller, &mut inner, fd);
        }

        // service expired deadlines in the same lock hold.
        if !inner.deadlines.is_empty() {
            collect_expired(poller, &mut inner, Instant::now(), &mut to_wake);
        }
        inner.publish(&poller.busy);
    }

    let woke = !to_wake.is_empty();
    for task in to_wake {
        green::wake(task);
    }
    woke || notified
}

/// poll worker `index`'s sockets without blocking. a busy worker calls this
/// between tasks so the tasks whose sockets became ready get queued even while
/// it always has other work. returns whether any task was woken. free when the
/// worker has nothing registered.
pub(crate) fn poll_ready(index: usize) -> bool {
    if !poller(index).busy.load(AtomicOrdering::SeqCst) {
        return false;
    }
    poll(index, Duration::ZERO)
}

/// park worker `index` until one of its sockets is ready, a deadline expires,
/// `notify` is called, or `timeout` runs out. returns true when the wait ran
/// out with nothing to do.
///
/// a worker with sockets or deadlines registered waits in `epoll_wait`. one
/// with nothing registered waits on its condvar instead, which is the cheaper
/// wake; nothing can register while it waits, because only the worker itself
/// registers on its poller.
pub(crate) fn idle_wait(index: usize, timeout: Duration) -> bool {
    let poller = poller(index);
    let woke = if poller.busy.load(AtomicOrdering::SeqCst) {
        poller.mode.store(WAIT_EPOLL, AtomicOrdering::SeqCst);
        !poller.notified.load(AtomicOrdering::SeqCst) && poll(index, timeout)
    } else {
        let guard = poller.park.lock().unwrap_or_else(|p| p.into_inner());
        poller.mode.store(WAIT_FUTEX, AtomicOrdering::SeqCst);
        let _ = poller
            .park_cv
            .wait_timeout_while(guard, timeout, |_| {
                !poller.notified.load(AtomicOrdering::SeqCst)
            })
            .unwrap_or_else(|p| p.into_inner());
        false
    };
    poller.mode.store(WAIT_NONE, AtomicOrdering::SeqCst);
    let notified = poller.notified.swap(false, AtomicOrdering::SeqCst);
    !(woke || notified)
}

/// the `epoll_wait` timeout for one poll: `cap`, or less if a deadline is due
/// sooner (0 if one is already due).
fn next_timeout_ms(poller: &Poller, cap: Duration) -> libc::c_int {
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
fn collect_expired(poller: &Poller, inner: &mut Inner, now: Instant, to_wake: &mut Vec<usize>) {
    // track which fds we touch so we can re-arm/disarm them once at the end.
    let mut touched: Vec<RawFd> = Vec::new();
    while let Some(top) = inner.deadlines.peek() {
        if top.0.at > now {
            break;
        }
        // panic-guard: `peek` above returned Some under this same `&mut Inner`, so the pop cannot fail.
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
        reconcile_fd(poller, inner, fd);
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
    // only touch the pollers if they were ever built; a process that never ran
    // a green worker has no registry to clean. any worker's poller may hold a
    // registration for this fd, so each is checked.
    let Some(pollers) = POLLERS.get() else {
        return;
    };

    // collect the tasks to wake, then wake them AFTER dropping the registry lock:
    // `green::wake` takes green's own slab/queue locks, and this lock must never
    // nest inside them (the module's lock discipline).
    let mut to_wake: Vec<usize> = Vec::new();
    for poller in pollers {
        let mut inner = lock_inner(poller);
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
        if inner.forget_fd(fd) {
            // fd was in the epoll set; remove it. errors (already gone) ignored.
            epoll_apply(poller.epfd, libc::EPOLL_CTL_DEL, fd, 0);
        }
        inner.publish(&poller.busy);
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
        let poller = poller(0);
        let fd: RawFd = 424_242;
        let outcome = Arc::new(AtomicI32::new(PENDING));
        {
            let mut inner = lock_inner(poller);
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
            inner.set_armed(fd, libc::EPOLLIN as u32);
        }

        on_close(fd);

        // the waiter's outcome was flipped to CLOSED (wait_io maps that to -1), and
        // the registry no longer holds the waiter or the fd.
        assert_eq!(outcome.load(AtomicOrdering::Acquire), CLOSED);
        let inner = lock_inner(poller);
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
        let poller = poller(0);
        let outcome = Arc::new(AtomicI32::new(PENDING));
        let at = Instant::now() + Duration::from_millis(5);
        register_timer(poller, at, usize::MAX, &outcome);

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
            let mut inner = lock_inner(poller);
            assert!(ours(&inner));

            // not due yet: the sweep leaves it alone.
            collect_expired(poller, &mut inner, at - Duration::from_millis(1), &mut to_wake);
            assert!(to_wake.is_empty());
            assert_eq!(outcome.load(AtomicOrdering::Acquire), PENDING);

            // due: it resolves to TIMED_OUT, queues the task, and cleans up.
            collect_expired(poller, &mut inner, at, &mut to_wake);
            assert!(!ours(&inner));
        }
        assert_eq!(to_wake, vec![usize::MAX]);
        assert_eq!(outcome.load(AtomicOrdering::Acquire), TIMED_OUT);
    }

    // on_close must only close out a *pending* wait: a waiter a readiness or
    // timeout already claimed keeps its terminal outcome, so a close racing a
    // fire never double-resolves the wait.
    #[test]
    fn on_close_leaves_already_resolved_waiter() {
        let poller = poller(0);
        let fd: RawFd = 424_243;
        let outcome = Arc::new(AtomicI32::new(READY));
        {
            let mut inner = lock_inner(poller);
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

    fn socket_pair() -> (RawFd, RawFd) {
        let mut fds = [0; 2];
        // SAFETY: socketpair writes two fds into the array we pass.
        let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        assert_eq!(rc, 0);
        (fds[0], fds[1])
    }

    #[test]
    fn deadlines_of_finished_waits_do_not_pile_up() {
        let mut inner = Inner::new();
        let far = Instant::now() + Duration::from_secs(60);
        let fd: RawFd = 424_250;
        // one wait stays pending the whole time, so its deadline must survive
        // every compaction.
        let kept = inner.next_seq;
        inner.next_seq += 1;
        inner.waiters.entry((fd + 1, Interest::Read)).or_default().push(Waiter {
            task: usize::MAX,
            seq: kept,
            outcome: Arc::new(AtomicI32::new(PENDING)),
        });
        inner.push_deadline(Deadline {
            at: far,
            seq: kept,
            target: DeadlineTarget::Io { fd: fd + 1, interest: Interest::Read },
        });
        for _ in 0..20_000 {
            // a wait that registers a deadline and then fires ready.
            add_read_waiter(&mut inner, fd);
            let seq = inner.next_seq - 1;
            inner.push_deadline(Deadline {
                at: far,
                seq,
                target: DeadlineTarget::Io { fd, interest: Interest::Read },
            });
            inner.waiters.remove(&(fd, Interest::Read));
        }
        assert!(inner.deadlines.len() <= DEADLINE_COMPACT_MIN, "{} entries", inner.deadlines.len());
        assert!(inner.deadlines.iter().any(|entry| entry.0.seq == kept));
    }

    fn add_read_waiter(inner: &mut Inner, fd: RawFd) {
        let seq = inner.next_seq;
        inner.next_seq += 1;
        inner.waiters.entry((fd, Interest::Read)).or_default().push(Waiter {
            task: usize::MAX,
            seq,
            outcome: Arc::new(AtomicI32::new(PENDING)),
        });
    }

    // a fired fd stays in the set disarmed, and a later wait re-arms it. if the
    // fd was closed without `on_close` and its number reused, the stale entry
    // must not swallow the new fd's arm: the MOD fails and is retried as an ADD.
    #[test]
    fn rearm_survives_a_stale_fd_entry() {
        let poller = Poller::new();
        let ready = |poller: &Poller| {
            let mut ev = [libc::epoll_event { events: 0, u64: 0 }; 4];
            // SAFETY: a zero-timeout wait on our own epoll fd into a local buffer.
            let n = unsafe { libc::epoll_wait(poller.epfd, ev.as_mut_ptr(), 4, 0) };
            ev[..n.max(0) as usize].iter().map(|e| e.u64).collect::<Vec<u64>>()
        };
        let (a, b) = socket_pair();
        // SAFETY: writing one byte to a socket we own.
        unsafe { libc::write(b, b"x".as_ptr().cast(), 1) };
        let mut inner = lock_inner(&poller);
        add_read_waiter(&mut inner, a);
        reconcile_fd(&poller, &mut inner, a);
        assert_eq!(ready(&poller), vec![a as u64]);

        // what a poll does with that fire: drop the waiters, mark it disarmed.
        inner.waiters.clear();
        inner.set_armed(a, 0);
        reconcile_fd(&poller, &mut inner, a);
        assert!(inner.fds.contains_key(&a), "a fired fd stays in the set");
        assert_eq!(inner.armed_fds, 0);

        // close both ends behind the registry's back and take the number again.
        // SAFETY: closing fds this test opened.
        unsafe {
            libc::close(a);
            libc::close(b);
        }
        let (c, d) = socket_pair();
        if c != a {
            // something else took the number; nothing to check.
            unsafe {
                libc::close(c);
                libc::close(d);
            }
            return;
        }
        // SAFETY: as above.
        unsafe { libc::write(d, b"x".as_ptr().cast(), 1) };
        add_read_waiter(&mut inner, c);
        reconcile_fd(&poller, &mut inner, c);
        assert_eq!(inner.armed_fds, 1);
        assert_eq!(ready(&poller), vec![c as u64], "the reused fd must be armed");
        drop(inner);
        unsafe {
            libc::close(c);
            libc::close(d);
        }
    }
}
