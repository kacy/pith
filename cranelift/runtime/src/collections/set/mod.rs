//! Set[T]: a reference-counted collection of unique ints, strings or bytes.
//!
//! The table is a `hashbrown::HashMap` with unit values, used directly rather
//! than through `std` so membership tests can probe with a borrowed element
//! (`ElemRef`) and never build an owned one.

pub use crate::collections::key::{KeyFlavor as ElemType, KeyRef as ElemRef, OwnedKey as SetElement};
use crate::collections::key::require_flavor;
use crate::handle_registry;
use hashbrown::hash_map::EntryRef;
use hashbrown::HashMap;

mod lifecycle;
mod ops;
#[cfg(test)]
mod tests;

pub use lifecycle::*;
pub use ops::*;

#[inline(always)]
pub(super) fn require_set_flavor(actual: ElemType, wanted: ElemType, entry: &str) {
    require_flavor("set", "elements", actual, wanted, entry);
}

/// FFI-compatible set handle
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PithSet {
    /// Pointer to internal set implementation (pub for cross-module access)
    pub ptr: *mut (),
}

/// The set behind a handle.
/// `#[repr(C)]` pins `magic` at offset 0 for raw-pointer validation.
#[repr(C)]
pub struct SetImpl {
    /// Magic word for the fast validity check (see set_magic_ok)
    pub(super) magic: u32,
    /// Shared-handle refcount (see ListImpl.rc)
    pub(super) rc: std::sync::atomic::AtomicU32,
    /// The actual hash table storing unique elements.
    ///
    /// A map with unit values rather than a `HashSet` because only the map has
    /// `entry_ref`: it probes with the caller's own bytes and builds the set's
    /// copy of the element only when the add misses. `HashSet` is that same
    /// table with the values fixed at `()`, so this costs nothing per element.
    pub(super) data: HashMap<SetElement, ()>,
    /// Type tag for elements (0=int, 1=string, 2=bytes)
    pub(super) elem_type: ElemType,
}

