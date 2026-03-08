//! String operations for the Forge runtime
//!
//! Forge strings are immutable, length-prefixed, and reference-counted.
//! They use the following layout:
//! ```
//! [RC Header][forge_string_t: { ptr, len, is_heap }][String Data...]
//! ```

use crate::arc::{forge_rc_alloc, forge_rc_release, forge_rc_retain, TypeTag};
use std::slice;

/// Forge string representation - compatible with C struct
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ForgeString {
    /// Pointer to UTF-8 data (may be static or heap-allocated)
    pub ptr: *const u8,
    /// Length in bytes (NOT character count)
    pub len: i64,
    /// Whether this string is heap-allocated and needs RC
    pub is_heap: bool,
}

// SAFETY: ForgeString is immutable after creation, so it's safe to share between threads
unsafe impl Send for ForgeString {}
unsafe impl Sync for ForgeString {}

/// Static empty string
pub static EMPTY_STRING: ForgeString = ForgeString {
    ptr: b"".as_ptr(),
    len: 0,
    is_heap: false,
};

/// Create a new heap-allocated string by copying data
/// 
/// # Safety
/// data must be valid UTF-8
#[no_mangle]
pub unsafe extern "C" fn forge_string_new(data: *const u8, len: i64) -> ForgeString {
    if len <= 0 {
        return EMPTY_STRING;
    }
    
    // Allocate with RC header
    let size = len as usize + std::mem::size_of::<ForgeString>();
    let mem = forge_rc_alloc(size, TypeTag::String as u32);
    
    if mem.is_null() {
        return EMPTY_STRING;
    }
    
    // Copy data
    std::ptr::copy_nonoverlapping(data, mem.add(std::mem::size_of::<ForgeString>()), len as usize);
    
    // Create the string struct inline
    let str_ptr = mem as *mut ForgeString;
    (*str_ptr).ptr = mem.add(std::mem::size_of::<ForgeString>());
    (*str_ptr).len = len;
    (*str_ptr).is_heap = true;
    
    *str_ptr
}

/// Create a string from a C string (null-terminated)
#[no_mangle]
pub unsafe extern "C" fn forge_string_from_cstr(cstr: *const i8) -> ForgeString {
    if cstr.is_null() {
        return EMPTY_STRING;
    }
    
    let len = strlen(cstr);
    forge_string_new(cstr as *const u8, len as i64)
}

/// Retain a string (increment RC if heap-allocated)
#[no_mangle]
pub unsafe extern "C" fn forge_string_retain(s: ForgeString) {
    if s.is_heap && !s.ptr.is_null() {
        // Get pointer to RC header via the string struct location
        let str_struct_ptr = (s.ptr as *mut u8).sub(std::mem::size_of::<ForgeString>()) as *mut ForgeString;
        forge_rc_retain(str_struct_ptr as *mut u8);
    }
}

/// Release a string (decrement RC, free if zero)
#[no_mangle]
pub unsafe extern "C" fn forge_string_release(s: ForgeString) {
    if !s.is_heap || s.ptr.is_null() {
        return;
    }
    
    // Get pointer to the inline ForgeString struct
    let str_struct_ptr = (s.ptr as *mut u8).sub(std::mem::size_of::<ForgeString>()) as *mut ForgeString;
    
    // Release with custom destructor
    forge_rc_release(str_struct_ptr as *mut u8, Some(forge_string_destructor));
}

/// Destructor for string memory
extern "C" fn forge_string_destructor(ptr: *mut u8) {
    // Nothing special needed - the memory is freed by arc::forge_rc_release
    let _ = ptr;
}

/// Concatenate two strings
#[no_mangle]
pub unsafe extern "C" fn forge_string_concat(a: ForgeString, b: ForgeString) -> ForgeString {
    let new_len = a.len + b.len;
    if new_len == 0 {
        return EMPTY_STRING;
    }
    
    let size = new_len as usize + std::mem::size_of::<ForgeString>();
    let mem = forge_rc_alloc(size, TypeTag::String as u32);
    
    // Copy both strings
    let data_ptr = mem.add(std::mem::size_of::<ForgeString>());
    std::ptr::copy_nonoverlapping(a.ptr, data_ptr, a.len as usize);
    std::ptr::copy_nonoverlapping(b.ptr, data_ptr.add(a.len as usize), b.len as usize);
    
    // Create string struct inline
    let str_ptr = mem as *mut ForgeString;
    (*str_ptr).ptr = data_ptr;
    (*str_ptr).len = new_len;
    (*str_ptr).is_heap = true;
    
    *str_ptr
}

/// Check string equality
#[no_mangle]
pub extern "C" fn forge_string_eq(a: ForgeString, b: ForgeString) -> bool {
    if a.len != b.len {
        return false;
    }
    if a.len == 0 {
        return true;
    }
    unsafe {
        let a_slice = slice::from_raw_parts(a.ptr, a.len as usize);
        let b_slice = slice::from_raw_parts(b.ptr, b.len as usize);
        a_slice == b_slice
    }
}

/// Get string length in bytes
#[no_mangle]
pub extern "C" fn forge_string_len(s: ForgeString) -> i64 {
    s.len
}

/// Create substring
#[no_mangle]
pub unsafe extern "C" fn forge_string_substring(s: ForgeString, start: i64, end: i64) -> ForgeString {
    if start < 0 || end > s.len || start >= end {
        return EMPTY_STRING;
    }
    
    let new_len = end - start;
    let size = new_len as usize + std::mem::size_of::<ForgeString>();
    let mem = forge_rc_alloc(size, TypeTag::String as u32);
    
    let data_ptr = mem.add(std::mem::size_of::<ForgeString>());
    std::ptr::copy_nonoverlapping(s.ptr.add(start as usize), data_ptr, new_len as usize);
    
    let str_ptr = mem as *mut ForgeString;
    (*str_ptr).ptr = data_ptr;
    (*str_ptr).len = new_len;
    (*str_ptr).is_heap = true;
    
    *str_ptr
}

// Add libc dependency for strlen
extern "C" {
    fn strlen(s: *const i8) -> usize;
}
