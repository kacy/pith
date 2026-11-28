//! List reference-counting lifecycle, teardown, sharing, and cycle-GC hooks.

use super::{
    list_magic_ok_for, release_taken_elements, retire_identity, take_elements_for_release,
    with_list_mut_handle, with_list_ref_handle, ListImpl, ListTypeTag, PithList,
};
use crate::child_kind;

/// Release list and free memory
#[no_mangle]
pub unsafe extern "C" fn pith_list_release(list: PithList) {
    // SAFETY: `list_magic_ok_for` validates alignment and magic word.
    if !unsafe { list_magic_ok_for(list.ptr as *const (), crate::quarantine::Op::Release) } {
        return;
    }
    // SAFETY: Magic validated; only atomic fields are accessed while shared.
    let impl_ref = unsafe { &*(list.ptr as *const ListImpl) };
    // cycle-gc suspect hook, before our count is given up: while we still
    // hold it the list cannot die under the hook (see `maybe_suspect_struct`
    // in runtime_core for why the ordering matters).
    if crate::cycle::cycle_gc_enabled()
        && impl_ref.rc.load(std::sync::atomic::Ordering::Relaxed) > 1
    {
        // SAFETY: Caller still holds a strong count so `list` stays live across the hook.
        unsafe { maybe_suspect_list(impl_ref, list.ptr as usize) };
    }
    let prev = impl_ref
        .rc
        .fetch_sub(1, std::sync::atomic::Ordering::Release);
    if prev > 1 {
        return;
    }
    if prev == 0 {
        crate::quarantine::over_release(crate::quarantine::Kind::List);
        // over-release: put the count back and refuse to double-free
        impl_ref.rc.store(0, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    acquire_last_count!(impl_ref.rc);

    // Detach owned elements, scrub magic, and retire/free the shell BEFORE
    // releasing child handles so a child's `Drop` hitting a GC safepoint can
    // never observe this dying list as a live root (and an `isolate_call`
    // longjmp cannot leak the shell).
    let buffered = crate::cycle::cycle_gc_enabled()
        && impl_ref
            .cycle_flags
            .load(std::sync::atomic::Ordering::Relaxed)
            & 1
            != 0;
    // SAFETY: Last strong count observed with acquire; `&ListImpl` borrow above
    // has ended so exclusive `&mut ListImpl` access cannot alias.
    let (tag, elems) = unsafe {
        let impl_mut = &mut *(list.ptr as *mut ListImpl);
        let taken = take_elements_for_release(impl_mut);
        impl_mut.magic = 0;
        retire_identity(list.ptr);
        taken
    };
    crate::perf_count(&crate::PERF_LIST_FREES, 1);
    if buffered {
        crate::cycle::graveyard_defer(list.ptr as usize, crate::cycle::CYCLE_KIND_LIST);
    } else {
        // SAFETY: Last owner, magic scrubbed, identity retired, not deferred to graveyard.
        unsafe { drop_list_shell(list.ptr as *mut ListImpl) };
    }
    // SAFETY: Detached handles were owned by this list under `tag`.
    unsafe { release_taken_elements(tag, elems) };
}

/// Mark a list as a cycle suspect and hand it to the buffer. Cold and
/// outlined so the release fast path stays frameless with the flag off.
#[cold]
#[inline(never)]
unsafe fn maybe_suspect_list(impl_ref: &ListImpl, ptr: usize) {
    if impl_ref
        .cycle_flags
        .fetch_or(1, std::sync::atomic::Ordering::Relaxed)
        & 1
        != 0
    {
        return; // already buffered
    }
    // SAFETY: Caller holds a strong count on `ptr` so it stays live across `cycle_suspect`.
    unsafe { crate::cycle::cycle_suspect(ptr, crate::cycle::CYCLE_KIND_LIST) };
}

/// Drop a list's buffered mark (overflow, or a collector drain). Magic-
/// checked, so a list that already died and was freed is a no-op.
pub(crate) unsafe fn cycle_clear_list_buffered(handle: i64) {
    let clear = |impl_ref: &ListImpl| {
        impl_ref
            .cycle_flags
            .fetch_and(!1, std::sync::atomic::Ordering::Relaxed);
    };
    // SAFETY: `with_list_ref_handle` validates `handle` before clearing its buffered bit.
    unsafe { with_list_ref_handle(handle, clear) };
}

// --- collector-facing accessors ---------------------------------------------
//
// the collection pass runs with the world stopped; these keep the field
// layout private to this file and magic-check every handle so a bad edge
// degrades to None or a no-op.

/// The `pith_cc_visit` child code for an element kind, or `None` when the
/// elements are not graph nodes: primitives carry no counts, and strings and
/// bytes are leaves the free-time cascade releases (they cannot sit in a
/// cycle). The codes are the closure env tag codes, the one child-kind
/// numbering the collector speaks.
/// Mark the list shared (see `share.rs`); true when this call set the mark.
pub(crate) unsafe fn list_mark_shared(handle: i64) -> bool {
    let mark = |impl_ref: &ListImpl| {
        impl_ref
            .cycle_flags
            .fetch_or(
                crate::share::CONTAINER_SHARED_FLAG,
                std::sync::atomic::Ordering::Release,
            )
            & crate::share::CONTAINER_SHARED_FLAG
            == 0
    };
    // SAFETY: `with_list_ref_handle` validates `handle` before setting its shared flag.
    unsafe { with_list_ref_handle(handle, mark) }.unwrap_or(false)
}

pub(crate) fn cycle_child_code(tag: ListTypeTag) -> Option<u8> {
    match tag {
        ListTypeTag::List => Some(child_kind::LIST),
        ListTypeTag::Map => Some(child_kind::MAP),
        ListTypeTag::Set => Some(child_kind::SET),
        ListTypeTag::Struct => Some(child_kind::STRUCT),
        ListTypeTag::Closure => Some(child_kind::CLOSURE),
        _ => None,
    }
}

/// The list's current reference count, or `None` when the handle no longer
/// validates (the list died into the graveyard).
pub(crate) unsafe fn cycle_list_strong_count(handle: i64) -> Option<u32> {
    let read_rc = |impl_ref: &ListImpl| impl_ref.rc.load(std::sync::atomic::Ordering::Relaxed);
    // SAFETY: `with_list_ref_handle` validates `handle` before reading its refcount.
    unsafe { with_list_ref_handle(handle, read_rc) }
}

/// Add `delta` to the reference count: the collector's teardown guard.
pub(crate) unsafe fn cycle_list_guard_strong(handle: i64, delta: u32) {
    let bump = |impl_ref: &ListImpl| {
        impl_ref
            .rc
            .fetch_add(delta, std::sync::atomic::Ordering::Relaxed);
    };
    // SAFETY: `with_list_ref_handle` validates `handle` before updating its refcount.
    unsafe { with_list_ref_handle(handle, bump) };
}

fn visit_list_children(impl_ref: &ListImpl, f: &mut dyn FnMut(i64, u8)) {
    let Some(code) = cycle_child_code(impl_ref.type_tag) else {
        return;
    };
    for i in 0..impl_ref.len() {
        if let Some(raw) = impl_ref.get_value(i) {
            if raw != 0 {
                f(raw, code);
            }
        }
    }
}

/// Report every element the list owns a count on, as (value, child code).
pub(crate) unsafe fn cycle_list_children(handle: i64, f: &mut dyn FnMut(i64, u8)) {
    // SAFETY: `with_list_ref_handle` validates `handle` before iterating its elements.
    unsafe { with_list_ref_handle(handle, |impl_ref| visit_list_children(impl_ref, f)) };
}

/// The destruction body of a garbage list: detach and clear storage while
/// holding `&mut`, then release every element after the borrow ends so cyclic
/// back-edges cannot alias `&mut ListImpl`.
pub(crate) unsafe fn cycle_list_release_elements(handle: i64) {
    // SAFETY: Collector holds the world stopped; `with_list_mut_handle` validates `handle`.
    let taken = unsafe { with_list_mut_handle(handle, take_elements_for_release) };
    if let Some((tag, elems)) = taken {
        // SAFETY: Detached handles were owned by `handle` under `tag`.
        unsafe { release_taken_elements(tag, elems) };
    }
}

/// Free a garbage list's shell: the tail of `pith_list_release` minus the
/// element cascade, which `cycle_list_release_elements` already ran. A shell
/// re-buffered during teardown parks in the graveyard so the fresh suspect
/// entry never dangles.
pub(crate) unsafe fn cycle_list_free_dead(handle: i64) {
    let read_buffered = |impl_ref: &ListImpl| {
        impl_ref
            .cycle_flags
            .load(std::sync::atomic::Ordering::Relaxed)
            & 1
            != 0
    };
    // SAFETY: `with_list_ref_handle` validates `handle` and scopes the borrow before free.
    let Some(buffered) = (unsafe { with_list_ref_handle(handle, read_buffered) }) else {
        return;
    };
    // SAFETY: Borrow ended above; collector owns this dead list exclusively.
    unsafe {
        (*(handle as *mut ListImpl)).magic = 0;
        retire_identity(handle as *mut ());
    }
    crate::perf_count(&crate::PERF_LIST_FREES, 1);
    if buffered && crate::cycle::cycle_gc_enabled() {
        crate::cycle::graveyard_defer(handle as usize, crate::cycle::CYCLE_KIND_LIST);
        return;
    }
    // SAFETY: Magic scrubbed, identity retired, and not deferred to graveyard.
    unsafe { drop_list_shell(handle as *mut ListImpl) };
}

/// Drop a list shell the graveyard parked: elements already released, magic
/// already scrubbed, registry entry already gone; only the box remains.
pub(crate) unsafe fn cycle_drop_list_shell(ptr: usize) {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { drop_list_shell(ptr as *mut ListImpl) };
}

/// Free a dead list's box, or quarantine it.
unsafe fn drop_list_shell(ptr: *mut ListImpl) {
    // SAFETY: Caller passes a valid `Box::into_raw` `ListImpl` pointer to free or quarantine.
    unsafe {
        let magic = std::ptr::addr_of_mut!((*ptr).magic);
        crate::quarantine::free_box(ptr, magic, crate::quarantine::Kind::List);
    }
}

/// Retain a list handle: one more owner of this shared handle.
#[no_mangle]
pub unsafe extern "C" fn pith_list_retain_handle(handle: i64) {
    crate::perf::perf_rc_op(crate::perf::RC_LIST_RETAIN);
    // SAFETY: Magic check validates handle before atomic retain increment.
    if unsafe { list_magic_ok_for(handle as *const (), crate::quarantine::Op::Retain) } {
        unsafe { &*(handle as *const ListImpl) }
            .rc
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Release a list handle; frees the list and its owned elements at zero.
#[no_mangle]
pub unsafe extern "C" fn pith_list_release_handle(handle: i64) {
    crate::perf::perf_rc_op(crate::perf::RC_LIST_RELEASE);
    // SAFETY: Preconditions forwarded from caller.
    unsafe {
        pith_list_release(PithList {
            ptr: handle as *mut (),
        });
    }
}

/// Destructor for list elements in collections
///
/// Called by cycle collector when freeing cyclic list objects
#[no_mangle]
pub extern "C" fn pith_list_destructor(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }

    let list = ptr as *const PithList;
    // SAFETY: Caller guarantees `ptr` points to a valid `PithList`.
    unsafe { pith_list_release(*list) };
}
