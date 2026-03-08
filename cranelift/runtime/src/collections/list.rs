//! List[T] - ordered, contiguous array-backed list with ARC
//!
//! Lists store elements densely in memory. For heap-allocated types
//! (like strings), we need to retain/release elements on operations.

use crate::arc::{forge_rc_alloc, forge_rc_release, TypeTag};
use crate::string::{ForgeString, forge_string_retain, forge_string_release};
use std::alloc::{alloc, dealloc, Layout};

/// List implementation structure
#[repr(C)]
pub struct ForgeListImpl {
    /// Raw element data
    data: *mut u8,
    /// Number of elements
    len: i64,
    /// Capacity in elements
    cap: i64,
    /// Size of each element in bytes
    elem_size: i64,
}

/// List handle (what Forge code manipulates)
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ForgeList {
    impl_ptr: *mut ForgeListImpl,
}

/// Type of element destructor function
pub type ElemDestructor = Option<extern "C" fn(*mut u8)>;

/// Global list of element destructors by type tag
static mut ELEM_DESTRUCTORS: [(TypeTag, ElemDestructor); 4] = [
    (TypeTag::String, Some(forge_string_elem_destructor)),
    (TypeTag::List, Some(forge_list_elem_destructor)),
    (TypeTag::Map, Some(forge_map_elem_destructor)),
    (TypeTag::Closure, None), // TODO: Implement closure destructor
];

extern "C" fn forge_string_elem_destructor(ptr: *mut u8) {
    unsafe {
        let s = ptr as *mut ForgeString;
        forge_string_release(*s);
    }
}

extern "C" fn forge_list_elem_destructor(ptr: *mut u8) {
    unsafe {
        let list = ptr as *mut ForgeList;
        forge_list_release(*list);
    }
}

extern "C" fn forge_map_elem_destructor(_ptr: *mut u8) {
    // TODO: Implement when Map is ready
}

/// Create a new empty list
/// 
/// # Arguments
/// * `elem_size` - Size of each element in bytes
#[no_mangle]
pub unsafe extern "C" fn forge_list_new(elem_size: i64) -> ForgeList {
    let impl_size = std::mem::size_of::<ForgeListImpl>();
    let mem = forge_rc_alloc(impl_size, TypeTag::List as u32);
    
    let impl_ptr = mem as *mut ForgeListImpl;
    (*impl_ptr).data = std::ptr::null_mut();
    (*impl_ptr).len = 0;
    (*impl_ptr).cap = 0;
    (*impl_ptr).elem_size = elem_size;
    
    ForgeList { impl_ptr }
}

/// Get list length
#[no_mangle]
pub extern "C" fn forge_list_len(list: ForgeList) -> i64 {
    if list.impl_ptr.is_null() {
        return 0;
    }
    unsafe { (*list.impl_ptr).len }
}

/// Get list capacity
#[no_mangle]
pub extern "C" fn forge_list_cap(list: ForgeList) -> i64 {
    if list.impl_ptr.is_null() {
        return 0;
    }
    unsafe { (*list.impl_ptr).cap }
}

/// Get pointer to element at index (returns null if out of bounds)
#[no_mangle]
pub extern "C" fn forge_list_get_ptr(list: ForgeList, index: i64) -> *mut u8 {
    if list.impl_ptr.is_null() {
        return std::ptr::null_mut();
    }
    
    unsafe {
        let impl_ref = &*list.impl_ptr;
        if index < 0 || index >= impl_ref.len {
            return std::ptr::null_mut();
        }
        
        impl_ref.data.add((index * impl_ref.elem_size) as usize)
    }
}

/// Grow list capacity if needed
unsafe fn ensure_capacity(list: &mut ForgeList, needed: i64) {
    let impl_ref = &mut *list.impl_ptr;
    
    if impl_ref.cap >= needed {
        return;
    }
    
    // Calculate new capacity (double until sufficient)
    let mut new_cap = if impl_ref.cap == 0 { 4 } else { impl_ref.cap * 2 };
    while new_cap < needed {
        new_cap *= 2;
    }
    
    // Allocate new data buffer
    let data_size = (new_cap * impl_ref.elem_size) as usize;
    let new_data = alloc(Layout::from_size_align(data_size, 8).unwrap());
    
    if !impl_ref.data.is_null() {
        // Copy existing data
        let old_size = (impl_ref.len * impl_ref.elem_size) as usize;
        std::ptr::copy_nonoverlapping(impl_ref.data, new_data, old_size);
        
        // Free old data
        dealloc(impl_ref.data, Layout::from_size_align(old_size, 8).unwrap());
    }
    
    impl_ref.data = new_data;
    impl_ref.cap = new_cap;
}

