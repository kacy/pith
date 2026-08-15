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
//! a `Condvar` — an os-thread waiter never blocks, it spins-then-yields. That
//! stays byte-for-byte under the green backend for os-thread callers. But a
//! **green task** (under `PITH_GREEN`) must not spin forever: if it holds the
//! lock and yields elsewhere, another green task hard-spinning to acquire the
//! same lock would peg its worker and could starve the holder that needs that
//! worker to resume and unlock. So a contended green task registers on
//! `green_waiters` and suspends its coroutine back to the scheduler; `unlock`
//! drains that list and wakes them to retry.
//!
//! ## the register/park race (why the re-check under the waiter lock)
//!
//! `state` is a lockless atomic, so — unlike the condvar primitives — the lock
//! state and the waiter list are not guarded together. A naive "fail the CAS,
//! then register and park" would race an `unlock` that clears `state` and drains
//! the (still empty) list in between, parking the task forever. We close that by
//! taking the `green_waiters` lock, re-checking the CAS under it, and only
//! registering if the lock is still held. `unlock` clears `state` *before* it
//! takes that same lock to drain, so the two critical sections serialize: either
//! the waiter registers before the drain (and is woken) or it re-checks after
//! the clear and re-acquires. This is the standard futex register-then-recheck.
//!
//! ## lock order
//!
//! `green_waiters` -> slab -> queue, matching the channel primitives: `unlock`
//! holds `green_waiters` while calling `green::wake` (which nests the slab and
//! queue locks), and a parking task *releases* `green_waiters` before it
//! suspends. So the mutex's waiter lock is never held across a suspend, and the
//! scheduler locks are never taken while probing for the mutex.

use crate::concurrency::green;
use crate::concurrency::scheduler::{backend, Backend};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

const MUTEX_MAGIC: u32 = 0x504d5458; // "PMTX"

#[repr(C)]
pub struct PithMutex {
    magic: u32,
    state: AtomicU32,
    /// slab ids of green tasks parked in `lock` under contention. an os-thread
    /// caller never registers here — it spins-then-yields as before — so this
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

/// try to take the lock exactly once with a *strong* compare-exchange. strong
/// (not `_weak`) matters on the green path: a spurious weak failure there would
/// make a task register and park while the lock is actually free, with no future
/// unlock to wake it. strong only fails when the lock is genuinely held.
fn try_acquire(m: &PithMutex) -> bool {
    m.state
        .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
}

/// os-thread lock path: spin briefly, then yield. unchanged from the original,
/// byte-for-byte, so the default (flag off) behavior is exactly as before.
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
        // contended: register under the waiter lock, but re-check the lock
        // *under that lock* first, so we cannot park just after an unlock already
        // cleared `state` and drained the list.
        let mut waiters = lock_waiters(m);
        if try_acquire(m) {
            return;
        }
        waiters.push(id);
        drop(waiters);
        green::park_current();
        // resumed by an unlock; loop and retry the acquire.
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
    // inside a green task? yield on contention. otherwise (main thread /
    // os-thread backend) spin-then-yield exactly as before.
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
    m.state.store(0, Ordering::Release);
    // wake any green tasks parked on this lock so they retry their acquire. only
    // meaningful under the green backend — an os-thread waiter spins and needs no
    // signal — so we skip the waiter lock entirely when green is off, keeping the
    // default unlock a single atomic store as before. the gate is process-wide
    // and monotonic, so it introduces no per-unlock race.
    if backend() == Backend::Green {
        let mut waiters = lock_waiters(m);
        for id in waiters.drain(..) {
            green::wake(id);
        }
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
