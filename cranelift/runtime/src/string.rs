//! String operations for the Forge runtime
//!
//! Hybrid approach: Idiomatic Rust internally, C-compatible FFI boundary.
//!
//! The FFI layer uses `ForgeString` structs that are compatible with the C runtime.
//! Internally, we use `std::string::String` for all operations.

use std::sync::Arc;

use std::alloc::{alloc, dealloc, Layout};

/// FFI-compatible string representation
///
/// This struct matches the layout expected by the compiler.
/// It contains a pointer to UTF-8 data, length, and heap flag.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ForgeString {
    /// Pointer to UTF-8 data (may be static literal or heap-allocated)
    pub ptr: *const u8,
    /// Length in bytes (NOT character count)
    pub len: i64,
    /// Whether this string owns heap-allocated memory
    pub is_heap: bool,
}

// SAFETY: ForgeString is immutable after creation
unsafe impl Send for ForgeString {}
unsafe impl Sync for ForgeString {}

/// Static empty string for FFI
pub static EMPTY_STRING: ForgeString = ForgeString {
    ptr: b"".as_ptr(),
    len: 0,
    is_heap: false,
};

/// Internal string representation using idiomatic Rust
///
/// Uses Arc for shared ownership and reference counting.
/// The string data is stored as Arc<str> which is immutable and thread-safe.
pub type InternalString = Arc<str>;

/// Create an internal String from a ForgeString
///
/// # Safety
/// The ForgeString must contain valid UTF-8 data
pub unsafe fn internal_from_forge(s: ForgeString) -> InternalString {
    if s.len == 0 {
        return Arc::from("");
    }

    let slice = std::slice::from_raw_parts(s.ptr, s.len as usize);
    // SAFETY: We assume the caller provides valid UTF-8
    let str_ref = std::str::from_utf8_unchecked(slice);
    Arc::from(str_ref)
}

/// Create a ForgeString from an internal String
///
/// Returns a heap-allocated ForgeString that must be released with forge_string_release
pub fn forge_from_internal(s: InternalString) -> ForgeString {
    if s.is_empty() {
        return EMPTY_STRING;
    }

    // Allocate memory and copy the string data
    let len = s.len();
    let layout = Layout::from_size_align(len, 1).unwrap();
    let ptr = unsafe { alloc(layout) };

    if ptr.is_null() {
        eprintln!("forge: out of memory");
        std::process::abort();
    }

    unsafe {
        std::ptr::copy_nonoverlapping(s.as_bytes().as_ptr(), ptr, len);
    }

    ForgeString {
        ptr,
        len: len as i64,
        is_heap: true,
    }
}

/// Create a new heap-allocated string by copying data
///
/// # Safety
/// data must be valid UTF-8
#[no_mangle]
pub unsafe extern "C" fn forge_string_new(data: *const u8, len: i64) -> ForgeString {
    if len <= 0 || data.is_null() {
        return EMPTY_STRING;
    }

    let slice = std::slice::from_raw_parts(data, len as usize);
    let s = Arc::from(std::str::from_utf8_unchecked(slice));

    forge_from_internal(s)
}

/// Create a string from a C string (null-terminated)
#[no_mangle]
pub unsafe extern "C" fn forge_string_from_cstr(cstr: *const i8) -> ForgeString {
    if cstr.is_null() {
        return EMPTY_STRING;
    }

    // Manual strlen
    let mut len = 0;
    let mut p = cstr;
    while *p != 0 {
        len += 1;
        p = p.add(1);
    }

    forge_string_new(cstr as *const u8, len)
}

/// ABI-compatible version that stores result via pointer
#[no_mangle]
pub unsafe extern "C" fn forge_string_from_cstr_ptr(cstr: *const i8, out_ptr: *mut ForgeString) {
    if out_ptr.is_null() {
        return;
    }

    let result = if cstr.is_null() {
        EMPTY_STRING
    } else {
        // Manual strlen
        let mut len = 0;
        let mut p = cstr;
        while *p != 0 {
            len += 1;
            p = p.add(1);
        }
        forge_string_new(cstr as *const u8, len)
    };

    *out_ptr = result;
}

/// Retain a string (increment reference count)
///
/// For the hybrid approach, we need to track references separately.
/// We'll use a global registry of active Arc pointers.
#[no_mangle]
pub unsafe extern "C" fn forge_string_retain(s: ForgeString) {
    if !s.is_heap || s.ptr.is_null() {
        return;
    }

    // Clone the Arc to increment reference count
    // We need to reconstruct the Arc from the raw pointer
    // This is tricky - we need to store the Arc somewhere
    // For now, we'll implement a simple reference count registry
    let _ = s; // TODO: Implement proper ARC tracking
}

