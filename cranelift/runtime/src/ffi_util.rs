//! Shared FFI helper utilities for the Pith runtime.
//!
//! These are used by modules that need to convert between Rust strings
//! and C strings at the runtime FFI boundary.

/// Borrow bytes from a null-terminated C string pointer.
///
/// # Safety
/// `ptr` must be either null or a valid pointer to a null-terminated string.
/// Rust cannot validate arbitrary non-null pointers, so callers must still
/// uphold pointer validity before crossing this FFI boundary.
pub unsafe fn cstr_bytes<'a>(ptr: *const i8) -> Option<&'a [u8]> {
    if ptr.is_null() {
        return None;
    }

    let len = crate::string::pith_cstring_len(ptr) as usize;
    Some(std::slice::from_raw_parts(ptr as *const u8, len))
}

/// Borrow UTF-8 text from a null-terminated C string pointer.
///
/// # Safety
/// Same requirements as [`cstr_bytes`].
pub unsafe fn cstr_str<'a>(ptr: *const i8) -> Option<&'a str> {
    let bytes = cstr_bytes(ptr)?;
    std::str::from_utf8(bytes).ok()
}

/// Borrow UTF-8 text from a C string, defaulting to empty text on failure.
///
/// # Safety
/// Same requirements as [`cstr_bytes`].
pub unsafe fn cstr_str_or_empty<'a>(ptr: *const i8) -> &'a str {
    cstr_str(ptr).unwrap_or("")
}

/// Copy UTF-8 text from a C string into an owned Rust string.
///
/// # Safety
/// Same requirements as [`cstr_bytes`].
pub unsafe fn cstr_string(ptr: *const i8) -> Option<String> {
    Some(cstr_str(ptr)?.to_string())
}

/// Convert a null-terminated C string pointer to a Rust `&str`.
///
/// # Safety
/// Same requirements as [`cstr_bytes`].
pub unsafe fn cstr_to_str<'a>(s: *const i8) -> &'a str {
    cstr_str_or_empty(s)
}

/// Allocate a new null-terminated C string from a Rust `&str`.
///
/// # Safety
/// The caller is responsible for eventually freeing the returned pointer.
pub unsafe fn alloc_cstring(s: &str) -> *mut i8 {
    crate::pith_copy_bytes_to_cstring(s.as_bytes())
}

/// Build a fallible builtin's result box, laid out as pith's heap result
/// tuple: `[is_ok, ok, err]`. A builtin that returns one takes the `tuple`
/// retkind, so every ok value, a zero included, is a value, and the error
/// slot carries the builtin's own message rather than one the compiler
/// synthesizes.
///
/// # Safety
/// `err` must be 0 or a pith C string the box now owns.
pub unsafe fn result_box(is_ok: i64, ok: i64, err: i64) -> i64 {
    let tuple = crate::pith_struct_alloc(3) as *mut i64;
    if tuple.is_null() {
        return 0;
    }
    *tuple = is_ok;
    *tuple.add(1) = ok;
    *tuple.add(2) = err;
    tuple as i64
}

/// A result box holding `value` as its ok payload.
///
/// # Safety
/// Allocates through the pith runtime; see [`result_box`].
pub unsafe fn result_ok(value: i64) -> i64 {
    result_box(1, value, 0)
}

/// A result box holding a copy of `message` as its error.
///
/// # Safety
/// Allocates through the pith runtime; see [`result_box`].
pub unsafe fn result_err(message: &[u8]) -> i64 {
    let err = crate::pith_copy_bytes_to_cstring(message) as i64;
    result_box(0, 0, err)
}

/// The result box of a byte-count write: the count on success, and
/// `"<builtin> failed: <reason>"` on failure. The prefix is the message the
/// compiler synthesized for these builtins before they built their own box,
/// so a caller matching on it still matches.
///
/// # Safety
/// Allocates through the pith runtime; see [`result_box`].
pub unsafe fn write_result(builtin: &str, outcome: Result<usize, String>) -> i64 {
    match outcome {
        Ok(count) => result_ok(count as i64),
        Err(reason) => result_err(format!("{builtin} failed: {reason}").as_bytes()),
    }
}

/// Read a result box back for a test: the ok payload, or the error text.
///
/// # Safety
/// `tuple` must be a box `result_box` built.
#[cfg(test)]
pub(crate) unsafe fn unbox_result(tuple: i64) -> Result<i64, String> {
    assert_ne!(tuple, 0, "the result box allocation failed");
    let slots = tuple as *const i64;
    let outcome = if *slots != 0 {
        Ok(*slots.add(1))
    } else {
        let err = *slots.add(2) as *const std::os::raw::c_char;
        Err(std::ffi::CStr::from_ptr(err).to_string_lossy().into_owned())
    };
    crate::pith_struct_release(tuple);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_cstring_returns_safe_defaults() {
        unsafe {
            assert!(cstr_bytes(std::ptr::null()).is_none());
            assert!(cstr_str(std::ptr::null()).is_none());
            assert_eq!(cstr_str_or_empty(std::ptr::null()), "");
            assert!(cstr_string(std::ptr::null()).is_none());
        }
    }

    #[test]
    fn invalid_utf8_returns_safe_defaults() {
        let invalid = [0xffu8, 0x00];
        let ptr = invalid.as_ptr() as *const i8;

        unsafe {
            assert_eq!(cstr_bytes(ptr), Some(&invalid[..1]));
            assert!(cstr_str(ptr).is_none());
            assert_eq!(cstr_str_or_empty(ptr), "");
            assert!(cstr_string(ptr).is_none());
        }
    }

    #[test]
    fn valid_cstring_round_trips() {
        let valid = b"pith\0";
        let ptr = valid.as_ptr() as *const i8;

        unsafe {
            assert_eq!(cstr_bytes(ptr), Some(&valid[..4]));
            assert_eq!(cstr_str(ptr), Some("pith"));
            assert_eq!(cstr_str_or_empty(ptr), "pith");
            assert_eq!(cstr_string(ptr), Some("pith".to_string()));
        }
    }
}
