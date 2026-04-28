//! Shared FFI helper utilities for the Pith runtime.
//!
//! These are used by json, toml, and other modules that need to
//! convert between Rust strings and C strings.

/// Convert a null-terminated C string pointer to a Rust `&str`.
///
/// # Safety
/// `s` must be either null or a valid pointer to a null-terminated string.
pub unsafe fn cstr_to_str<'a>(s: *const i8) -> &'a str {
    if s.is_null() {
        return "";
    }
    let len = crate::string::pith_cstring_len(s) as usize;
    let slice = std::slice::from_raw_parts(s as *const u8, len);
    std::str::from_utf8(slice).unwrap_or("")
}

/// Allocate a new null-terminated C string from a Rust `&str`.
///
/// # Safety
/// The caller is responsible for eventually freeing the returned pointer.
pub unsafe fn alloc_cstring(s: &str) -> *mut i8 {
    crate::pith_copy_bytes_to_cstring(s.as_bytes())
}
