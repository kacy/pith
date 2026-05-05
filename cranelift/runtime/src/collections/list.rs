//! List[T] - ordered collection with ARC
//!
//! Hybrid approach: Uses idiomatic Rust Vec internally for all operations,
//! but presents FFI-compatible interface matching the C runtime.

use crate::handle_registry::{self, HandleKind};
use crate::string::{pith_string_release, pith_string_retain, PithString};
/// FFI-compatible list handle
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PithList {
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
    /// Packed storage for 8-byte values and handles.
    pub values8: Vec<i64>,
    /// Cached pointer to 8-byte storage for native inlined reads.
    pub values8_ptr: *const i64,
    /// Cached length for 8-byte storage.
    pub values8_len: usize,
}

/// Magic number to identify ListImpl pointers
pub const LIST_MAGIC: u32 = 0x464F5247;

/// Type tag for list elements
#[repr(C)]
#[derive(Clone, Copy)]
pub enum ListTypeTag {
    Primitive, // Int, Float, Bool - stored by value
    String,    // PithString - needs retain/release
    List,      // Nested list - needs retain/release
    Map,       // Map - needs retain/release
}

pub const LIST_IMPL_ELEM_SIZE_OFFSET: i32 = std::mem::offset_of!(ListImpl, elem_size) as i32;
pub const LIST_IMPL_VALUES8_PTR_OFFSET: i32 = std::mem::offset_of!(ListImpl, values8_ptr) as i32;
pub const LIST_IMPL_VALUES8_LEN_OFFSET: i32 = std::mem::offset_of!(ListImpl, values8_len) as i32;

impl ListImpl {
    fn new(elem_size: usize, type_tag: ListTypeTag) -> Self {
        let mut list = ListImpl {
            magic: LIST_MAGIC,
            elements: Vec::new(),
            values8: Vec::new(),
            values8_ptr: std::ptr::null(),
            values8_len: 0,
            elem_size,
            type_tag,
        };
        list.sync_value_view();
        list
    }

    fn uses_value_storage(&self) -> bool {
        self.elem_size == 8
    }

    pub(crate) fn sync_value_view(&mut self) {
        if self.uses_value_storage() {
            self.values8_len = self.values8.len();
            self.values8_ptr = if self.values8_len == 0 {
                std::ptr::null()
            } else {
                self.values8.as_ptr()
            };
        } else {
            self.values8_len = 0;
            self.values8_ptr = std::ptr::null();
        }
    }

    pub(crate) fn len(&self) -> usize {
        if self.uses_value_storage() {
            self.values8.len()
        } else {
            self.elements.len()
        }
    }

    fn push(&mut self, elem: &[u8]) {
        if self.uses_value_storage() {
            self.values8
                .push(unsafe { std::ptr::read_unaligned(elem.as_ptr() as *const i64) });
            self.sync_value_view();
        } else {
            self.elements.push(elem.to_vec());
        }
    }

    fn push_value(&mut self, value: i64) {
        if self.uses_value_storage() {
            self.values8.push(value);
            self.sync_value_view();
        } else {
            let bytes = value.to_ne_bytes();
            let elem_len = self.elem_size.min(bytes.len());
            self.elements.push(bytes[..elem_len].to_vec());
        }
    }

    fn pop(&mut self) -> Option<Vec<u8>> {
        if self.uses_value_storage() {
            let popped = self.values8.pop().map(|value| value.to_ne_bytes().to_vec());
            self.sync_value_view();
            popped
        } else {
            self.elements.pop()
        }
    }

    unsafe fn get_value_unchecked(&self, index: usize) -> i64 {
        if self.uses_value_storage() {
            *self.values8.get_unchecked(index)
        } else {
            let elem = self.elements.get_unchecked(index);
            std::ptr::read_unaligned(elem.as_ptr() as *const i64)
        }
    }

    pub(crate) fn get_value(&self, index: usize) -> Option<i64> {
        if self.uses_value_storage() {
            self.values8.get(index).copied()
        } else {
            self.elements
                .get(index)
                .map(|elem| unsafe { std::ptr::read_unaligned(elem.as_ptr() as *const i64) })
        }
    }

    fn set(&mut self, index: usize, elem: &[u8]) {
        if self.uses_value_storage() {
            if index < self.values8.len() {
                self.values8[index] =
                    unsafe { std::ptr::read_unaligned(elem.as_ptr() as *const i64) };
            }
        } else if index < self.elements.len() {
            self.elements[index] = elem.to_vec();
        }
    }

