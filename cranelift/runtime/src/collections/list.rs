//! List[T] - ordered collection with ARC
//!
//! Hybrid approach: Uses idiomatic Rust Vec internally for all operations,
//! but presents FFI-compatible interface matching the C runtime.

use crate::string::{forge_string_release, forge_string_retain, ForgeString};

/// FFI-compatible list handle
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ForgeList {
    /// Pointer to internal list implementation (pub for cross-module access)
    pub ptr: *mut (),
}

/// Internal list implementation using idiomatic Rust
///
/// Stores elements as `Vec<Arc<[u8]>>` where each element is a byte slice.
/// For primitive types (Int, Float), we store the raw bytes.
/// For heap types (String, List, Map), we store references.
#[repr(C)]
pub struct ListImpl {
    /// Magic number to identify ListImpl pointers (0x464F5247 = "FORG")
    pub magic: u32,
    /// Size of each element in bytes
    pub elem_size: usize,
    /// Type tag for determining how to handle elements
    pub type_tag: ListTypeTag,
    /// Element data as byte vectors
    pub elements: Vec<Vec<u8>>,
}

/// Magic number to identify ListImpl pointers
pub const LIST_MAGIC: u32 = 0x464F5247;

/// Type tag for list elements
#[derive(Clone, Copy)]
pub enum ListTypeTag {
    Primitive, // Int, Float, Bool - stored by value
    String,    // ForgeString - needs retain/release
    List,      // Nested list - needs retain/release
    Map,       // Map - needs retain/release
}

impl ListImpl {
    fn new(elem_size: usize, type_tag: ListTypeTag) -> Self {
        ListImpl {
            magic: LIST_MAGIC,
            elements: Vec::new(),
            elem_size,
            type_tag,
        }
    }

    fn len(&self) -> usize {
        self.elements.len()
    }

    fn push(&mut self, elem: &[u8]) {
        self.elements.push(elem.to_vec());
    }

    fn pop(&mut self) -> Option<Vec<u8>> {
        self.elements.pop()
    }

    fn get(&self, index: usize) -> Option<&[u8]> {
        self.elements.get(index).map(|v| v.as_slice())
    }

    fn set(&mut self, index: usize, elem: &[u8]) {
        if index < self.elements.len() {
            self.elements[index] = elem.to_vec();
        }
    }

    fn insert(&mut self, index: usize, elem: &[u8]) {
        if index <= self.elements.len() {
            self.elements.insert(index, elem.to_vec());
        }
    }

    fn remove(&mut self, index: usize) -> Option<Vec<u8>> {
        if index < self.elements.len() {
            Some(self.elements.remove(index))
        } else {
            None
        }
    }

    fn clear(&mut self) {
        self.elements.clear();
    }

    fn swap(&mut self, a: usize, b: usize) {
        if a < self.elements.len() && b < self.elements.len() {
            self.elements.swap(a, b);
        }
    }
}

/// Create a new empty list
///
/// # Arguments
/// * `elem_size` - Size of each element in bytes
/// * `type_tag` - Type tag for element handling (0=primitive, 1=string, 2=list, 3=map)
/// Create a new list with default element size (8 bytes, primitive)
#[no_mangle]
pub unsafe extern "C" fn forge_list_new_default() -> ForgeList {
    forge_list_new(8, 0)
}

#[no_mangle]
pub unsafe extern "C" fn forge_list_new(elem_size: i64, type_tag: i32) -> ForgeList {
    let tag = match type_tag {
        1 => ListTypeTag::String,
        2 => ListTypeTag::List,
        3 => ListTypeTag::Map,
        _ => ListTypeTag::Primitive,
    };

    let list_impl = ListImpl::new(elem_size as usize, tag);
    let boxed = Box::new(list_impl);

    ForgeList {
        ptr: Box::into_raw(boxed) as *mut (),
    }
}

/// Get list length
#[no_mangle]
pub extern "C" fn forge_list_len(list: ForgeList) -> i64 {
    if list.ptr.is_null() {
        return 0;
    }

    unsafe {
        let impl_ref = &*(list.ptr as *const ListImpl);
        impl_ref.len() as i64
    }
}

