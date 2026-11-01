//! Mutex synchronization primitive
//!
//! An atomic spin-then-yield lock behind a magic-tagged heap handle: the
//! uncontended path is one compare-exchange each way, cheap enough to
//! guard short critical sections (registry map operations) without a
//! syscall. Contended waiters spin briefly, then yield.
//!
//! ## green tasks
//!
//! Unlike the channel/waitgroup/semaphore primitives, this lock is not built on
//! a `Condvar`: an os-thread waiter never blocks, it spins and then yields,
//! whichever backend is active. A **green task** must not spin forever: if it
//! holds the lock and yields elsewhere, another green task hard-spinning to
//! acquire the same lock would peg its worker and could starve the holder
//! that needs that worker to resume and unlock. So a contended green task
//! registers on `green_waiters` and suspends its coroutine back to the
//! scheduler; `unlock` drains that list and wakes them to retry.
//!
//! ## the register/park race (why the re-check under the waiter lock)
//!
//! `state` is 0 when free, 1 when held, and 2 when held with green tasks
//! registered to be woken. `unlock` swaps in 0 and only takes the waiter lock
//! when the old state was 2, so an unlock nobody waits on is one atomic swap.
//!
//! A waiter takes the `green_waiters` lock, then moves the state from 1 to 2
//! (or finds it already 2) before it registers. If the lock was released in
//! between, that move fails on the 0 and the waiter retries the acquire
//! instead of parking. An `unlock` that reads 2 takes the waiter lock to
//! drain, which waits for a waiter that set the 2 to finish registering, so
//! every registered waiter is woken. This is the standard futex
//! register-then-recheck.
//!
//! ## lock order
//!
//! `green_waiters` -> scheduler queues, matching the channel primitives:
//! `unlock` holds `green_waiters` while calling `green::wake` (which may take a
//! queue lock), and a parking task *releases* `green_waiters` before it
//! suspends. So the mutex's waiter lock is never held across a suspend, and the
//! scheduler locks are never taken while probing for the mutex.

use crate::concurrency::green;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

const MUTEX_MAGIC: u32 = 0x504d5458; // "PMTX"

#[repr(C)]
pub struct PithMutex {
    magic: u32,
    state: AtomicU32,
    /// ids of green tasks parked in `lock` under contention. an os-thread
    /// caller never registers here (it spins and then yields), so this
    /// list and its lock are touched only when the green backend is active.
    /// drained and woken by `unlock`.
    green_waiters: Mutex<Vec<usize>>,
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

fn lock_waiters(m: &PithMutex) -> std::sync::MutexGuard<'_, Vec<usize>> {
    m.green_waiters
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// try to take the lock exactly once, with a *strong* compare-exchange. on the
/// green path a weak one could fail spuriously and send a task off to park
/// while the lock is free, with no unlock coming to wake it.
fn try_acquire(m: &PithMutex) -> bool {
    m.state
        .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
}

/// os-thread lock path: spin briefly, then yield.
fn lock_os(m: &PithMutex) {
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
            // this lock blocks by spinning, not on a condvar, so the yield arm
            // doubles as the cycle collector's stop gate: a contended waiter
            // touches no reference count while it waits, holds no lock, and
            // its own frames are frozen for the length of the park. one
            // relaxed load when the collector flag is off.
            crate::cycle::mutator_gate();
            std::thread::yield_now();
        }
    }
}

/// green lock path: try once, and on contention register + suspend rather than
/// spin, so the worker is freed to run the holder. see the register/park race
/// note in the module header for why the re-check happens under the waiter lock.
fn lock_green(m: &PithMutex, id: usize) {
    loop {
        if try_acquire(m) {
            return;
        }
        // contended: under the waiter lock, mark the mutex as having waiters.
        // finding it free instead means an unlock came in between, so retry.
        let mut waiters = lock_waiters(m);
        match m.state.compare_exchange(1, 2, Ordering::Acquire, Ordering::Relaxed) {
            Ok(_) => {}
            Err(2) => {}
            Err(_) => continue,
        }
        // a wake from elsewhere can bring the task back here while its entry
        // is still listed, since only an unlock removes entries. it is never
        // listed twice, so the drain wakes it once.
        if !waiters.contains(&id) {
            waiters.push(id);
        }
        drop(waiters);
        green::park_current(id);
        // resumed by an unlock, or by a stray wake; loop and retry the acquire.
    }
}

/// Create a new mutex
///
/// Returns an opaque handle to the mutex
#[no_mangle]
pub extern "C" fn pith_mutex_new() -> *mut PithMutexHandle {
    Box::into_raw(Box::new(PithMutex {
        magic: MUTEX_MAGIC,
        state: AtomicU32::new(0),
        green_waiters: Mutex::new(Vec::new()),
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
    // inside a green task, park on contention. otherwise (main thread,
    // os-thread backend) spin, then yield the thread.
    match green::current_task() {
        Some(id) => lock_green(m, id),
        None => lock_os(m),
    }
}

/// Unlock the mutex
///
/// # Safety
/// handle must be a valid locked mutex handle
#[no_mangle]
pub unsafe extern "C" fn pith_mutex_unlock(handle: *mut PithMutexHandle) {
    let Some(m) = mutex_ref(handle) else {
        return;
    };
    // wake any green tasks parked on this lock so they retry their acquire.
    // only a parked green task sets state 2, so an unlock with no one waiting
    // never takes the waiter lock. an os-thread waiter spins and needs no
    // signal.
    if m.state.swap(0, Ordering::Release) == 2 {
        let mut waiters = lock_waiters(m);
        for id in waiters.drain(..) {
            green::wake(id);
        }
    }
}

/// how many green waiter entries the mutex lists.
#[cfg(test)]
pub(crate) unsafe fn green_waiters_for_tests(handle: *mut PithMutexHandle) -> usize {
    mutex_ref(handle).map_or(0, |m| lock_waiters(m).len())
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