    fn set_value(&mut self, index: usize, value: i64) {
        if self.uses_value_storage() {
            if index < self.values8.len() {
                self.values8[index] = value;
            }
        } else if index < self.elements.len() {
            let bytes = value.to_ne_bytes();
            let elem_len = self.elem_size.min(bytes.len());
            self.elements[index] = bytes[..elem_len].to_vec();
        }
    }

    fn remove(&mut self, index: usize) -> Option<Vec<u8>> {
        if self.uses_value_storage() {
            if index < self.values8.len() {
                let removed = Some(self.values8.remove(index).to_ne_bytes().to_vec());
                self.sync_value_view();
                removed
            } else {
                None
            }
        } else if index < self.elements.len() {
            Some(self.elements.remove(index))
        } else {
            None
        }
    }

    fn clear(&mut self) {
        if self.uses_value_storage() {
            self.values8.clear();
            self.sync_value_view();
        } else {
            self.elements.clear();
        }
    }

    fn swap(&mut self, a: usize, b: usize) {
        if self.uses_value_storage() {
            if a < self.values8.len() && b < self.values8.len() {
                self.values8.swap(a, b);
            }
        } else if a < self.elements.len() && b < self.elements.len() {
            self.elements.swap(a, b);
        }
    }
}

unsafe fn list_ref<'a>(list: PithList) -> Option<&'a ListImpl> {
    if !handle_registry::is_valid(list.ptr as *const (), HandleKind::List) {
        return None;
    }
    Some(&*(list.ptr as *const ListImpl))
}

unsafe fn list_mut<'a>(list: PithList) -> Option<&'a mut ListImpl> {
    if !handle_registry::is_valid(list.ptr as *const (), HandleKind::List) {
        return None;
    }
    Some(&mut *(list.ptr as *mut ListImpl))
}

pub(crate) unsafe fn list_ref_from_handle<'a>(handle: i64) -> Option<&'a ListImpl> {
    if !handle_registry::is_valid(handle as *const (), HandleKind::List) {
        return None;
    }
    Some(&*(handle as *const ListImpl))
}

pub(crate) unsafe fn list_mut_from_handle<'a>(handle: i64) -> Option<&'a mut ListImpl> {
    if !handle_registry::is_valid(handle as *const (), HandleKind::List) {
        return None;
    }
    Some(&mut *(handle as *mut ListImpl))
}

/// Create a new empty list
///
/// # Arguments
/// * `elem_size` - Size of each element in bytes
/// * `type_tag` - Type tag for element handling (0=primitive, 1=string, 2=list, 3=map)
/// Create a new list with default element size (8 bytes, primitive)
#[no_mangle]
pub unsafe extern "C" fn pith_list_new_default() -> PithList {
    pith_list_new(8, 0)
}

#[no_mangle]
pub unsafe extern "C" fn pith_list_new(elem_size: i64, type_tag: i32) -> PithList {
    let tag = match type_tag {
        1 => ListTypeTag::String,
        2 => ListTypeTag::List,
        3 => ListTypeTag::Map,
        _ => ListTypeTag::Primitive,
    };

    let list_impl = ListImpl::new(elem_size as usize, tag);
    let boxed = Box::new(list_impl);
    let ptr = Box::into_raw(boxed) as *mut ();
    handle_registry::register(ptr as *const (), HandleKind::List);

    PithList { ptr }
}

/// Get list length
#[no_mangle]
pub extern "C" fn pith_list_len(list: PithList) -> i64 {
    unsafe {
        list_ref(list)
            .map(|impl_ref| impl_ref.len() as i64)
            .unwrap_or(0)
    }
}

/// Check if a raw pointer looks like a ListImpl (has the magic number)
pub fn is_list_ptr(ptr: *const ()) -> bool {
    handle_registry::is_valid(ptr, HandleKind::List)
}

/// Auto-detect len: works on both lists and C strings
/// If the pointer has the list magic number, returns list length.
/// Otherwise, returns C string length (strlen).
#[no_mangle]
pub extern "C" fn pith_auto_len(ptr: i64) -> i64 {
    if ptr == 0 {
        return 0;
    }
    let raw = ptr as *const ();
    if is_list_ptr(raw) {
        let list = PithList {
            ptr: raw as *mut (),
        };
        pith_list_len(list)
    } else {
        crate::string::pith_cstring_len(ptr as *const i8)
    }
}

