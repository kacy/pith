//! Suspect tracking for the trial-deletion cycle collector.
//!
//! ARC alone cannot reclaim a reference cycle: every object in the ring keeps
//! its neighbor's count above zero, so no release ever reaches the free path.
//! A trial-deletion collector (Bacon-Rajan) starts from *suspects* — objects
//! whose count was decremented but stayed above zero, the only way a garbage
//! cycle can form — trial-decrements the counts internal to the candidate
//! graph, and frees the members whose counts reach zero with the internal
//! edges discounted. This module is the suspect-tracking half only: the
//! buffer, the per-object buffered bit, and the graveyard. The collection
//! pass lands separately.
//!
//! Invariants the buffer maintains, in force only when `PITH_CYCLE_GC` is on:
//!
//! - a buffered struct holds one weak count owned by the buffer, so the
//!   32-byte header outlives the value and a buffer entry never dangles; a
//!   struct that dies while buffered is identified by bit 0 of its dead word.
//! - a buffered closure, list, or map that dies runs its normal death path —
//!   children released, magic scrubbed, registry entry dropped — except the
//!   final free of the impl shell, which is deferred to the graveyard for the
//!   collector to reclaim. Everything inside the shell was already destructed,
//!   so the deferred drop frees only the shell's own storage.
//! - on buffer overflow the object is un-marked and (for a struct) the
//!   buffer's weak count is dropped: the failure mode is a leak, never a
//!   dangle.
//!
//! With the flag off — the default — every release path pays one relaxed
//! atomic load and a predicted branch, the same shape as the perf-stats gate.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Mutex;

/// Kind codes for buffer and graveyard entries. These resemble the element
/// tag codes in `collections::list::element_tag_from_code`, but they are an
/// independent runtime-internal enum, not part of the ir contract — only the
/// four kinds that can participate in a cycle appear here, and the numbering
/// owes the wire codes nothing.
pub(crate) const CYCLE_KIND_STRUCT: u8 = 1;
pub(crate) const CYCLE_KIND_CLOSURE: u8 = 2;
pub(crate) const CYCLE_KIND_LIST: u8 = 3;
pub(crate) const CYCLE_KIND_MAP: u8 = 4;

/// Hard cap on buffered suspects. A full buffer stops admitting new suspects
/// until the collector drains it; the cycles those missed suspects rooted are
/// leaked, not collected, which is the acceptable direction to fail in.
pub const CYCLE_SUSPECT_CAP: usize = 65536;

// 0 = not probed yet, 1 = disabled, 2 = enabled. probed once from the env on
// first use; the settled path is one relaxed load and a predictable branch,
// same as the struct-pool and perf-stats gates.
static CYCLE_GC_STATE: AtomicU8 = AtomicU8::new(0);

#[cold]
fn cycle_gc_probe() -> bool {
    let enabled = matches!(
        std::env::var("PITH_CYCLE_GC").as_deref(),
        Ok("1") | Ok("on") | Ok("true")
    );
    CYCLE_GC_STATE.store(if enabled { 2 } else { 1 }, Ordering::Relaxed);
    enabled
}

/// Whether `PITH_CYCLE_GC` asked for suspect tracking. Cheap enough for the
/// release fast paths: a relaxed load and a branch once the probe has run.
#[inline(always)]
pub fn cycle_gc_enabled() -> bool {
    match CYCLE_GC_STATE.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => cycle_gc_probe(),
    }
}

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
fn lock_suspects() -> std::sync::MutexGuard<'static, Vec<(usize, u8)>> {
    match SUSPECTS.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn lock_graveyard() -> std::sync::MutexGuard<'static, Vec<(usize, u8)>> {
    match GRAVEYARD.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Un-mark an object the buffer is giving up on (overflow, or a test drain).
/// For a struct this also drops the weak count the buffer took. The clear
/// helpers all magic-check, so an object that died and was reclaimed in the
/// meantime degrades to a no-op.
unsafe fn discard_suspect(ptr: usize, kind: u8) {
    match kind {
        CYCLE_KIND_STRUCT => {
            crate::runtime_core::cycle_clear_struct_buffered(ptr as i64);
            crate::runtime_core::pith_struct_weak_release(ptr as i64);
        }
        CYCLE_KIND_CLOSURE => crate::runtime_core::cycle_clear_closure_buffered(ptr as i64),
        CYCLE_KIND_LIST => crate::collections::list::cycle_clear_list_buffered(ptr as i64),
        CYCLE_KIND_MAP => crate::collections::map::cycle_clear_map_buffered(ptr as i64),
        _ => {}
    }
}

