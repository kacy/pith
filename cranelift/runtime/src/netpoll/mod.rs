//! epoll netpoller for the green-thread backend.
//!
//! under the green backend a small pool of worker OS threads runs many pith
//! tasks as stackful coroutines (see `concurrency::green`). every blocking
//! coordination op (channels, mutex, waitgroup, semaphore, await) yields the
//! coroutine back to its worker instead of parking the worker OS thread.
//! without a reactor, socket I/O would be the one hard park: a green task that
//! read from a socket with no data ready would block its whole worker on
//! `poll`, starving every other task queued on that worker.
//!
//! this module closes that gap. every green worker owns a poller: its own epoll
//! instance, eventfd and waiter registry. when a socket would block, the task
//! registers its (fd, interest) with the poller of the worker it is running on
//! and suspends its coroutine; the worker runs other tasks. the worker polls its
//! own epoll when it runs out of work, and parks *in* `epoll_wait`, so readiness
//! on one of its sockets wakes it directly. polling never resumes a coroutine:
//! it only calls `green::wake(task)`, which re-enqueues the task through the
//! scheduler's ordinary wake path.
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
//! ## epoll mode: edge-triggered, registered once
//!
//! the first wait on an fd adds it to the poller's epoll set for
//! `EPOLLIN | EPOLLOUT | EPOLLRDHUP | EPOLLET`, and it stays there, unchanged,
//! until `on_close` removes it. a wait therefore costs no `epoll_ctl` call. the
//! poller used to arm each fd level-triggered with `EPOLLONESHOT` and re-arm it
//! with an `EPOLL_CTL_MOD` on every wait that parked, which was one syscall per
//! request on a keep-alive connection. go's netpoller has the same shape as this
//! one.
//!
//! an edge is reported once, when the fd's readiness changes. that is enough
//! because a wait follows a syscall that returned `EWOULDBLOCK`: any data that
//! arrives after that syscall is a new edge. the edge either finds the task's
//! waiter and wakes it, or, if it is harvested while no waiter is registered for
//! that direction, sets a readiness bit on the fd (`FdReg::ready`). the next wait
//! in that direction consumes the bit and returns at once instead of parking, so
//! an edge that comes before the task parks is not lost. the first wait on an fd
//! is covered by the `EPOLL_CTL_ADD`, which reports readiness the fd already
//! has. a stale bit costs one spurious retry, which is always safe: the task
//! re-runs its syscall, and the syscall is the source of truth.
//!
//! a caller that waits without first seeing `EWOULDBLOCK` (`tcp_wait_readable`,
//! a terminal read) asks the fd with a zero-timeout `poll` before it waits; see
//! `fdio::wait_guarded_level`.
//!
//! ## the waiter registry
//!
//! each poller's `Mutex<Inner>` guards everything its worker and a closing thread
//! share:
//!
//! - `waiters`: for each `(fd, interest)`, the list of tasks blocked on it.
//!   multiple tasks may wait on one fd (e.g. several accept loops on a listener),
//!   so on readiness the poller wakes *all* waiters for that (fd, interest) and
//!   clears the list; each retries its syscall, one wins, the losers re-block.
//!   tasks on different workers that wait on one fd register it in each of their
//!   pollers; epoll allows one fd in several instances.
//! - `fds`: each fd in this poller's epoll set, with the readiness bits that
//!   edges left behind while nobody waited. an fd closed without `on_close`
//!   would leave a stale entry here, and a new fd that reused its number would
//!   never be added to the set, so every path that closes an fd a task may have
//!   waited on calls `on_close` first (`fd_handle::finish_close`,
//!   `network::close_raw`, the pidfd wait in `process`).
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
//! `epoll_wait` (which blocks) and never across `green::wake` (which takes a
//! green run-queue lock). a worker registering a waiter takes its poller's lock,
//! adds the fd to epoll under it if this is the fd's first wait, drops it, then
//! parks. a poll takes it only to drain fired events and expired deadlines into
//! a local list, drops it, then wakes. so this lock never nests inside green's
//! locks nor they inside it, so no cross-module lock cycle is possible.

use crate::concurrency::green;
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering as AtomicOrdering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

mod epoll;
mod registry;
#[cfg(test)]
mod tests;

use epoll::{close_in, poll, register_io, RegisterOutcome};
use registry::{register_timer, Inner, Interest, CLOSED, PENDING, READY, TIMED_OUT};

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
    /// the registry holds a waiter, a deadline or a timer, so a poll can find
    /// something.
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
    crate::sync::lock(&poller.inner)
}

fn pollers() -> &'static [Poller] {
    POLLERS.get_or_init(|| (0..green::pool_size()).map(|_| Poller::new()).collect())
}