/// Push element to end of list
///
/// # Safety
/// * `elem` must point to valid data of size `elem_size`
#[no_mangle]
pub unsafe extern "C" fn pith_list_push(list: *mut PithList, elem: *const u8, elem_size: i64) {
    if list.is_null() || elem.is_null() {
        return;
    }

    let Some(impl_ref) = list_mut(*list) else {
        return;
    };
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_LIST_PUSHES, 1);

    // Verify element size matches
    if impl_ref.elem_size != elem_size as usize {
        eprintln!("pith: list element size mismatch");
        return;
    }

    // Copy element data
    let elem_slice = std::slice::from_raw_parts(elem, elem_size as usize);

    // Handle string elements specially (retain)
    if matches!(impl_ref.type_tag, ListTypeTag::String) {
        let s = elem as *const PithString;
        pith_string_retain(*s);
    }

    impl_ref.push(elem_slice);
}

/// Push an i64-sized value into a list using the list handle directly.
/// This is a simpler ABI used by generated method calls.
#[no_mangle]
pub unsafe extern "C" fn pith_list_push_value(list: PithList, value: i64) {
    let Some(impl_ref) = list_mut(list) else {
        return;
    };
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_LIST_PUSHES, 1);
    impl_ref.push_value(value);
}

/// Set element at index (value-based API, stores i64).
#[no_mangle]
pub unsafe extern "C" fn pith_list_set_value(list: PithList, index: i64, value: i64) {
    let Some(impl_ref) = list_mut(list) else {
        return;
    };
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_LIST_SETS, 1);
    let idx = index as usize;
    if idx >= impl_ref.len() {
        return;
    }
    impl_ref.set_value(idx, value);
}

/// Join a list of C string pointers with a separator.
/// Returns a newly allocated C string.
#[no_mangle]
pub unsafe extern "C" fn pith_list_join(list: PithList, sep: *const i8) -> *mut i8 {
    let Some(impl_ref) = list_ref(list) else {
        return std::ptr::null_mut();
    };
    if impl_ref.len() == 0 {
        return crate::pith_cstring_empty();
    }

    let sep_len = if sep.is_null() {
        0usize
    } else {
        crate::string::pith_cstring_len(sep) as usize
    };

    let mut total_len = 0usize;
    let mut i = 0usize;
    while i < impl_ref.len() {
        if let Some(raw) = impl_ref.get_value(i) {
            let ptr_val = raw as *const i8;
            if !ptr_val.is_null() {
                total_len += crate::string::pith_cstring_len(ptr_val) as usize;
            }
        }
        if i + 1 < impl_ref.len() {
            total_len += sep_len;
        }
        i += 1;
    }

    let out = crate::pith_alloc(crate::pith_layout(total_len + 1, 1)) as *mut i8;

    let mut write = 0usize;
    let mut i = 0usize;
    while i < impl_ref.len() {
        if let Some(raw) = impl_ref.get_value(i) {
            let ptr_val = raw as *const i8;
            if !ptr_val.is_null() {
                let len = crate::string::pith_cstring_len(ptr_val) as usize;
                std::ptr::copy_nonoverlapping(ptr_val as *const u8, out.add(write) as *mut u8, len);
                write += len;
            }
        }
        if i + 1 < impl_ref.len() && sep_len > 0 {
            std::ptr::copy_nonoverlapping(sep as *const u8, out.add(write) as *mut u8, sep_len);
            write += sep_len;
        }
        i += 1;
    }

    *out.add(write) = 0;
    out
}

/// Pop element from end of list
///
/// Returns true if successful, false if list is empty
#[no_mangle]
pub unsafe extern "C" fn pith_list_pop(list: *mut PithList, elem_size: i64, out: *mut u8) -> bool {
    if list.is_null() || out.is_null() {
        return false;
    }

    let Some(impl_ref) = list_mut(*list) else {
        return false;
    };

    if impl_ref.elem_size != elem_size as usize {
        eprintln!("pith: list element size mismatch");
        return false;
    }

    match impl_ref.pop() {
        Some(elem_data) => {
            // Copy to output buffer
            std::ptr::copy_nonoverlapping(elem_data.as_ptr(), out, elem_data.len());

            // Handle string elements (release the copy in the list)
            if matches!(impl_ref.type_tag, ListTypeTag::String) {
                let s = out as *const PithString;
                pith_string_release(*s);
            }

            true
        }
        None => false,
    }
}

/// Get element at index for pointer-sized elements (returns i64 directly)
/// Returns 0 if out of bounds or on error
#[no_mangle]
pub unsafe extern "C" fn pith_list_get_value(list: PithList, index: i64) -> i64 {
    let Some(impl_ref) = list_ref(list) else {
        return 0;
    };
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_LIST_GETS, 1);
    crate::perf_count(&crate::PERF_LIST_GET_VALUE_CALLS, 1);
    crate::perf_count(&crate::PERF_LIST_GET_VALUE_CHECKED_CALLS, 1);

    if index < 0 || index >= impl_ref.len() as i64 {
        return 0;
    }

    if impl_ref.elem_size != 8 {
        crate::perf_count(&crate::PERF_LIST_GET_ELEM_OTHER, 1);
        return 0;
    }

    crate::perf_count(&crate::PERF_LIST_GET_ELEM8, 1);
    impl_ref.get_value_unchecked(index as usize)
}