/// Push element to end of list (copies data, retains if needed)
/// 
/// # Safety
/// * `elem` must point to valid data of size `elem_size`
/// * For heap types, caller must have already retained the element
#[no_mangle]
pub unsafe extern "C" fn forge_list_push(list: *mut ForgeList, elem: *const u8, elem_size: i64) {
    if list.is_null() || (*list).impl_ptr.is_null() || elem.is_null() {
        return;
    }
    
    let impl_ref = &mut *(*list).impl_ptr;
    
    // Verify element size matches
    if impl_ref.elem_size != elem_size {
        eprintln!("forge: list element size mismatch");
        return;
    }
    
    // Grow if needed
    ensure_capacity(&mut *list, impl_ref.len + 1);
    
    let impl_ref = &mut *(*list).impl_ptr;
    let dest = impl_ref.data.add((impl_ref.len * impl_ref.elem_size) as usize);
    
    // Copy element data
    std::ptr::copy_nonoverlapping(elem, dest, elem_size as usize);
    
    impl_ref.len += 1;
}

/// Pop element from end of list
/// 
/// Returns true if successful, false if list is empty
#[no_mangle]
pub unsafe extern "C" fn forge_list_pop(list: *mut ForgeList, elem_size: i64, out: *mut u8) -> bool {
    if list.is_null() || (*list).impl_ptr.is_null() || out.is_null() {
        return false;
    }
    
    let impl_ref = &mut *(*list).impl_ptr;
    
    if impl_ref.len <= 0 {
        return false;
    }
    
    // Verify element size matches
    if impl_ref.elem_size != elem_size {
        eprintln!("forge: list element size mismatch");
        return false;
    }
    
    impl_ref.len -= 1;
    let src = impl_ref.data.add((impl_ref.len * impl_ref.elem_size) as usize);
    
    // Copy element to output
    std::ptr::copy_nonoverlapping(src, out, elem_size as usize);
    
    true
}

/// Get element at index (copies to out buffer)
/// 
/// Returns true if successful, false if index out of bounds
#[no_mangle]
pub unsafe extern "C" fn forge_list_get(list: ForgeList, index: i64, elem_size: i64, out: *mut u8) -> bool {
    if list.impl_ptr.is_null() || out.is_null() {
        return false;
    }
    
    let impl_ref = &*list.impl_ptr;
    
    if index < 0 || index >= impl_ref.len {
        return false;
    }
    
    if impl_ref.elem_size != elem_size {
        eprintln!("forge: list element size mismatch");
        return false;
    }
    
    let src = impl_ref.data.add((index * impl_ref.elem_size) as usize);
    std::ptr::copy_nonoverlapping(src, out, elem_size as usize);
    
    true
}

/// Set element at index (copies from elem buffer)
/// 
/// Returns true if successful, false if index out of bounds
#[no_mangle]
pub unsafe extern "C" fn forge_list_set(list: ForgeList, index: i64, elem: *const u8, elem_size: i64) -> bool {
    if list.impl_ptr.is_null() || elem.is_null() {
        return false;
    }
    
    let impl_ref = &mut *list.impl_ptr;
    
    if index < 0 || index >= impl_ref.len {
        return false;
    }
    
    if impl_ref.elem_size != elem_size {
        eprintln!("forge: list element size mismatch");
        return false;
    }
    
    let dest = impl_ref.data.add((index * impl_ref.elem_size) as usize);
    std::ptr::copy_nonoverlapping(elem, dest, elem_size as usize);
    
    true
}

/// Remove element at index (shifts remaining elements)
/// 
/// Returns true if successful
#[no_mangle]
pub unsafe extern "C" fn forge_list_remove(list: *mut ForgeList, index: i64, elem_size: i64) -> bool {
    if list.is_null() || (*list).impl_ptr.is_null() {
        return false;
    }
    
    let impl_ref = &mut *(*list).impl_ptr;
    
    if index < 0 || index >= impl_ref.len {
        return false;
    }
    
    if impl_ref.elem_size != elem_size {
        eprintln!("forge: list element size mismatch");
        return false;
    }
    
    // Shift elements after index down by one
    let src = impl_ref.data.add(((index + 1) * impl_ref.elem_size) as usize);
    let dest = impl_ref.data.add((index * impl_ref.elem_size) as usize);
    let bytes_to_move = ((impl_ref.len - index - 1) * impl_ref.elem_size) as usize;
    
    if bytes_to_move > 0 {
        std::ptr::copy(src, dest, bytes_to_move);
    }
    
    impl_ref.len -= 1;
    true
}

