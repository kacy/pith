//! WaitGroup synchronization primitive
//!
//! A WaitGroup waits for a collection of tasks to finish.

use crate::handle_registry::{self, HandleKind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

/// WaitGroup state
pub struct WaitGroupState {
    count: usize,
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
    cvar.wait(state)
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Create a new WaitGroup
///
/// Returns an opaque handle to the waitgroup
#[no_mangle]
pub extern "C" fn pith_waitgroup_new() -> *mut PithWaitGroupHandle {
    let state = WaitGroupState { count: 0 };
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
    let mut guard = lock_state(lock);
    while guard.count > 0 {
        guard = wait_state(cvar, guard);
    }
}
