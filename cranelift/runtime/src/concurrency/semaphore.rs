//! Semaphore synchronization primitive
//!
//! A counting semaphore for limiting concurrent access.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};

/// Semaphore state
pub struct SemaphoreState {
    count: usize,
    max: usize,
}

/// Opaque handle to a Pith Semaphore
pub type PithSemaphoreHandle = Arc<(Mutex<SemaphoreState>, Condvar)>;

fn lock_state(lock: &Mutex<SemaphoreState>) -> MutexGuard<'_, SemaphoreState> {
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait_state<'a>(
    cvar: &Condvar,
    state: MutexGuard<'a, SemaphoreState>,
) -> MutexGuard<'a, SemaphoreState> {
    cvar.wait(state)
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Create a new Semaphore
///
/// # Arguments
/// * `initial` - Initial count (number of permits available)
///
/// Returns an opaque handle to the semaphore
#[no_mangle]
pub extern "C" fn pith_semaphore_new(initial: i64) -> *mut PithSemaphoreHandle {
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
pub unsafe extern "C" fn pith_semaphore_acquire(handle: *mut PithSemaphoreHandle) {
    if handle.is_null() {
        return;
    }
    let sem = &*handle;
    let (lock, cvar) = &**sem;
    let mut guard = lock_state(lock);
    while guard.count == 0 {
        guard = wait_state(cvar, guard);
    }
    guard.count -= 1;
}

/// Release a permit to the semaphore (increment counter)
///
/// # Safety
/// handle must be a valid semaphore handle
#[no_mangle]
pub unsafe extern "C" fn pith_semaphore_release(handle: *mut PithSemaphoreHandle) {
    if handle.is_null() {
        return;
    }
    let sem = &*handle;
    let (lock, cvar) = &**sem;
    let mut state = lock_state(lock);
    if state.count < state.max {
        state.count += 1;
    }
    cvar.notify_one();
}
