//! Semaphore synchronization primitive
//!
//! A counting semaphore for limiting concurrent access.
//!
//! Like [`channel`], a semaphore serves two kinds of waiter at once. An
//! **os-thread** caller that would block in `acquire()` condvar-waits, exactly
//! as it always has. A **green task** (under `PITH_GREEN`) must not park its
//! worker OS thread — it registers on `green_waiters` under the semaphore lock
//! and suspends its coroutine back to the scheduler; a later `release()`
//! re-enqueues it.
//!
//! ## one waiter per permit
//!
//! `release()` frees exactly one permit, so it wakes exactly one waiter — the
//! os-thread side does `cvar.notify_one()` and the green side wakes a single
//! parked task (FIFO, hence the `VecDeque`). This deliberately mirrors the
//! condvar `notify_one` rather than the channel's wake-all: a thundering herd of
//! green tasks all re-checking one permit would be wasteful, and semaphore
//! fairness matters. Waking both an os-thread waiter and a green waiter for one
//! permit is possible when both kinds are parked; that is a harmless spurious
//! wakeup — the loser re-checks `count == 0` under the lock and re-parks.
//!
//! The lock order and wake/park race are handled as in [`channel`]: the waiter
//! registers under the semaphore lock and releases it before suspending, and the
//! wake runs while holding the lock (semaphore -> slab -> queue).
//!
//! [`channel`]: crate::concurrency::channel

use crate::concurrency::green;
use crate::handle_registry::{self, HandleKind};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

/// Semaphore state
pub struct SemaphoreState {
    count: usize,
    max: usize,
    /// slab ids of green tasks suspended in `acquire()`, in arrival order.
    /// `release()` wakes the front one — one waiter per freed permit. empty (and
    /// free) whenever no green task is waiting, so the os-thread path is
    /// untouched.
    green_waiters: VecDeque<usize>,
}

/// Opaque handle to a Pith Semaphore
pub type PithSemaphoreHandle = Arc<(Mutex<SemaphoreState>, Condvar)>;

unsafe fn semaphore_ref<'a>(handle: *mut PithSemaphoreHandle) -> Option<&'a PithSemaphoreHandle> {
    if !handle_registry::is_valid(handle as *const (), HandleKind::Semaphore) {
        return None;
    }
    Some(&*handle)
}

fn lock_state(lock: &Mutex<SemaphoreState>) -> MutexGuard<'_, SemaphoreState> {
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait_state<'a>(
    cvar: &Condvar,
    state: MutexGuard<'a, SemaphoreState>,
) -> MutexGuard<'a, SemaphoreState> {
    // the wait is a native window for the cycle collector: no heap handle is
    // read until the condvar hands the lock back. the bracket's exit runs
    // holding the re-acquired semaphore lock, which is safe — a stop that
    // parks us there stops only mutators, and the collection pass itself
    // never takes a semaphore lock.
    let _native = crate::cycle::native_bracket();
    cvar.wait(state)
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// wake a single green task parked in `acquire()` so it re-checks the permit
/// count. paired with the `cvar.notify_one()` in `release()` — one freed permit
/// wakes one waiter. called while holding the semaphore lock; `green::wake`
/// nests the slab and queue locks under it (semaphore -> slab -> queue).
fn wake_one_green_waiter(state: &mut SemaphoreState) {
    if let Some(id) = state.green_waiters.pop_front() {
        green::wake(id);
    }
}

/// block the current `acquire()` caller until the next `release()`. an os-thread
/// caller condvar-waits as before; a green task registers on `green_waiters`
/// under the lock, releases it, and suspends its coroutine (never holding the
/// lock across the suspend — the worker may re-enter this semaphore).
fn block_on_acquire<'a>(
    lock: &'a Mutex<SemaphoreState>,
    cvar: &Condvar,
    mut state: MutexGuard<'a, SemaphoreState>,
    green_task: Option<usize>,
) -> MutexGuard<'a, SemaphoreState> {
    match green_task {
        None => wait_state(cvar, state),
        Some(id) => {
            state.green_waiters.push_back(id);
            drop(state);
            green::park_current();
            lock_state(lock)
        }
    }
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
        count: initial.max(0) as usize,
        max: initial.max(0) as usize,
        green_waiters: VecDeque::new(),
    };
    let sem = Arc::new((Mutex::new(state), Condvar::new()));
    let ptr = Box::into_raw(Box::new(sem));
    handle_registry::register(ptr as *const (), HandleKind::Semaphore);
    ptr
}

/// Acquire a permit from the semaphore (decrement counter)
///
/// Blocks until a permit is available.
///
/// # Safety
/// handle must be a valid semaphore handle
#[no_mangle]
pub unsafe extern "C" fn pith_semaphore_acquire(handle: *mut PithSemaphoreHandle) {
    let Some(sem) = semaphore_ref(handle) else {
        return;
    };
    let (lock, cvar) = &**sem;
    // computed once: whether we run inside a green task does not change across a
    // suspend/resume. None => os-thread caller => condvar-wait as before.
    let green_task = green::current_task();
    let mut guard = lock_state(lock);
    while guard.count == 0 {
        guard = block_on_acquire(lock, cvar, guard, green_task);
    }
    guard.count -= 1;
}

/// Release a permit to the semaphore (increment counter)
///
/// # Safety
/// handle must be a valid semaphore handle
#[no_mangle]
pub unsafe extern "C" fn pith_semaphore_release(handle: *mut PithSemaphoreHandle) {
    let Some(sem) = semaphore_ref(handle) else {
        return;
    };
    let (lock, cvar) = &**sem;
    let mut state = lock_state(lock);
    if state.count < state.max {
        state.count += 1;
    }
    // one freed permit wakes one waiter: an os-thread waiter via notify_one, and
    // a single parked green task. see the module note on one-waiter-per-permit.
    cvar.notify_one();
    wake_one_green_waiter(&mut state);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_semaphore_handles_are_ignored() {
        unsafe {
            let handle = 12345usize as *mut PithSemaphoreHandle;
            pith_semaphore_acquire(handle);
            pith_semaphore_release(handle);
        }
    }
}
