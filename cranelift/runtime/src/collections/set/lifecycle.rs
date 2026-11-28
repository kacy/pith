//! Set reference-counting lifecycle, teardown, and cycle-GC hooks.

use super::{set_magic_ok, set_magic_ok_for, with_set_mut_handle, with_set_ref_handle, PithSet, SetImpl};

/// Release set and free memory
#[no_mangle]
pub unsafe extern "C" fn pith_set_release(set: PithSet) {
    // SAFETY: `set_magic_ok_for` validates alignment and magic word.
    if !unsafe { set_magic_ok_for(set.ptr as *const (), crate::quarantine::Op::Release) } {
        return;
    }
    // SAFETY: Magic validated; only the atomic refcount is accessed while shared.
    let impl_ref = unsafe { &*(set.ptr as *const SetImpl) };
    let prev = impl_ref
        .rc
        .fetch_sub(1, std::sync::atomic::Ordering::Release);
    if prev > 1 {
        return;
    }
    if prev == 0 {
        crate::quarantine::over_release(crate::quarantine::Kind::Set);
        impl_ref.rc.store(0, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    acquire_last_count!(impl_ref.rc);

    // Free the set implementation. Scrub the magic first so any handle
    // that outlives the set fails the fast validity check.
    // SAFETY: Last strong count observed with acquire; `impl_ref` borrow ended above.
    unsafe {
        (*(set.ptr as *mut SetImpl)).magic = 0;
        drop_set_shell(set.ptr as *mut SetImpl);
    }
}

/// Retain a set handle: one more owner of this shared handle.
#[no_mangle]
pub unsafe extern "C" fn pith_set_retain_handle(handle: i64) {
    crate::perf::perf_rc_op(crate::perf::RC_SET_RETAIN);
    // SAFETY: Magic check validates handle before atomic retain increment.
    if unsafe { set_magic_ok_for(handle as *const (), crate::quarantine::Op::Retain) } {
        unsafe { &*(handle as *const SetImpl) }
            .rc
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Release a set handle; frees the set and its owned elements at zero.
#[no_mangle]
pub unsafe extern "C" fn pith_set_release_handle(handle: i64) {
    crate::perf::perf_rc_op(crate::perf::RC_SET_RELEASE);
    // SAFETY: Preconditions forwarded from caller.
    unsafe {
        pith_set_release(PithSet {
            ptr: handle as *mut (),
        });
    }
}

// --- collector-facing accessors ---------------------------------------------
//
// a set stores only ints and byte strings, so it holds no edges into the
// ownership graph and the collector never walks into one. it still has a
// reference count of its own, though, so a set every one of whose owners is
// garbage is garbage too. these let the collection pass count it and, when
// it turns out white, tear it down. every entry point magic-checks so a bad
// edge degrades to None or a no-op.

/// The set's current reference count, or `None` for an invalid handle.
pub(crate) unsafe fn cycle_set_strong_count(handle: i64) -> Option<u32> {
    let read_rc = |impl_ref: &SetImpl| impl_ref.rc.load(std::sync::atomic::Ordering::Relaxed);
    // SAFETY: `with_set_ref_handle` validates `handle` before reading its refcount.
    unsafe { with_set_ref_handle(handle, read_rc) }
}

/// Add `delta` to the reference count: the collector's teardown guard.
pub(crate) unsafe fn cycle_set_guard_strong(handle: i64, delta: u32) {
    let bump = |impl_ref: &SetImpl| {
        impl_ref
            .rc
            .fetch_add(delta, std::sync::atomic::Ordering::Relaxed);
    };
    // SAFETY: `with_set_ref_handle` validates `handle` before updating its refcount.
    unsafe { with_set_ref_handle(handle, bump) };
}

/// The destruction body of a garbage set: drop the owned elements by
/// clearing, leaving an empty set so the shell free cannot repeat it.
pub(crate) unsafe fn cycle_set_release_elements(handle: i64) {
    // SAFETY: Collector holds the world stopped; `with_set_mut_handle` validates `handle`.
    unsafe { with_set_mut_handle(handle, SetImpl::clear) };
}

/// Free a garbage set's shell: the tail of `pith_set_release` minus the
/// element loop, which `cycle_set_release_elements` already ran. Sets carry
/// no buffered bit and never enter the suspect buffer, so a plain drop is
/// always safe here.
pub(crate) unsafe fn cycle_set_free_dead(handle: i64) {
    // SAFETY: Magic check validates `handle` before scrubbing and freeing.
    if !unsafe { set_magic_ok(handle as *const ()) } {
        return;
    }
    // SAFETY: Collector owns this dead set exclusively.
    unsafe {
        (*(handle as *mut SetImpl)).magic = 0;
        drop_set_shell(handle as *mut SetImpl);
    }
}

/// Free a dead set's box, or quarantine it.
unsafe fn drop_set_shell(ptr: *mut SetImpl) {
    // SAFETY: Caller passes a valid `Box::into_raw` `SetImpl` pointer to free or quarantine.
    unsafe {
        let magic = std::ptr::addr_of_mut!((*ptr).magic);
        crate::quarantine::free_box(ptr, magic, crate::quarantine::Kind::Set);
    }
}
/// Destructor for set elements in collections
///
/// Called by cycle collector when freeing cyclic set objects
#[no_mangle]
pub extern "C" fn pith_set_destructor(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }

    let set = ptr as *const PithSet;
    // SAFETY: Caller guarantees `ptr` points to a valid `PithSet`.
    unsafe { pith_set_release(*set) };
}
