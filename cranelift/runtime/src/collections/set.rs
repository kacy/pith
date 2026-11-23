//! Set[T]: a reference-counted collection of unique ints, strings or bytes.
//!
//! The table is a `hashbrown::HashMap` with unit values, used directly rather
//! than through `std` so membership tests can probe with a borrowed element
//! (`ElemRef`) and never build an owned one.

use crate::handle_registry;
use hashbrown::hash_map::EntryRef;
use hashbrown::{Equivalent, HashMap};
use std::hash::{Hash, Hasher};

/// FFI-compatible set handle
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PithSet {
    /// Pointer to internal set implementation (pub for cross-module access)
    pub ptr: *mut (),
}

/// Set element type for the internal HashSet.
///
/// A string and a bytes element are both stored as an owned copy of their
/// content, which is what makes membership a content question: two `Bytes`
/// values built by different routes land in one entry when their bytes
/// agree, exactly as two strings do. The two variants stay distinct so a
/// set never mixes flavors, though the constructor's element tag is what
/// keeps the entry points in their own flavor (see `require_flavor`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetElement {
    Int(i64),
    String(Vec<u8>),
    Bytes(Vec<u8>),
}

impl Hash for SetElement {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            SetElement::Int(n) => {
                0u8.hash(state);
                n.hash(state);
            }
            SetElement::String(bytes) => {
                1u8.hash(state);
                bytes.hash(state);
            }
            SetElement::Bytes(bytes) => {
                2u8.hash(state);
                bytes.hash(state);
            }
        }
    }
}

/// An element the caller already holds the bytes of: the probe form of
/// `SetElement`.
///
/// A membership test, a removal, and an add of an element the set already
/// holds are all questions about content the caller is holding already, so
/// none of them needs the set's own copy. Only an add that misses does, and
/// `From<&ElemRef> for SetElement` is the one place that copy is made.
///
/// As with `KeyRef` in `map.rs`, the `Hash` impl has to write exactly the
/// bytes `SetElement`'s writes for the same content, or a set silently stops
/// finding elements it holds. `elem_ref_hashes_like_set_element` pins it.
#[derive(Clone, Copy, Debug)]
pub enum ElemRef<'a> {
    Int(i64),
    Str(&'a [u8]),
    Bytes(&'a [u8]),
}

impl Hash for ElemRef<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match *self {
            ElemRef::Int(n) => {
                0u8.hash(state);
                n.hash(state);
            }
            ElemRef::Str(bytes) => {
                1u8.hash(state);
                bytes.hash(state);
            }
            ElemRef::Bytes(bytes) => {
                2u8.hash(state);
                bytes.hash(state);
            }
        }
    }
}

impl Equivalent<SetElement> for ElemRef<'_> {
    fn equivalent(&self, elem: &SetElement) -> bool {
        match (*self, elem) {
            (ElemRef::Int(a), SetElement::Int(b)) => a == *b,
            (ElemRef::Str(a), SetElement::String(b)) => a == b.as_slice(),
            (ElemRef::Bytes(a), SetElement::Bytes(b)) => a == b.as_slice(),
            _ => false,
        }
    }
}

/// The set's own copy of a borrowed element. The single `to_vec` in the set's
/// element path, reached only when an add misses.
impl From<&ElemRef<'_>> for SetElement {
    fn from(elem: &ElemRef<'_>) -> SetElement {
        match *elem {
            ElemRef::Int(n) => SetElement::Int(n),
            ElemRef::Str(bytes) => SetElement::String(bytes.to_vec()),
            ElemRef::Bytes(bytes) => SetElement::Bytes(bytes.to_vec()),
        }
    }
}

/// The set behind a handle.
pub struct SetImpl {
    /// Magic word for the fast validity check (see set_magic_ok)
    magic: u32,
    /// Shared-handle refcount (see ListImpl.rc)
    rc: std::sync::atomic::AtomicU32,
    /// The actual hash table storing unique elements.
    ///
    /// A map with unit values rather than a `HashSet` because only the map has
    /// `entry_ref`: it probes with the caller's own bytes and builds the set's
    /// copy of the element only when the add misses. `HashSet` is that same
    /// table with the values fixed at `()`, so this costs nothing per element.
    data: HashMap<SetElement, ()>,
    /// Type tag for elements (0=int, 1=string, 2=bytes)
    elem_type: ElemType,
}

/// Element type enumeration
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElemType {
    Int,
    String,
    Bytes,
}

impl ElemType {
    fn from_code(code: i32) -> ElemType {
        match code {
            1 => ElemType::String,
            2 => ElemType::Bytes,
            _ => ElemType::Int,
        }
    }
}