/// Release a string (decrement reference count, free if zero)
#[no_mangle]
pub unsafe extern "C" fn forge_string_release(s: ForgeString) {
    if !s.is_heap || s.ptr.is_null() {
        return;
    }

    // Free the allocated memory
    let layout = Layout::from_size_align(s.len as usize, 1).unwrap();
    dealloc(s.ptr as *mut u8, layout);
}

/// Destructor for string elements in collections
///
/// Called by cycle collector when freeing cyclic string objects
#[no_mangle]
pub extern "C" fn forge_string_destructor(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }

    unsafe {
        let s = ptr as *const ForgeString;
        forge_string_release(*s);
    }
}

/// Concatenate two strings
#[no_mangle]
pub unsafe extern "C" fn forge_string_concat(a: ForgeString, b: ForgeString) -> ForgeString {
    let a_internal = internal_from_forge(a);
    let b_internal = internal_from_forge(b);

    let mut result = String::with_capacity(a_internal.len() + b_internal.len());
    result.push_str(&a_internal);
    result.push_str(&b_internal);

    forge_from_internal(Arc::from(result))
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
        let a_slice = std::slice::from_raw_parts(a.ptr, a.len as usize);
        let b_slice = std::slice::from_raw_parts(b.ptr, b.len as usize);
        a_slice == b_slice
    }
}

/// Check string inequality
#[no_mangle]
pub extern "C" fn forge_string_neq(a: ForgeString, b: ForgeString) -> bool {
    !forge_string_eq(a, b)
}

/// String less-than comparison (lexicographic)
#[no_mangle]
pub extern "C" fn forge_string_lt(a: ForgeString, b: ForgeString) -> bool {
    unsafe {
        let a_internal = internal_from_forge(a);
        let b_internal = internal_from_forge(b);
        a_internal < b_internal
    }
}

/// String greater-than comparison (lexicographic)
#[no_mangle]
pub extern "C" fn forge_string_gt(a: ForgeString, b: ForgeString) -> bool {
    unsafe {
        let a_internal = internal_from_forge(a);
        let b_internal = internal_from_forge(b);
        a_internal > b_internal
    }
}

/// String less-than-or-equal comparison (lexicographic)
#[no_mangle]
pub extern "C" fn forge_string_lte(a: ForgeString, b: ForgeString) -> bool {
    unsafe {
        let a_internal = internal_from_forge(a);
        let b_internal = internal_from_forge(b);
        a_internal <= b_internal
    }
}

/// String greater-than-or-equal comparison (lexicographic)
#[no_mangle]
pub extern "C" fn forge_string_gte(a: ForgeString, b: ForgeString) -> bool {
    unsafe {
        let a_internal = internal_from_forge(a);
        let b_internal = internal_from_forge(b);
        a_internal >= b_internal
    }
}

/// Get string length in bytes
#[no_mangle]
pub extern "C" fn forge_string_len(s: ForgeString) -> i64 {
    s.len
}

/// Create substring
#[no_mangle]
pub unsafe extern "C" fn forge_string_substring(
    s: ForgeString,
    start: i64,
    end: i64,
) -> ForgeString {
    if start < 0 || end > s.len || start >= end {
        return EMPTY_STRING;
    }

    let internal = internal_from_forge(s);
    let substr = &internal[start as usize..end as usize];

    forge_from_internal(Arc::from(substr))
}

/// Check if string contains substring
#[no_mangle]
pub extern "C" fn forge_string_contains(haystack: ForgeString, needle: ForgeString) -> bool {
    if needle.len == 0 {
        return true;
    }
    if needle.len > haystack.len {
        return false;
    }

    unsafe {
        let hay_internal = internal_from_forge(haystack);
        let needle_internal = internal_from_forge(needle);
        hay_internal.contains(&*needle_internal)
    }
}

/// Check if string starts with prefix
#[no_mangle]
pub extern "C" fn forge_string_starts_with(s: ForgeString, prefix: ForgeString) -> bool {
    if prefix.len > s.len {
        return false;
    }
    if prefix.len == 0 {
        return true;
    }

    unsafe {
        let s_internal = internal_from_forge(s);
        let p_internal = internal_from_forge(prefix);
        s_internal.starts_with(&*p_internal)
    }
}

/// Check if string ends with suffix
#[no_mangle]
pub extern "C" fn forge_string_ends_with(s: ForgeString, suffix: ForgeString) -> bool {
    if suffix.len > s.len {
        return false;
    }
    if suffix.len == 0 {
        return true;
    }

    unsafe {
        let s_internal = internal_from_forge(s);
        let suf_internal = internal_from_forge(suffix);
        s_internal.ends_with(&*suf_internal)
    }
}

