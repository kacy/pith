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

use super::{green, task};
use std::sync::OnceLock;

/// Which task backend spawn/await use. Chosen once from the environment and
/// cached for the life of the process.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Backend {
    /// One OS thread per task (`std::thread::spawn`), joined on await. The
    /// default and, in phase 1, the only backend with real behavior.
    OsThread,
    /// M:N green threads on a worker pool: tasks run as stackful coroutines on
    /// a fixed set of workers. Selected by `PITH_GREEN`. Experimental — see
    /// `green` for the P1a scope and the independent-tasks-only constraint.
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

    #[test]
    fn backend_defaults_to_os_threads_when_flag_absent() {
        // The cached backend reflects the ambient env at first read. In the
        // test harness `PITH_GREEN` is unset, so the default must hold.
        assert_eq!(backend(), Backend::OsThread);
    }
}
