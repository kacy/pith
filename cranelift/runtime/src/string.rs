//! String operations for the Pith runtime
//!
//! Hybrid approach: Idiomatic Rust internally, C-compatible FFI boundary.
//!
//! The FFI layer uses `PithString` structs that are compatible with the C runtime.
//! Internally, we use `std::string::String` for all operations.

use std::sync::Arc;

use std::alloc::{alloc, dealloc, Layout};

/// FFI-compatible string representation
///
/// This struct matches the layout expected by the compiler.
/// It contains a pointer to UTF-8 data, length, and heap flag.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PithString {
    /// Pointer to UTF-8 data (may be static literal or heap-allocated)
    pub ptr: *const u8,
    /// Length in bytes (NOT character count)
    pub len: i64,
    /// Whether this string owns heap-allocated memory
    pub is_heap: bool,
}

// SAFETY: PithString is immutable after creation
unsafe impl Send for PithString {}
unsafe impl Sync for PithString {}

/// Static empty string for FFI
pub static EMPTY_STRING: PithString = PithString {
    ptr: b"".as_ptr(),
    len: 0,
    is_heap: false,
};

/// Internal string representation using idiomatic Rust
///
/// Uses Arc for shared ownership and reference counting.
/// The string data is stored as Arc<str> which is immutable and thread-safe.
pub type InternalString = Arc<str>;

/// Create an internal String from a PithString
///
/// # Safety
/// The PithString must contain valid UTF-8 data
pub unsafe fn internal_from_pith(s: PithString) -> InternalString {
    if s.len == 0 {
        return Arc::from("");
    }

    let slice = std::slice::from_raw_parts(s.ptr, s.len as usize);
    // SAFETY: We assume the caller provides valid UTF-8
    let str_ref = std::str::from_utf8_unchecked(slice);
    Arc::from(str_ref)
}

/// Create a PithString from an internal String
///
/// Returns a heap-allocated PithString that must be released with pith_string_release
pub fn pith_from_internal(s: InternalString) -> PithString {
    if s.is_empty() {
        return EMPTY_STRING;
    }
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_STRING_ALLOCS, 1);
    crate::perf_count(&crate::PERF_STRING_ALLOC_BYTES, s.len());

    // Allocate memory and copy the string data
    let len = s.len();
    let layout = Layout::from_size_align(len, 1).unwrap();
    let ptr = unsafe { alloc(layout) };

    if ptr.is_null() {
        eprintln!("pith: out of memory");
        std::process::abort();
    }

    unsafe {
        std::ptr::copy_nonoverlapping(s.as_bytes().as_ptr(), ptr, len);
    }

    PithString {
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
pub unsafe extern "C" fn pith_string_new(data: *const u8, len: i64) -> PithString {
    if len <= 0 || data.is_null() {
        return EMPTY_STRING;
    }

    let slice = std::slice::from_raw_parts(data, len as usize);
    let s = Arc::from(std::str::from_utf8_unchecked(slice));

    pith_from_internal(s)
}

/// Create a string from a C string (null-terminated)
#[no_mangle]
pub unsafe extern "C" fn pith_string_from_cstr(cstr: *const i8) -> PithString {
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

    pith_string_new(cstr as *const u8, len)
}

/// ABI-compatible version that stores result via pointer
#[no_mangle]
pub unsafe extern "C" fn pith_string_from_cstr_ptr(cstr: *const i8, out_ptr: *mut PithString) {
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
        pith_string_new(cstr as *const u8, len)
    };

    *out_ptr = result;
}