/// A bytes-flavored set reached through a string entry point, or the
/// reverse, is an emitter defect: the string path would read the bytes
/// handle's header as a c-string and the bytes path would read a string as
/// a bytes object, and either way distinct values collapse into one entry
/// in silence. That silent collapse is the bug E254 was introduced to stop
/// (issues #920 and #955), so the mismatch fails loudly instead.
fn require_flavor(actual: ElemType, wanted: ElemType, entry: &str) {
    if actual == wanted || (actual != ElemType::Bytes && wanted != ElemType::Bytes) {
        return;
    }
    eprintln!(
        "pith runtime error: {} called on a set whose elements are {:?}, not {:?}",
        entry, actual, wanted
    );
    // the flavor is fixed at construction, so a caller of the wrong flavor is a
    // compiler bug with no answer that is not a wrong answer.
    // panic-guard: a container reached through the wrong flavor's entry point.
    std::process::exit(1);
}

impl SetImpl {
    fn new(elem_type: ElemType, _elem_size: usize) -> Self {
        SetImpl {
            magic: SET_MAGIC,
            rc: std::sync::atomic::AtomicU32::new(1),
            data: HashMap::new(),
            elem_type,
        }
    }

    fn len(&self) -> usize {
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

    fn contains(&self, elem: &ElemRef<'_>) -> bool {
        self.data.contains_key(elem)
    }

    fn remove(&mut self, elem: &ElemRef<'_>) -> bool {
        self.data.remove(elem).is_some()
    }

    fn clear(&mut self) {
        self.data.clear();
    }

    fn iter(&self) -> impl Iterator<Item = &SetElement> {
        self.data.keys()
    }
}

/// Magic word for SetImpl ("PSET"). Distinct per collection kind so a list
/// handle passed where a set is expected still fails validation.
const SET_MAGIC: u32 = 0x50534554;

/// Fast validity check: one memory read instead of a global registry lock
/// per access. Freed sets get their magic scrubbed in pith_set_free.
#[inline]
unsafe fn set_magic_ok(ptr: *const ()) -> bool {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { set_magic_ok_for(ptr, crate::quarantine::Op::Use) }
}

/// `set_magic_ok`, naming what the caller does with the set in a quarantine
/// report.
#[inline(always)]
unsafe fn set_magic_ok_for(ptr: *const (), op: crate::quarantine::Op) -> bool {
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
unsafe fn with_set_ref<R>(set: PithSet, f: impl FnOnce(&SetImpl) -> R) -> Option<R> {
    if !unsafe { set_magic_ok(set.ptr as *const ()) } {
        return None;
    }
    // SAFETY: Magic check validated pointer; borrow is scoped to `f`.
    let impl_ref = unsafe { &*(set.ptr as *const SetImpl) };
    Some(f(impl_ref))
}

/// Validate `set` and borrow its `SetImpl` mutably for the duration of `f`.
#[inline(always)]
unsafe fn with_set_mut<R>(set: PithSet, f: impl FnOnce(&mut SetImpl) -> R) -> Option<R> {
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
unsafe fn with_set_ref2<R>(
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
unsafe fn with_set_ref2_handle<R>(
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
unsafe fn with_set_mut_and_ref<R>(
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
unsafe fn with_cstr_elem<R>(s: *const i8, f: impl FnOnce(&ElemRef<'_>) -> R) -> Option<R> {
    // SAFETY: Preconditions forwarded from caller; borrow is scoped to `f`.
    unsafe { crate::ffi_util::with_cstr_bytes(s, |bytes| f(&ElemRef::Str(bytes))) }
}

/// Borrow a bytes handle's content as `ElemRef::Bytes` for the duration of `f`.
/// A null handle is the empty value, matching `pith_bytes_eq`.
#[inline(always)]
unsafe fn with_bytes_elem<R>(handle: i64, f: impl FnOnce(&ElemRef<'_>) -> R) -> Option<R> {
    if handle == 0 {
        return Some(f(&ElemRef::Bytes(&[])));
    }
    // SAFETY: `with_bytes` validates `handle` and scopes the borrowed slice to `f`.
    unsafe { crate::bytes::with_bytes(handle, |bytes| f(&ElemRef::Bytes(bytes))) }
}

/// Create a new empty set
///
/// # Arguments
/// * `elem_type` - 0 for int elements, 1 for string elements, 2 for bytes
/// * `elem_size` - Size of each element in bytes
///
/// Elements are stored as owned copies (ints, or the set's own byte
/// buffers), so there is nothing element-wise to retain or release.
#[no_mangle]
pub unsafe extern "C" fn pith_set_new(elem_type: i32, elem_size: i64) -> PithSet {
    let set_impl = SetImpl::new(ElemType::from_code(elem_type), elem_size as usize);
    let boxed = Box::new(set_impl);
    let ptr = Box::into_raw(boxed) as *mut ();

    PithSet { ptr }
}

/// Get set length
#[no_mangle]
pub extern "C" fn pith_set_len(set: PithSet) -> i64 {
    // SAFETY: `with_set_ref` validates `set` before reading its length.
    unsafe { with_set_ref(set, |impl_ref| impl_ref.len() as i64) }.unwrap_or(0)
}

#[inline(always)]
fn set_contains_int_impl(impl_ref: &SetImpl, elem: i64) -> bool {
    matches!(impl_ref.elem_type, ElemType::Int) && impl_ref.contains(&ElemRef::Int(elem))
}

/// Check if set contains integer element
#[no_mangle]
pub extern "C" fn pith_set_contains_int(set: PithSet, elem: i64) -> bool {
    // SAFETY: `with_set_ref` validates `set` before probing it.
    unsafe { with_set_ref(set, |impl_ref| set_contains_int_impl(impl_ref, elem)) }.unwrap_or(false)
}

#[inline(always)]
fn set_remove_int_impl(impl_ref: &mut SetImpl, elem: i64) -> bool {
    matches!(impl_ref.elem_type, ElemType::Int) && impl_ref.remove(&ElemRef::Int(elem))
}

/// Remove integer element from set
///
/// Returns true if element was present and removed
#[no_mangle]
pub unsafe extern "C" fn pith_set_remove_int(set: *mut PithSet, elem: i64) -> bool {
    if set.is_null() {
        return false;
    }
    // SAFETY: `set` is non-null per caller contract.
    let set_val = unsafe { *set };
    // SAFETY: `with_set_mut` validates `set_val` before mutating it.
    unsafe { with_set_mut(set_val, |impl_ref| set_remove_int_impl(impl_ref, elem)) }
        .unwrap_or(false)
}

/// Clear all elements from set
#[no_mangle]
pub unsafe extern "C" fn pith_set_clear(set: *mut PithSet) {
    if set.is_null() {
        return;
    }
    // SAFETY: `set` is non-null per caller contract.
    let set_val = unsafe { *set };
    // SAFETY: `with_set_mut` validates `set_val` before clearing it.
    unsafe { with_set_mut(set_val, SetImpl::clear) };
}

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

fn set_to_list_int_impl(impl_ref: &SetImpl) -> crate::collections::list::PithList {
    use crate::collections::list::{pith_list_new, pith_list_push, PithList};

    if !matches!(impl_ref.elem_type, ElemType::Int) {
        return PithList {
            ptr: std::ptr::null_mut(),
        };
    }

    // SAFETY: Allocates a fresh primitive int list.
    let mut list = unsafe { pith_list_new(std::mem::size_of::<i64>() as i64, 0) };

    for elem in impl_ref.iter() {
        if let SetElement::Int(n) = elem {
            let n_ptr = n as *const i64 as *const u8;
            // SAFETY: `list` is valid and `n_ptr` points to a readable `i64`.
            unsafe { pith_list_push(&mut list, n_ptr, std::mem::size_of::<i64>() as i64) };
        }
    }

    list
}

/// Convert set to list (for int elements)
///
/// # Safety
/// Returns a new list that must be released
#[no_mangle]
pub unsafe extern "C" fn pith_set_to_list_int(set: PithSet) -> crate::collections::list::PithList {
    // SAFETY: `with_set_ref` validates `set` before iterating its elements.
    unsafe { with_set_ref(set, set_to_list_int_impl) }.unwrap_or(
        crate::collections::list::PithList {
            ptr: std::ptr::null_mut(),
        },
    )
}

fn set_to_list_string_impl(impl_ref: &SetImpl) -> crate::collections::list::PithList {
    use crate::collections::list::{pith_list_new, pith_list_push_value, PithList};

    if !matches!(impl_ref.elem_type, ElemType::String) {
        return PithList {
            ptr: std::ptr::null_mut(),
        };
    }

    // string-tagged: the list owns the freshly copied element strings
    // SAFETY: Allocates a fresh string-tagged list.
    let list = unsafe { pith_list_new(8, 1) };

    for elem in impl_ref.iter() {
        if let SetElement::String(bytes) = elem {
            let ptr = crate::pith_copy_bytes_to_cstring(bytes);
            // SAFETY: `list` and `ptr` are freshly allocated handles.
            unsafe {
                pith_list_push_value(list, ptr as i64);
                crate::pith_cstring_release(ptr as *const i8);
            }
        }
    }

    list
}

/// Convert set to list (for string elements)
///
/// # Safety
/// Returns a new list that must be released
#[no_mangle]
pub unsafe extern "C" fn pith_set_to_list_string(
    set: PithSet,
) -> crate::collections::list::PithList {
    // SAFETY: `with_set_ref` validates `set` before iterating its elements.
    unsafe { with_set_ref(set, set_to_list_string_impl) }.unwrap_or(
        crate::collections::list::PithList {
            ptr: std::ptr::null_mut(),
        },
    )
}

fn set_to_list_cstr_impl(impl_ref: &SetImpl) -> i64 {
    require_flavor(impl_ref.elem_type, ElemType::String, "set_to_list");
    set_to_list_string_impl(impl_ref).ptr as i64
}

/// Convert a string set handle to a list handle of C-string pointers.
///
/// Returns the raw `ListImpl` pointer as i64 to match the Cranelift collection ABI.
#[no_mangle]
pub unsafe extern "C" fn pith_set_to_list_cstr(set_handle: i64) -> i64 {
    // SAFETY: `with_set_ref_handle` validates `set_handle` before converting it.
    unsafe { with_set_ref_handle(set_handle, set_to_list_cstr_impl) }
        .unwrap_or_else(|| unsafe { crate::collections::list::pith_list_new(8, 0) }.ptr as i64)
}

/// Convert an int set handle to a list handle of i64 elements.
///
/// Returns the raw `ListImpl` pointer as i64 to match the Cranelift collection ABI.
#[no_mangle]
pub unsafe extern "C" fn pith_set_to_list_int_handle(set_handle: i64) -> i64 {
    // SAFETY: `with_set_ref_handle` validates `set_handle` before converting it.
    unsafe { with_set_ref_handle(set_handle, |impl_ref| set_to_list_int_impl(impl_ref).ptr as i64) }
        .unwrap_or_else(|| unsafe { crate::collections::list::pith_list_new(8, 0) }.ptr as i64)
}

// ---------------------------------------------------------------------------
// Handle-based C-string variants for Cranelift codegen
// ---------------------------------------------------------------------------

#[inline(always)]
fn set_new_handle_impl(elem_type: i32) -> i64 {
    let set_impl = SetImpl::new(ElemType::from_code(elem_type), 8);
    let boxed = Box::new(set_impl);
    let ptr = Box::into_raw(boxed);
    ptr as i64
}

/// Create a new string set (handle-based). Returns SetImpl pointer as i64.
/// Create a new string set with default settings
#[no_mangle]
pub unsafe extern "C" fn pith_set_new_default() -> i64 {
    set_new_handle_impl(1)
}

#[no_mangle]
pub unsafe extern "C" fn pith_set_new_int() -> i64 {
    set_new_handle_impl(0)
}

/// Create a set of `Bytes` elements, compared and hashed by content.
#[no_mangle]
pub unsafe extern "C" fn pith_set_new_bytes() -> i64 {
    set_new_handle_impl(2)
}

#[no_mangle]
pub unsafe extern "C" fn pith_set_new_handle(elem_type: i32) -> i64 {
    set_new_handle_impl(elem_type)
}

/// Get set length (handle-based).
#[no_mangle]
pub unsafe extern "C" fn pith_set_len_handle(set_handle: i64) -> i64 {
    // SAFETY: `with_set_ref_handle` validates `set_handle` before reading its length.
    unsafe { with_set_ref_handle(set_handle, |impl_ref| impl_ref.len() as i64) }.unwrap_or(0)
}

/// Insert a C-string element into the set. Returns 1 if newly inserted, 0 if already present.
#[no_mangle]
pub unsafe extern "C" fn pith_set_add_cstr(set_handle: i64, elem: *const i8) -> i64 {
    if elem.is_null() {
        return 0;
    }
    let run = |impl_ref: &mut SetImpl| {
        require_flavor(impl_ref.elem_type, ElemType::String, "set_add");
        // SAFETY: `elem` is non-null and a valid C string per caller contract.
        let added = unsafe { with_cstr_elem(elem, |set_elem| impl_ref.insert(set_elem)) }
            .unwrap_or(false);
        i64::from(added)
    };
    // SAFETY: `with_set_mut_handle` validates `set_handle` before mutating it.
    unsafe { with_set_mut_handle(set_handle, run) }.unwrap_or(0)
}

#[inline(always)]
fn set_add_int_impl(impl_ref: &mut SetImpl, elem: i64) -> i64 {
    if !matches!(impl_ref.elem_type, ElemType::Int) {
        return 0;
    }
    i64::from(impl_ref.insert(&ElemRef::Int(elem)))
}

/// Insert an integer element into the set. Returns 1 if newly inserted, 0 if already present.
#[no_mangle]
pub unsafe extern "C" fn pith_set_add_int_handle(set_handle: i64, elem: i64) -> i64 {
    // SAFETY: `with_set_mut_handle` validates `set_handle` before mutating it.
    unsafe { with_set_mut_handle(set_handle, |impl_ref| set_add_int_impl(impl_ref, elem)) }
        .unwrap_or(0)
}

/// Check if a C-string element exists in the set. Returns 1 if present, 0 otherwise.
#[no_mangle]
pub unsafe extern "C" fn pith_set_contains_cstr(set_handle: i64, elem: *const i8) -> i64 {
    if elem.is_null() {
        return 0;
    }
    let run = |impl_ref: &SetImpl| {
        require_flavor(impl_ref.elem_type, ElemType::String, "set_contains");
        // SAFETY: `elem` is non-null and a valid C string per caller contract.
        let found = unsafe { with_cstr_elem(elem, |set_elem| impl_ref.contains(set_elem)) }
            .unwrap_or(false);
        i64::from(found)
    };
    // SAFETY: `with_set_ref_handle` validates `set_handle` before probing it.
    unsafe { with_set_ref_handle(set_handle, run) }.unwrap_or(0)
}

/// Check if an integer element exists in the set. Returns 1 if present, 0 otherwise.
#[no_mangle]
pub unsafe extern "C" fn pith_set_contains_int_handle(set_handle: i64, elem: i64) -> i64 {
    // SAFETY: `with_set_ref_handle` validates `set_handle` before probing it.
    let found = unsafe { with_set_ref_handle(set_handle, |impl_ref| set_contains_int_impl(impl_ref, elem)) }
        .unwrap_or(false);
    i64::from(found)
}

/// Remove a C-string element from the set.
#[no_mangle]
pub unsafe extern "C" fn pith_set_remove_cstr(set_handle: i64, elem: *const i8) {
    if elem.is_null() {
        return;
    }
    let run = |impl_ref: &mut SetImpl| {
        require_flavor(impl_ref.elem_type, ElemType::String, "set_remove");
        // SAFETY: `elem` is non-null and a valid C string per caller contract.
        unsafe { with_cstr_elem(elem, |set_elem| impl_ref.remove(set_elem)) };
    };
    // SAFETY: `with_set_mut_handle` validates `set_handle` before mutating it.
    unsafe { with_set_mut_handle(set_handle, run) };
}

// ---------------------------------------------------------------------------
// Bytes-element variants: the content-hashing flavor
// ---------------------------------------------------------------------------
//
// a `Bytes` element is stored the way a string element is: as the set's own
// copy of the content. the handle the caller passes is only read, never
// retained, so the caller keeps whatever count it holds on it, and a set
// releases nothing element-wise when it frees. iteration hands out fresh
// bytes objects the way string iteration hands out fresh strings, owned by
// the list `pith_set_to_list_bytes` builds.

/// Insert a bytes element by content. Returns 1 if newly inserted, 0 if it
/// was already present or the handle is not a bytes object.
#[no_mangle]
pub unsafe extern "C" fn pith_set_add_bytes(set_handle: i64, elem: i64) -> i64 {
    let run = |impl_ref: &mut SetImpl| {
        require_flavor(impl_ref.elem_type, ElemType::Bytes, "set_add_bytes");
        // SAFETY: `with_bytes_elem` validates `elem` before borrowing its bytes.
        let added = unsafe { with_bytes_elem(elem, |set_elem| impl_ref.insert(set_elem)) }
            .unwrap_or(false);
        i64::from(added)
    };
    // SAFETY: `with_set_mut_handle` validates `set_handle` before mutating it.
    unsafe { with_set_mut_handle(set_handle, run) }.unwrap_or(0)
}

/// Check membership of a bytes element by content. Returns 1 if present.
#[no_mangle]
pub unsafe extern "C" fn pith_set_contains_bytes(set_handle: i64, elem: i64) -> i64 {
    let run = |impl_ref: &SetImpl| {
        require_flavor(impl_ref.elem_type, ElemType::Bytes, "set_contains_bytes");
        // SAFETY: `with_bytes_elem` validates `elem` before borrowing its bytes.
        let found = unsafe { with_bytes_elem(elem, |set_elem| impl_ref.contains(set_elem)) }
            .unwrap_or(false);
        i64::from(found)
    };
    // SAFETY: `with_set_ref_handle` validates `set_handle` before probing it.
    unsafe { with_set_ref_handle(set_handle, run) }.unwrap_or(0)
}

/// Remove a bytes element by content.
#[no_mangle]
pub unsafe extern "C" fn pith_set_remove_bytes(set_handle: i64, elem: i64) {
    let run = |impl_ref: &mut SetImpl| {
        require_flavor(impl_ref.elem_type, ElemType::Bytes, "set_remove_bytes");
        // SAFETY: `with_bytes_elem` validates `elem` before borrowing its bytes.
        unsafe { with_bytes_elem(elem, |set_elem| impl_ref.remove(set_elem)) };
    };
    // SAFETY: `with_set_mut_handle` validates `set_handle` before mutating it.
    unsafe { with_set_mut_handle(set_handle, run) };
}

fn set_to_list_bytes_impl(impl_ref: &SetImpl) -> i64 {
    use crate::collections::list::{pith_list_new, pith_list_push_value_owned};

    require_flavor(impl_ref.elem_type, ElemType::Bytes, "set_to_list_bytes");
    // SAFETY: Allocates a fresh bytes-tagged list.
    let list = unsafe { pith_list_new(8, 5) };
    for elem in impl_ref.iter() {
        if let SetElement::Bytes(bytes) = elem {
            let handle = crate::bytes::pith_bytes_from_vec(bytes.clone());
            // SAFETY: `list` is freshly allocated and `handle` is an owned bytes handle.
            unsafe { pith_list_push_value_owned(list, handle) };
        }
    }
    list.ptr as i64
}

/// Convert a bytes set handle to a list handle of fresh bytes objects. The
/// list is bytes-tagged and owns each element, so releasing the list
/// releases them; the set's own storage is untouched.
#[no_mangle]
pub unsafe extern "C" fn pith_set_to_list_bytes(set_handle: i64) -> i64 {
    // SAFETY: `with_set_ref_handle` validates `set_handle` before iterating its elements.
    unsafe { with_set_ref_handle(set_handle, set_to_list_bytes_impl) }
        .unwrap_or_else(|| unsafe { crate::collections::list::pith_list_new(8, 0) }.ptr as i64)
}

/// Remove an integer element from the set.
#[no_mangle]
pub unsafe extern "C" fn pith_set_remove_int_handle(set_handle: i64, elem: i64) {
    // SAFETY: `with_set_mut_handle` validates `set_handle` before mutating it.
    unsafe { with_set_mut_handle(set_handle, |impl_ref| set_remove_int_impl(impl_ref, elem)) };
}

/// Clear all elements from set (handle-based).
#[no_mangle]
pub unsafe extern "C" fn pith_set_clear_handle(set_handle: i64) {
    // SAFETY: `with_set_mut_handle` validates `set_handle` before clearing it.
    unsafe { with_set_mut_handle(set_handle, SetImpl::clear) };
}

/// Check if set is empty (handle-based). Returns 1 if empty, 0 otherwise.
#[no_mangle]
pub unsafe extern "C" fn pith_set_is_empty_handle(set_handle: i64) -> i64 {
    // SAFETY: `with_set_ref_handle` validates `set_handle` before checking its length.
    let empty = unsafe { with_set_ref_handle(set_handle, |impl_ref| impl_ref.len() == 0) }
        .unwrap_or(true);
    i64::from(empty)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn bogus_set() -> PithSet {
        PithSet {
            ptr: 12345usize as *mut (),
        }
    }

    #[test]
    fn invalid_set_handles_return_safe_defaults() {
        unsafe {
            let mut set = bogus_set();
            assert_eq!(pith_set_len(bogus_set()), 0);
            assert!(!pith_set_contains_int(bogus_set(), 7));
            assert_eq!(pith_set_len_handle(12345), 0);
            assert_eq!(pith_set_contains_int_handle(12345, 7), 0);
            assert_eq!(pith_set_is_empty_handle(12345), 1);
            pith_set_clear(&mut set);
            pith_set_clear_handle(12345);
            pith_set_release(bogus_set());
        }
    }

    #[test]
    fn released_set_handles_are_rejected() {
        unsafe {
            let set = pith_set_new(0, 8);
            let handle = set.ptr as i64;
            assert_eq!(pith_set_len(set), 0);
            pith_set_release(set);
            assert_eq!(pith_set_len(set), 0);
            assert_eq!(pith_set_len_handle(handle), 0);
            assert_eq!(pith_set_is_empty_handle(handle), 1);
            pith_set_release(set);
        }
    }

    /// A bytes set hashes and compares by content: two objects built by
    /// different routes with the same bytes are one element, two values that
    /// share a prefix and a length are two, and the empty value is a member
    /// like any other. The set keeps copies, so the caller's count on each
    /// handle is untouched by every operation.
    #[test]
    fn bytes_set_is_keyed_by_content() {
        unsafe {
            let set = pith_set_new_bytes();
            let from_vec = crate::bytes::pith_bytes_from_vec(b"abc".to_vec());
            let from_str = crate::bytes::pith_bytes_from_string_utf8(c"abc".as_ptr());
            let sibling = crate::bytes::pith_bytes_from_vec(b"abd".to_vec());
            let empty = crate::bytes::pith_bytes_from_vec(Vec::new());
            assert_ne!(from_vec, from_str, "two distinct objects");

            assert_eq!(pith_set_add_bytes(set, from_vec), 1);
            assert_eq!(
                pith_set_add_bytes(set, from_str),
                0,
                "same content, one entry"
            );
            assert_eq!(
                pith_set_add_bytes(set, sibling),
                1,
                "shared prefix, distinct"
            );
            assert_eq!(pith_set_add_bytes(set, empty), 1);
            assert_eq!(pith_set_len_handle(set), 3);

            assert_eq!(pith_set_contains_bytes(set, from_str), 1);
            assert_eq!(pith_set_contains_bytes(set, sibling), 1);
            assert_eq!(pith_set_contains_bytes(set, empty), 1);
            assert_eq!(
                pith_set_contains_bytes(set, 0),
                1,
                "null is the empty value"
            );
            let absent = crate::bytes::pith_bytes_from_vec(b"ab".to_vec());
            assert_eq!(
                pith_set_contains_bytes(set, absent),
                0,
                "a prefix is not a member"
            );

            pith_set_remove_bytes(set, from_vec);
            assert_eq!(
                pith_set_contains_bytes(set, from_str),
                0,
                "removed by content"
            );
            assert_eq!(pith_set_len_handle(set), 2);

            // the list of members owns fresh bytes objects, one per element
            let list = pith_set_to_list_bytes(set);
            let list_handle = crate::collections::list::PithList {
                ptr: list as *mut (),
            };
            assert_eq!(crate::collections::list::pith_list_len(list_handle), 2);
            let first = crate::collections::list::pith_list_get_value(list_handle, 0);
            assert_ne!(first, sibling);
            assert_ne!(first, empty);
            assert!(crate::bytes::with_bytes(first, |_| ()).is_some());
            crate::collections::list::pith_list_release_handle(list);

            for h in [from_vec, from_str, sibling, empty, absent] {
                let rc = crate::bytes::with_bytes_obj(h, |b| {
                    b.rc.load(std::sync::atomic::Ordering::Relaxed)
                })
                .expect("the set never took a count");
                assert_eq!(rc, 1);
                crate::bytes::pith_bytes_release(h);
            }
            pith_set_release_handle(set);
        }
    }

    // --- the borrowed-element contract --------------------------------------

    fn hash_probe_inputs() -> Vec<Vec<u8>> {
        vec![
            Vec::new(),
            b"a".to_vec(),
            b"ab".to_vec(),
            b"abc".to_vec(),
            b"user-1000-region".to_vec(),
            vec![0u8],
            vec![b'a', 0, b'b'],
            vec![0, 0, 0, 0],
            "\u{e9}".as_bytes().to_vec(),
            "\u{65e5}\u{672c}\u{8a9e}".as_bytes().to_vec(),
            "\u{1f600}".as_bytes().to_vec(),
            vec![b'x'; 255],
            vec![b'y'; 4096],
            vec![0u8; 8],
        ]
    }

    fn hash_with(state: &std::collections::hash_map::RandomState, value: &impl Hash) -> u64 {
        use std::hash::BuildHasher;
        let mut hasher = state.build_hasher();
        value.hash(&mut hasher);
        hasher.finish()
    }

    /// The set's half of the silent hazard: `ElemRef` is what every probe
    /// hashes and `SetElement` is what the table stores. If the two ever wrote
    /// different bytes for the same content, every string set in every program
    /// would stop finding elements it holds, with no diagnostic anywhere.
    #[test]
    fn elem_ref_hashes_like_set_element() {
        let state = std::collections::hash_map::RandomState::new();
        for bytes in hash_probe_inputs() {
            assert_eq!(
                hash_with(&state, &ElemRef::Str(&bytes)),
                hash_with(&state, &SetElement::String(bytes.clone())),
                "string flavor, {} bytes",
                bytes.len()
            );
            assert_eq!(
                hash_with(&state, &ElemRef::Bytes(&bytes)),
                hash_with(&state, &SetElement::Bytes(bytes.clone())),
                "bytes flavor, {} bytes",
                bytes.len()
            );
            assert_ne!(
                hash_with(&state, &ElemRef::Str(&bytes)),
                hash_with(&state, &ElemRef::Bytes(&bytes)),
                "flavors must not collide, {} bytes",
                bytes.len()
            );
            assert!(ElemRef::Str(&bytes).equivalent(&SetElement::String(bytes.clone())));
            assert!(ElemRef::Bytes(&bytes).equivalent(&SetElement::Bytes(bytes.clone())));
            assert!(!ElemRef::Str(&bytes).equivalent(&SetElement::Bytes(bytes.clone())));
            assert_eq!(
                SetElement::from(&ElemRef::Str(&bytes)),
                SetElement::String(bytes.clone())
            );
            assert_eq!(
                SetElement::from(&ElemRef::Bytes(&bytes)),
                SetElement::Bytes(bytes.clone())
            );

            // and through the table rather than the hasher
            let mut table: HashMap<SetElement, ()> = HashMap::new();
            table.insert(SetElement::String(bytes.clone()), ());
            table.insert(SetElement::Bytes(bytes.clone()), ());
            assert!(table.contains_key(&ElemRef::Str(&bytes)));
            assert!(table.contains_key(&ElemRef::Bytes(&bytes)));
        }
        assert_ne!(
            hash_with(&state, &ElemRef::Str(b"ab")),
            hash_with(&state, &ElemRef::Str(b"abc"))
        );
        for n in [i64::MIN, -1, 0, 1, 255, i64::MAX] {
            assert_eq!(
                hash_with(&state, &ElemRef::Int(n)),
                hash_with(&state, &SetElement::Int(n)),
                "int flavor, {n}"
            );
            assert!(ElemRef::Int(n).equivalent(&SetElement::Int(n)));
        }
    }

    #[test]
    fn set_hits_allocate_nothing_for_the_element() {
        use crate::collections::alloc_probe::allocations_during;
        unsafe {
            let set = pith_set_new_default();
            let elem = b"user-1000-region\0".as_ptr() as *const i8;
            let absent = b"user-1001-region\0".as_ptr() as *const i8;
            pith_set_add_cstr(set, elem);

            assert_eq!(
                allocations_during(|| {
                    pith_set_contains_cstr(set, elem);
                }),
                0
            );
            assert_eq!(
                allocations_during(|| {
                    pith_set_contains_cstr(set, absent);
                }),
                0
            );
            // adding what the set already holds reports "not new" and
            // allocates nothing
            assert_eq!(
                allocations_during(|| {
                    assert_eq!(pith_set_add_cstr(set, elem), 0);
                }),
                0
            );
            assert_eq!(
                allocations_during(|| {
                    pith_set_remove_cstr(set, elem);
                }),
                0
            );
            assert_eq!(pith_set_len_handle(set), 0);

            pith_set_release_handle(set);
        }
    }

    #[test]
    fn bytes_set_hits_allocate_nothing_for_the_element() {
        use crate::collections::alloc_probe::allocations_during;
        unsafe {
            let set = pith_set_new_bytes();
            let elem = crate::bytes::pith_bytes_from_vec(b"user-1000-region".to_vec());
            let absent = crate::bytes::pith_bytes_from_vec(b"user-1001-region".to_vec());
            pith_set_add_bytes(set, elem);

            assert_eq!(
                allocations_during(|| {
                    pith_set_contains_bytes(set, elem);
                }),
                0
            );
            assert_eq!(
                allocations_during(|| {
                    pith_set_contains_bytes(set, absent);
                }),
                0
            );
            assert_eq!(
                allocations_during(|| {
                    assert_eq!(pith_set_add_bytes(set, elem), 0);
                }),
                0
            );
            assert_eq!(
                allocations_during(|| {
                    pith_set_remove_bytes(set, elem);
                }),
                0
            );
            assert_eq!(pith_set_len_handle(set), 0);

            crate::bytes::pith_bytes_release(elem);
            crate::bytes::pith_bytes_release(absent);
            pith_set_release_handle(set);
        }
    }

    /// A fresh element costs exactly the one copy the set keeps.
    #[test]
    fn a_new_set_element_allocates_once() {
        use crate::collections::alloc_probe::allocations_during;
        unsafe {
            let set = pith_set_new_default();
            pith_set_add_cstr(set, b"seed\0".as_ptr() as *const i8);
            let fresh = b"an-element-the-set-has-never-seen\0".as_ptr() as *const i8;
            assert_eq!(
                allocations_during(|| {
                    assert_eq!(pith_set_add_cstr(set, fresh), 1);
                }),
                1
            );
            assert_eq!(pith_set_contains_cstr(set, fresh), 1);
            pith_set_release_handle(set);
        }
    }

    #[test]
    fn with_set_mut_and_ref_handles_self_aliasing_and_distinct_sets() {
        unsafe {
            let a = pith_set_new(0, 8);
            let b = pith_set_new(0, 8);
            pith_set_add_int_handle(a.ptr as i64, 1);
            pith_set_add_int_handle(b.ptr as i64, 2);

            // distinct sets take `on_distinct`
            let distinct = with_set_mut_and_ref(
                a,
                b,
                |dst, src| {
                    for elem in src.iter() {
                        if let SetElement::Int(n) = elem {
                            dst.insert(&ElemRef::Int(*n));
                        }
                    }
                    "distinct"
                },
                |_| "same",
            );
            assert_eq!(distinct, Some("distinct"));
            assert_eq!(pith_set_len(a), 2);
            assert!(pith_set_contains_int(a, 1));
            assert!(pith_set_contains_int(a, 2));

            // self-aliasing takes `on_same` with a single `&mut SetImpl`
            let same = with_set_mut_and_ref(
                a,
                a,
                |_, _| "distinct",
                |set| {
                    set.insert(&ElemRef::Int(3));
                    "same"
                },
            );
            assert_eq!(same, Some("same"));
            assert!(pith_set_contains_int(a, 3));

            // invalid handles return None
            assert!(with_set_mut_and_ref(bogus_set(), a, |_, _| (), |_| ()).is_none());
            assert!(with_set_mut_and_ref(a, bogus_set(), |_, _| (), |_| ()).is_none());

            // shared ref2 supports both distinct and aliased handles
            assert_eq!(
                with_set_ref2_handle(a.ptr as i64, b.ptr as i64, |x, y| (x.len(), y.len())),
                Some((3, 1))
            );
            assert_eq!(
                with_set_ref2_handle(a.ptr as i64, a.ptr as i64, |x, y| x.len() + y.len()),
                Some(6)
            );
            assert!(with_set_ref2_handle(a.ptr as i64, 12345, |_, _| ()).is_none());

            pith_set_release(a);
            pith_set_release(b);
        }
    }
}