/// Get element at index for pointer-sized elements without an upper-bound check.
/// This is used only in compiler-generated loops that already guard the index.
#[no_mangle]
pub unsafe extern "C" fn pith_list_get_value_unchecked(list: PithList, index: i64) -> i64 {
    if index < 0 {
        return 0;
    }

    let Some(impl_ref) = list_ref(list) else {
        return 0;
    };
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_LIST_GETS, 1);
    crate::perf_count(&crate::PERF_LIST_GET_VALUE_CALLS, 1);
    crate::perf_count(&crate::PERF_LIST_GET_VALUE_UNCHECKED_CALLS, 1);

    if impl_ref.elem_size != 8 {
        crate::perf_count(&crate::PERF_LIST_GET_ELEM_OTHER, 1);
        return 0;
    }
    if index >= impl_ref.len() as i64 {
        return 0;
    }

    crate::perf_count(&crate::PERF_LIST_GET_ELEM8, 1);
    impl_ref.get_value_unchecked(index as usize)
}

/// Get element at index (copies to out buffer)
///
/// Returns true if successful, false if index out of bounds
#[no_mangle]
pub unsafe extern "C" fn pith_list_get(
    list: PithList,
    index: i64,
    elem_size: i64,
    out: *mut u8,
) -> bool {
    if out.is_null() {
        return false;
    }

    let Some(impl_ref) = list_ref(list) else {
        return false;
    };
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_LIST_GETS, 1);
    crate::perf_count(&crate::PERF_LIST_GET_BYTES_CALLS, 1);

    if index < 0 || index >= impl_ref.len() as i64 {
        return false;
    }

    if impl_ref.elem_size != elem_size as usize {
        eprintln!("pith: list element size mismatch");
        return false;
    }
    if elem_size == 8 {
        crate::perf_count(&crate::PERF_LIST_GET_ELEM8, 1);
    } else {
        crate::perf_count(&crate::PERF_LIST_GET_ELEM_OTHER, 1);
    }

    match impl_ref.get_value(index as usize) {
        Some(value) => {
            if elem_size == 8 {
                std::ptr::copy_nonoverlapping(value.to_ne_bytes().as_ptr(), out, 8);
            } else if let Some(elem_data) = impl_ref.elements.get(index as usize) {
                std::ptr::copy_nonoverlapping(elem_data.as_ptr(), out, elem_data.len());
            } else {
                return false;
            }

            // Retain string elements (caller gets a reference)
            if matches!(impl_ref.type_tag, ListTypeTag::String) {
                let s = out as *const PithString;
                pith_string_retain(*s);
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
pub unsafe extern "C" fn pith_list_set(
    list: PithList,
    index: i64,
    elem: *const u8,
    elem_size: i64,
) -> bool {
    if elem.is_null() {
        return false;
    }

    let Some(impl_ref) = list_mut(list) else {
        return false;
    };
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_LIST_SETS, 1);

    if index < 0 || index >= impl_ref.len() as i64 {
        return false;
    }

    if impl_ref.elem_size != elem_size as usize {
        eprintln!("pith: list element size mismatch");
        return false;
    }

    // Release old element if it's a heap type
    if matches!(impl_ref.type_tag, ListTypeTag::String) {
        if let Some(old_s) = impl_ref.get_value(index as usize) {
            pith_string_release(*(old_s as *const PithString));
        }
    }

    // Copy new element
    let elem_slice = std::slice::from_raw_parts(elem, elem_size as usize);

    // Retain new element
    if matches!(impl_ref.type_tag, ListTypeTag::String) {
        let s = elem as *const PithString;
        pith_string_retain(*s);
    }

    impl_ref.set(index as usize, elem_slice);
    true
}

/// Remove element at index
///
/// Returns true if successful
#[no_mangle]
pub unsafe extern "C" fn pith_list_remove(list: *mut PithList, index: i64, elem_size: i64) -> bool {
    if list.is_null() {
        return false;
    }

    let Some(impl_ref) = list_mut(*list) else {
        return false;
    };
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_LIST_REMOVES, 1);

    if index < 0 || index >= impl_ref.len() as i64 {
        return false;
    }

    if impl_ref.elem_size != elem_size as usize {
        eprintln!("pith: list element size mismatch");
        return false;
    }

    // Release element before removal
    if matches!(impl_ref.type_tag, ListTypeTag::String) {
        if let Some(s) = impl_ref.get_value(index as usize) {
            pith_string_release(*(s as *const PithString));
        }
    }

    impl_ref.remove(index as usize);
    true
}

/// Remove element at index (by-value variant — works with internal pointer)
#[no_mangle]
pub unsafe extern "C" fn pith_list_remove_value(list: PithList, index: i64) -> i64 {
    let Some(impl_ref) = list_mut(list) else {
        return 0;
    };
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_LIST_REMOVES, 1);

    if index < 0 || index >= impl_ref.len() as i64 {
        return 0;
    }

    // Release string element if needed
    if matches!(impl_ref.type_tag, ListTypeTag::String) {
        if let Some(s) = impl_ref.get_value(index as usize) {
            pith_string_release(*(s as *const PithString));
        }
    }

    impl_ref.remove(index as usize);
    1
}