/// Trim whitespace from both ends
#[no_mangle]
pub unsafe extern "C" fn forge_string_trim(s: ForgeString) -> ForgeString {
    if s.len == 0 {
        return EMPTY_STRING;
    }

    let internal = internal_from_forge(s);
    let trimmed = internal.trim();

    if trimmed.is_empty() {
        return EMPTY_STRING;
    }

    forge_from_internal(Arc::from(trimmed))
}

/// Convert to uppercase
#[no_mangle]
pub unsafe extern "C" fn forge_string_to_upper(s: ForgeString) -> ForgeString {
    if s.len == 0 {
        return EMPTY_STRING;
    }

    let internal = internal_from_forge(s);
    let upper = internal.to_uppercase();

    forge_from_internal(Arc::from(upper))
}

/// Convert to lowercase
#[no_mangle]
pub unsafe extern "C" fn forge_string_to_lower(s: ForgeString) -> ForgeString {
    if s.len == 0 {
        return EMPTY_STRING;
    }

    let internal = internal_from_forge(s);
    let lower = internal.to_lowercase();

    forge_from_internal(Arc::from(lower))
}

/// Find index of substring (returns -1 if not found)
#[no_mangle]
pub extern "C" fn forge_string_index_of(haystack: ForgeString, needle: ForgeString) -> i64 {
    if needle.len == 0 {
        return 0;
    }
    if needle.len > haystack.len {
        return -1;
    }

    unsafe {
        let hay_internal = internal_from_forge(haystack);
        let needle_internal = internal_from_forge(needle);

        match hay_internal.find(&*needle_internal) {
            Some(idx) => idx as i64,
            None => -1,
        }
    }
}

/// Find last index of substring (returns -1 if not found)
#[no_mangle]
pub extern "C" fn forge_string_last_index_of(haystack: ForgeString, needle: ForgeString) -> i64 {
    if needle.len == 0 {
        return haystack.len;
    }
    if needle.len > haystack.len {
        return -1;
    }

    unsafe {
        let hay_internal = internal_from_forge(haystack);
        let needle_internal = internal_from_forge(needle);

        match hay_internal.rfind(&*needle_internal) {
            Some(idx) => idx as i64,
            None => -1,
        }
    }
}

/// Repeat string n times
#[no_mangle]
pub unsafe extern "C" fn forge_string_repeat(s: ForgeString, n: i64) -> ForgeString {
    if n <= 0 || s.len == 0 {
        return EMPTY_STRING;
    }

    let internal = internal_from_forge(s);
    let repeated = internal.repeat(n as usize);

    forge_from_internal(Arc::from(repeated))
}

/// Replace all occurrences of old with new_s
#[no_mangle]
pub unsafe extern "C" fn forge_string_replace(
    s: ForgeString,
    old: ForgeString,
    new_s: ForgeString,
) -> ForgeString {
    if old.len == 0 || s.len == 0 {
        return forge_string_substring(s, 0, s.len); // Return copy of original
    }

    let s_internal = internal_from_forge(s);
    let old_internal = internal_from_forge(old);
    let new_internal = internal_from_forge(new_s);

    let replaced = s_internal.replace(&*old_internal, &new_internal);

    forge_from_internal(Arc::from(replaced))
}

/// Get single character at index as new string
#[no_mangle]
pub unsafe extern "C" fn forge_string_char_at(s: ForgeString, index: i64) -> ForgeString {
    if index < 0 || index >= s.len {
        return EMPTY_STRING;
    }

    // Get the byte at index (note: this is byte index, not char index)
    let byte = *s.ptr.add(index as usize);

    // Create a single-character string
    let mut buf = vec![byte];
    buf.push(0); // Null terminator for safety

    let ptr = Box::into_raw(buf.into_boxed_slice()) as *const u8;

    ForgeString {
        ptr,
        len: 1,
        is_heap: true,
    }
}

/// Create string from single character code
#[no_mangle]
pub unsafe extern "C" fn forge_chr(code: i64) -> ForgeString {
    let byte = (code & 0xFF) as u8;

    let mut buf = vec![byte];
    buf.push(0);

    let ptr = Box::into_raw(buf.into_boxed_slice()) as *const u8;

    ForgeString {
        ptr,
        len: 1,
        is_heap: true,
    }
}

/// Get character code at index (or -1 if out of bounds)
#[no_mangle]
pub extern "C" fn forge_ord(s: ForgeString, index: i64) -> i64 {
    if index < 0 || index >= s.len {
        return -1;
    }
    unsafe { *s.ptr.add(index as usize) as i64 }
}

