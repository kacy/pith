//! Atomic integer cell
//!
//! A single shared integer that any task can read and write safely. Used
//! where a value must cross a task boundary without a lock or a channel —
//! a cancellation code, a shared counter — so the value stays coherent
//! across threads while a plain collection would race.

use crate::handle_registry::{self, HandleKind};
use std::sync::atomic::{AtomicI64, Ordering};

unsafe fn cell_ref<'a>(handle: i64) -> Option<&'a AtomicI64> {
    if !handle_registry::is_valid(handle as *const (), HandleKind::AtomicInt) {
        return None;
    }
    Some(&*(handle as *const AtomicI64))
}

/// Create an atomic cell holding `initial`.
#[no_mangle]
pub extern "C" fn pith_atomic_int_new(initial: i64) -> i64 {
    let ptr = Box::into_raw(Box::new(AtomicI64::new(initial)));
    handle_registry::register(ptr as *const (), HandleKind::AtomicInt);
    ptr as i64
}

/// Read the current value.
///
/// # Safety
/// handle must be a valid atomic-cell handle or garbage (registry-checked).
#[no_mangle]
pub unsafe extern "C" fn pith_atomic_int_get(handle: i64) -> i64 {
    match cell_ref(handle) {
        Some(cell) => cell.load(Ordering::Acquire),
        None => 0,
    }
}

/// Store a new value.
///
/// # Safety
/// handle must be a valid atomic-cell handle or garbage (registry-checked).
#[no_mangle]
pub unsafe extern "C" fn pith_atomic_int_set(handle: i64, value: i64) {
    if let Some(cell) = cell_ref(handle) {
        cell.store(value, Ordering::Release);
    }
}

/// Store `new` only if the cell currently holds `expected`; returns 1 on
/// success, 0 if another writer got there first. lets one writer claim a
/// cell exactly once (a context is cancelled for a single reason).
///
/// # Safety
/// handle must be a valid atomic-cell handle or garbage (registry-checked).
#[no_mangle]
pub unsafe extern "C" fn pith_atomic_int_compare_set(handle: i64, expected: i64, new: i64) -> i64 {
    match cell_ref(handle) {
        Some(cell) => match cell.compare_exchange(expected, new, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => 1,
            Err(_) => 0,
        },
        None => 0,
    }
}