/// Insert element at index (shifts elements to make room)
/// 
/// Returns true if successful
#[no_mangle]
pub unsafe extern "C" fn forge_list_insert(list: *mut ForgeList, index: i64, elem: *const u8, elem_size: i64) -> bool {
    if list.is_null() || (*list).impl_ptr.is_null() || elem.is_null() {
        return false;
    }
    
    let impl_ref = &mut *(*list).impl_ptr;
    
    if index < 0 || index > impl_ref.len {
        return false;
    }
    
    if impl_ref.elem_size != elem_size {
        eprintln!("forge: list element size mismatch");
        return false;
    }
    
    // Grow if needed
    ensure_capacity(&mut *list, impl_ref.len + 1);
    
    let impl_ref = &mut *(*list).impl_ptr;
    
    // Shift elements from index onwards up by one
    if index < impl_ref.len {
        let src = impl_ref.data.add((index * impl_ref.elem_size) as usize);
        let dest = impl_ref.data.add(((index + 1) * impl_ref.elem_size) as usize);
        let bytes_to_move = ((impl_ref.len - index) * impl_ref.elem_size) as usize;
        std::ptr::copy(src, dest, bytes_to_move);
    }
    
    // Insert new element
    let dest = impl_ref.data.add((index * impl_ref.elem_size) as usize);
    std::ptr::copy_nonoverlapping(elem, dest, elem_size as usize);
    
    impl_ref.len += 1;
    true
}

/// Clear all elements from list (doesn't free memory)
#[no_mangle]
pub unsafe extern "C" fn forge_list_clear(list: *mut ForgeList) {
    if list.is_null() || (*list).impl_ptr.is_null() {
        return;
    }
    
    let impl_ref = &mut *(*list).impl_ptr;
    impl_ref.len = 0;
}

/// Release list and free memory
/// 
/// Note: This does NOT release elements - caller must handle element cleanup first
#[no_mangle]
pub unsafe extern "C" fn forge_list_release(list: ForgeList) {
    if list.impl_ptr.is_null() {
        return;
    }
    
    let impl_ref = &mut *list.impl_ptr;
    
    // Free element data buffer
    if !impl_ref.data.is_null() {
        let data_size = (impl_ref.cap * impl_ref.elem_size) as usize;
        dealloc(impl_ref.data, Layout::from_size_align(data_size, 8).unwrap());
    }
    
    // Release the implementation structure (via ARC)
    let destructor: ElemDestructor = None; // No special destructor for impl
    forge_rc_release(list.impl_ptr as *mut u8, destructor);
}

/// Retain all elements in a list of strings
#[no_mangle]
pub unsafe extern "C" fn forge_list_retain_all_strings(list: ForgeList) {
    if list.impl_ptr.is_null() {
        return;
    }
    
    let impl_ref = &*list.impl_ptr;
    let elem_size = std::mem::size_of::<ForgeString>() as i64;
    
    if impl_ref.elem_size != elem_size {
        return; // Not a string list
    }
    
    for i in 0..impl_ref.len {
        let elem_ptr = impl_ref.data.add((i * elem_size) as usize) as *mut ForgeString;
        forge_string_retain(*elem_ptr);
    }
}

/// Release all elements in a list of strings
#[no_mangle]
pub unsafe extern "C" fn forge_list_release_all_strings(list: ForgeList) {
    if list.impl_ptr.is_null() {
        return;
    }
    
    let impl_ref = &*list.impl_ptr;
    let elem_size = std::mem::size_of::<ForgeString>() as i64;
    
    if impl_ref.elem_size != elem_size {
        return; // Not a string list
    }
    
    for i in 0..impl_ref.len {
        let elem_ptr = impl_ref.data.add((i * elem_size) as usize) as *mut ForgeString;
        forge_string_release(*elem_ptr);
    }
}

/// Check if list contains a string element
#[no_mangle]
pub unsafe extern "C" fn forge_list_contains_string(list: ForgeList, s: ForgeString) -> bool {
    if list.impl_ptr.is_null() {
        return false;
    }
    
    let impl_ref = &*list.impl_ptr;
    let elem_size = std::mem::size_of::<ForgeString>() as i64;
    
    if impl_ref.elem_size != elem_size {
        return false; // Not a string list
    }
    
    use crate::string::forge_string_eq;
    
    for i in 0..impl_ref.len {
        let elem_ptr = impl_ref.data.add((i * elem_size) as usize) as *mut ForgeString;
        if forge_string_eq(*elem_ptr, s) {
            return true;
        }
    }
    
    false
}

/// Find index of string in list (returns -1 if not found)
#[no_mangle]
pub unsafe extern "C" fn forge_list_index_of_string(list: ForgeList, s: ForgeString) -> i64 {
    if list.impl_ptr.is_null() {
        return -1;
    }
    
    let impl_ref = &*list.impl_ptr;
    let elem_size = std::mem::size_of::<ForgeString>() as i64;
    
    if impl_ref.elem_size != elem_size {
        return -1; // Not a string list
    }
    
    use crate::string::forge_string_eq;
    
    for i in 0..impl_ref.len {
        let elem_ptr = impl_ref.data.add((i * elem_size) as usize) as *mut ForgeString;
        if forge_string_eq(*elem_ptr, s) {
            return i;
        }
    }
    
    -1
}
