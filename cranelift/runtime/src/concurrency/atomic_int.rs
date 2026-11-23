//! Atomic integer cell
//!
//! A single shared integer that any task can read and write safely. Used
//! where a value must cross a task boundary without a lock or a channel
//! (a cancellation code, a shared counter), so the value stays coherent
//! across threads while a plain collection would race.

use crate::concurrency::sync_handle::{self, SyncHeader, ATOMIC_INT_MAGIC};
use std::sync::atomic::{AtomicI64, Ordering};

/// A cell is validated by its magic word, the way lists and maps are, rather
/// than by a registry lookup. The registry took a lock on every load and
/// store, which made the one primitive meant to avoid a lock take one. A
/// cell is owned by its count (see `sync_handle`) and freed at the last
/// release, which scrubs the magic.
#[repr(C)]
pub struct Cell {
    header: SyncHeader,
    value: AtomicI64,
}

#[inline(always)]
unsafe fn with_cell<R>(handle: i64, f: impl FnOnce(&AtomicI64) -> R) -> Option<R> {
    // SAFETY: `Cell` is `#[repr(C)]` starting with `SyncHeader` tagged `ATOMIC_INT_MAGIC`.
    unsafe {
        sync_handle::with_sync_ref(
            handle as *const (),
            ATOMIC_INT_MAGIC,
            crate::quarantine::Op::Use,
            |c: &Cell| f(&c.value),
        )
    }
}

/// the last count is gone.
pub(crate) unsafe fn free_cell(p: *mut Cell) {
    // SAFETY: Caller guarantees `p` is an unowned `Cell` from `Box::into_raw`.
    unsafe { sync_handle::free_box(p) };
}

#[inline(always)]
fn cell_get(cell: &AtomicI64) -> i64 {
    cell.load(Ordering::Acquire)
}

#[inline(always)]
fn cell_set(cell: &AtomicI64, value: i64) {
    cell.store(value, Ordering::Release);
}

#[inline(always)]
fn cell_compare_set(cell: &AtomicI64, expected: i64, new: i64) -> i64 {
    match cell.compare_exchange(expected, new, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => 1,
        Err(_) => 0,
    }
}

#[inline(always)]
fn cell_fetch_add(cell: &AtomicI64, delta: i64) -> i64 {
    cell.fetch_add(delta, Ordering::AcqRel)
}

#[inline(always)]
fn cell_fetch_sub(cell: &AtomicI64, delta: i64) -> i64 {
    cell.fetch_sub(delta, Ordering::AcqRel)
}

/// Create an atomic cell holding `initial`.
#[no_mangle]
pub extern "C" fn pith_atomic_int_new(initial: i64) -> i64 {
    let ptr = Box::into_raw(Box::new(Cell {
        header: SyncHeader::new(ATOMIC_INT_MAGIC),
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
    // SAFETY: `handle` is validated by `with_cell` before borrowing its atomic value.
    unsafe { with_cell(handle, cell_get) }.unwrap_or(0)
}

/// Store a new value.
///
/// # Safety
/// handle must be a valid atomic-cell handle or garbage (magic-checked).
#[no_mangle]
pub unsafe extern "C" fn pith_atomic_int_set(handle: i64, value: i64) {
    // SAFETY: `handle` is validated by `with_cell` before borrowing its atomic value.
    unsafe { with_cell(handle, |cell| cell_set(cell, value)) };
}

/// Store `new` only if the cell currently holds `expected`; returns 1 on
/// success, 0 if another writer got there first. lets one writer claim a
/// cell exactly once (a context is cancelled for a single reason).
///
/// # Safety
/// handle must be a valid atomic-cell handle or garbage (magic-checked).
#[no_mangle]
pub unsafe extern "C" fn pith_atomic_int_compare_set(handle: i64, expected: i64, new: i64) -> i64 {
    // SAFETY: `handle` is validated by `with_cell` before borrowing its atomic value.
    unsafe { with_cell(handle, |cell| cell_compare_set(cell, expected, new)) }.unwrap_or(0)
}

/// Atomically add `delta` to the cell and return its previous value.
///
/// # Safety
/// handle must be a valid atomic-cell handle or garbage (magic-checked).
#[no_mangle]
pub unsafe extern "C" fn pith_atomic_int_fetch_add(handle: i64, delta: i64) -> i64 {
    // SAFETY: `handle` is validated by `with_cell` before borrowing its atomic value.
    unsafe { with_cell(handle, |cell| cell_fetch_add(cell, delta)) }.unwrap_or(0)
}

/// Atomically subtract `delta` from the cell and return its previous value.
///
/// # Safety
/// handle must be a valid atomic-cell handle or garbage (magic-checked).
#[no_mangle]
pub unsafe extern "C" fn pith_atomic_int_fetch_sub(handle: i64, delta: i64) -> i64 {
    // SAFETY: `handle` is validated by `with_cell` before borrowing its atomic value.
    unsafe { with_cell(handle, |cell| cell_fetch_sub(cell, delta)) }.unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::concurrency::sync_handle::pith_sync_release;

    #[test]
    fn fetch_add_and_fetch_sub_return_previous_values() {
        let h = pith_atomic_int_new(10);
        unsafe {
            assert_eq!(pith_atomic_int_fetch_add(h, 5), 10);
            assert_eq!(pith_atomic_int_get(h), 15);
            assert_eq!(pith_atomic_int_fetch_sub(h, 3), 15);
            assert_eq!(pith_atomic_int_get(h), 12);
            assert_eq!(pith_atomic_int_fetch_add(h, -2), 12);
            assert_eq!(pith_atomic_int_get(h), 10);
            pith_sync_release(h);
        }
    }
}