/// Clear all elements from list (by-value variant)
#[no_mangle]
pub unsafe extern "C" fn pith_list_clear_value(list: PithList) {
    let Some(impl_ref) = list_mut(list) else {
        return;
    };

    if matches!(impl_ref.type_tag, ListTypeTag::String) {
        for i in 0..impl_ref.len() {
            if let Some(s) = impl_ref.get_value(i) {
                pith_string_release(*(s as *const PithString));
            }
        }
    }

    impl_ref.clear();
}

/// Reverse list in-place (by-value variant — works with internal pointer)
#[no_mangle]
pub unsafe extern "C" fn pith_list_reverse_value(list: PithList) {
    let Some(impl_ref) = list_mut(list) else {
        return;
    };
    let len = impl_ref.len();
    for i in 0..len / 2 {
        impl_ref.swap(i, len - 1 - i);
    }
}

/// Clear all elements from list
#[no_mangle]
pub unsafe extern "C" fn pith_list_clear(list: *mut PithList) {
    if list.is_null() {
        return;
    }

    let Some(impl_ref) = list_mut(*list) else {
        return;
    };

    // Release all elements if they're heap types
    if matches!(impl_ref.type_tag, ListTypeTag::String) {
        for i in 0..impl_ref.len() {
            if let Some(s) = impl_ref.get_value(i) {
                pith_string_release(*(s as *const PithString));
            }
        }
    }

    impl_ref.clear();
}

/// Reverse list elements in-place
#[no_mangle]
pub unsafe extern "C" fn pith_list_reverse(list: PithList) {
    let Some(impl_ref) = list_mut(list) else {
        return;
    };
    if impl_ref.uses_value_storage() {
        impl_ref.values8.reverse();
        impl_ref.sync_value_view();
    } else {
        impl_ref.elements.reverse();
    }
}

/// Release list and free memory
#[no_mangle]
pub unsafe extern "C" fn pith_list_release(list: PithList) {
    let Some(impl_ref) = list_mut(list) else {
        return;
    };

    // Release all elements
    if matches!(impl_ref.type_tag, ListTypeTag::String) {
        for i in 0..impl_ref.len() {
            if let Some(s) = impl_ref.get_value(i) {
                pith_string_release(*(s as *const PithString));
            }
        }
    }

    // Free the list implementation
    handle_registry::unregister(list.ptr as *const (), HandleKind::List);
    let _ = Box::from_raw(list.ptr as *mut ListImpl);
}

/// Destructor for list elements in collections
///
/// Called by cycle collector when freeing cyclic list objects
#[no_mangle]
pub extern "C" fn pith_list_destructor(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }

    unsafe {
        let list = ptr as *const PithList;
        pith_list_release(*list);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bogus_list() -> PithList {
        PithList {
            ptr: 12345usize as *mut (),
        }
    }

    #[test]
    fn invalid_list_handles_return_safe_defaults() {
        unsafe {
            assert_eq!(pith_list_len(bogus_list()), 0);
            assert_eq!(pith_list_get_value(bogus_list(), 0), 0);
            assert_eq!(pith_list_get_value_unchecked(bogus_list(), 0), 0);
            pith_list_set_value(bogus_list(), 0, 7);
            pith_list_reverse(bogus_list());
            pith_list_release(bogus_list());
        }
    }

    #[test]
    fn released_list_handles_are_rejected() {
        unsafe {
            let list = pith_list_new(8, 0);
            pith_list_push_value(list, 7);
            assert_eq!(pith_list_len(list), 1);
            pith_list_release(list);
            assert_eq!(pith_list_len(list), 0);
            assert_eq!(pith_list_get_value(list, 0), 0);
            pith_list_release(list);
        }
    }

}