impl SetImpl {
    pub(super) fn new(elem_type: ElemType, _elem_size: usize) -> Self {
        SetImpl {
            magic: SET_MAGIC,
            rc: std::sync::atomic::AtomicU32::new(1),
            data: HashMap::new(),
            elem_type,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.data.len()
    }

    /// Add a borrowed element, reporting whether it was new.
    ///
    /// The probe runs against the caller's own bytes; the set materialises its
    /// own copy of the element only on the vacant arm, so adding an element the
    /// set already holds allocates nothing.
    fn insert(&mut self, elem: &ElemRef<'_>) -> bool {
        match self.data.entry_ref(elem) {
            EntryRef::Occupied(_) => false,
            EntryRef::Vacant(entry) => {
                entry.insert(());
                true
            }
        }
    }

    pub(super) fn contains(&self, elem: &ElemRef<'_>) -> bool {
        self.data.contains_key(elem)
    }

    pub(super) fn remove(&mut self, elem: &ElemRef<'_>) -> bool {
        self.data.remove(elem).is_some()
    }

    pub(super) fn clear(&mut self) {
        self.data.clear();
    }

    fn iter(&self) -> impl Iterator<Item = &SetElement> {
        self.data.keys()
    }
}

/// Magic word for SetImpl ("PSET"). Distinct per collection kind so a list
/// handle passed where a set is expected still fails validation.
pub(super) const SET_MAGIC: u32 = 0x50534554;

/// Fast validity check: one memory read instead of a global registry lock
/// per access. Freed sets get their magic scrubbed in pith_set_free.
#[inline]
pub(super) unsafe fn set_magic_ok(ptr: *const ()) -> bool {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { set_magic_ok_for(ptr, crate::quarantine::Op::Use) }
}

/// `set_magic_ok`, naming what the caller does with the set in a quarantine
/// report.
#[inline(always)]
pub(super) unsafe fn set_magic_ok_for(ptr: *const (), op: crate::quarantine::Op) -> bool {
    handle_registry::plausibly_aligned::<SetImpl>(ptr)
        && unsafe {
            // SAFETY: Alignment verified above; `magic_is` reads the magic field to validate.
            crate::quarantine::magic_is(
                std::ptr::addr_of!((*(ptr as *const SetImpl)).magic),
                SET_MAGIC,
                op,
            )
        }
}

/// Validate `set` and borrow its `SetImpl` immutably for the duration of `f`.
#[inline(always)]
pub(super) unsafe fn with_set_ref<R>(set: PithSet, f: impl FnOnce(&SetImpl) -> R) -> Option<R> {
    if !unsafe { set_magic_ok(set.ptr as *const ()) } {
        return None;
    }
    // SAFETY: Magic check validated pointer; borrow is scoped to `f`.
    let impl_ref = unsafe { &*(set.ptr as *const SetImpl) };
    Some(f(impl_ref))
}

/// Validate `set` and borrow its `SetImpl` mutably for the duration of `f`.
#[inline(always)]
pub(super) unsafe fn with_set_mut<R>(set: PithSet, f: impl FnOnce(&mut SetImpl) -> R) -> Option<R> {
    if !unsafe { set_magic_ok(set.ptr as *const ()) } {
        return None;
    }
    // SAFETY: Magic check validated pointer; caller ensures exclusive access for `f`.
    let impl_mut = unsafe { &mut *(set.ptr as *mut SetImpl) };
    Some(f(impl_mut))
}

/// Validate `handle` and borrow its `SetImpl` immutably for the duration of `f`.
#[inline(always)]
pub(crate) unsafe fn with_set_ref_handle<R>(
    handle: i64,
    f: impl FnOnce(&SetImpl) -> R,
) -> Option<R> {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { with_set_ref(PithSet { ptr: handle as *mut () }, f) }
}

/// Validate `handle` and borrow its `SetImpl` mutably for the duration of `f`.
#[inline(always)]
pub(crate) unsafe fn with_set_mut_handle<R>(
    handle: i64,
    f: impl FnOnce(&mut SetImpl) -> R,
) -> Option<R> {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { with_set_mut(PithSet { ptr: handle as *mut () }, f) }
}

/// Validate two sets and borrow both immutably for the duration of `f`.
#[allow(dead_code)]
#[inline(always)]
pub(super) unsafe fn with_set_ref2<R>(
    a: PithSet,
    b: PithSet,
    f: impl FnOnce(&SetImpl, &SetImpl) -> R,
) -> Option<R> {
    if !unsafe { set_magic_ok(a.ptr as *const ()) }
        || !unsafe { set_magic_ok(b.ptr as *const ()) }
    {
        return None;
    }
    // SAFETY: Both pointers passed magic validation and both borrows are shared (`&SetImpl`),
    // so aliasing `a.ptr == b.ptr` is sound.
    let a_ref = unsafe { &*(a.ptr as *const SetImpl) };
    let b_ref = unsafe { &*(b.ptr as *const SetImpl) };
    Some(f(a_ref, b_ref))
}

/// Validate two set handles and borrow both immutably for the duration of `f`.
#[allow(dead_code)]
#[inline(always)]
pub(super) unsafe fn with_set_ref2_handle<R>(
    a: i64,
    b: i64,
    f: impl FnOnce(&SetImpl, &SetImpl) -> R,
) -> Option<R> {
    // SAFETY: Preconditions forwarded from caller.
    unsafe {
        with_set_ref2(
            PithSet { ptr: a as *mut () },
            PithSet { ptr: b as *mut () },
            f,
        )
    }
}

/// Validate `dst` (mutable) and `src` (shared) without aliasing UB.
#[allow(dead_code)]
#[inline(always)]
pub(super) unsafe fn with_set_mut_and_ref<R>(
    dst: PithSet,
    src: PithSet,
    on_distinct: impl FnOnce(&mut SetImpl, &SetImpl) -> R,
    on_same: impl FnOnce(&mut SetImpl) -> R,
) -> Option<R> {
    if !unsafe { set_magic_ok(dst.ptr as *const ()) } {
        return None;
    }
    if dst.ptr == src.ptr {
        // SAFETY: `dst` validated; only one `&mut` reference is minted.
        let same = unsafe { &mut *(dst.ptr as *mut SetImpl) };
        return Some(on_same(same));
    }
    if !unsafe { set_magic_ok(src.ptr as *const ()) } {
        return None;
    }
    // SAFETY: `dst` and `src` are both valid and distinct pointers.
    let dst_mut = unsafe { &mut *(dst.ptr as *mut SetImpl) };
    let src_ref = unsafe { &*(src.ptr as *const SetImpl) };
    Some(on_distinct(dst_mut, src_ref))
}

/// Borrow a non-null C-string element as `ElemRef::Str` for the duration of `f`.
#[inline(always)]
pub(super) unsafe fn with_cstr_elem<R>(s: *const i8, f: impl FnOnce(&ElemRef<'_>) -> R) -> Option<R> {
    // SAFETY: Preconditions forwarded from caller; borrow is scoped to `f`.
    unsafe { crate::ffi_util::with_cstr_bytes(s, |bytes| f(&ElemRef::Str(bytes))) }
}

/// Borrow a bytes handle's content as `ElemRef::Bytes` for the duration of `f`.
/// A null handle is the empty value, matching `pith_bytes_eq`.
#[inline(always)]
pub(super) unsafe fn with_bytes_elem<R>(handle: i64, f: impl FnOnce(&ElemRef<'_>) -> R) -> Option<R> {
    if handle == 0 {
        return Some(f(&ElemRef::Bytes(&[])));
    }
    // SAFETY: `with_bytes` validates `handle` and scopes the borrowed slice to `f`.
    unsafe { crate::bytes::with_bytes(handle, |bytes| f(&ElemRef::Bytes(bytes))) }
}
