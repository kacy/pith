//! Map reference-counting lifecycle, teardown, sharing, and cycle-GC hooks.

use super::{map_magic_ok_for, with_map_mut_handle, with_map_ref_handle, MapImpl, PithMap};
use crate::collections::list::{release_element, ListTypeTag};

impl MapImpl {
    /// Detach and clear every stored entry, returning `(val_tag, raw_handles)`
    /// so callers drop `&mut MapImpl` (and, on final release, scrub magic and
    /// free the shell) BEFORE releasing any child values. Primitive maps clear
    /// with zero allocation.
    #[must_use]
    pub(super) fn take_values_for_release(&mut self) -> (ListTypeTag, Vec<i64>) {
        if !self.val_is_heap() {
            self.clear();
            return (ListTypeTag::Primitive, Vec::new());
        }
        let tag = self.val_tag;
        let data = std::mem::take(&mut self.data);
        self.int_values8 = None;
        let vals: Vec<i64> = data.into_values().filter_map(|v| v.word()).collect();
        (tag, vals)
    }
}

/// Release heap values detached by `MapImpl::take_values_for_release`.
///
/// # Safety
/// Every handle in `vals` must hold one count of kind `tag`.
pub(super) unsafe fn release_taken_values(tag: ListTypeTag, vals: Vec<i64>) {
    if tag == ListTypeTag::Primitive {
        return;
    }
    for raw in vals {
        // SAFETY: Map owned one count per detached heap value.
        unsafe { release_element(tag, raw) };
    }
}
/// Release map and free memory
#[no_mangle]
pub unsafe extern "C" fn pith_map_release(map: PithMap) {
    // SAFETY: `map_magic_ok_for` validates alignment and magic word.
    if !unsafe { map_magic_ok_for(map.ptr as *const (), crate::quarantine::Op::Release) } {
        return;
    }
    // SAFETY: Magic validated; only atomic fields are accessed while shared.
    let impl_ref = unsafe { &*(map.ptr as *const MapImpl) };
    // cycle-gc suspect hook, before our count is given up: while we still
    // hold it the map cannot die under the hook (see `maybe_suspect_struct`
    // in runtime_core for why the ordering matters).
    if crate::cycle::cycle_gc_enabled()
        && impl_ref.rc.load(std::sync::atomic::Ordering::Relaxed) > 1
    {
        // SAFETY: Caller still holds a strong count so `map` stays live across the hook.
        unsafe { maybe_suspect_map(impl_ref, map.ptr as usize) };
    }
    let prev = impl_ref
        .rc
        .fetch_sub(1, std::sync::atomic::Ordering::Release);
    if prev > 1 {
        return;
    }
    if prev == 0 {
        crate::quarantine::over_release(crate::quarantine::Kind::Map);
        impl_ref.rc.store(0, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    acquire_last_count!(impl_ref.rc);

    // Detach values, scrub magic, and retire/free the shell BEFORE releasing
    // child values so a child's `Drop` hitting a GC safepoint can never observe
    // this dying map as a live root (and an `isolate_call` longjmp cannot leak
    // the shell).
    let buffered = crate::cycle::cycle_gc_enabled()
        && impl_ref
            .cycle_flags
            .load(std::sync::atomic::Ordering::Relaxed)
            & 1
            != 0;
    // SAFETY: Last strong count observed with acquire; `&MapImpl` borrow above
    // has ended so exclusive `&mut MapImpl` access cannot alias.
    let (tag, vals) = unsafe {
        let impl_mut = &mut *(map.ptr as *mut MapImpl);
        let taken = impl_mut.take_values_for_release();
        impl_mut.magic = 0;
        taken
    };
    if buffered {
        crate::cycle::graveyard_defer(map.ptr as usize, crate::cycle::CYCLE_KIND_MAP);
    } else {
        // SAFETY: Last owner, magic scrubbed, not deferred to graveyard.
        unsafe { drop_map_shell(map.ptr as *mut MapImpl) };
    }
    // SAFETY: Detached handles were owned by this map under `tag`.
    unsafe { release_taken_values(tag, vals) };
}

/// Mark a map as a cycle suspect and hand it to the buffer. Cold and
/// outlined so the release fast path stays frameless with the flag off.
#[cold]
#[inline(never)]
unsafe fn maybe_suspect_map(impl_ref: &MapImpl, ptr: usize) {
    if impl_ref
        .cycle_flags
        .fetch_or(1, std::sync::atomic::Ordering::Relaxed)
        & 1
        != 0
    {
        return; // already buffered
    }
    // SAFETY: Caller holds a strong count on `ptr` so it stays live across `cycle_suspect`.
    unsafe { crate::cycle::cycle_suspect(ptr, crate::cycle::CYCLE_KIND_MAP) };
}

/// Drop a map's buffered mark (overflow, or a collector drain). Magic-
/// checked, so a map that already died and was freed is a no-op.
pub(crate) unsafe fn cycle_clear_map_buffered(handle: i64) {
    let clear = |impl_ref: &MapImpl| {
        impl_ref
            .cycle_flags
            .fetch_and(!1, std::sync::atomic::Ordering::Relaxed);
    };
    // SAFETY: `with_map_ref_handle` validates `handle` before clearing its buffered bit.
    unsafe { with_map_ref_handle(handle, clear) };
}

// --- collector-facing accessors ---------------------------------------------
//
// mirrors the list accessors: the collection pass holds the world stopped,
// and every entry point magic-checks so a bad edge degrades to None or a
// no-op rather than a wild read.

/// The map's current reference count, or `None` when the handle no longer
/// validates (the map died into the graveyard).
pub(crate) unsafe fn cycle_map_strong_count(handle: i64) -> Option<u32> {
    let read_rc = |impl_ref: &MapImpl| impl_ref.rc.load(std::sync::atomic::Ordering::Relaxed);
    // SAFETY: `with_map_ref_handle` validates `handle` before reading its refcount.
    unsafe { with_map_ref_handle(handle, read_rc) }
}

/// Add `delta` to the reference count: the collector's teardown guard.
pub(crate) unsafe fn cycle_map_guard_strong(handle: i64, delta: u32) {
    let bump = |impl_ref: &MapImpl| {
        impl_ref
            .rc
            .fetch_add(delta, std::sync::atomic::Ordering::Relaxed);
    };
    // SAFETY: `with_map_ref_handle` validates `handle` before updating its refcount.
    unsafe { with_map_ref_handle(handle, bump) };
}

/// Report every value the map owns a count on, as (value, child code).
/// Keys are copies (ints, byte strings), never counted handles, so only the
/// value side has edges; the scalar fast path stores raw ints and owns none.
/// Mark the map shared (see `share.rs`); true when this call set the mark.
pub(crate) unsafe fn map_mark_shared(handle: i64) -> bool {
    let mark = |impl_ref: &MapImpl| {
        impl_ref
            .cycle_flags
            .fetch_or(
                crate::share::CONTAINER_SHARED_FLAG,
                std::sync::atomic::Ordering::Release,
            )
            & crate::share::CONTAINER_SHARED_FLAG
            == 0
    };
    // SAFETY: `with_map_ref_handle` validates `handle` before setting its shared flag.
    unsafe { with_map_ref_handle(handle, mark) }.unwrap_or(false)
}

fn visit_map_children(impl_ref: &MapImpl, f: &mut dyn FnMut(i64, u8)) {
    let Some(code) = crate::collections::list::cycle_child_code(impl_ref.val_tag) else {
        return;
    };
    for val in impl_ref.data.values() {
        match val.word() {
            Some(raw) if raw != 0 => f(raw, code),
            _ => {}
        }
    }
}

pub(crate) unsafe fn cycle_map_children(handle: i64, f: &mut dyn FnMut(i64, u8)) {
    // SAFETY: `with_map_ref_handle` validates `handle` before iterating its values.
    unsafe { with_map_ref_handle(handle, |impl_ref| visit_map_children(impl_ref, f)) };
}

/// The destruction body of a garbage map: detach and clear storage while
/// holding `&mut`, then release every value after the borrow ends so cyclic
/// back-edges cannot alias `&mut MapImpl`.
pub(crate) unsafe fn cycle_map_release_values(handle: i64) {
    // SAFETY: Collector holds the world stopped; `with_map_mut_handle` validates `handle`.
    let taken = unsafe { with_map_mut_handle(handle, MapImpl::take_values_for_release) };
    if let Some((tag, vals)) = taken {
        // SAFETY: Detached handles were owned by `handle` under `tag`.
        unsafe { release_taken_values(tag, vals) };
    }
}

/// Free a garbage map's shell: the tail of `pith_map_release` minus the value
/// cascade, which `cycle_map_release_values` already ran. A shell re-buffered
/// during teardown parks in the graveyard so the fresh suspect entry never
/// dangles.
pub(crate) unsafe fn cycle_map_free_dead(handle: i64) {
    let read_buffered = |impl_ref: &MapImpl| {
        impl_ref
            .cycle_flags
            .load(std::sync::atomic::Ordering::Relaxed)
            & 1
            != 0
    };
    // SAFETY: `with_map_ref_handle` validates `handle` and scopes the borrow before free.
    let Some(buffered) = (unsafe { with_map_ref_handle(handle, read_buffered) }) else {
        return;
    };
    // SAFETY: Borrow ended above; collector owns this dead map exclusively.
    unsafe { (*(handle as *mut MapImpl)).magic = 0 };
    if buffered && crate::cycle::cycle_gc_enabled() {
        crate::cycle::graveyard_defer(handle as usize, crate::cycle::CYCLE_KIND_MAP);
        return;
    }
    // SAFETY: Magic scrubbed and not deferred to graveyard.
    unsafe { drop_map_shell(handle as *mut MapImpl) };
}

/// Drop a map shell the graveyard parked: values already released, magic
/// already scrubbed, registry entry already gone; only the box remains.
pub(crate) unsafe fn cycle_drop_map_shell(ptr: usize) {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { drop_map_shell(ptr as *mut MapImpl) };
}

/// Free a dead map's box, or quarantine it.
unsafe fn drop_map_shell(ptr: *mut MapImpl) {
    // SAFETY: Caller passes a valid `Box::into_raw` `MapImpl` pointer to free or quarantine.
    unsafe {
        let magic = std::ptr::addr_of_mut!((*ptr).magic);
        crate::quarantine::free_box(ptr, magic, crate::quarantine::Kind::Map);
    }
}
/// Retain a map handle: one more owner of this shared handle.
#[no_mangle]
pub unsafe extern "C" fn pith_map_retain_handle(handle: i64) {
    crate::perf::perf_rc_op(crate::perf::RC_MAP_RETAIN);
    // SAFETY: Magic check validates handle before atomic retain increment.
    if unsafe { map_magic_ok_for(handle as *const (), crate::quarantine::Op::Retain) } {
        unsafe { &*(handle as *const MapImpl) }
            .rc
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Release a map handle; frees the map and its owned values at zero.
#[no_mangle]
pub unsafe extern "C" fn pith_map_release_handle(handle: i64) {
    crate::perf::perf_rc_op(crate::perf::RC_MAP_RELEASE);
    // SAFETY: Preconditions forwarded from caller.
    unsafe {
        pith_map_release(PithMap {
            ptr: handle as *mut (),
        });
    }
}

/// Destructor for map elements in collections
///
/// Called by cycle collector when freeing cyclic map objects
#[no_mangle]
pub extern "C" fn pith_map_destructor(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }

    let map = ptr as *const PithMap;
    // SAFETY: Caller guarantees `ptr` points to a valid `PithMap`.
    unsafe { pith_map_release(*map) };
}