/// Check if a raw pointer looks like a ListImpl (has the magic number)
pub fn is_list_ptr(ptr: *const ()) -> bool {
    if ptr.is_null() {
        return false;
    }
    unsafe {
        let magic = *(ptr as *const u32);
        magic == LIST_MAGIC
    }
}

/// Auto-detect len: works on both lists and C strings
/// If the pointer has the list magic number, returns list length.
/// Otherwise, returns C string length (strlen).
#[no_mangle]
pub extern "C" fn forge_auto_len(ptr: i64) -> i64 {
    if ptr == 0 {
        return 0;
    }
    let raw = ptr as *const ();
    if is_list_ptr(raw) {
        let list = ForgeList { ptr: raw as *mut () };
        forge_list_len(list)
    } else {
        crate::string::forge_cstring_len(ptr as *const i8)
    }
}

/// Push element to end of list
///
/// # Safety
/// * `elem` must point to valid data of size `elem_size`
#[no_mangle]
pub unsafe extern "C" fn forge_list_push(list: *mut ForgeList, elem: *const u8, elem_size: i64) {
    if list.is_null() || (*list).ptr.is_null() || elem.is_null() {
        return;
    }

    let impl_ref = &mut *((*list).ptr as *mut ListImpl);

    // Verify element size matches
    if impl_ref.elem_size != elem_size as usize {
        eprintln!("forge: list element size mismatch");
        return;
    }

    // Copy element data
    let elem_slice = std::slice::from_raw_parts(elem, elem_size as usize);

    // Handle string elements specially (retain)
    if matches!(impl_ref.type_tag, ListTypeTag::String) {
        let s = elem as *const ForgeString;
        forge_string_retain(*s);
    }

    impl_ref.push(elem_slice);
}

/// Push an i64-sized value into a list using the list handle directly.
/// This is a simpler ABI used by generated method calls.
#[no_mangle]
pub unsafe extern "C" fn forge_list_push_value(list: ForgeList, value: i64) {
    if list.ptr.is_null() {
        return;
    }

    let impl_ref = &mut *(list.ptr as *mut ListImpl);
    let bytes = value.to_ne_bytes();
    let elem_len = impl_ref.elem_size.min(bytes.len());
    impl_ref.push(&bytes[..elem_len]);
}

/// Set element at index (value-based API, stores i64).
#[no_mangle]
pub unsafe extern "C" fn forge_list_set_value(list: ForgeList, index: i64, value: i64) {
    if list.ptr.is_null() {
        return;
    }
    let impl_ref = &mut *(list.ptr as *mut ListImpl);
    let idx = index as usize;
    if idx >= impl_ref.elements.len() {
        return;
    }
    let bytes = value.to_ne_bytes();
    let elem_len = impl_ref.elem_size.min(bytes.len());
    impl_ref.elements[idx] = bytes[..elem_len].to_vec();
}

/// Join a list of C string pointers with a separator.
/// Returns a newly allocated C string.
#[no_mangle]
pub unsafe extern "C" fn forge_list_join(list: ForgeList, sep: *const i8) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if list.ptr.is_null() {
        return std::ptr::null_mut();
    }

    let impl_ref = &*(list.ptr as *const ListImpl);
    if impl_ref.elements.is_empty() {
        let ptr = alloc(Layout::from_size_align(1, 1).unwrap()) as *mut i8;
        if !ptr.is_null() {
            *ptr = 0;
        }
        return ptr;
    }

    let sep_len = if sep.is_null() {
        0usize
    } else {
        crate::string::forge_cstring_len(sep) as usize
    };

    let mut total_len = 0usize;
    for (i, elem) in impl_ref.elements.iter().enumerate() {
        if elem.len() >= 8 {
            let ptr_val = i64::from_ne_bytes(elem[..8].try_into().unwrap()) as *const i8;
            if !ptr_val.is_null() {
                total_len += crate::string::forge_cstring_len(ptr_val) as usize;
            }
        }
        if i + 1 < impl_ref.elements.len() {
            total_len += sep_len;
        }
    }

    let out = alloc(Layout::from_size_align(total_len + 1, 1).unwrap()) as *mut i8;
    if out.is_null() {
        return std::ptr::null_mut();
    }

    let mut write = 0usize;
    for (i, elem) in impl_ref.elements.iter().enumerate() {
        if elem.len() >= 8 {
            let ptr_val = i64::from_ne_bytes(elem[..8].try_into().unwrap()) as *const i8;
            if !ptr_val.is_null() {
                let len = crate::string::forge_cstring_len(ptr_val) as usize;
                std::ptr::copy_nonoverlapping(ptr_val as *const u8, out.add(write) as *mut u8, len);
                write += len;
            }
        }
        if i + 1 < impl_ref.elements.len() && sep_len > 0 {
            std::ptr::copy_nonoverlapping(sep as *const u8, out.add(write) as *mut u8, sep_len);
            write += sep_len;
        }
    }

    *out.add(write) = 0;
    out
}

