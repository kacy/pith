//! Mutex synchronization primitive
//!
//! Provides FFI-compatible mutex operations for the Pith runtime.

use crate::handle_registry::{self, HandleKind};
use std::sync::{Arc, Mutex};

/// Opaque handle to a Pith mutex
pub type PithMutexHandle = Arc<Mutex<()>>;

unsafe fn mutex_ref<'a>(handle: *mut PithMutexHandle) -> Option<&'a PithMutexHandle> {
    if !handle_registry::is_valid(handle as *const (), HandleKind::Mutex) {
        return None;
    }
    Some(&*handle)
}

/// Create a new mutex
///
/// Returns an opaque handle to the mutex
#[no_mangle]
pub extern "C" fn pith_mutex_new() -> *mut PithMutexHandle {
    let mutex = Arc::new(Mutex::new(()));
    let ptr = Box::into_raw(Box::new(mutex));
    handle_registry::register(ptr as *const (), HandleKind::Mutex);
    ptr
}

/// Lock the mutex
///
/// # Safety
/// handle must be a valid mutex handle obtained from pith_mutex_new
#[no_mangle]
pub unsafe extern "C" fn pith_mutex_lock(handle: *mut PithMutexHandle) {
    let Some(mutex) = mutex_ref(handle) else {
        return;
    };
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
    if mutex_ref(handle).is_none() {
        return;
    }
    // In Rust, we can't directly unlock a mutex from outside the guard
    // The proper implementation would require storing guards separately
    // For now, this is a placeholder that does nothing
    // A proper implementation would track guards in a separate data structure
}
