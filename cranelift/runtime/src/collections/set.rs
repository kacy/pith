//! Set[T] - unique element collection
//!
//! Hybrid approach: Uses hashbrown::HashSet internally for O(1) operations,
//! but presents FFI-compatible interface matching the C runtime.

use crate::string::{pith_string_release, PithString};
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
    /// The actual hash set storing unique elements
    data: HashSet<SetElement>,
    /// Type tag for elements (0=int, 1=string)
    elem_type: ElemType,
    /// Size of each element in bytes
    elem_size: usize,
    /// Whether elements are heap types (need retain/release)
    elem_is_heap: bool,
}

/// Element type enumeration
#[derive(Clone, Copy, Debug)]
pub enum ElemType {
    Int,
    String,
}

impl SetImpl {
    fn new(elem_type: ElemType, elem_size: usize, elem_is_heap: bool) -> Self {
        SetImpl {
            data: HashSet::new(),
            elem_type,
            elem_size,
            elem_is_heap,
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

/// Create a new empty set
///
/// # Arguments
/// * `elem_type` - 0 for int elements, 1 for string elements
/// * `elem_size` - Size of each element in bytes
/// * `elem_is_heap` - Whether elements are heap types (need retain/release)
#[no_mangle]
pub unsafe extern "C" fn pith_set_new(
    elem_type: i32,
    elem_size: i64,
    elem_is_heap: bool,
) -> PithSet {
    let etype = match elem_type {
        1 => ElemType::String,
        _ => ElemType::Int,
    };

    let set_impl = SetImpl::new(etype, elem_size as usize, elem_is_heap);
    let boxed = Box::new(set_impl);

    PithSet {
        ptr: Box::into_raw(boxed) as *mut (),
    }
}

/// Get set length
#[no_mangle]
pub extern "C" fn pith_set_len(set: PithSet) -> i64 {
    if set.ptr.is_null() {
        return 0;
    }

    unsafe {
        let impl_ref = &*(set.ptr as *const SetImpl);
        impl_ref.len() as i64
    }
}

/// Check if set contains integer element
#[no_mangle]
pub extern "C" fn pith_set_contains_int(set: PithSet, elem: i64) -> bool {
    if set.ptr.is_null() {
        return false;
    }

    unsafe {
        let impl_ref = &*(set.ptr as *const SetImpl);

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
    if set.is_null() || (*set).ptr.is_null() {
        return false;
    }

    let impl_ref = &mut *((*set).ptr as *mut SetImpl);

    if !matches!(impl_ref.elem_type, ElemType::Int) {
        return false;
    }

    impl_ref.remove(&SetElement::Int(elem))
}

/// Clear all elements from set
#[no_mangle]
pub unsafe extern "C" fn pith_set_clear(set: *mut PithSet) {
    if set.is_null() || (*set).ptr.is_null() {
        return;
    }

    let impl_ref = &mut *((*set).ptr as *mut SetImpl);

    // Release all elements if they're heap types
    if impl_ref.elem_is_heap {
        for elem in impl_ref.iter() {
            if let SetElement::String(bytes) = elem {
                // Reconstruct the string from bytes to release it
                let s = PithString {
                    ptr: bytes.as_ptr(),
                    len: bytes.len() as i64,
                    is_heap: true,
                };
                pith_string_release(s);
            }
        }
    }

    impl_ref.clear();
}

/// Release set and free memory
#[no_mangle]
pub unsafe extern "C" fn pith_set_release(set: PithSet) {
    if set.ptr.is_null() {
        return;
    }

    let impl_ref = &mut *(set.ptr as *mut SetImpl);

    // Release all elements if they're heap types
    if impl_ref.elem_is_heap {
        for elem in impl_ref.iter() {
            if let SetElement::String(bytes) = elem {
                let s = PithString {
                    ptr: bytes.as_ptr(),
                    len: bytes.len() as i64,
                    is_heap: true,
                };
                pith_string_release(s);
            }
        }
    }

    // Free the set implementation
    let _ = Box::from_raw(set.ptr as *mut SetImpl);
}

/// Convert set to list (for int elements)
///
/// # Safety
/// Returns a new list that must be released
#[no_mangle]
pub unsafe extern "C" fn pith_set_to_list_int(
    set: PithSet,
) -> crate::collections::list::PithList {
    use crate::collections::list::{pith_list_new, pith_list_push, PithList};

    if set.ptr.is_null() {
        return PithList {
            ptr: std::ptr::null_mut(),
        };
    }

    let impl_ref = &*(set.ptr as *const SetImpl);

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
    use std::alloc::{alloc, Layout};

    if set.ptr.is_null() {
        return PithList {
            ptr: std::ptr::null_mut(),
        };
    }

    let impl_ref = &*(set.ptr as *const SetImpl);

    if !matches!(impl_ref.elem_type, ElemType::String) {
        return PithList {
            ptr: std::ptr::null_mut(),
        };
    }

    let list = pith_list_new(8, 0);

    for elem in impl_ref.iter() {
        if let SetElement::String(bytes) = elem {
            let len = bytes.len();
            let layout = Layout::from_size_align(len + 1, 1).unwrap();
            let ptr = alloc(layout) as *mut i8;
            if !ptr.is_null() {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, len);
                *ptr.add(len) = 0;
                pith_list_push_value(list, ptr as i64);
            }
        }
    }

    list
}

/// Convert a string set handle to a list handle of C-string pointers.
///
/// Returns the raw `ListImpl` pointer as i64 to match the Cranelift collection ABI.
#[no_mangle]
pub unsafe extern "C" fn pith_set_to_list_cstr(set_handle: i64) -> i64 {
    if set_handle == 0 {
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
    if set_handle == 0 {
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
    let set_impl = SetImpl::new(etype, 8, false);
    let boxed = Box::new(set_impl);
    Box::into_raw(boxed) as i64
}

/// Get set length (handle-based).
#[no_mangle]
pub unsafe extern "C" fn pith_set_len_handle(set_handle: i64) -> i64 {
    if set_handle == 0 {
        return 0;
    }
    let impl_ref = &*(set_handle as *const SetImpl);
    impl_ref.len() as i64
}

/// Insert a C-string element into the set. Returns 1 if newly inserted, 0 if already present.
#[no_mangle]
pub unsafe extern "C" fn pith_set_add_cstr(set_handle: i64, elem: *const i8) -> i64 {
    if set_handle == 0 || elem.is_null() {
        return 0;
    }
    let impl_ref = &mut *(set_handle as *mut SetImpl);
    let set_elem = cstr_to_set_element(elem);
    if impl_ref.insert(set_elem) { 1 } else { 0 }
}

/// Insert an integer element into the set. Returns 1 if newly inserted, 0 if already present.
#[no_mangle]
pub unsafe extern "C" fn pith_set_add_int_handle(set_handle: i64, elem: i64) -> i64 {
    if set_handle == 0 {
        return 0;
    }
    let impl_ref = &mut *(set_handle as *mut SetImpl);
    if !matches!(impl_ref.elem_type, ElemType::Int) {
        return 0;
    }
    if impl_ref.insert(SetElement::Int(elem)) { 1 } else { 0 }
}

/// Check if a C-string element exists in the set. Returns 1 if present, 0 otherwise.
#[no_mangle]
pub unsafe extern "C" fn pith_set_contains_cstr(set_handle: i64, elem: *const i8) -> i64 {
    if set_handle == 0 || elem.is_null() {
        return 0;
    }
    let impl_ref = &*(set_handle as *const SetImpl);
    let set_elem = cstr_to_set_element(elem);
    if impl_ref.contains(&set_elem) { 1 } else { 0 }
}

/// Check if an integer element exists in the set. Returns 1 if present, 0 otherwise.
#[no_mangle]
pub unsafe extern "C" fn pith_set_contains_int_handle(set_handle: i64, elem: i64) -> i64 {
    if set_handle == 0 {
        return 0;
    }
    let impl_ref = &*(set_handle as *const SetImpl);
    if !matches!(impl_ref.elem_type, ElemType::Int) {
        return 0;
    }
    if impl_ref.contains(&SetElement::Int(elem)) { 1 } else { 0 }
}

/// Remove a C-string element from the set.
#[no_mangle]
pub unsafe extern "C" fn pith_set_remove_cstr(set_handle: i64, elem: *const i8) {
    if set_handle == 0 || elem.is_null() {
        return;
    }
    let impl_ref = &mut *(set_handle as *mut SetImpl);
    let set_elem = cstr_to_set_element(elem);
    impl_ref.remove(&set_elem);
}

/// Remove an integer element from the set.
#[no_mangle]
pub unsafe extern "C" fn pith_set_remove_int_handle(set_handle: i64, elem: i64) {
    if set_handle == 0 {
        return;
    }
    let impl_ref = &mut *(set_handle as *mut SetImpl);
    if !matches!(impl_ref.elem_type, ElemType::Int) {
        return;
    }
    impl_ref.remove(&SetElement::Int(elem));
}

/// Clear all elements from set (handle-based).
#[no_mangle]
pub unsafe extern "C" fn pith_set_clear_handle(set_handle: i64) {
    if set_handle == 0 {
        return;
    }
    let impl_ref = &mut *(set_handle as *mut SetImpl);
    impl_ref.clear();
}

/// Check if set is empty (handle-based). Returns 1 if empty, 0 otherwise.
#[no_mangle]
pub unsafe extern "C" fn pith_set_is_empty_handle(set_handle: i64) -> i64 {
    if set_handle == 0 {
        return 1;
    }
    let impl_ref = &*(set_handle as *const SetImpl);
    if impl_ref.len() == 0 { 1 } else { 0 }
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
