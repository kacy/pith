//! stand-in for the epoll reactor on platforms without epoll (macOS, the BSDs).
//!
//! the real reactor (`netpoll`) is built on `epoll` and `eventfd`, which are
//! linux-only. this module presents the same entry points so `network.rs` and
//! the green scheduler stay platform-agnostic — `lib.rs` picks one or the other
//! at compile time.
//!
//! the tradeoff: there is no reactor here, so a green task that waits on a
//! socket blocks its worker OS thread on `poll` for the duration of the wait,
//! exactly as every green task did before the reactor landed. correctness is
//! unaffected — `wait_io` honors the same tri-state contract, and reads and
//! writes still make progress — but a worker running several green tasks will
//! stall the rest of them while one waits on i/o.
//!
//! that makes this good enough for local development on a mac and wrong for
//! serving load, and it is why green is only the *default* backend on linux
//! (see `concurrency::scheduler`). `PITH_GREEN=1` still turns it on here for
//! anyone who wants it. the fix is a kqueue reactor behind these same two
//! functions; until then, ship green-thread workloads on linux.

use crate::concurrency::green;
use std::os::unix::io::RawFd;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

/// one park spot per green worker: a pending-notify flag and the condvar the
/// worker waits on. the flag outlives a notify that lands before the wait, so
/// none is lost.
struct Park {
    notified: Mutex<bool>,
    cv: Condvar,
}

static PARKS: OnceLock<Vec<Park>> = OnceLock::new();

fn park_spot(index: usize) -> &'static Park {
    &PARKS.get_or_init(|| {
        (0..green::pool_size())
            .map(|_| Park {
                notified: Mutex::new(false),
                cv: Condvar::new(),
            })
            .collect()
    })[index]
}

/// wake worker `index` out of `idle_wait`.
pub(crate) fn notify(index: usize) {
    let park = park_spot(index);
    *park.notified.lock().unwrap_or_else(|p| p.into_inner()) = true;
    park.cv.notify_one();
}

/// no sockets to poll here: a socket wait holds its worker (see the module
/// comment), so nothing is ever ready to wake.
pub(crate) fn poll_ready(_index: usize) -> bool {
    false
}

/// park worker `index` until `notify` or `timeout`. returns true when the wait
/// ran out without a notify.
pub(crate) fn idle_wait(index: usize, timeout: Duration) -> bool {
    let park = park_spot(index);
    let guard = park.notified.lock().unwrap_or_else(|p| p.into_inner());
    let (mut notified, _) = park
        .cv
        .wait_timeout_while(guard, timeout, |n| !*n)
        .unwrap_or_else(|p| p.into_inner());
    let was = *notified;
    *notified = false;
    !was
}

/// block until `fd` is ready for `read`/write, it times out, or an error is
/// detected. returns the tri-state readiness contract: `1` ready, `0`
/// timed out, `-1` error.
///
/// `task` is the caller's green slab id, unused here: with no reactor there is
/// nobody to hand the task to, so we park the worker on `poll` instead of
/// suspending the coroutine. `live` is asked after the wait, as the linux
/// `poll` branch asks it: a handle closed while the wait was inside reports an
/// error rather than a readiness.
pub(crate) fn wait_io(
    fd: RawFd,
    read: bool,
    timeout_ms: i64,
    _task: usize,
    live: &dyn Fn() -> bool,
) -> i64 {
    let events = if read { libc::POLLIN } else { libc::POLLOUT };
    let ready = crate::fdio::poll_wait_any_fd(fd as i64, events, timeout_ms);
    if ready == 1 && !live() {
        return -1;
    }
    ready
}

/// sleep for `ms` milliseconds. `task` is the caller's green slab id, unused
/// here: with no reactor there is no timer heap to park the task on, so the
/// worker OS thread blocks for the duration — the same degradation socket
/// waits take on this platform (see the module comment). a zero or negative
/// duration returns immediately, matching the linux reactor's contract.
pub(crate) fn sleep_task(ms: i64, _task: usize) {
    if ms <= 0 {
        return;
    }
    std::thread::sleep(std::time::Duration::from_millis(ms as u64));
}

/// no-op: without a reactor there is no per-fd registration to tear down. the
/// linux build cleans up parked waiters here; a `poll` wait owns its fd for the
/// length of one call and leaves nothing behind.
pub(crate) fn on_close(_fd: RawFd) {}
