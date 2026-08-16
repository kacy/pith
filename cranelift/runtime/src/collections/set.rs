//! Set[T] - unique element collection
//!
//! Hybrid approach: Uses hashbrown::HashSet internally for O(1) operations,
//! but presents FFI-compatible interface matching the C runtime.

use crate::handle_registry::{self, HandleKind};
use hashbrown::HashSet;
use std::hash::{Hash, Hasher};

/// FFI-compatible set handle
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PithSet {
    /// Pointer to internal set implementation (pub for cross-module access)
    pub ptr: *mut (),
}

/// Set element type for the internal HashSet
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetElement {
    Int(i64),
    String(Vec<u8>),
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
        }
    }
}

/// Internal set implementation using idiomatic Rust
pub struct SetImpl {
    /// Magic word for the fast validity check (see set_magic_ok)
    magic: u32,
    /// Shared-handle refcount (see ListImpl.rc)
    rc: std::sync::atomic::AtomicU32,
    /// The actual hash set storing unique elements
    data: HashSet<SetElement>,
    /// Type tag for elements (0=int, 1=string)
    elem_type: ElemType,
}

/// Element type enumeration
#[derive(Clone, Copy, Debug)]
pub enum ElemType {
    Int,
    String,
}

impl SetImpl {
    fn new(elem_type: ElemType, _elem_size: usize) -> Self {
        SetImpl {
            magic: SET_MAGIC,
            rc: std::sync::atomic::AtomicU32::new(1),
            data: HashSet::new(),
            elem_type,
        }
    }

    fn len(&self) -> usize {
        self.data.len()
    }

    fn insert(&mut self, elem: SetElement) -> bool {
        self.data.insert(elem)
    }

    fn contains(&self, elem: &SetElement) -> bool {
        self.data.contains(elem)
    }

    fn remove(&mut self, elem: &SetElement) -> bool {
        self.data.remove(elem)
    }

    fn clear(&mut self) {
        self.data.clear();
    }

    fn iter(&self) -> impl Iterator<Item = &SetElement> {
        self.data.iter()
    }
}

/// Magic word for SetImpl ("PSET"). Distinct per collection kind so a list
/// handle passed where a set is expected still fails validation.
const SET_MAGIC: u32 = 0x50534554;

/// Fast validity check: one memory read instead of a global registry lock
/// per access. Freed sets get their magic scrubbed in pith_set_free.
#[inline]
unsafe fn set_magic_ok(ptr: *const ()) -> bool {
    handle_registry::plausibly_aligned::<SetImpl>(ptr)
        && (*(ptr as *const SetImpl)).magic == SET_MAGIC
}

unsafe fn set_ref<'a>(set: PithSet) -> Option<&'a SetImpl> {
    if !set_magic_ok(set.ptr as *const ()) {
        return None;
    }
    Some(&*(set.ptr as *const SetImpl))
}

unsafe fn set_mut<'a>(set: PithSet) -> Option<&'a mut SetImpl> {
    if !set_magic_ok(set.ptr as *const ()) {
        return None;
    }
    Some(&mut *(set.ptr as *mut SetImpl))
}

unsafe fn set_ref_from_handle<'a>(handle: i64) -> Option<&'a SetImpl> {
    if !set_magic_ok(handle as *const ()) {
        return None;
    }
    Some(&*(handle as *const SetImpl))
}

unsafe fn set_mut_from_handle<'a>(handle: i64) -> Option<&'a mut SetImpl> {
    if !set_magic_ok(handle as *const ()) {
        return None;
    }
    Some(&mut *(handle as *mut SetImpl))
}

/// Create a new empty set
///
/// # Arguments
/// * `elem_type` - 0 for int elements, 1 for string elements
/// * `elem_size` - Size of each element in bytes
///
/// Elements are stored as owned copies (ints, or the set's own byte
/// buffers), so there is nothing element-wise to retain or release.
#[no_mangle]
pub unsafe extern "C" fn pith_set_new(elem_type: i32, elem_size: i64) -> PithSet {
    let etype = match elem_type {
        1 => ElemType::String,
        _ => ElemType::Int,
    };

    let set_impl = SetImpl::new(etype, elem_size as usize);
    let boxed = Box::new(set_impl);
    let ptr = Box::into_raw(boxed) as *mut ();
    handle_registry::register(ptr as *const (), HandleKind::Set);

    PithSet { ptr }
}