/// Pop element from end of list
///
/// Returns true if successful, false if list is empty
#[no_mangle]
pub unsafe extern "C" fn forge_list_pop(
    list: *mut ForgeList,
    elem_size: i64,
    out: *mut u8,
) -> bool {
    if list.is_null() || (*list).ptr.is_null() || out.is_null() {
        return false;
    }

    let impl_ref = &mut *((*list).ptr as *mut ListImpl);

    if impl_ref.elem_size != elem_size as usize {
        eprintln!("forge: list element size mismatch");
        return false;
    }

    match impl_ref.pop() {
        Some(elem_data) => {
            // Copy to output buffer
            std::ptr::copy_nonoverlapping(elem_data.as_ptr(), out, elem_data.len());

            // Handle string elements (release the copy in the list)
            if matches!(impl_ref.type_tag, ListTypeTag::String) {
                let s = out as *const ForgeString;
                forge_string_release(*s);
            }

            true
        }
        None => false,
    }
}

/// Get element at index for pointer-sized elements (returns i64 directly)
/// Returns 0 if out of bounds or on error
#[no_mangle]
pub unsafe extern "C" fn forge_list_get_value(list: ForgeList, index: i64) -> i64 {
    if list.ptr.is_null() {
        return 0;
    }

    let impl_ref = &*(list.ptr as *const ListImpl);

    if index < 0 || index >= impl_ref.len() as i64 {
        return 0;
    }

    // For pointer-sized elements (8 bytes), read directly
    if impl_ref.elem_size == 8 {
        match impl_ref.get(index as usize) {
            Some(elem_data) if elem_data.len() == 8 => {
                let val = i64::from_ne_bytes(elem_data[..8].try_into().unwrap_or([0; 8]));
                val
            }
            _ => 0,
        }
    } else {
        0
    }
}

/// Get element at index (copies to out buffer)
///
/// Returns true if successful, false if index out of bounds
#[no_mangle]
pub unsafe extern "C" fn forge_list_get(
    list: ForgeList,
    index: i64,
    elem_size: i64,
    out: *mut u8,
) -> bool {
    if list.ptr.is_null() || out.is_null() {
        return false;
    }

    let impl_ref = &*(list.ptr as *const ListImpl);

    if index < 0 || index >= impl_ref.len() as i64 {
        return false;
    }

    if impl_ref.elem_size != elem_size as usize {
        eprintln!("forge: list element size mismatch");
        return false;
    }

    match impl_ref.get(index as usize) {
        Some(elem_data) => {
            std::ptr::copy_nonoverlapping(elem_data.as_ptr(), out, elem_data.len());

            // Retain string elements (caller gets a reference)
            if matches!(impl_ref.type_tag, ListTypeTag::String) {
                let s = out as *const ForgeString;
                forge_string_retain(*s);
            }

            true
        }
        None => false,
    }
}