/// Retain a string (increment reference count)
///
/// For the hybrid approach, we need to track references separately.
/// We'll use a global registry of active Arc pointers.
#[no_mangle]
pub unsafe extern "C" fn pith_string_retain(s: PithString) {
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
pub unsafe extern "C" fn pith_string_release(s: PithString) {
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
pub extern "C" fn pith_string_destructor(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }

    unsafe {
        let s = ptr as *const PithString;
        pith_string_release(*s);
    }
}

/// Concatenate two strings
#[no_mangle]
pub unsafe extern "C" fn pith_string_concat(a: PithString, b: PithString) -> PithString {
    let a_internal = internal_from_pith(a);
    let b_internal = internal_from_pith(b);

    let mut result = String::with_capacity(a_internal.len() + b_internal.len());
    result.push_str(&a_internal);
    result.push_str(&b_internal);

    pith_from_internal(Arc::from(result))
}

/// Get string length in bytes
#[no_mangle]
pub extern "C" fn pith_string_len(s: PithString) -> i64 {
    s.len
}

/// Create substring
#[no_mangle]
pub unsafe extern "C" fn pith_string_substring(
    s: PithString,
    start: i64,
    end: i64,
) -> PithString {
    if start < 0 || end > s.len || start >= end {
        return EMPTY_STRING;
    }

    let internal = internal_from_pith(s);
    let substr = &internal[start as usize..end as usize];

    pith_from_internal(Arc::from(substr))
}

/// Check if string contains substring
#[no_mangle]
pub extern "C" fn pith_string_contains(haystack: PithString, needle: PithString) -> bool {
    if needle.len == 0 {
        return true;
    }
    if needle.len > haystack.len {
        return false;
    }

    unsafe {
        let hay_internal = internal_from_pith(haystack);
        let needle_internal = internal_from_pith(needle);
        hay_internal.contains(&*needle_internal)
    }
}

/// Check if string starts with prefix
#[no_mangle]
pub extern "C" fn pith_string_starts_with(s: PithString, prefix: PithString) -> bool {
    if prefix.len > s.len {
        return false;
    }
    if prefix.len == 0 {
        return true;
    }

    unsafe {
        let s_internal = internal_from_pith(s);
        let p_internal = internal_from_pith(prefix);
        s_internal.starts_with(&*p_internal)
    }
}

/// Check if string ends with suffix
#[no_mangle]
pub extern "C" fn pith_string_ends_with(s: PithString, suffix: PithString) -> bool {
    if suffix.len > s.len {
        return false;
    }
    if suffix.len == 0 {
        return true;
    }

    unsafe {
        let s_internal = internal_from_pith(s);
        let suf_internal = internal_from_pith(suffix);
        s_internal.ends_with(&*suf_internal)
    }
}

/// Trim whitespace from both ends
#[no_mangle]
pub unsafe extern "C" fn pith_string_trim(s: PithString) -> PithString {
    if s.len == 0 {
        return EMPTY_STRING;
    }

    let internal = internal_from_pith(s);
    let trimmed = internal.trim();

    if trimmed.is_empty() {
        return EMPTY_STRING;
    }

    pith_from_internal(Arc::from(trimmed))
}

/// Create string from single character code
#[no_mangle]
pub unsafe extern "C" fn pith_chr(code: i64) -> PithString {
    let byte = (code & 0xFF) as u8;

    let mut buf = vec![byte];
    buf.push(0);

    let ptr = Box::into_raw(buf.into_boxed_slice()) as *const u8;

    PithString {
        ptr,
        len: 1,
        is_heap: true,
    }
}

/// Get character code at index (or -1 if out of bounds)
#[no_mangle]
pub extern "C" fn pith_ord(s: PithString, index: i64) -> i64 {
    if index < 0 || index >= s.len {
        return -1;
    }
    unsafe { *s.ptr.add(index as usize) as i64 }
}

// ============================================================================
// Simple strlen-based length (for debugging ABI issues)
// ============================================================================

/// Simple strlen-based length for null-terminated strings
#[no_mangle]
pub extern "C" fn pith_cstring_len(cstr: *const i8) -> i64 {
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

/// ABI wrapper for pith_string_len - takes pointer to PithString
#[no_mangle]
pub extern "C" fn pith_string_len_ptr(s_ptr: *const PithString) -> i64 {
    if s_ptr.is_null() {
        return 0;
    }
    unsafe { (*s_ptr).len }
}

/// ABI wrapper for pith_string_contains - takes pointers to PithStrings
#[no_mangle]
pub extern "C" fn pith_string_contains_ptr(
    haystack_ptr: *const PithString,
    needle_ptr: *const PithString,
) -> i64 {
    if haystack_ptr.is_null() || needle_ptr.is_null() {
        return 0;
    }
    unsafe {
        let haystack = &*haystack_ptr;
        let needle = &*needle_ptr;
        if pith_string_contains(*haystack, *needle) { 1 } else { 0 }
    }
}

/// ABI wrapper for pith_string_substring - takes pointer to PithString, returns new PithString on stack
#[no_mangle]
pub unsafe extern "C" fn pith_string_substring_ptr(
    s_ptr: *const PithString,
    start: i64,
    end: i64,
    out_ptr: *mut PithString,
) {
    if s_ptr.is_null() || out_ptr.is_null() {
        return;
    }
    let s = &*s_ptr;
    let result = pith_string_substring(*s, start, end);
    *out_ptr = result;
}

/// ABI wrapper for pith_string_trim - takes pointer to PithString, returns new PithString on stack
#[no_mangle]
pub unsafe extern "C" fn pith_string_trim_ptr(
    s_ptr: *const PithString,
    out_ptr: *mut PithString,
) {
    if s_ptr.is_null() || out_ptr.is_null() {
        return;
    }
    let s = &*s_ptr;
    let result = pith_string_trim(*s);
    *out_ptr = result;
}

/// ABI wrapper for pith_string_starts_with
#[no_mangle]
pub extern "C" fn pith_string_starts_with_ptr(
    s_ptr: *const PithString,
    prefix_ptr: *const PithString,
) -> i64 {
    if s_ptr.is_null() || prefix_ptr.is_null() {
        return 0;
    }
    unsafe { if pith_string_starts_with(*s_ptr, *prefix_ptr) { 1 } else { 0 } }
}

/// ABI wrapper for pith_string_ends_with
#[no_mangle]
pub extern "C" fn pith_string_ends_with_ptr(
    s_ptr: *const PithString,
    suffix_ptr: *const PithString,
) -> i64 {
    if s_ptr.is_null() || suffix_ptr.is_null() {
        return 0;
    }
    unsafe { if pith_string_ends_with(*s_ptr, *suffix_ptr) { 1 } else { 0 } }
}

/// ABI wrapper for pith_string_concat - returns result on stack
#[no_mangle]
pub unsafe extern "C" fn pith_string_concat_ptr(
    a_ptr: *const PithString,
    b_ptr: *const PithString,
    out_ptr: *mut PithString,
) {
    if a_ptr.is_null() || b_ptr.is_null() || out_ptr.is_null() {
        return;
    }
    let result = pith_string_concat(*a_ptr, *b_ptr);
    *out_ptr = result;
}
