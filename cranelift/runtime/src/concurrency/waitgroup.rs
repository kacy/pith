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

fn delta_magnitude(delta: i64) -> usize {
    let value = delta.unsigned_abs();
    if value > usize::MAX as u64 {
        usize::MAX
    } else {
        value as usize
    }
}

fn apply_delta(count: usize, delta: i64) -> usize {
    if delta >= 0 {
        count.saturating_add(delta_magnitude(delta))
    } else {
        count.saturating_sub(delta_magnitude(delta))
    }
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
    let (lock, cvar) = &**wg;
    let mut state = lock_state(lock);
    state.count = apply_delta(state.count, delta);
    if state.count == 0 {
        cvar.notify_all();
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn invalid_waitgroup_handles_are_ignored() {
        unsafe {
            let handle = 12345usize as *mut PithWaitGroupHandle;
            pith_waitgroup_add(handle, 1);
            pith_waitgroup_done(handle);
            pith_waitgroup_wait(handle);
        }
    }

    #[test]
    fn negative_add_wakes_waiters_when_count_reaches_zero() {
        let handle = pith_waitgroup_new();
        unsafe {
            pith_waitgroup_add(handle, 1);
        }

        let finished = Arc::new(AtomicBool::new(false));
        let finished_for_thread = finished.clone();
        let handle_addr = handle as usize;
        let waiter = std::thread::spawn(move || {
            let handle = handle_addr as *mut PithWaitGroupHandle;
            unsafe {
                pith_waitgroup_wait(handle);
            }
            finished_for_thread.store(true, Ordering::SeqCst);
        });

        std::thread::sleep(Duration::from_millis(25));
        assert!(!finished.load(Ordering::SeqCst));

        unsafe {
            pith_waitgroup_add(handle, -1);
        }

        assert!(waiter.join().is_ok());
        assert!(finished.load(Ordering::SeqCst));
    }

    #[test]
    fn large_negative_delta_saturates_at_zero() {
        let handle = pith_waitgroup_new();
        unsafe {
            pith_waitgroup_add(handle, 2);
            pith_waitgroup_add(handle, i64::MIN);
            pith_waitgroup_wait(handle);
        }
    }
}
