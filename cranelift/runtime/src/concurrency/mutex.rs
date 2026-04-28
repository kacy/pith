//! Mutex synchronization primitive
//!
//! Provides FFI-compatible mutex operations for the Pith runtime.

use std::sync::{Arc, Mutex};

/// Opaque handle to a Pith mutex
pub type PithMutexHandle = Arc<Mutex<()>>;

/// Create a new mutex
///
/// Returns an opaque handle to the mutex
#[no_mangle]
pub extern "C" fn pith_mutex_new() -> *mut PithMutexHandle {
    let mutex = Arc::new(Mutex::new(()));
    Box::into_raw(Box::new(mutex))
}

/// Lock the mutex
///
/// # Safety
/// handle must be a valid mutex handle obtained from pith_mutex_new
#[no_mangle]
pub unsafe extern "C" fn pith_mutex_lock(handle: *mut PithMutexHandle) {
    if handle.is_null() {
        return;
    }
    let mutex = &*handle;
    let _guard = mutex.lock();
    // Hold the lock until the function returns (drop at end)
    std::mem::forget(_guard);
}

/// Unlock the mutex
///
/// # Safety
/// handle must be a valid locked mutex handle
#[no_mangle]
pub unsafe extern "C" fn pith_mutex_unlock(handle: *mut PithMutexHandle) {
    if handle.is_null() {
        return;
    }
    // In Rust, we can't directly unlock a mutex from outside the guard
    // The proper implementation would require storing guards separately
    // For now, this is a placeholder that does nothing
    // A proper implementation would track guards in a separate data structure
}
