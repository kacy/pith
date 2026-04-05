//! WaitGroup synchronization primitive
//!
//! A WaitGroup waits for a collection of tasks to finish.

use std::sync::{Arc, Condvar, Mutex};

/// WaitGroup state
pub struct WaitGroupState {
    count: usize,
}

/// Opaque handle to a Forge WaitGroup
pub type ForgeWaitGroupHandle = Arc<(Mutex<WaitGroupState>, Condvar)>;

/// Create a new WaitGroup
///
/// Returns an opaque handle to the waitgroup
#[no_mangle]
pub extern "C" fn forge_waitgroup_new() -> *mut ForgeWaitGroupHandle {
    let state = WaitGroupState { count: 0 };
    let wg = Arc::new((Mutex::new(state), Condvar::new()));
    Box::into_raw(Box::new(wg))
}

/// Add delta to the WaitGroup counter
///
/// # Safety
/// handle must be a valid waitgroup handle
#[no_mangle]
pub unsafe extern "C" fn forge_waitgroup_add(handle: *mut ForgeWaitGroupHandle, delta: i64) {
    if handle.is_null() {
        return;
    }
    let wg = &*handle;
    let (lock, _) = &**wg;
    if let Ok(mut state) = lock.lock() {
        state.count = (state.count as i64 + delta) as usize;
    }
}

/// Decrement the WaitGroup counter (Done)
///
/// # Safety
/// handle must be a valid waitgroup handle
#[no_mangle]
pub unsafe extern "C" fn forge_waitgroup_done(handle: *mut ForgeWaitGroupHandle) {
    if handle.is_null() {
        return;
    }
    let wg = &*handle;
    let (lock, cvar) = &**wg;
    if let Ok(mut state) = lock.lock() {
        if state.count > 0 {
            state.count -= 1;
        }
        if state.count == 0 {
            cvar.notify_all();
        }
    }
}

/// Wait for the WaitGroup counter to reach zero
///
/// # Safety
/// handle must be a valid waitgroup handle
#[no_mangle]
pub unsafe extern "C" fn forge_waitgroup_wait(handle: *mut ForgeWaitGroupHandle) {
    if handle.is_null() {
        return;
    }
    let wg = &*handle;
    let (lock, cvar) = &**wg;
    let mut guard = lock.lock().unwrap();
    while guard.count > 0 {
        guard = cvar.wait(guard).unwrap();
    }
}

/// Free a WaitGroup handle
///
/// # Safety
/// handle must be a valid waitgroup handle
#[no_mangle]
pub unsafe extern "C" fn forge_waitgroup_free(handle: *mut ForgeWaitGroupHandle) {
    if !handle.is_null() {
        let _ = Box::from_raw(handle);
    }
}