/// Get set length
#[no_mangle]
pub extern "C" fn pith_set_len(set: PithSet) -> i64 {
    unsafe {
        set_ref(set)
            .map(|impl_ref| impl_ref.len() as i64)
            .unwrap_or(0)
    }
}

/// Check if set contains integer element
#[no_mangle]
pub extern "C" fn pith_set_contains_int(set: PithSet, elem: i64) -> bool {
    unsafe {
        let Some(impl_ref) = set_ref(set) else {
            return false;
        };

        if !matches!(impl_ref.elem_type, ElemType::Int) {
            return false;
        }

        impl_ref.contains(&SetElement::Int(elem))
    }
}

/// Remove integer element from set
///
/// Returns true if element was present and removed
#[no_mangle]
pub unsafe extern "C" fn pith_set_remove_int(set: *mut PithSet, elem: i64) -> bool {
    if set.is_null() {
        return false;
    }

    let Some(impl_ref) = set_mut(*set) else {
        return false;
    };

    if !matches!(impl_ref.elem_type, ElemType::Int) {
        return false;
    }

    impl_ref.remove(&SetElement::Int(elem))
}

/// Clear all elements from set
#[no_mangle]
pub unsafe extern "C" fn pith_set_clear(set: *mut PithSet) {
    if set.is_null() {
        return;
    }

    let Some(impl_ref) = set_mut(*set) else {
        return;
    };

    impl_ref.clear();
}