/// Set element at index (copies from elem buffer)
///
/// Returns true if successful, false if index out of bounds
#[no_mangle]
pub unsafe extern "C" fn forge_list_set(
    list: ForgeList,
    index: i64,
    elem: *const u8,
    elem_size: i64,
) -> bool {
    if list.ptr.is_null() || elem.is_null() {
        return false;
    }

    let impl_ref = &mut *(list.ptr as *mut ListImpl);

    if index < 0 || index >= impl_ref.len() as i64 {
        return false;
    }

    if impl_ref.elem_size != elem_size as usize {
        eprintln!("forge: list element size mismatch");
        return false;
    }

    // Release old element if it's a heap type
    if matches!(impl_ref.type_tag, ListTypeTag::String) {
        let old_elem = impl_ref.get(index as usize).unwrap();
        let old_s = old_elem.as_ptr() as *const ForgeString;
        forge_string_release(*old_s);
    }

    // Copy new element
    let elem_slice = std::slice::from_raw_parts(elem, elem_size as usize);

    // Retain new element
    if matches!(impl_ref.type_tag, ListTypeTag::String) {
        let s = elem as *const ForgeString;
        forge_string_retain(*s);
    }

    impl_ref.set(index as usize, elem_slice);
    true
}

/// Insert element at index
///
/// Returns true if successful
#[no_mangle]
pub unsafe extern "C" fn forge_list_insert(
    list: *mut ForgeList,
    index: i64,
    elem: *const u8,
    elem_size: i64,
) -> bool {
    if list.is_null() || (*list).ptr.is_null() || elem.is_null() {
        return false;
    }

    let impl_ref = &mut *((*list).ptr as *mut ListImpl);

    if index < 0 || index > impl_ref.len() as i64 {
        return false;
    }

    if impl_ref.elem_size != elem_size as usize {
        eprintln!("forge: list element size mismatch");
        return false;
    }

    let elem_slice = std::slice::from_raw_parts(elem, elem_size as usize);

    // Retain element
    if matches!(impl_ref.type_tag, ListTypeTag::String) {
        let s = elem as *const ForgeString;
        forge_string_retain(*s);
    }

    impl_ref.insert(index as usize, elem_slice);
    true
}

/// Remove element at index
///
/// Returns true if successful
#[no_mangle]
pub unsafe extern "C" fn forge_list_remove(
    list: *mut ForgeList,
    index: i64,
    elem_size: i64,
) -> bool {
    if list.is_null() || (*list).ptr.is_null() {
        return false;
    }

    let impl_ref = &mut *((*list).ptr as *mut ListImpl);

    if index < 0 || index >= impl_ref.len() as i64 {
        return false;
    }

    if impl_ref.elem_size != elem_size as usize {
        eprintln!("forge: list element size mismatch");
        return false;
    }

    // Release element before removal
    if matches!(impl_ref.type_tag, ListTypeTag::String) {
        let elem = impl_ref.get(index as usize).unwrap();
        let s = elem.as_ptr() as *const ForgeString;
        forge_string_release(*s);
    }

    impl_ref.remove(index as usize);
    true
}

/// Remove element at index (by-value variant — works with internal pointer)
#[no_mangle]
pub unsafe extern "C" fn forge_list_remove_value(list: ForgeList, index: i64) -> i64 {
    if list.ptr.is_null() {
        return 0;
    }

    let impl_ref = &mut *(list.ptr as *mut ListImpl);

    if index < 0 || index >= impl_ref.len() as i64 {
        return 0;
    }

    // Release string element if needed
    if matches!(impl_ref.type_tag, ListTypeTag::String) {
        let elem = impl_ref.get(index as usize).unwrap();
        let s = elem.as_ptr() as *const ForgeString;
        forge_string_release(*s);
    }

    impl_ref.remove(index as usize);
    1
}

/// Clear all elements from list (by-value variant)
#[no_mangle]
pub unsafe extern "C" fn forge_list_clear_value(list: ForgeList) {
    if list.ptr.is_null() {
        return;
    }

    let impl_ref = &mut *(list.ptr as *mut ListImpl);

    if matches!(impl_ref.type_tag, ListTypeTag::String) {
        for i in 0..impl_ref.len() {
            let elem = impl_ref.get(i).unwrap();
            let s = elem.as_ptr() as *const ForgeString;
            forge_string_release(*s);
        }
    }

    impl_ref.clear();
}

