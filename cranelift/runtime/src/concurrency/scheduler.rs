//! task backend selection (the green-thread seam)
//!
//! spawn and await funnel through here so the backend choice is made in exactly
//! one place and the codegen-facing FFI never has to know which one it got.
//! there are two: os threads, one kernel thread per task, and green, M:N
//! coroutines on a worker pool.
//!
//! green is the default on linux, because that is where the epoll reactor lives
//! and where it is faster on every shape this repo measures. everywhere else the
//! default is os threads: `netpoll_fallback` has no reactor, so a green task
//! waiting on a socket would hold its worker for the whole wait. `PITH_GREEN`
//! overrides the choice in either direction on any platform.

use super::{green, task};
use std::sync::OnceLock;

/// Which task backend spawn/await use. Chosen once from the environment and
/// cached for the life of the process.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Backend {
    /// One OS thread per task (`std::thread::spawn`), joined on await. The
    /// default on every platform but linux, and the opt-out on linux via
    /// `PITH_GREEN=0`.
    OsThread,
    /// M:N green threads on a worker pool: tasks run as stackful coroutines on
    /// a fixed set of workers. The default on linux; available elsewhere with
    /// `PITH_GREEN=1`, but without a reactor to yield socket waits to.
    Green,
}

/// The backend used when `PITH_GREEN` says nothing.
///
/// Linux gets green: the epoll reactor in `netpoll` yields socket waits, and
/// dns resolution runs on a blocking pool, so the two things that used to hold a
/// worker for an unbounded time no longer do. Other platforms compile
/// `netpoll_fallback`, which has no reactor at all, so green there would turn
/// every socket wait into a stalled worker.
const fn platform_default() -> Backend {
    if cfg!(target_os = "linux") {
        Backend::Green
    } else {
        Backend::OsThread
    }
}

/// Map a `PITH_GREEN` value to a backend. `None` is the variable being unset (or
/// unreadable), which means the caller expressed no preference.
///
/// Unrecognized values fall to the platform default rather than to os threads,
/// so the only values that move the backend are the ones spelled out here. A
/// typo like `PITH_GREEN=maybe` then behaves exactly like not setting the
/// variable, which is easier to reason about than a rule where some unknown
/// values mean "off" and the absent one means "default".
///
/// The off spellings are deliberately generous. `PITH_GREEN=0` is the escape
/// hatch for anyone the linux default hurts, and someone reaching for it who
/// writes `no` or `NO` must not silently get green instead — that is the one
/// misreading with a real cost. Values are trimmed and lowercased for the same
/// reason.
fn backend_from_env(value: Option<&str>) -> Backend {
    match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("1") | Some("on") | Some("true") | Some("yes") | Some("y") => Backend::Green,
        Some("0") | Some("off") | Some("false") | Some("no") | Some("n") => Backend::OsThread,
        _ => platform_default(),
    }
}

/// Read `PITH_GREEN` once and cache the choice. Follows the same env-flag
/// pattern as the struct freelist toggle in `runtime_core`: parse on first
/// use, store in a `OnceLock`, never look again.
pub(crate) fn backend() -> Backend {
    static BACKEND: OnceLock<Backend> = OnceLock::new();
    *BACKEND.get_or_init(|| backend_from_env(std::env::var("PITH_GREEN").ok().as_deref()))
}

/// Spawn a task running `closure_handle`, returning its task handle.
///
/// # Safety
/// `closure_handle` must be a valid closure handle or 0 (validated downstream).
pub(crate) unsafe fn spawn(closure_handle: i64) -> i64 {
    match backend() {
        Backend::OsThread => task::os_thread_spawn(closure_handle),
        Backend::Green => green::green_spawn(closure_handle),
    }
}

/// Await a task and return its result.
///
/// # Safety
/// `task_handle` must be a task handle from `spawn` or garbage (validated).
pub(crate) unsafe fn await_task(task_handle: i64) -> i64 {
    match backend() {
        Backend::OsThread => task::os_thread_await(task_handle),
        // Stance (a): block the calling thread on the task's condvar until a
        // worker completes it. Yielding the caller's coroutine instead is P2.
        Backend::Green => green::green_await(task_handle),
    }
}

/// Is a task finished? Routed through the seam so it consults whichever backend
/// actually owns the task (the green slab or the os-thread table).
pub(crate) fn task_is_done(task_handle: i64) -> i64 {
    match backend() {
        Backend::OsThread => task::os_thread_is_done(task_handle),
        Backend::Green => green::green_is_done(task_handle),
    }
}

/// Detach a task (drop the join side). Routed through the seam for the same
/// reason as `task_is_done`.
pub(crate) fn task_detach(task_handle: i64) {
    match backend() {
        Backend::OsThread => task::os_thread_detach(task_handle),
        Backend::Green => green::green_detach(task_handle),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `backend()` caches in a `OnceLock` and reads the ambient process env, so
    // it cannot be exercised more than once per test binary. The parsing lives
    // in `backend_from_env`, which is pure, and that is what these cover.

    #[test]
    fn unset_falls_to_the_platform_default() {
        assert_eq!(backend_from_env(None), platform_default());
    }

    #[test]
    fn platform_default_is_green_only_on_linux() {
        if cfg!(target_os = "linux") {
            assert_eq!(platform_default(), Backend::Green);
        } else {
            assert_eq!(platform_default(), Backend::OsThread);
        }
    }

    #[test]
    fn on_spellings_force_green() {
        for value in ["1", "on", "true", "yes", "y", "ON", "True", " on "] {
            assert_eq!(backend_from_env(Some(value)), Backend::Green, "{value:?}");
        }
    }

    #[test]
    fn off_spellings_force_os_threads() {
        for value in ["0", "off", "false", "no", "n", "OFF", "False", " off ", "NO"] {
            assert_eq!(backend_from_env(Some(value)), Backend::OsThread, "{value:?}");
        }
    }

    #[test]
    fn unrecognized_values_behave_like_unset() {
        for value in ["", "maybe", "2", "green", "osthread"] {
            assert_eq!(backend_from_env(Some(value)), platform_default(), "{value:?}");
        }
    }

    #[test]
    fn the_cached_backend_agrees_with_the_ambient_env() {
        // Whatever the harness was launched with, the cached choice has to be
        // the one the parser would make for it.
        let expected = backend_from_env(std::env::var("PITH_GREEN").ok().as_deref());
        assert_eq!(backend(), expected);
    }
}
