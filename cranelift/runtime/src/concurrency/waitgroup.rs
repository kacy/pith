//! WaitGroup synchronization primitive
//!
//! A WaitGroup waits for a collection of tasks to finish.
//!
//! Like [`channel`], a waitgroup serves two kinds of waiter at once. An
//! **os-thread** caller (the main thread, or the whole os-thread task backend)
//! that would block on `wait()` condvar-waits, exactly as it always has. A
//! **green task** (under `PITH_GREEN`) must not park its worker OS thread — it
//! registers itself on `green_waiters` under the waitgroup lock and suspends its
//! coroutine back to the scheduler, freeing the worker to run other tasks; the
//! `done()` that drains the counter to zero re-enqueues it. `done()` therefore
//! wakes the green waiters alongside its `cvar.notify_all()`, so whichever kind
//! is parked makes progress.
//!
//! The lock order and wake/park race are handled exactly as in [`channel`]: the
//! waiter registers under the waitgroup lock and releases it before suspending,
//! and `wake_green_waiters` runs while holding the lock (waitgroup -> slab ->
//! queue). See the channel module header for the full reasoning.
//!
//! [`channel`]: crate::concurrency::channel

use crate::concurrency::green;
use crate::handle_registry::{self, HandleKind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

/// WaitGroup state
pub struct WaitGroupState {
    count: usize,
    /// slab ids of green tasks suspended in `wait()`. woken when the counter
    /// reaches zero; empty (and free) whenever no green task is waiting, so the
    /// os-thread path is untouched.
    green_waiters: Vec<usize>,
}

/// Opaque handle to a Pith WaitGroup
pub type PithWaitGroupHandle = Arc<(Mutex<WaitGroupState>, Condvar)>;

unsafe fn waitgroup_ref<'a>(handle: *mut PithWaitGroupHandle) -> Option<&'a PithWaitGroupHandle> {
    if !handle_registry::is_valid(handle as *const (), HandleKind::WaitGroup) {
        return None;
    }
    Some(&*handle)
}

fn lock_state(lock: &Mutex<WaitGroupState>) -> MutexGuard<'_, WaitGroupState> {
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait_state<'a>(
    cvar: &Condvar,
    state: MutexGuard<'a, WaitGroupState>,
) -> MutexGuard<'a, WaitGroupState> {
    // the wait is a native window for the cycle collector: no heap handle is
    // read until the condvar hands the lock back. the bracket's exit runs
    // holding the re-acquired waitgroup lock, which is safe — a stop that
    // parks us there stops only mutators, and the collection pass itself
    // never takes a waitgroup lock.
    let _native = crate::cycle::native_bracket();
    cvar.wait(state)
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// wake every green task parked in `wait()` so it re-checks the counter. paired
/// with the `cvar.notify_all()` in `done()` so green and os-thread waiters are
/// signaled together. called while holding the waitgroup lock; `green::wake`
/// nests the slab and queue locks under it (waitgroup -> slab -> queue).
fn wake_green_waiters(state: &mut WaitGroupState) {
    for id in std::mem::take(&mut state.green_waiters) {
        green::wake(id);
    }
}

/// block the current `wait()` caller until the next `done()`. an os-thread
/// caller condvar-waits as before; a green task registers on `green_waiters`
/// under the lock, releases it, and suspends its coroutine (never holding the
/// lock across the suspend — the worker may re-enter this waitgroup).
fn block_on_wait<'a>(
    lock: &'a Mutex<WaitGroupState>,
    cvar: &Condvar,
    mut state: MutexGuard<'a, WaitGroupState>,
    green_task: Option<usize>,
) -> MutexGuard<'a, WaitGroupState> {
    match green_task {
        None => wait_state(cvar, state),
        Some(id) => {
            state.green_waiters.push(id);
            drop(state);
            green::park_current(id);
            lock_state(lock)
        }
    }
}

/// Create a new WaitGroup
///
/// Returns an opaque handle to the waitgroup
#[no_mangle]
pub extern "C" fn pith_waitgroup_new() -> *mut PithWaitGroupHandle {
    let state = WaitGroupState {
        count: 0,
        green_waiters: Vec::new(),
    };
    let wg = Arc::new((Mutex::new(state), Condvar::new()));
    let ptr = Box::into_raw(Box::new(wg));
    handle_registry::register(ptr as *const (), HandleKind::WaitGroup);
    ptr
}

/// Add delta to the WaitGroup counter
///
/// # Safety
/// handle must be a valid waitgroup handle
#[no_mangle]
pub unsafe extern "C" fn pith_waitgroup_add(handle: *mut PithWaitGroupHandle, delta: i64) {
    let Some(wg) = waitgroup_ref(handle) else {
        return;
    };
    let (lock, _) = &**wg;
    let mut state = lock_state(lock);
    state.count = (state.count as i64 + delta).max(0) as usize;
}

/// Decrement the WaitGroup counter (Done)
///
/// # Safety
/// handle must be a valid waitgroup handle
#[no_mangle]
pub unsafe extern "C" fn pith_waitgroup_done(handle: *mut PithWaitGroupHandle) {
    let Some(wg) = waitgroup_ref(handle) else {
        return;
    };
    let (lock, cvar) = &**wg;
    let mut state = lock_state(lock);
    if state.count > 0 {
        state.count -= 1;
    }
    if state.count == 0 {
        cvar.notify_all();
        wake_green_waiters(&mut state);
    }
}

/// Wait for the WaitGroup counter to reach zero
///
/// # Safety
/// handle must be a valid waitgroup handle
#[no_mangle]
pub unsafe extern "C" fn pith_waitgroup_wait(handle: *mut PithWaitGroupHandle) {
    let Some(wg) = waitgroup_ref(handle) else {
        return;
    };
    let (lock, cvar) = &**wg;
    // computed once: whether we run inside a green task does not change across a
    // suspend/resume. None => os-thread caller => condvar-wait as before.
    let green_task = green::current_task();
    let mut guard = lock_state(lock);
    while guard.count > 0 {
        guard = block_on_wait(lock, cvar, guard, green_task);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_waitgroup_handles_are_ignored() {
        unsafe {
            let handle = 12345usize as *mut PithWaitGroupHandle;
            pith_waitgroup_add(handle, 1);
            pith_waitgroup_done(handle);
            pith_waitgroup_wait(handle);
        }
    }
}
