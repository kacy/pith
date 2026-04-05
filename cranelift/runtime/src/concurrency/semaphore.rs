//! Semaphore synchronization primitive
//!
//! A counting semaphore for limiting concurrent access.

use std::sync::{Arc, Condvar, Mutex};

/// Semaphore state
pub struct SemaphoreState {
    count: usize,
    max: usize,
}

/// Opaque handle to a Forge Semaphore
pub type ForgeSemaphoreHandle = Arc<(Mutex<SemaphoreState>, Condvar)>;

/// Create a new Semaphore
///
/// # Arguments
/// * `initial` - Initial count (number of permits available)
///
/// Returns an opaque handle to the semaphore
#[no_mangle]
pub extern "C" fn forge_semaphore_new(initial: i64) -> *mut ForgeSemaphoreHandle {
    let state = SemaphoreState {
        count: initial as usize,
        max: initial as usize,
    };
    let sem = Arc::new((Mutex::new(state), Condvar::new()));
    Box::into_raw(Box::new(sem))
}

/// Acquire a permit from the semaphore (decrement counter)
///
/// Blocks until a permit is available.
///
/// # Safety
/// handle must be a valid semaphore handle
#[no_mangle]
pub unsafe extern "C" fn forge_semaphore_acquire(handle: *mut ForgeSemaphoreHandle) {
    if handle.is_null() {
        return;
    }
    let sem = &*handle;
    let (lock, cvar) = &**sem;
    let mut guard = lock.lock().unwrap();
    while guard.count == 0 {
        guard = cvar.wait(guard).unwrap();
    }
    guard.count -= 1;
}

/// Release a permit to the semaphore (increment counter)
///
/// # Safety
/// handle must be a valid semaphore handle
#[no_mangle]
pub unsafe extern "C" fn forge_semaphore_release(handle: *mut ForgeSemaphoreHandle) {
    if handle.is_null() {
        return;
    }
    let sem = &*handle;
    let (lock, cvar) = &**sem;
    if let Ok(mut state) = lock.lock() {
        if state.count < state.max {
            state.count += 1;
        }
        cvar.notify_one();
    }
}

/// Free a Semaphore handle
///
/// # Safety
/// handle must be a valid semaphore handle
#[no_mangle]
pub unsafe extern "C" fn forge_semaphore_free(handle: *mut ForgeSemaphoreHandle) {
    if !handle.is_null() {
        let _ = Box::from_raw(handle);
    }
}
