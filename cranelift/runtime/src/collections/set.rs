//! Set[T] - unique element collection
//!
//! Hybrid approach: Uses hashbrown::HashSet internally for O(1) operations,
//! but presents FFI-compatible interface matching the C runtime.

use crate::string::{ForgeString, forge_string_retain, forge_string_release};
use hashbrown::HashSet;
use std::hash::{Hash, Hasher};

/// FFI-compatible set handle
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ForgeSet {
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
pub unsafe extern "C" fn forge_set_new(elem_type: i32, elem_size: i64, elem_is_heap: bool) -> ForgeSet {
    let etype = match elem_type {
        1 => ElemType::String,
        _ => ElemType::Int,
    };
    
    let set_impl = SetImpl::new(etype, elem_size as usize, elem_is_heap);
    let boxed = Box::new(set_impl);
    
    ForgeSet {
        ptr: Box::into_raw(boxed) as *mut (),
    }
}

/// Get set length
#[no_mangle]
pub extern "C" fn forge_set_len(set: ForgeSet) -> i64 {
    if set.ptr.is_null() {
        return 0;
    }
    
    unsafe {
        let impl_ref = &*(set.ptr as *const SetImpl);
        impl_ref.len() as i64
    }
}

/// Insert integer element
/// 
/// Returns true if element was inserted (was not already present)
#[no_mangle]
pub unsafe extern "C" fn forge_set_insert_int(set: *mut ForgeSet, elem: i64) -> bool {
    if set.is_null() || (*set).ptr.is_null() {
        return false;
    }
    
    let impl_ref = &mut *((*set).ptr as *mut SetImpl);
    
    if !matches!(impl_ref.elem_type, ElemType::Int) {
        eprintln!("forge: set element type mismatch (expected int)");
        return false;
    }
    
    impl_ref.insert(SetElement::Int(elem))
}

/// Insert string element
/// 
/// Returns true if element was inserted (was not already present)
#[no_mangle]
pub unsafe extern "C" fn forge_set_insert_string(set: *mut ForgeSet, elem: ForgeString) -> bool {
    if set.is_null() || (*set).ptr.is_null() {
        return false;
    }
    
    let impl_ref = &mut *((*set).ptr as *mut SetImpl);
    
    if !matches!(impl_ref.elem_type, ElemType::String) {
        eprintln!("forge: set element type mismatch (expected string)");
        return false;
    }
    
    // Copy element data
    let elem_slice = std::slice::from_raw_parts(elem.ptr, elem.len as usize);
    let elem_vec = elem_slice.to_vec();
    
    // Retain the string as it's being stored
    if impl_ref.elem_is_heap {
        forge_string_retain(elem);
    }
    
    let was_inserted = impl_ref.insert(SetElement::String(elem_vec));
    
    // If element already existed, we need to release the retained copy
    if !was_inserted && impl_ref.elem_is_heap {
        forge_string_release(elem);
    }
    
    was_inserted
}

/// Check if set contains integer element
#[no_mangle]
pub extern "C" fn forge_set_contains_int(set: ForgeSet, elem: i64) -> bool {
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

/// Check if set contains string element
#[no_mangle]
pub unsafe extern "C" fn forge_set_contains_string(set: ForgeSet, elem: ForgeString) -> bool {
    if set.ptr.is_null() {
        return false;
    }
    
    let impl_ref = &*(set.ptr as *const SetImpl);
    
    if !matches!(impl_ref.elem_type, ElemType::String) {
        return false;
    }
    
    let elem_slice = std::slice::from_raw_parts(elem.ptr, elem.len as usize);
    let set_elem = SetElement::String(elem_slice.to_vec());
    
    impl_ref.contains(&set_elem)
}

/// Remove integer element from set
/// 
/// Returns true if element was present and removed
#[no_mangle]
pub unsafe extern "C" fn forge_set_remove_int(set: *mut ForgeSet, elem: i64) -> bool {
    if set.is_null() || (*set).ptr.is_null() {
        return false;
    }
    
    let impl_ref = &mut *((*set).ptr as *mut SetImpl);
    
    if !matches!(impl_ref.elem_type, ElemType::Int) {
        return false;
    }
    
    impl_ref.remove(&SetElement::Int(elem))
}

/// Remove string element from set
/// 
/// Returns true if element was present and removed
#[no_mangle]
pub unsafe extern "C" fn forge_set_remove_string(set: *mut ForgeSet, elem: ForgeString) -> bool {
    if set.is_null() || (*set).ptr.is_null() {
        return false;
    }
    
    let impl_ref = &mut *((*set).ptr as *mut SetImpl);
    
    if !matches!(impl_ref.elem_type, ElemType::String) {
        return false;
    }
    
    let elem_slice = std::slice::from_raw_parts(elem.ptr, elem.len as usize);
    let set_elem = SetElement::String(elem_slice.to_vec());
    
    let was_removed = impl_ref.remove(&set_elem);
    
    // Release the element if it was present
    if was_removed && impl_ref.elem_is_heap {
        forge_string_release(elem);
    }
    
    was_removed
}

/// Clear all elements from set
#[no_mangle]
pub unsafe extern "C" fn forge_set_clear(set: *mut ForgeSet) {
    if set.is_null() || (*set).ptr.is_null() {
        return;
    }
    
    let impl_ref = &mut *((*set).ptr as *mut SetImpl);
    
    // Release all elements if they're heap types
    if impl_ref.elem_is_heap {
        for elem in impl_ref.iter() {
            if let SetElement::String(bytes) = elem {
                // Reconstruct the string from bytes to release it
                let s = ForgeString {
                    ptr: bytes.as_ptr(),
                    len: bytes.len() as i64,
                    is_heap: true,
                };
                forge_string_release(s);
            }
        }
    }
    
    impl_ref.clear();
}

/// Release set and free memory
#[no_mangle]
pub unsafe extern "C" fn forge_set_release(set: ForgeSet) {
    if set.ptr.is_null() {
        return;
    }
    
    let impl_ref = &mut *(set.ptr as *mut SetImpl);
    
    // Release all elements if they're heap types
    if impl_ref.elem_is_heap {
        for elem in impl_ref.iter() {
            if let SetElement::String(bytes) = elem {
                let s = ForgeString {
                    ptr: bytes.as_ptr(),
                    len: bytes.len() as i64,
                    is_heap: true,
                };
                forge_string_release(s);
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
pub unsafe extern "C" fn forge_set_to_list_int(set: ForgeSet) -> crate::collections::list::ForgeList {
    use crate::collections::list::{ForgeList, forge_list_new, forge_list_push};
    
    if set.ptr.is_null() {
        return ForgeList { ptr: std::ptr::null_mut() };
    }
    
    let impl_ref = &*(set.ptr as *const SetImpl);
    
    if !matches!(impl_ref.elem_type, ElemType::Int) {
        return ForgeList { ptr: std::ptr::null_mut() };
    }
    
    let mut list = forge_list_new(std::mem::size_of::<i64>() as i64, 0);
    
    for elem in impl_ref.iter() {
        if let SetElement::Int(n) = elem {
            let n_ptr = n as *const i64 as *const u8;
            forge_list_push(&mut list, n_ptr, std::mem::size_of::<i64>() as i64);
        }
    }
    
    list
}

/// Convert set to list (for string elements)
/// 
/// # Safety
/// Returns a new list that must be released
#[no_mangle]
pub unsafe extern "C" fn forge_set_to_list_string(set: ForgeSet) -> crate::collections::list::ForgeList {
    use crate::collections::list::{ForgeList, forge_list_new, forge_list_push};
    
    if set.ptr.is_null() {
        return ForgeList { ptr: std::ptr::null_mut() };
    }
    
    let impl_ref = &*(set.ptr as *const SetImpl);
    
    if !matches!(impl_ref.elem_type, ElemType::String) {
        return ForgeList { ptr: std::ptr::null_mut() };
    }
    
    let mut list = forge_list_new(impl_ref.elem_size as i64, 1); // type tag 1 = string
    
    for elem in impl_ref.iter() {
        if let SetElement::String(bytes) = elem {
            // Retain as we're copying to list
            let s = ForgeString {
                ptr: bytes.as_ptr(),
                len: bytes.len() as i64,
                is_heap: true,
            };
            forge_string_retain(s);
            
            forge_list_push(&mut list, bytes.as_ptr(), impl_ref.elem_size as i64);
        }
    }
    
    list
}

/// Destructor for set elements in collections
/// 
/// Called by cycle collector when freeing cyclic set objects
#[no_mangle]
pub extern "C" fn forge_set_destructor(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    
    unsafe {
        let set = ptr as *const ForgeSet;
        forge_set_release(*set);
    }
}

