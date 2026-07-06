//! Mutex synchronization primitive
//!
//! An atomic spin-then-yield lock behind a magic-tagged heap handle: the
//! uncontended path is one compare-exchange each way, cheap enough to
//! guard short critical sections (registry map operations) without a
//! syscall. Contended waiters spin briefly, then yield.

use std::sync::atomic::{AtomicU32, Ordering};

const MUTEX_MAGIC: u32 = 0x504d5458; // "PMTX"

#[repr(C)]
pub struct PithMutex {
    magic: u32,
    state: AtomicU32,
}

pub type PithMutexHandle = PithMutex;

unsafe fn mutex_ref<'a>(handle: *mut PithMutexHandle) -> Option<&'a PithMutex> {
    if handle.is_null() || (handle as usize) % 4 != 0 {
        return None;
    }
    let m = &*(handle as *const PithMutex);
    if m.magic != MUTEX_MAGIC {
        return None;
    }
    Some(m)
}

/// Create a new mutex
///
/// Returns an opaque handle to the mutex
#[no_mangle]
pub extern "C" fn pith_mutex_new() -> *mut PithMutexHandle {
    Box::into_raw(Box::new(PithMutex {
        magic: MUTEX_MAGIC,
        state: AtomicU32::new(0),
    }))
}

/// Lock the mutex
///
/// # Safety
/// handle must be a valid mutex handle obtained from pith_mutex_new
#[no_mangle]
pub unsafe extern "C" fn pith_mutex_lock(handle: *mut PithMutexHandle) {
    let Some(m) = mutex_ref(handle) else {
        return;
    };
    let mut spins = 0u32;
    loop {
        if m
            .state
            .compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return;
        }
        spins += 1;
        if spins < 64 {
            std::hint::spin_loop();
        } else {
            std::thread::yield_now();
        }
    }
}

/// Unlock the mutex
///
/// # Safety
/// handle must be a valid locked mutex handle
#[no_mangle]
pub unsafe extern "C" fn pith_mutex_unlock(handle: *mut PithMutexHandle) {
    if let Some(m) = mutex_ref(handle) {
        m.state.store(0, Ordering::Release);
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