/// the poller of worker `index`.
fn poller(index: usize) -> &'static Poller {
    &pollers()[index]
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
            let _g = crate::sync::lock(&poller.park);
            poller.park_cv.notify_one();
        }
        WAIT_EPOLL => {
            let one: u64 = 1;
            // SAFETY: writing 8 bytes to our own eventfd; EAGAIN (counter
            // saturated) is fine, since the worker will still see a pending
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
/// task (the caller checks `green::current_task`); `task` is that task's id.
///
/// `timeout_ms < 0` means wait forever (no deadline registered).
///
/// `live` says whether the handle behind `fd` is still open. it is asked once
/// the waiter is registered and before the task parks, which closes the last
/// gap in the close teardown: a close that lands before the registration runs
/// `on_close` on an empty list and would leave this task parked with nobody to
/// wake it. a closer marks the handle dead before it calls `on_close`, and the
/// waiter registers before it asks, so one of the two always sees the other:
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
    let Some(index) = green::current_worker() else {
        return -1;
    };
    let poller = poller(index);

    let deadline = if timeout_ms < 0 {
        None
    } else if timeout_ms == 0 {
        Some(Instant::now())
    } else {
        crate::fdio::deadline_after_ms(timeout_ms)
    };

    // register under the lock. no notify is needed for the deadline: this
    // worker is the one that polls this registry, and it reads the nearest
    // deadline when it next parks.
    let reg = {
        let mut inner = lock_inner(poller);
        let reg = register_io(poller, &mut inner, fd, interest, deadline, task);
        inner.publish(&poller.busy);
        reg
    };
    let (seq, outcome) = match reg {
        RegisterOutcome::Ready => {
            // an edge came while nobody waited; the caller's retry will see it.
            return if live() { 1 } else { -1 };
        }
        RegisterOutcome::Failed => {
            // epoll_ctl refused this fd; parking would wait forever.
            return -1;
        }
        RegisterOutcome::Parked(seq, outcome) => (seq, outcome),
    };
    if !live() {
        // every waiter on this fd waits on the same dead handle (the number
        // cannot have moved on while this call holds it), so the ordinary close
        // teardown resolves them all, this one included; the wake it queues
        // for us is caught by the park below.
        on_close(fd);
    }

    // suspend the coroutine back to the worker. `green::wake` (from a poll)
    // re-enqueues us; even if that wake lands before we finish suspending,
    // green's `wake_pending` flag re-enqueues us from the park path, so it is
    // never lost. thus when `park_current` returns, `outcome` has been set.
    green::park_current(task);

    let res = outcome.load(AtomicOrdering::Acquire);
    if res == PENDING {
        // spurious wake before a terminal outcome: remove our waiter so a stale
        // entry cannot later wake this task out of an unrelated park.
        let mut inner = lock_inner(poller);
        inner.remove_waiter(fd, interest, seq);
        inner.publish(&poller.busy);
    }
    match res {
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
/// and suspend; the deadline sweep wakes us. must be called only from inside a
/// green task (the caller checks `green::current_task`); `task` is that task's
/// id.
///
/// a zero or negative duration returns immediately: there is nothing to wait
/// for, so nothing is registered.
pub(crate) fn sleep_task(ms: i64, task: usize) {
    if ms <= 0 {
        return;
    }
    let Some(index) = green::current_worker() else {
        let clamped = ms.min(crate::fdio::MAX_WAIT_MS) as u64;
        std::thread::sleep(Duration::from_millis(clamped));
        return;
    };
    let outcome = Arc::new(AtomicI32::new(PENDING));
    let Some(at) = crate::fdio::deadline_after_ms(ms) else {
        return;
    };
    register_timer(poller(index), at, task, &outcome);

    // park until the sweep marks us done. unlike `wait_io`, where a spurious
    // wake is safe because the task re-runs its syscall, a sleep has no
    // syscall to act as the source of truth, so an early resume must go back
    // to sleep: our timer entry is still registered and the sweep will wake us
    // again. once the outcome is terminal the entry is gone and we return.
    while outcome.load(AtomicOrdering::Acquire) == PENDING {
        green::park_current(task);
    }
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
/// a worker with waiters, deadlines or timers registered waits in
/// `epoll_wait`. one with nothing registered waits on its condvar instead,
/// which is the cheaper wake; nothing can register while it waits, because
/// only the worker itself registers on its poller.
pub(crate) fn idle_wait(index: usize, timeout: Duration) -> bool {
    let poller = poller(index);
    let woke = if poller.busy.load(AtomicOrdering::SeqCst) {
        poller.mode.store(WAIT_EPOLL, AtomicOrdering::SeqCst);
        !poller.notified.load(AtomicOrdering::SeqCst) && poll(index, timeout)
    } else {
        let guard = crate::sync::lock(&poller.park);
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
/// clean up the waiter registry for `fd` when it is closed, and WAKE any task
/// still parked on it with a closed/error outcome. epoll auto-removes a closed fd,
/// but a still-open dup could keep it alive, so we `EPOLL_CTL_DEL` explicitly
/// (ignoring ENOENT) too.
///
/// waking the parked waiters is the point: the common server teardown pattern is
/// one task closing a connection another task is blocked reading. dropping the
/// reader's registration silently would leave its coroutine parked forever,
/// leaking its stack and task id and hanging anyone awaiting it. instead each
/// parked waiter's outcome is flipped to `CLOSED` and the task is woken, so
/// its `wait_io` returns an error, its read/write unwinds, and its resources are
/// reclaimed. a readiness or timeout that already claimed the waiter still wins
/// (see `Waiter::resolve`): we only close out a truly-pending wait.
pub(crate) fn on_close(fd: RawFd) {
    // only touch the pollers if they were ever built; a process that never ran
    // a green worker has no registry to clean. any worker's poller may hold a
    // registration for this fd, so each is checked.
    let Some(pollers) = POLLERS.get() else {
        return;
    };

    // collect the tasks to wake, then wake them AFTER dropping the registry lock:
    // `green::wake` takes a green run-queue lock, and this lock must never nest
    // with it (the module's lock discipline).
    let mut to_wake: Vec<usize> = Vec::new();
    for poller in pollers {
        close_in(poller, fd, &mut to_wake);
    }

    for task in to_wake {
        green::wake(task);
    }
}
