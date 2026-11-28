//! Suspect buffer and graveyard queues for trial-deletion cycle collection.

use super::thread::wake_collector_if_needed;
use super::{CYCLE_KIND_CLOSURE, CYCLE_KIND_LIST, CYCLE_KIND_MAP, CYCLE_KIND_STRUCT, CYCLE_SUSPECT_CAP};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

// the suspect buffer and the graveyard are plain locked vecs: both are
// touched only with the flag on, from #[cold] outlined bodies, so contention
// is not a design constraint yet. entries are (pointer, kind code); struct
// entries hold the data pointer (base + 32), the form every struct entry
// point takes.
static SUSPECTS: Mutex<Vec<(usize, u8)>> = Mutex::new(Vec::new());
static GRAVEYARD: Mutex<Vec<(usize, u8)>> = Mutex::new(Vec::new());

/// Suspects admitted to the buffer since process start.
pub static CYCLE_SUSPECTS_BUFFERED: AtomicU64 = AtomicU64::new(0);
/// Suspects turned away by the cap (each one a potential leaked cycle).
pub static CYCLE_SUSPECTS_OVERFLOWED: AtomicU64 = AtomicU64::new(0);
/// Impl shells whose final free was deferred to the graveyard.
pub static CYCLE_GRAVEYARD_DEFERRED: AtomicU64 = AtomicU64::new(0);

// a poisoned lock only means some thread panicked mid-push; the vec itself
// is still coherent, and the release paths must not turn that into a second
// panic across the ffi boundary.
pub(super) fn lock_suspects() -> std::sync::MutexGuard<'static, Vec<(usize, u8)>> {
    crate::sync::lock(&SUSPECTS)
}

pub(super) fn lock_graveyard() -> std::sync::MutexGuard<'static, Vec<(usize, u8)>> {
    crate::sync::lock(&GRAVEYARD)
}

/// Un-mark an object the buffer is giving up on (overflow, or a test drain).
/// For a struct this also drops the weak count the buffer took. The clear
/// helpers all magic-check, so an object that died and was reclaimed in the
/// meantime degrades to a no-op.
pub(super) unsafe fn discard_suspect(ptr: usize, kind: u8) {
    match kind {
        CYCLE_KIND_STRUCT => {
            // SAFETY: Caller guarantees `ptr` came from a buffered struct entry.
            unsafe {
                crate::runtime_core::cycle_clear_struct_buffered(ptr as i64);
                crate::runtime_core::pith_struct_weak_release(ptr as i64);
            }
        }
        CYCLE_KIND_CLOSURE => {
            // SAFETY: Caller guarantees `ptr` came from a buffered closure entry.
            unsafe { crate::runtime_core::cycle_clear_closure_buffered(ptr as i64) }
        }
        CYCLE_KIND_LIST => {
            // SAFETY: Caller guarantees `ptr` came from a buffered list entry.
            unsafe { crate::collections::list::cycle_clear_list_buffered(ptr as i64) }
        }
        CYCLE_KIND_MAP => {
            // SAFETY: Caller guarantees `ptr` came from a buffered map entry.
            unsafe { crate::collections::map::cycle_clear_map_buffered(ptr as i64) }
        }
        _ => {}
    }
}

pub(super) fn admit_suspect(ptr: usize, kind: u8) -> Option<usize> {
    let mut suspects = lock_suspects();
    if suspects.len() < CYCLE_SUSPECT_CAP {
        suspects.push((ptr, kind));
        CYCLE_SUSPECTS_BUFFERED.fetch_add(1, Ordering::Relaxed);
        Some(suspects.len())
    } else {
        None
    }
}
/// Admit a suspect to the buffer, or turn it away at the cap. The caller has
/// already won the object's buffered bit (and, for a struct, taken the
/// buffer's weak count), so on overflow this undoes both: the object is
/// simply not tracked, and the cycle it might root is leaked.
///
/// # Safety
/// `ptr` must be the live object the caller just marked, in the form its
/// kind's entry points take (the data pointer for structs, the impl pointer
/// for the rest). The caller must still hold its own strong count on the
/// object, so the object cannot die under this call.
#[cold]
#[inline(never)]
pub(crate) unsafe fn cycle_suspect(ptr: usize, kind: u8) {
    // outside the buffer lock: the collector wake path takes its own lock,
    // and holding both here would order them against the collector's
    // wait-for-work, which reads the buffer length under the control lock.
    let Some(len) = admit_suspect(ptr, kind) else {
        CYCLE_SUSPECTS_OVERFLOWED.fetch_add(1, Ordering::Relaxed);
        // SAFETY: Caller guarantees `ptr` was just marked buffered and stays alive.
        unsafe { discard_suspect(ptr, kind) };
        return;
    };
    wake_collector_if_needed(len);
}

/// Park a dead closure/list/map shell for the collector to free. Called from
/// the final-death arm after children are released, the magic is scrubbed,
/// and the registry entry is gone. The shell no longer validates anywhere;
/// only its own allocation remains. Structs never come here: their header is
/// kept alive by the buffer's weak count instead.
#[cold]
#[inline(never)]
pub(crate) fn graveyard_defer(ptr: usize, kind: u8) {
    lock_graveyard().push((ptr, kind));
    CYCLE_GRAVEYARD_DEFERRED.fetch_add(1, Ordering::Relaxed);
}