/// Admit a suspect to the buffer, or turn it away at the cap. The caller has
/// already won the object's buffered bit (and, for a struct, taken the
/// buffer's weak count), so on overflow this undoes both — the object is
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
    {
        let mut suspects = lock_suspects();
        if suspects.len() < CYCLE_SUSPECT_CAP {
            suspects.push((ptr, kind));
            CYCLE_SUSPECTS_BUFFERED.fetch_add(1, Ordering::Relaxed);
            return;
        }
    }
    CYCLE_SUSPECTS_OVERFLOWED.fetch_add(1, Ordering::Relaxed);
    discard_suspect(ptr, kind);
}

/// Park a dead closure/list/map shell for the collector to free. Called from
/// the final-death arm after children are released, the magic is scrubbed,
/// and the registry entry is gone — the shell no longer validates anywhere,
/// only its own allocation remains. Structs never come here: their header is
/// kept alive by the buffer's weak count instead.
#[cold]
#[inline(never)]
pub(crate) fn graveyard_defer(ptr: usize, kind: u8) {
    lock_graveyard().push((ptr, kind));
    CYCLE_GRAVEYARD_DEFERRED.fetch_add(1, Ordering::Relaxed);
}

/// Print the suspect-tracking counters to stderr.
#[no_mangle]
pub extern "C" fn pith_cycle_gc_stats() {
    eprintln!(
        "pith cycle gc: buffered={} overflowed={} graveyard_deferred={} pending_suspects={} pending_graveyard={}",
        CYCLE_SUSPECTS_BUFFERED.load(Ordering::Relaxed),
        CYCLE_SUSPECTS_OVERFLOWED.load(Ordering::Relaxed),
        CYCLE_GRAVEYARD_DEFERRED.load(Ordering::Relaxed),
        lock_suspects().len(),
        lock_graveyard().len(),
    );
}

// test support: the enable state and the buffers are process globals, so
// every test that turns the flag on serializes on CYCLE_TEST_LOCK and puts
// the world back before releasing it. the probe is bypassed by storing the
// settled state directly.
#[cfg(test)]
pub(crate) static CYCLE_TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
pub(crate) fn force_enabled_for_tests(enabled: bool) {
    CYCLE_GC_STATE.store(if enabled { 2 } else { 1 }, Ordering::Relaxed);
}

#[cfg(test)]
pub(crate) fn suspects_len_for_tests() -> usize {
    lock_suspects().len()
}

#[cfg(test)]
pub(crate) fn suspect_count_for_tests(ptr: usize) -> usize {
    lock_suspects().iter().filter(|(p, _)| *p == ptr).count()
}

#[cfg(test)]
pub(crate) fn graveyard_count_for_tests(ptr: usize) -> usize {
    lock_graveyard().iter().filter(|(p, _)| *p == ptr).count()
}

