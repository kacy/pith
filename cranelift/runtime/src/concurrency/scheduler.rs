//! task backend selection (the green-thread seam)
//!
//! spawn and await funnel through here so a future M:N green-thread backend
//! can slot in behind a flag without touching the codegen-facing FFI. today
//! there is exactly one backend — os threads, unchanged — and this module is
//! inert: every path lands in the same `os_thread_*` code that has always run.
//!
//! the point of landing the seam first, empty, is that turning `PITH_GREEN`
//! on right now must be a no-op. that gives the real scheduler a safe place to
//! grow into: when run queues + stackful coroutines arrive, only the `Green`
//! arms below change, and the default (flag off) path stays byte-for-byte the
//! os-thread behavior.

use super::task;
use std::sync::OnceLock;

/// Which task backend spawn/await use. Chosen once from the environment and
/// cached for the life of the process.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Backend {
    /// One OS thread per task (`std::thread::spawn`), joined on await. The
    /// default and, in phase 1, the only backend with real behavior.
    OsThread,
    /// M:N green threads on a worker pool. Selected by `PITH_GREEN`, but not
    /// yet built — it currently delegates to the os-thread path (see below).
    Green,
}

/// Read `PITH_GREEN` once and cache the choice. Follows the same env-flag
/// pattern as the struct freelist toggle in `runtime_core`: parse on first
/// use, store in a `OnceLock`, never look again.
///
/// Off / unset / anything unrecognized => `OsThread`, so the default path is
/// exactly today's behavior with zero overhead beyond one cached read.
pub(crate) fn backend() -> Backend {
    static BACKEND: OnceLock<Backend> = OnceLock::new();
    *BACKEND.get_or_init(|| match std::env::var("PITH_GREEN").as_deref() {
        Ok("1") | Ok("on") | Ok("true") => Backend::Green,
        _ => Backend::OsThread,
    })
}

/// Spawn a task running `closure_handle`, returning its task handle.
///
/// # Safety
/// `closure_handle` must be a valid closure handle or 0 (validated downstream).
pub(crate) unsafe fn spawn(closure_handle: i64) -> i64 {
    match backend() {
        // Both arms deliberately share the os-thread path in phase 1: the
        // green backend's run-queue enqueue + coroutine creation lands in a
        // later PR. Keeping the match here establishes the seam and proves the
        // flag threads through without changing behavior. Split the `Green`
        // arm when the scheduler exists.
        Backend::OsThread | Backend::Green => task::os_thread_spawn(closure_handle),
    }
}

/// Await a task and return its result.
///
/// # Safety
/// `task_handle` must be a task handle from `spawn` or garbage (validated).
pub(crate) unsafe fn await_task(task_handle: i64) -> i64 {
    match backend() {
        // Same phase-1 stance as `spawn`: the green path (park the caller's
        // coroutine on the joinee, resume it on completion) is not built yet,
        // so both arms join the os thread.
        Backend::OsThread | Backend::Green => task::os_thread_await(task_handle),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_defaults_to_os_threads_when_flag_absent() {
        // The cached backend reflects the ambient env at first read. In the
        // test harness `PITH_GREEN` is unset, so the default must hold.
        assert_eq!(backend(), Backend::OsThread);
    }
}