/// Reverse list in-place (by-value variant — works with internal pointer)
#[no_mangle]
pub unsafe extern "C" fn forge_list_reverse_value(list: ForgeList) {
    if list.ptr.is_null() {
        return;
    }
    let impl_ref = &mut *(list.ptr as *mut ListImpl);
    let len = impl_ref.len();
    for i in 0..len / 2 {
        impl_ref.swap(i, len - 1 - i);
    }
}

/// Clear all elements from list
#[no_mangle]
pub unsafe extern "C" fn forge_list_clear(list: *mut ForgeList) {
    if list.is_null() || (*list).ptr.is_null() {
        return;
    }

    let impl_ref = &mut *((*list).ptr as *mut ListImpl);

    // Release all elements if they're heap types
    if matches!(impl_ref.type_tag, ListTypeTag::String) {
        for i in 0..impl_ref.len() {
            let elem = impl_ref.get(i).unwrap();
            let s = elem.as_ptr() as *const ForgeString;
            forge_string_release(*s);
        }
    }

    impl_ref.clear();
}

/// Check if list is empty
#[no_mangle]
pub extern "C" fn forge_list_is_empty(list: ForgeList) -> i64 {
    if list.ptr.is_null() {
        return 1;
    }
    unsafe {
        let impl_ref = &*(list.ptr as *const ListImpl);
        if impl_ref.len() == 0 {
            1
        } else {
            0
        }
    }
}

/// Reverse list elements in-place
#[no_mangle]
pub unsafe extern "C" fn forge_list_reverse(list: ForgeList) {
    if list.ptr.is_null() {
        return;
    }
    let impl_ref = &mut *(list.ptr as *mut ListImpl);
    impl_ref.elements.reverse();
}

/// Release list and free memory
#[no_mangle]
pub unsafe extern "C" fn forge_list_release(list: ForgeList) {
    if list.ptr.is_null() {
        return;
    }

    let impl_ref = &mut *(list.ptr as *mut ListImpl);

    // Release all elements
    if matches!(impl_ref.type_tag, ListTypeTag::String) {
        for i in 0..impl_ref.len() {
            let elem = impl_ref.get(i).unwrap();
            let s = elem.as_ptr() as *const ForgeString;
            forge_string_release(*s);
        }
    }

    // Free the list implementation
    let _ = Box::from_raw(list.ptr as *mut ListImpl);
}

/// Check if list contains a string element
#[no_mangle]
pub unsafe extern "C" fn forge_list_contains_string(list: ForgeList, s: ForgeString) -> bool {
    if list.ptr.is_null() {
        return false;
    }

    let impl_ref = &*(list.ptr as *const ListImpl);

    if !matches!(impl_ref.type_tag, ListTypeTag::String) {
        return false;
    }

    for i in 0..impl_ref.len() {
        let elem = impl_ref.get(i).unwrap();
        let elem_s = elem.as_ptr() as *const ForgeString;
        if crate::string::forge_string_eq(*elem_s, s) {
            return true;
        }
    }

    false
}

/// Check if a list of C-string pointers contains the given C-string.
#[no_mangle]
pub unsafe extern "C" fn forge_list_contains_cstr(list_handle: i64, s: *const i8) -> i64 {
    if list_handle == 0 || s.is_null() {
        return 0;
    }

    let impl_ref = &*(list_handle as *const ListImpl);
    let needle_len = crate::string::forge_cstring_len(s) as usize;
    let needle = std::slice::from_raw_parts(s as *const u8, needle_len);

    for elem in &impl_ref.elements {
        if elem.len() < 8 {
            continue;
        }
        let ptr_val = i64::from_ne_bytes(elem[..8].try_into().unwrap_or([0; 8])) as *const i8;
        if ptr_val.is_null() {
            continue;
        }
        let elem_len = crate::string::forge_cstring_len(ptr_val) as usize;
        if elem_len != needle_len {
            continue;
        }
        let elem_bytes = std::slice::from_raw_parts(ptr_val as *const u8, elem_len);
        if elem_bytes == needle {
            return 1;
        }
    }

    0
}

