//! String operations for the Pith runtime
//!
//! Hybrid approach: Idiomatic Rust internally, C-compatible FFI boundary.
//!
//! The FFI layer uses `PithString` structs that are compatible with the C runtime.
//! Internally, we use `std::string::String` for all operations.

use std::alloc::dealloc;

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

/// Borrow a PithString's bytes as &str without copying.
///
/// Falls back to "" on invalid utf-8, matching what the old arc round-trip
/// did. The runtime only constructs valid utf-8, so the check is a guard,
/// not a code path programs rely on.
///
/// # Safety
/// The PithString's ptr/len must describe live memory.
unsafe fn as_str<'a>(s: &PithString) -> &'a str {
    if s.len <= 0 || s.ptr.is_null() {
        return "";
    }
    let slice = std::slice::from_raw_parts(s.ptr, s.len as usize);
    std::str::from_utf8(slice).unwrap_or("")
}

/// Allocate one runtime buffer and copy `text` into it.
///
/// This is the single place a derived string touches the allocator. The old
/// path went text → Arc<str> → runtime buffer, copying everything twice.
pub fn pith_from_str(text: &str) -> PithString {
    if text.is_empty() {
        return EMPTY_STRING;
    }
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_STRING_ALLOCS, 1);
    crate::perf_count(&crate::PERF_STRING_ALLOC_BYTES, text.len());

    let len = text.len();
    let layout = crate::pith_layout(len, 1);
    let ptr = unsafe { crate::pith_alloc(layout) };
    unsafe {
        std::ptr::copy_nonoverlapping(text.as_bytes().as_ptr(), ptr, len);
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
    match std::str::from_utf8(slice) {
        Ok(text) => pith_from_str(text),
        Err(_) => EMPTY_STRING,
    }
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

/// Retain a string.
///
/// Heap strings use copy-on-derive ownership: every operation that produces a
/// string (`pith_from_str`, concat, substring, trim, ...) allocates and owns
/// its own buffer, so two live `PithString`s never share one allocation.
/// With no shared buffer there is no reference count to bump, so retain is a
/// no-op under the current model.
///
/// The symbol is kept so the retain/release pair stays symmetric. If strings
/// ever move to shared ownership, the count increment belongs here — and
/// `pith_from_str` and `pith_string_release` would need a matching count
/// header.
#[no_mangle]
pub unsafe extern "C" fn pith_string_retain(s: PithString) {
    let _ = s;
}

/// Release a string (decrement reference count, free if zero)
#[no_mangle]
pub unsafe extern "C" fn pith_string_release(s: PithString) {
    if !s.is_heap || s.ptr.is_null() {
        return;
    }

    // Free the allocated memory
    let layout = crate::pith_layout(s.len as usize, 1);
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

/// Concatenate two strings into one fresh allocation
#[no_mangle]
pub unsafe extern "C" fn pith_string_concat(a: PithString, b: PithString) -> PithString {
    let a_str = as_str(&a);
    let b_str = as_str(&b);
    if a_str.is_empty() {
        return pith_from_str(b_str);
    }
    if b_str.is_empty() {
        return pith_from_str(a_str);
    }

    let len = a_str.len() + b_str.len();
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_STRING_ALLOCS, 1);
    crate::perf_count(&crate::PERF_STRING_ALLOC_BYTES, len);

    let layout = crate::pith_layout(len, 1);
    let ptr = crate::pith_alloc(layout);
    std::ptr::copy_nonoverlapping(a_str.as_ptr(), ptr, a_str.len());
    std::ptr::copy_nonoverlapping(b_str.as_ptr(), ptr.add(a_str.len()), b_str.len());

    PithString {
        ptr,
        len: len as i64,
        is_heap: true,
    }
}

/// Get string length in bytes
#[no_mangle]
pub extern "C" fn pith_string_len(s: PithString) -> i64 {
    s.len
}

/// Create substring
#[no_mangle]
pub unsafe extern "C" fn pith_string_substring(s: PithString, start: i64, end: i64) -> PithString {
    if start < 0 || end > s.len || start >= end {
        return EMPTY_STRING;
    }

    // .get() rather than slice syntax: a range that lands inside a utf-8
    // sequence yields "" instead of a panic.
    match as_str(&s).get(start as usize..end as usize) {
        Some(substr) => pith_from_str(substr),
        None => EMPTY_STRING,
    }
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

    unsafe { as_str(&haystack).contains(as_str(&needle)) }
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

    unsafe { as_str(&s).starts_with(as_str(&prefix)) }
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

    unsafe { as_str(&s).ends_with(as_str(&suffix)) }
}

/// Trim whitespace from both ends
#[no_mangle]
pub unsafe extern "C" fn pith_string_trim(s: PithString) -> PithString {
    if s.len == 0 {
        return EMPTY_STRING;
    }

    pith_from_str(as_str(&s).trim())
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
        if pith_string_contains(*haystack, *needle) {
            1
        } else {
            0
        }
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
pub unsafe extern "C" fn pith_string_trim_ptr(s_ptr: *const PithString, out_ptr: *mut PithString) {
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
    unsafe {
        if pith_string_starts_with(*s_ptr, *prefix_ptr) {
            1
        } else {
            0
        }
    }
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
    unsafe {
        if pith_string_ends_with(*s_ptr, *suffix_ptr) {
            1
        } else {
            0
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    // a heap string owns its buffer; retain leaves it untouched and a single
    // release frees it. this is the retain/release contract under copy-on-derive
    // ownership — one retain, one release, no double free.
    #[test]
    fn retain_keeps_the_buffer_intact_then_release_frees() {
        unsafe {
            let data = b"hello";
            let s = pith_string_new(data.as_ptr(), data.len() as i64);
            assert!(s.is_heap);
            assert_eq!(s.len, 5);

            pith_string_retain(s);

            // retain must not move or corrupt the bytes.
            let bytes = std::slice::from_raw_parts(s.ptr, s.len as usize);
            assert_eq!(bytes, b"hello");

            pith_string_release(s);
        }
    }

    // non-heap strings (static literals, the empty string) own nothing, so both
    // calls must be safe no-ops.
    #[test]
    fn retain_and_release_ignore_non_heap_strings() {
        unsafe {
            pith_string_retain(EMPTY_STRING);
            pith_string_release(EMPTY_STRING);
        }
    }
}
