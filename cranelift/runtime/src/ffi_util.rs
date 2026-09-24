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

/// A fallible builtin's result as two machine words: the ok flag, and one
/// payload that is the ok value when `is_ok` is 1 and an owned pith C string
/// (the error message) when it is 0. A builtin that returns one gets the
/// `rcall` form, so every ok value, a zero included, is a value, and the
/// error carries the builtin's own message rather than one the compiler
/// synthesizes.
///
/// A `#[repr(C)]` struct of two `i64`s comes back in two registers on every
/// target the compiler supports: `rax:rdx` under the x86-64 System V ABI and
/// `x0:x1` under AAPCS64, Apple's variant included. Those are the registers a
/// Cranelift signature with two `I64` returns reads, so these functions are
/// declared with `I64,I64` returns in `runtime_functions.txt` and no heap
/// result box exists on either path. A float payload travels as its bit
/// pattern in the second word rather than as an `f64`: AAPCS64 returns a
/// mixed `{i64, f64}` struct in `x0:x1`, where a Cranelift `(I64, F64)`
/// signature would read `x0` and `v0`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResultPair {
    pub is_ok: i64,
    pub payload: i64,
}

// the two-register return depends on the struct being exactly two words.
const _: () = assert!(std::mem::size_of::<ResultPair>() == 16);

/// A successful result carrying `value`.
pub fn result_ok(value: i64) -> ResultPair {
    ResultPair {
        is_ok: 1,
        payload: value,
    }
}

/// A failed result carrying a copy of `message`, which the caller then owns.
///
/// # Safety
/// Allocates the message through the pith runtime.
pub unsafe fn result_err(message: &[u8]) -> ResultPair {
    ResultPair {
        is_ok: 0,
        payload: crate::pith_copy_bytes_to_cstring(message) as i64,
    }
}

/// The result of a byte-count write: the count on success, and
/// `"<builtin> failed: <reason>"` on failure. The prefix is the message the
/// compiler synthesized for these builtins before they reported their own
/// reason, so a caller matching on it still matches.
///
/// # Safety
/// Allocates the error message through the pith runtime; see [`result_err`].
pub unsafe fn write_result(builtin: &str, outcome: Result<usize, String>) -> ResultPair {
    match outcome {
        Ok(count) => result_ok(count as i64),
        Err(reason) => result_err(format!("{builtin} failed: {reason}").as_bytes()),
    }
}

/// Read a result back for a test: the ok payload, or the error text. The
/// error string is released here, as the compiled caller releases it.
///
/// # Safety
/// `pair` must come from one of the builtins above.
#[cfg(test)]
pub(crate) unsafe fn unbox_result(pair: ResultPair) -> Result<i64, String> {
    if pair.is_ok != 0 {
        return Ok(pair.payload);
    }
    let err = pair.payload as *const std::os::raw::c_char;
    assert!(!err.is_null(), "a failed result carries no message");
    let text = std::ffi::CStr::from_ptr(err).to_string_lossy().into_owned();
    crate::pith_cstring_release(err);
    Err(text)
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