/// Release set and free memory
#[no_mangle]
pub unsafe extern "C" fn pith_set_release(set: PithSet) {
    let Some(impl_ref) = set_mut(set) else {
        return;
    };
    let prev = impl_ref
        .rc
        .fetch_sub(1, std::sync::atomic::Ordering::Release);
    if prev > 1 {
        return;
    }
    if prev == 0 {
        impl_ref.rc.store(0, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);

    // Free the set implementation. Scrub the magic first so any handle
    // that outlives the set fails the fast validity check.
    (*(set.ptr as *mut SetImpl)).magic = 0;
    handle_registry::unregister(set.ptr as *const (), HandleKind::Set);
    let _ = Box::from_raw(set.ptr as *mut SetImpl);
}

/// Retain a set handle: one more owner of this shared handle.
#[no_mangle]
pub unsafe extern "C" fn pith_set_retain_handle(handle: i64) {
    if let Some(impl_ref) = set_mut_from_handle(handle) {
        impl_ref
            .rc
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Release a set handle; frees the set and its owned elements at zero.
#[no_mangle]
pub unsafe extern "C" fn pith_set_release_handle(handle: i64) {
    pith_set_release(PithSet {
        ptr: handle as *mut (),
    });
}

// --- collector-facing accessors ---------------------------------------------
//
// a set stores only ints and byte strings, so it holds no edges into the
// ownership graph and the collector never walks into one. it still has a
// reference count of its own, though, so a set every one of whose owners is
// garbage is garbage too — these let the collection pass count it and, when
// it turns out white, tear it down. every entry point magic-checks so a bad
// edge degrades to None or a no-op.

/// The set's current reference count, or `None` for an invalid handle.
pub(crate) unsafe fn cycle_set_strong_count(handle: i64) -> Option<u32> {
    set_ref_from_handle(handle)
        .map(|impl_ref| impl_ref.rc.load(std::sync::atomic::Ordering::Relaxed))
}

/// Add `delta` to the reference count — the collector's teardown guard.
pub(crate) unsafe fn cycle_set_guard_strong(handle: i64, delta: u32) {
    if let Some(impl_ref) = set_ref_from_handle(handle) {
        impl_ref
            .rc
            .fetch_add(delta, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The destruction body of a garbage set: drop the owned elements by
/// clearing, leaving an empty set so the shell free cannot repeat it.
pub(crate) unsafe fn cycle_set_release_elements(handle: i64) {
    let Some(impl_ref) = set_mut_from_handle(handle) else {
        return;
    };
    impl_ref.clear();
}

/// Free a garbage set's shell: the tail of `pith_set_release` minus the
/// element loop, which `cycle_set_release_elements` already ran. Sets carry
/// no buffered bit and never enter the suspect buffer, so a plain drop is
/// always safe here.
pub(crate) unsafe fn cycle_set_free_dead(handle: i64) {
    if set_ref_from_handle(handle).is_none() {
        return;
    }
    (*(handle as *mut SetImpl)).magic = 0;
    handle_registry::unregister(handle as *const (), HandleKind::Set);
    drop(Box::from_raw(handle as *mut SetImpl));
}

/// Convert set to list (for int elements)
///
/// # Safety
/// Returns a new list that must be released
#[no_mangle]
pub unsafe extern "C" fn pith_set_to_list_int(set: PithSet) -> crate::collections::list::PithList {
    use crate::collections::list::{pith_list_new, pith_list_push, PithList};

    let Some(impl_ref) = set_ref(set) else {
        return PithList {
            ptr: std::ptr::null_mut(),
        };
    };

    if !matches!(impl_ref.elem_type, ElemType::Int) {
        return PithList {
            ptr: std::ptr::null_mut(),
        };
    }

    let mut list = pith_list_new(std::mem::size_of::<i64>() as i64, 0);

    for elem in impl_ref.iter() {
        if let SetElement::Int(n) = elem {
            let n_ptr = n as *const i64 as *const u8;
            pith_list_push(&mut list, n_ptr, std::mem::size_of::<i64>() as i64);
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
    use crate::collections::list::{pith_list_new, pith_list_push_value, PithList};

    let Some(impl_ref) = set_ref(set) else {
        return PithList {
            ptr: std::ptr::null_mut(),
        };
    };

    if !matches!(impl_ref.elem_type, ElemType::String) {
        return PithList {
            ptr: std::ptr::null_mut(),
        };
    }

    // string-tagged: the list owns the freshly copied element strings
    let list = pith_list_new(8, 1);

    for elem in impl_ref.iter() {
        if let SetElement::String(bytes) = elem {
            let ptr = crate::pith_copy_bytes_to_cstring(bytes);
            pith_list_push_value(list, ptr as i64);
            crate::pith_cstring_release(ptr as *const i8);
        }
    }

    list
}

/// Convert a string set handle to a list handle of C-string pointers.
///
/// Returns the raw `ListImpl` pointer as i64 to match the Cranelift collection ABI.
#[no_mangle]
pub unsafe extern "C" fn pith_set_to_list_cstr(set_handle: i64) -> i64 {
    if !set_magic_ok(set_handle as *const ()) {
        let empty = crate::collections::list::pith_list_new(8, 0);
        return empty.ptr as i64;
    }

    let list = pith_set_to_list_string(PithSet {
        ptr: set_handle as *mut (),
    });
    list.ptr as i64
}

/// Convert an int set handle to a list handle of i64 elements.
///
/// Returns the raw `ListImpl` pointer as i64 to match the Cranelift collection ABI.
#[no_mangle]
pub unsafe extern "C" fn pith_set_to_list_int_handle(set_handle: i64) -> i64 {
    if !set_magic_ok(set_handle as *const ()) {
        let empty = crate::collections::list::pith_list_new(8, 0);
        return empty.ptr as i64;
    }

    let list = pith_set_to_list_int(PithSet {
        ptr: set_handle as *mut (),
    });
    list.ptr as i64
}

// ---------------------------------------------------------------------------
// Handle-based C-string variants for Cranelift codegen
// ---------------------------------------------------------------------------

unsafe fn cstr_to_set_element(s: *const i8) -> SetElement {
    let mut len = 0usize;
    let mut p = s;
    while *p != 0 {
        len += 1;
        p = p.add(1);
    }
    let bytes = std::slice::from_raw_parts(s as *const u8, len);
    SetElement::String(bytes.to_vec())
}

/// Create a new string set (handle-based). Returns SetImpl pointer as i64.
/// Create a new string set with default settings
#[no_mangle]
pub unsafe extern "C" fn pith_set_new_default() -> i64 {
    pith_set_new_handle(1)
}

#[no_mangle]
pub unsafe extern "C" fn pith_set_new_int() -> i64 {
    pith_set_new_handle(0)
}

#[no_mangle]
pub unsafe extern "C" fn pith_set_new_handle(elem_type: i32) -> i64 {
    let etype = match elem_type {
        1 => ElemType::String,
        _ => ElemType::Int,
    };
    let set_impl = SetImpl::new(etype, 8);
    let boxed = Box::new(set_impl);
    let ptr = Box::into_raw(boxed);
    handle_registry::register(ptr as *const (), HandleKind::Set);
    ptr as i64
}

/// Get set length (handle-based).
#[no_mangle]
pub unsafe extern "C" fn pith_set_len_handle(set_handle: i64) -> i64 {
    set_ref_from_handle(set_handle)
        .map(|impl_ref| impl_ref.len() as i64)
        .unwrap_or(0)
}

/// Insert a C-string element into the set. Returns 1 if newly inserted, 0 if already present.
#[no_mangle]
pub unsafe extern "C" fn pith_set_add_cstr(set_handle: i64, elem: *const i8) -> i64 {
    if elem.is_null() {
        return 0;
    }
    let Some(impl_ref) = set_mut_from_handle(set_handle) else {
        return 0;
    };
    let set_elem = cstr_to_set_element(elem);
    if impl_ref.insert(set_elem) {
        1
    } else {
        0
    }
}

/// Insert an integer element into the set. Returns 1 if newly inserted, 0 if already present.
#[no_mangle]
pub unsafe extern "C" fn pith_set_add_int_handle(set_handle: i64, elem: i64) -> i64 {
    let Some(impl_ref) = set_mut_from_handle(set_handle) else {
        return 0;
    };
    if !matches!(impl_ref.elem_type, ElemType::Int) {
        return 0;
    }
    if impl_ref.insert(SetElement::Int(elem)) {
        1
    } else {
        0
    }
}

/// Check if a C-string element exists in the set. Returns 1 if present, 0 otherwise.
#[no_mangle]
pub unsafe extern "C" fn pith_set_contains_cstr(set_handle: i64, elem: *const i8) -> i64 {
    if elem.is_null() {
        return 0;
    }
    let Some(impl_ref) = set_ref_from_handle(set_handle) else {
        return 0;
    };
    let set_elem = cstr_to_set_element(elem);
    if impl_ref.contains(&set_elem) {
        1
    } else {
        0
    }
}

/// Check if an integer element exists in the set. Returns 1 if present, 0 otherwise.
#[no_mangle]
pub unsafe extern "C" fn pith_set_contains_int_handle(set_handle: i64, elem: i64) -> i64 {
    let Some(impl_ref) = set_ref_from_handle(set_handle) else {
        return 0;
    };
    if !matches!(impl_ref.elem_type, ElemType::Int) {
        return 0;
    }
    if impl_ref.contains(&SetElement::Int(elem)) {
        1
    } else {
        0
    }
}

/// Remove a C-string element from the set.
#[no_mangle]
pub unsafe extern "C" fn pith_set_remove_cstr(set_handle: i64, elem: *const i8) {
    if elem.is_null() {
        return;
    }
    let Some(impl_ref) = set_mut_from_handle(set_handle) else {
        return;
    };
    let set_elem = cstr_to_set_element(elem);
    impl_ref.remove(&set_elem);
}

/// Remove an integer element from the set.
#[no_mangle]
pub unsafe extern "C" fn pith_set_remove_int_handle(set_handle: i64, elem: i64) {
    let Some(impl_ref) = set_mut_from_handle(set_handle) else {
        return;
    };
    if !matches!(impl_ref.elem_type, ElemType::Int) {
        return;
    }
    impl_ref.remove(&SetElement::Int(elem));
}

/// Clear all elements from set (handle-based).
#[no_mangle]
pub unsafe extern "C" fn pith_set_clear_handle(set_handle: i64) {
    let Some(impl_ref) = set_mut_from_handle(set_handle) else {
        return;
    };
    impl_ref.clear();
}

/// Check if set is empty (handle-based). Returns 1 if empty, 0 otherwise.
#[no_mangle]
pub unsafe extern "C" fn pith_set_is_empty_handle(set_handle: i64) -> i64 {
    let Some(impl_ref) = set_ref_from_handle(set_handle) else {
        return 1;
    };
    if impl_ref.len() == 0 {
        1
    } else {
        0
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

    unsafe {
        let set = ptr as *const PithSet;
        pith_set_release(*set);
    }
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
}
