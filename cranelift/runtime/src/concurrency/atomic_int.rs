//! Atomic integer cell
//!
//! A single shared integer that any task can read and write safely. Used
//! where a value must cross a task boundary without a lock or a channel
//! (a cancellation code, a shared counter), so the value stays coherent
//! across threads while a plain collection would race.

use crate::handle_registry;
use std::sync::atomic::{AtomicI64, Ordering};

/// Magic word for a cell ("PATM").
const CELL_MAGIC: u64 = 0x5041_544d;

/// A cell is validated by its magic word, the way lists and maps are, rather
/// than by a registry lookup. The registry took a lock on every load and
/// store, which made the one primitive meant to avoid a lock take one.
/// Cells are never freed, so a valid handle always reads its magic.
#[repr(C)]
struct Cell {
    magic: u64,
    value: AtomicI64,
}

unsafe fn cell_ref<'a>(handle: i64) -> Option<&'a AtomicI64> {
    let ptr = handle as *const ();
    if !handle_registry::plausibly_aligned::<Cell>(ptr) || (*(ptr as *const Cell)).magic != CELL_MAGIC {
        return None;
    }
    Some(&(*(ptr as *const Cell)).value)
}

/// Create an atomic cell holding `initial`.
#[no_mangle]
pub extern "C" fn pith_atomic_int_new(initial: i64) -> i64 {
    let ptr = Box::into_raw(Box::new(Cell {
        magic: CELL_MAGIC,
        value: AtomicI64::new(initial),
    }));
    ptr as i64
}

/// Read the current value.
///
/// # Safety
/// handle must be a valid atomic-cell handle or garbage (magic-checked).
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
/// handle must be a valid atomic-cell handle or garbage (magic-checked).
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
/// handle must be a valid atomic-cell handle or garbage (magic-checked).
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
