//! Suspect tracking for the trial-deletion cycle collector.
//!
//! ARC alone cannot reclaim a reference cycle: every object in the ring keeps
//! its neighbor's count above zero, so no release ever reaches the free path.
//! A trial-deletion collector (Bacon-Rajan) starts from *suspects*: objects
//! whose count was decremented but stayed above zero, the only way a garbage
//! cycle can form. It trial-decrements the counts internal to the candidate
//! graph, and frees the members whose counts reach zero with the internal
//! edges discounted. This module holds the suspect tracking (the buffer, the
//! per-object buffered bit, and the graveyard), the stop-the-world
//! rendezvous the collection pass runs inside (see the mutator registry
//! below), the collection pass, and the collector thread that runs it.
//!
//! Invariants the buffer maintains, in force only when `PITH_CYCLE_GC` is on:
//!
//! - a buffered struct holds one weak count owned by the buffer, so the
//!   32-byte header outlives the value and a buffer entry never dangles; a
//!   struct that dies while buffered is identified by bit 0 of its header
//!   flag word.
//! - a buffered closure, list, or map that dies runs its normal death path
//!   (children released, magic scrubbed, registry entry dropped) except the
//!   final free of the impl shell, which is deferred to the graveyard for the
//!   collector to reclaim. Everything inside the shell was already destructed,
//!   so the deferred drop frees only the shell's own storage.
//! - on buffer overflow the object is un-marked and (for a struct) the
//!   buffer's weak count is dropped: the failure mode is a leak, never a
//!   dangle.
//!
//! With the flag off, the default, every release path pays one relaxed
//! atomic load and a predicted branch, the same shape as the perf-stats gate.


#[cfg(test)]
use std::sync::atomic::Ordering;
#[cfg(test)]
use std::sync::Mutex;

pub(crate) mod collect;
pub(crate) mod stw;
pub(crate) mod suspects;
pub(crate) mod thread;
pub(crate) mod tracers;
#[cfg(test)]
mod tests;

pub use collect::{
    CYCLE_COLLECTIONS, CYCLE_GRAVEYARD_FREED, CYCLE_ROOTS_EXAMINED, CYCLE_WHITES_FREED,
};
pub(crate) use stw::{
    abandon_mutator_slot, adopt_mutator_slot, mutator_gate, mutator_slot_for_spawn, native_bracket,
    register_main_mutator, stop_requested, MutatorSlot,
};
pub use suspects::{
    CYCLE_GRAVEYARD_DEFERRED, CYCLE_SUSPECTS_BUFFERED, CYCLE_SUSPECTS_OVERFLOWED,
};
pub(crate) use suspects::{cycle_suspect, graveyard_defer};
pub use thread::{pith_cycle_gc_collect_now, pith_cycle_gc_stats};
pub use tracers::{
    pith_cc_register_tracer, pith_cc_register_user_drop, pith_cc_visit, CYCLE_VISITS_UNHOOKED,
};
pub(crate) use tracers::struct_children;

pub(crate) const CYCLE_KIND_STRUCT: u8 = 1;
pub(crate) const CYCLE_KIND_CLOSURE: u8 = 2;
pub(crate) const CYCLE_KIND_LIST: u8 = 3;
pub(crate) const CYCLE_KIND_MAP: u8 = 4;

/// Hard cap on buffered suspects. A full buffer stops admitting new suspects
/// until the collector drains it; the cycles those missed suspects rooted are
/// leaked, not collected, which is the acceptable direction to fail in.
pub const CYCLE_SUSPECT_CAP: usize = 65536;

/// Whether `PITH_CYCLE_GC` asked for suspect tracking.
#[inline(always)]
pub fn cycle_gc_enabled() -> bool {
    crate::switches::any(crate::switches::CYCLE_GC)
}
// test support: the enable state and the buffers are process globals, so
// every test that turns the flag on serializes on CYCLE_TEST_LOCK and puts
// the world back before releasing it.
#[cfg(test)]
pub(crate) static CYCLE_TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
pub(crate) fn force_enabled_for_tests(enabled: bool) {
    crate::switches::set_cycle_gc_for_tests(enabled);
}

#[cfg(test)]
pub(crate) fn suspects_len_for_tests() -> usize {
    suspects::lock_suspects().len()
}

// the timeout cell is process-global like the enable state; stop-the-world
// tests pin it explicitly (under CYCLE_TEST_LOCK) rather than inherit
// whatever an earlier test resolved.
#[cfg(test)]
pub(crate) fn set_stop_timeout_for_tests(ms: u64) {
    stw::STOP_TIMEOUT_MS.store(ms, Ordering::Relaxed);
}

#[cfg(test)]
pub(crate) fn tracer_count_for_tests() -> usize {
    tracers::lock_tracers().len()
}

#[cfg(test)]
pub(crate) fn suspect_count_for_tests(ptr: usize) -> usize {
    suspects::lock_suspects().iter().filter(|(p, _)| *p == ptr).count()
}

#[cfg(test)]
pub(crate) fn graveyard_count_for_tests(ptr: usize) -> usize {
    suspects::lock_graveyard().iter().filter(|(p, _)| *p == ptr).count()
}

/// Drain both buffers so the next test starts clean. Buffer entries are
/// discarded properly (bit cleared, struct weak count dropped, which frees
/// the header of a struct that died while buffered). Graveyard shells are
/// deliberately leaked rather than dropped here: freeing them belongs to a
/// collection pass, and a bounded leak inside the test process is harmless.
#[cfg(test)]
pub(crate) fn reset_for_tests() {
    let entries = std::mem::take(&mut *suspects::lock_suspects());
    for (ptr, kind) in entries {
        // SAFETY: Each entry came from the suspect buffer with a matching kind.
        unsafe { suspects::discard_suspect(ptr, kind) };
    }
    suspects::lock_graveyard().clear();
}
