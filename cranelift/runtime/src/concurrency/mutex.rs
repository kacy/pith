//! Mutex synchronization primitive
//!
//! Provides FFI-compatible mutex operations for the Pith runtime.

use crate::handle_registry::{self, HandleKind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

/// Opaque handle to a Pith mutex
pub type PithMutexHandle = Arc<(Mutex<MutexState>, Condvar)>;

pub struct MutexState {
    locked: bool,
}

unsafe fn mutex_ref<'a>(handle: *mut PithMutexHandle) -> Option<&'a PithMutexHandle> {
    if !handle_registry::is_valid(handle as *const (), HandleKind::Mutex) {
        return None;
    }
    Some(&*handle)
}

fn lock_state(lock: &Mutex<MutexState>) -> MutexGuard<'_, MutexState> {
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait_state<'a>(cvar: &Condvar, state: MutexGuard<'a, MutexState>) -> MutexGuard<'a, MutexState> {
    cvar.wait(state)
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Create a new mutex
///
/// Returns an opaque handle to the mutex
#[no_mangle]
pub extern "C" fn pith_mutex_new() -> *mut PithMutexHandle {
    let mutex = Arc::new((Mutex::new(MutexState { locked: false }), Condvar::new()));
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
    let (lock, cvar) = &**mutex;
    let mut state = lock_state(lock);
    while state.locked {
        state = wait_state(cvar, state);
    }
    state.locked = true;
}

/// Unlock the mutex
///
/// # Safety
/// handle must be a valid locked mutex handle
#[no_mangle]
pub unsafe extern "C" fn pith_mutex_unlock(handle: *mut PithMutexHandle) {
    let Some(mutex) = mutex_ref(handle) else {
        return;
    };
    let (lock, cvar) = &**mutex;
    let mut state = lock_state(lock);
    if state.locked {
        state.locked = false;
        cvar.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn invalid_mutex_handles_are_ignored() {
        unsafe {
            let handle = 12345usize as *mut PithMutexHandle;
            pith_mutex_lock(handle);
            pith_mutex_unlock(handle);
        }
    }

    #[test]
    fn mutex_blocks_until_unlocked() {
        let handle = pith_mutex_new();
        unsafe {
            pith_mutex_lock(handle);
        }

        let entered = Arc::new(AtomicBool::new(false));
        let entered_for_thread = entered.clone();
        let handle_addr = handle as usize;
        let waiter = std::thread::spawn(move || {
            let handle = handle_addr as *mut PithMutexHandle;
            unsafe {
                pith_mutex_lock(handle);
            }
            entered_for_thread.store(true, Ordering::SeqCst);
            unsafe {
                pith_mutex_unlock(handle);
            }
        });

        std::thread::sleep(Duration::from_millis(25));
        assert!(!entered.load(Ordering::SeqCst));

        unsafe {
            pith_mutex_unlock(handle);
        }
        assert!(waiter.join().is_ok());
        assert!(entered.load(Ordering::SeqCst));
    }

    #[test]
    fn double_unlock_is_ignored() {
        let handle = pith_mutex_new();
        unsafe {
            pith_mutex_lock(handle);
            pith_mutex_unlock(handle);
            pith_mutex_unlock(handle);
        }
    }
}