/// Find index of string in list
#[no_mangle]
pub unsafe extern "C" fn forge_list_index_of_string(list: ForgeList, s: ForgeString) -> i64 {
    if list.ptr.is_null() {
        return -1;
    }

    let impl_ref = &*(list.ptr as *const ListImpl);

    if !matches!(impl_ref.type_tag, ListTypeTag::String) {
        return -1;
    }

    for i in 0..impl_ref.len() {
        let elem = impl_ref.get(i).unwrap();
        let elem_s = elem.as_ptr() as *const ForgeString;
        if crate::string::forge_string_eq(*elem_s, s) {
            return i as i64;
        }
    }

    -1
}

/// Find the index of a C-string in a list of C-string pointers.
#[no_mangle]
pub unsafe extern "C" fn forge_list_index_of_cstr(list_handle: i64, s: *const i8) -> i64 {
    if list_handle == 0 || s.is_null() {
        return -1;
    }

    let impl_ref = &*(list_handle as *const ListImpl);
    let needle_len = crate::string::forge_cstring_len(s) as usize;
    let needle = std::slice::from_raw_parts(s as *const u8, needle_len);

    for (i, elem) in impl_ref.elements.iter().enumerate() {
        if elem.len() < 8 {
            continue;
        }
        let ptr_val = i64::from_ne_bytes(elem[..8].try_into().unwrap_or([0; 8])) as *const i8;
        if ptr_val.is_null() {
            continue;
        }
        let elem_len = crate::string::forge_cstring_len(ptr_val) as usize;
        if elem_len != needle_len {
            continue;
        }
        let elem_bytes = std::slice::from_raw_parts(ptr_val as *const u8, elem_len);
        if elem_bytes == needle {
            return i as i64;
        }
    }

    -1
}

/// Find index of integer value in list
#[no_mangle]
pub unsafe extern "C" fn forge_list_index_of_int(list: ForgeList, value: i64) -> i64 {
    if list.ptr.is_null() {
        return -1;
    }

    let impl_ref = &*(list.ptr as *const ListImpl);

    for i in 0..impl_ref.len() {
        let elem = impl_ref.get(i).unwrap();
        if elem.len() >= 8 {
            let stored = i64::from_le_bytes(elem[..8].try_into().unwrap_or([0u8; 8]));
            if stored == value {
                return i as i64;
            }
        }
    }

    -1
}

/// Retain all elements (for string lists during copy)
#[no_mangle]
pub unsafe extern "C" fn forge_list_retain_all_strings(list: ForgeList) {
    if list.ptr.is_null() {
        return;
    }

    let impl_ref = &*(list.ptr as *const ListImpl);

    if !matches!(impl_ref.type_tag, ListTypeTag::String) {
        return;
    }

    for i in 0..impl_ref.len() {
        let elem = impl_ref.get(i).unwrap();
        let s = elem.as_ptr() as *const ForgeString;
        forge_string_retain(*s);
    }
}

/// Release all elements (for cleanup)
#[no_mangle]
pub unsafe extern "C" fn forge_list_release_all_strings(list: ForgeList) {
    if list.ptr.is_null() {
        return;
    }

    let impl_ref = &*(list.ptr as *const ListImpl);

    if !matches!(impl_ref.type_tag, ListTypeTag::String) {
        return;
    }

    for i in 0..impl_ref.len() {
        let elem = impl_ref.get(i).unwrap();
        let s = elem.as_ptr() as *const ForgeString;
        forge_string_release(*s);
    }
}

/// Check if list contains an integer value
#[no_mangle]
pub unsafe extern "C" fn forge_list_contains_int(list: ForgeList, value: i64) -> i64 {
    if list.ptr.is_null() {
        return 0;
    }

    let impl_ref = &*(list.ptr as *const ListImpl);

    for i in 0..impl_ref.len() {
        let elem = impl_ref.get(i).unwrap();
        if elem.len() >= 8 {
            let stored = i64::from_le_bytes(elem[..8].try_into().unwrap_or([0u8; 8]));
            if stored == value {
                return 1;
            }
        }
    }

    0
}

/// Check if list is empty
#[no_mangle]
pub extern "C" fn forge_list_is_empty_int(list: ForgeList) -> i64 {
    forge_list_is_empty(list)
}