/// Drain both buffers so the next test starts clean. Buffer entries are
/// discarded properly (bit cleared, struct weak count dropped — which frees
/// the header of a struct that died while buffered). Graveyard shells are
/// deliberately leaked: the collector that frees them does not exist yet,
/// and a bounded leak inside the test process is harmless.
#[cfg(test)]
pub(crate) fn reset_for_tests() {
    let entries = std::mem::take(&mut *lock_suspects());
    for (ptr, kind) in entries {
        unsafe { discard_suspect(ptr, kind) };
    }
    lock_graveyard().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collections::list::{
        pith_list_len, pith_list_push_value, pith_list_release, pith_list_retain_handle,
    };
    use crate::runtime_core::{
        closure_buffered_for_tests, pith_closure_get_fn, pith_closure_new, pith_closure_release,
        pith_closure_retain, pith_closure_set_env_rc, pith_struct_alloc, pith_struct_release,
        pith_struct_retain, pith_struct_weak_load, pith_struct_weak_release,
        pith_struct_weak_retain, struct_dead_word_for_tests, struct_weak_count_for_tests,
    };

    fn locked() -> std::sync::MutexGuard<'static, ()> {
        match CYCLE_TEST_LOCK.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    #[test]
    fn flag_off_release_paths_stay_untouched() {
        let _guard = locked();
        force_enabled_for_tests(false);
        reset_for_tests();
        unsafe {
            let s = pith_struct_alloc(2);
            pith_struct_retain(s);
            pith_struct_release(s); // 2 -> 1: would buffer with the flag on
            assert_eq!(struct_dead_word_for_tests(s), 0);
            assert_eq!(struct_weak_count_for_tests(s), 1);
            assert_eq!(suspect_count_for_tests(s as usize), 0);
            pith_struct_release(s); // frees normally

            let list = crate::collections::list::pith_list_new(8, 0);
            pith_list_retain_handle(list.ptr as i64);
            pith_list_release(list);
            assert_eq!(
                suspect_count_for_tests(list.ptr as usize),
                0,
                "flag off must keep the buffer empty"
            );
            pith_list_release(list); // frees normally, no graveyard
            assert_eq!(graveyard_count_for_tests(list.ptr as usize), 0);
        }
        assert_eq!(suspects_len_for_tests(), 0);
    }

    #[test]
    fn struct_decremented_to_nonzero_is_buffered_exactly_once() {
        let _guard = locked();
        force_enabled_for_tests(true);
        reset_for_tests();
        unsafe {
            let s = pith_struct_alloc(2);
            pith_struct_retain(s);
            pith_struct_retain(s); // strong 3
            pith_struct_release(s); // 3 -> 2: buffered
            assert_eq!(suspect_count_for_tests(s as usize), 1);
            assert_eq!(
                struct_dead_word_for_tests(s),
                2,
                "buffered bit set, not dead"
            );
            assert_eq!(struct_weak_count_for_tests(s), 2, "buffer holds one weak");
            pith_struct_release(s); // 2 -> 1: already buffered, no second entry
            assert_eq!(suspect_count_for_tests(s as usize), 1);
            assert_eq!(struct_weak_count_for_tests(s), 2);

            force_enabled_for_tests(false);
            reset_for_tests(); // clears the bit, drops the buffer's weak
            assert_eq!(struct_dead_word_for_tests(s), 0);
            assert_eq!(struct_weak_count_for_tests(s), 1);
            pith_struct_release(s);
        }
    }

    #[test]
    fn buffered_struct_survives_the_death_of_its_value() {
        let _guard = locked();
        force_enabled_for_tests(true);
        reset_for_tests();
        unsafe {
            let s = pith_struct_alloc(1);
            pith_struct_retain(s);
            pith_struct_release(s); // 2 -> 1: buffered, weak now 2
            pith_struct_release(s); // 1 -> 0: dies while buffered
                                    // dead bit and buffered bit both set; the buffer's weak count
                                    // is the only thing keeping the header alive.
            assert_eq!(struct_dead_word_for_tests(s), 3);
            assert_eq!(struct_weak_count_for_tests(s), 1);
            assert_eq!(pith_struct_weak_load(s), 0, "dead reads as none");
            assert_eq!(suspect_count_for_tests(s as usize), 1);

            force_enabled_for_tests(false);
            reset_for_tests(); // drops the buffer's weak: header freed here
        }
    }

    #[test]
    fn weak_load_ignores_the_buffered_bit() {
        let _guard = locked();
        force_enabled_for_tests(true);
        reset_for_tests();
        unsafe {
            let s = pith_struct_alloc(1);
            pith_struct_weak_retain(s);
            pith_struct_retain(s);
            pith_struct_release(s); // buffered, still alive
            assert_eq!(struct_dead_word_for_tests(s), 2);
            assert_eq!(pith_struct_weak_load(s), s, "buffered but alive");
            pith_struct_release(s); // dies buffered
            assert_eq!(pith_struct_weak_load(s), 0, "buffered and dead");
            pith_struct_weak_release(s);

            force_enabled_for_tests(false);
            reset_for_tests();
        }
    }

    #[test]
    fn buffered_list_dies_into_the_graveyard() {
        let _guard = locked();
        force_enabled_for_tests(true);
        reset_for_tests();
        unsafe {
            // a struct element observed through a weak reference proves the
            // list's death still cascades into its elements.
            let elem = pith_struct_alloc(1);
            pith_struct_weak_retain(elem);

            let list = crate::collections::list::pith_list_new(8, 4); // struct elements
            pith_list_push_value(list, elem); // list retains
            pith_struct_release(elem); // list holds the only strong count
            assert_eq!(pith_struct_weak_load(elem), elem);

            pith_list_retain_handle(list.ptr as i64);
            pith_list_release(list); // 2 -> 1: buffered
            assert_eq!(suspect_count_for_tests(list.ptr as usize), 1);
            pith_list_release(list); // 1 -> 0: dies buffered

            assert_eq!(graveyard_count_for_tests(list.ptr as usize), 1);
            assert_eq!(pith_struct_weak_load(elem), 0, "element was released");
            assert_eq!(
                pith_list_len(list),
                0,
                "magic scrubbed: handle no longer validates"
            );
            pith_list_retain_handle(list.ptr as i64); // must be a no-op, not a revival

            pith_struct_weak_release(elem);
            force_enabled_for_tests(false);
            reset_for_tests();
        }
    }

    #[test]
    fn buffered_closure_dies_into_the_graveyard() {
        let _guard = locked();
        force_enabled_for_tests(true);
        reset_for_tests();
        unsafe {
            let captured = pith_struct_alloc(1);
            pith_struct_weak_retain(captured);

            let closure = pith_closure_new(0x1000);
            // tag 6 = struct, per the closure env tag codes; ownership of the
            // strong count transfers into the closure.
            pith_closure_set_env_rc(closure, 0, captured, 6);

            pith_closure_retain(closure);
            pith_closure_release(closure); // 2 -> 1: buffered
            assert!(closure_buffered_for_tests(closure));
            assert_eq!(suspect_count_for_tests(closure as usize), 1);
            pith_closure_release(closure); // 1 -> 0: dies buffered

            assert_eq!(graveyard_count_for_tests(closure as usize), 1);
            assert_eq!(pith_struct_weak_load(captured), 0, "capture was released");
            assert_eq!(pith_closure_get_fn(closure), 0, "magic scrubbed");

            pith_struct_weak_release(captured);
            force_enabled_for_tests(false);
            reset_for_tests();
        }
    }

    #[test]
    fn overflow_degrades_to_a_leak_never_a_dangle() {
        let _guard = locked();
        force_enabled_for_tests(true);
        reset_for_tests();
        unsafe {
            let mut kept = Vec::new();
            while suspects_len_for_tests() < CYCLE_SUSPECT_CAP {
                let s = pith_struct_alloc(1);
                pith_struct_retain(s);
                pith_struct_release(s); // buffered
                kept.push(s);
            }

            let overflowed_before = CYCLE_SUSPECTS_OVERFLOWED.load(Ordering::Relaxed);
            let victim = pith_struct_alloc(1);
            pith_struct_retain(victim);
            pith_struct_release(victim); // buffer full: turned away
            assert_eq!(
                suspect_count_for_tests(victim as usize),
                0,
                "no push at cap"
            );
            assert_eq!(struct_dead_word_for_tests(victim), 0, "bit cleared again");
            assert_eq!(
                struct_weak_count_for_tests(victim),
                1,
                "weak released again"
            );
            assert!(CYCLE_SUSPECTS_OVERFLOWED.load(Ordering::Relaxed) > overflowed_before);

            force_enabled_for_tests(false);
            reset_for_tests();
            pith_struct_release(victim);
            for s in kept {
                pith_struct_release(s);
            }
        }
    }

    #[test]
    fn concurrent_releases_buffer_a_shared_struct_once() {
        let _guard = locked();
        force_enabled_for_tests(true);
        reset_for_tests();
        unsafe {
            let s = pith_struct_alloc(1);
            for _ in 0..8 {
                pith_struct_retain(s); // strong 9: one per releasing step + ours
            }
            let a = std::thread::spawn(move || {
                for _ in 0..4 {
                    pith_struct_release(s);
                }
            });
            let b = std::thread::spawn(move || {
                for _ in 0..4 {
                    pith_struct_release(s);
                }
            });
            a.join().expect("releasing thread panicked");
            b.join().expect("releasing thread panicked");

            assert_eq!(pith_struct_weak_load(s), s, "still alive: we hold a count");
            assert_eq!(
                suspect_count_for_tests(s as usize),
                1,
                "one entry, many releases"
            );
            assert_eq!(struct_dead_word_for_tests(s), 2);
            assert_eq!(struct_weak_count_for_tests(s), 2);

            force_enabled_for_tests(false);
            reset_for_tests();
            pith_struct_release(s);
        }
    }
}