/// Convert int to string
#[no_mangle]
pub extern "C" fn forge_int_to_string(n: i64) -> ForgeString {
    let s = Arc::from(n.to_string());
    forge_from_internal(s)
}

/// Convert uint to string  
#[no_mangle]
pub extern "C" fn forge_uint_to_string(n: u64) -> ForgeString {
    let s = Arc::from(n.to_string());
    forge_from_internal(s)
}

/// Convert float to string
#[no_mangle]
pub extern "C" fn forge_float_to_string(n: f64) -> ForgeString {
    let s = Arc::from(format!("{:.6}", n));
    forge_from_internal(s)
}

/// Convert bool to string
#[no_mangle]
pub extern "C" fn forge_bool_to_string(b: bool) -> ForgeString {
    if b {
        unsafe { forge_string_new(b"true".as_ptr(), 4) }
    } else {
        unsafe { forge_string_new(b"false".as_ptr(), 5) }
    }
}

// ============================================================================
// Simple strlen-based length (for debugging ABI issues)
// ============================================================================

/// Simple strlen-based length for null-terminated strings
#[no_mangle]
pub extern "C" fn forge_cstring_len(cstr: *const i8) -> i64 {
    if cstr.is_null() {
        return 0;
    }
    unsafe {
        let mut len = 0i64;
        let mut p = cstr;
        while *p != 0 {
            len += 1;
            p = p.add(1);
        }
        len
    }
}

/// ABI wrapper for forge_string_len - takes pointer to ForgeString
#[no_mangle]
pub extern "C" fn forge_string_len_ptr(s_ptr: *const ForgeString) -> i64 {
    if s_ptr.is_null() {
        return 0;
    }
    unsafe { (*s_ptr).len }
}

/// ABI wrapper for forge_string_contains - takes pointers to ForgeStrings
#[no_mangle]
pub extern "C" fn forge_string_contains_ptr(
    haystack_ptr: *const ForgeString,
    needle_ptr: *const ForgeString,
) -> i64 {
    if haystack_ptr.is_null() || needle_ptr.is_null() {
        return 0;
    }
    unsafe {
        let haystack = &*haystack_ptr;
        let needle = &*needle_ptr;
        if forge_string_contains(*haystack, *needle) { 1 } else { 0 }
    }
}

/// ABI wrapper for forge_string_substring - takes pointer to ForgeString, returns new ForgeString on stack
#[no_mangle]
pub unsafe extern "C" fn forge_string_substring_ptr(
    s_ptr: *const ForgeString,
    start: i64,
    end: i64,
    out_ptr: *mut ForgeString,
) {
    if s_ptr.is_null() || out_ptr.is_null() {
        return;
    }
    let s = &*s_ptr;
    let result = forge_string_substring(*s, start, end);
    *out_ptr = result;
}

/// ABI wrapper for forge_string_trim - takes pointer to ForgeString, returns new ForgeString on stack  
#[no_mangle]
pub unsafe extern "C" fn forge_string_trim_ptr(
    s_ptr: *const ForgeString,
    out_ptr: *mut ForgeString,
) {
    if s_ptr.is_null() || out_ptr.is_null() {
        return;
    }
    let s = &*s_ptr;
    let result = forge_string_trim(*s);
    *out_ptr = result;
}

/// ABI wrapper for forge_string_starts_with
#[no_mangle]
pub extern "C" fn forge_string_starts_with_ptr(
    s_ptr: *const ForgeString,
    prefix_ptr: *const ForgeString,
) -> i64 {
    if s_ptr.is_null() || prefix_ptr.is_null() {
        return 0;
    }
    unsafe { if forge_string_starts_with(*s_ptr, *prefix_ptr) { 1 } else { 0 } }
}

/// ABI wrapper for forge_string_ends_with
#[no_mangle]
pub extern "C" fn forge_string_ends_with_ptr(
    s_ptr: *const ForgeString,
    suffix_ptr: *const ForgeString,
) -> i64 {
    if s_ptr.is_null() || suffix_ptr.is_null() {
        return 0;
    }
    unsafe { if forge_string_ends_with(*s_ptr, *suffix_ptr) { 1 } else { 0 } }
}

/// ABI wrapper for forge_string_concat - returns result on stack
#[no_mangle]
pub unsafe extern "C" fn forge_string_concat_ptr(
    a_ptr: *const ForgeString,
    b_ptr: *const ForgeString,
    out_ptr: *mut ForgeString,
) {
    if a_ptr.is_null() || b_ptr.is_null() || out_ptr.is_null() {
        return;
    }
    let result = forge_string_concat(*a_ptr, *b_ptr);
    *out_ptr = result;
}