/// Destructor for list elements in collections
///
/// Called by cycle collector when freeing cyclic list objects
#[no_mangle]
pub extern "C" fn forge_list_destructor(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }

    unsafe {
        let list = ptr as *const ForgeList;
        forge_list_release(*list);
    }
}

// ===============================================================
// Functional list operations: map, filter, reduce
// ===============================================================

/// Apply a function to each element, return a new list.
/// fn_ptr is a function pointer: fn(i64) -> i64
#[no_mangle]
pub unsafe extern "C" fn forge_list_map(list_ptr: i64, fn_ptr: i64) -> i64 {
    if list_ptr == 0 { return 0; }
    let src = &*(list_ptr as *const ListImpl);
    let func: extern "C" fn(i64) -> i64 = std::mem::transmute(fn_ptr as *const ());
    let result = forge_list_new(8, 0);
    let result_ptr = result.ptr as i64;

    for i in 0..src.len() {
        if let Some(elem_data) = src.get(i) {
            let val = if elem_data.len() >= 8 {
                i64::from_ne_bytes(elem_data[..8].try_into().unwrap_or([0; 8]))
            } else { 0 };
            let mapped = func(val);
            forge_list_push_value(ForgeList { ptr: result_ptr as *mut () }, mapped);
        }
    }
    result_ptr
}

/// Return a new list containing only elements where predicate returns non-zero.
/// fn_ptr is a function pointer: fn(i64) -> i64 (truthy = non-zero)
#[no_mangle]
pub unsafe extern "C" fn forge_list_filter(list_ptr: i64, fn_ptr: i64) -> i64 {
    let list = ForgeList { ptr: list_ptr as *mut () };
    let result = forge_list_new(8, 0);
    if list.ptr.is_null() {
        return result.ptr as i64;
    }

    let impl_ref = &*(list.ptr as *const ListImpl);
    let func: extern "C" fn(i64) -> i64 = std::mem::transmute(fn_ptr as *const ());

    for i in 0..impl_ref.len() {
        if let Some(elem_data) = impl_ref.get(i) {
            let val = if elem_data.len() >= 8 {
                i64::from_ne_bytes(elem_data[..8].try_into().unwrap_or([0; 8]))
            } else { 0 };
            if func(val) != 0 {
                forge_list_push_value(result, val);
            }
        }
    }
    result.ptr as i64
}

/// Reduce a list to a single value using an accumulator function.
/// fn_ptr: fn(accumulator: i64, element: i64) -> i64
#[no_mangle]
pub unsafe extern "C" fn forge_list_reduce(list_ptr: i64, init: i64, fn_ptr: i64) -> i64 {
    let list = ForgeList { ptr: list_ptr as *mut () };
    if list.ptr.is_null() {
        return init;
    }

    let impl_ref = &*(list.ptr as *const ListImpl);
    let func: extern "C" fn(i64, i64) -> i64 = std::mem::transmute(fn_ptr as *const ());

    let mut acc = init;
    for i in 0..impl_ref.len() {
        if let Some(elem_data) = impl_ref.get(i) {
            let val = if elem_data.len() >= 8 {
                i64::from_ne_bytes(elem_data[..8].try_into().unwrap_or([0; 8]))
            } else { 0 };
            acc = func(acc, val);
        }
    }
    acc
}

/// Apply function to each element (no return value, side effects only).
/// fn_ptr: fn(i64) -> i64
#[no_mangle]
pub unsafe extern "C" fn forge_list_each(list_ptr: i64, fn_ptr: i64) {
    let list = ForgeList { ptr: list_ptr as *mut () };
    if list.ptr.is_null() {
        return;
    }

    let impl_ref = &*(list.ptr as *const ListImpl);
    let func: extern "C" fn(i64) -> i64 = std::mem::transmute(fn_ptr as *const ());

    for i in 0..impl_ref.len() {
        if let Some(elem_data) = impl_ref.get(i) {
            let val = if elem_data.len() >= 8 {
                i64::from_ne_bytes(elem_data[..8].try_into().unwrap_or([0; 8]))
            } else { 0 };
            func(val);
        }
    }
}
