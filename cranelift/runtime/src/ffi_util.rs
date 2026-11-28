//! Shared FFI helper utilities for the Pith runtime.
//!
//! These are used by modules that need to convert between Rust strings
//! and C strings at the runtime FFI boundary.

/// Borrow bytes from a null-terminated C string pointer for the duration of `f`.
///
/// The borrow cannot escape the closure, preventing dangling references across
/// handle release or mutation.
///
/// # Safety
/// `ptr` must be either null or a valid pointer to a null-terminated string
/// that remains immutable for the duration of `f`.
#[inline(always)]
pub unsafe fn with_cstr_bytes<R>(ptr: *const i8, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
    if ptr.is_null() {
        return None;
    }
    let len = crate::string::pith_cstring_len(ptr) as usize;
    // SAFETY: Caller guarantees `ptr` points to a readable, null-terminated C string
    // of length `len` that stays immutable for the duration of `f`.
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len) };
    Some(f(bytes))
}

/// Borrow bytes from two null-terminated C string pointers for the duration of `f`.
///
/// # Safety
/// Both pointers must be null or valid pointers to null-terminated strings that
/// remain immutable for the duration of `f`.
#[inline(always)]
pub unsafe fn with_cstr_bytes2<R>(
    a: *const i8,
    b: *const i8,
    f: impl FnOnce(&[u8], &[u8]) -> R,
) -> Option<R> {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { with_cstr_bytes(a, |a_bytes| with_cstr_bytes(b, |b_bytes| f(a_bytes, b_bytes))) }
        .flatten()
}

/// Borrow UTF-8 text from a null-terminated C string pointer for the duration of `f`.
///
/// Returns `None` if `ptr` is null or not valid UTF-8.
///
/// # Safety
/// Same requirements as [`with_cstr_bytes`].
#[inline(always)]
pub unsafe fn with_cstr_str<R>(ptr: *const i8, f: impl FnOnce(&str) -> R) -> Option<R> {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { with_cstr_bytes(ptr, |bytes| std::str::from_utf8(bytes).ok().map(f)) }.flatten()
}

/// Borrow UTF-8 text from two null-terminated C string pointers for the duration of `f`.
///
/// Returns `None` if either pointer is null or not valid UTF-8.
///
/// # Safety
/// Same requirements as [`with_cstr_bytes2`].
#[inline(always)]
pub unsafe fn with_cstr_str2<R>(
    a: *const i8,
    b: *const i8,
    f: impl FnOnce(&str, &str) -> R,
) -> Option<R> {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { with_cstr_str(a, |a_str| with_cstr_str(b, |b_str| f(a_str, b_str))) }.flatten()
}

/// Borrow UTF-8 text from a C string for the duration of `f`, passing `""` on null
/// or invalid UTF-8.
///
/// # Safety
/// Same requirements as [`with_cstr_bytes`].
#[inline(always)]
pub unsafe fn with_cstr_str_or_empty<R>(ptr: *const i8, f: impl FnOnce(&str) -> R) -> R {
    if ptr.is_null() {
        return f("");
    }
    let len = crate::string::pith_cstring_len(ptr) as usize;
    // SAFETY: Caller guarantees `ptr` points to a readable, null-terminated C string
    // of length `len` that stays immutable for the duration of `f`.
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len) };
    f(std::str::from_utf8(bytes).unwrap_or(""))
}

/// Borrow raw bytes from a C string for the duration of `f`, passing `b""` on null.
///
/// # Safety
/// Same requirements as [`with_cstr_bytes`].
#[inline(always)]
pub unsafe fn with_cstr_bytes_or_empty<R>(ptr: *const i8, f: impl FnOnce(&[u8]) -> R) -> R {
    if ptr.is_null() {
        return f(&[]);
    }
    let len = crate::string::pith_cstring_len(ptr) as usize;
    // SAFETY: Caller guarantees `ptr` points to a readable, null-terminated C string
    // of length `len` that stays immutable for the duration of `f`.
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len) };
    f(bytes)
}

/// Copy UTF-8 text from a C string into an owned Rust string.
///
/// # Safety
/// Same requirements as [`with_cstr_str`].
pub unsafe fn cstr_string(ptr: *const i8) -> Option<String> {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { with_cstr_str(ptr, str::to_owned) }
}

/// Allocate a new null-terminated C string from a Rust `&str`.
///
/// The caller owns the returned pith string handle.
pub fn alloc_cstring(s: &str) -> *mut i8 {
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
pub fn result_err(message: &[u8]) -> ResultPair {
    ResultPair {
        is_ok: 0,
        payload: crate::pith_copy_bytes_to_cstring(message) as i64,
    }
}

/// The result of a byte-count write: the count on success, and
/// `"<builtin> failed: <reason>"` on failure. The prefix is the message the
/// compiler synthesized for these builtins before they reported their own
/// reason, so a caller matching on it still matches.
pub fn write_result(builtin: &str, outcome: Result<usize, String>) -> ResultPair {
    match outcome {
        Ok(count) => result_ok(count as i64),
        Err(reason) => result_err(format!("{builtin} failed: {reason}").as_bytes()),
    }
}

/// Borrow a C string's raw bytes, run a write operation `f`, and pack the
/// outcome as a [`ResultPair`].
///
/// # Safety
/// `data` must be null or a valid null-terminated C string for the duration of `f`.
#[inline]
pub unsafe fn write_cstr_result(
    builtin: &str,
    data: *const i8,
    f: impl FnOnce(&[u8]) -> Result<usize, String>,
) -> ResultPair {
    // SAFETY: Preconditions forwarded from caller.
    let outcome = unsafe { with_cstr_bytes(data, f) }
        .unwrap_or_else(|| Err("invalid string".to_string()));
    write_result(builtin, outcome)
}

/// Borrow a `Bytes` handle's slice, run a write operation `f`, and pack the
/// outcome as a [`ResultPair`].
///
/// # Safety
/// `data` is validated by [`crate::bytes::with_bytes`] before borrowing its slice.
#[inline]
pub unsafe fn write_bytes_result(
    builtin: &str,
    data: i64,
    f: impl FnOnce(&[u8]) -> Result<usize, String>,
) -> ResultPair {
    // SAFETY: `with_bytes` validates `data` before borrowing its slice.
    let outcome = unsafe { crate::bytes::with_bytes(data, f) }
        .unwrap_or_else(|| Err("invalid bytes value".to_string()));
    write_result(builtin, outcome)
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
    // SAFETY: Failed ResultPair carries an owned pith C string.
    let text = unsafe { std::ffi::CStr::from_ptr(err) }
        .to_string_lossy()
        .into_owned();
    // SAFETY: Caller transferred ownership of the error string in `pair`.
    unsafe { crate::pith_cstring_release(err) };
    Err(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A string with no runtime header, 16 zero bytes into its own buffer.
    /// `pith_cstring_len` probes the 16 bytes in front of a pointer for a
    /// header, and every string the runtime hands out has one. A bare literal
    /// has whatever static the linker put in front of it instead, and when
    /// those bytes spelled a header the probe read its length and the tests
    /// failed on one build and passed on another. Zeros there make the probe
    /// miss, so these tests always reach the byte-counting fallback.
    struct Bare(Vec<u64>);

    impl Bare {
        fn new(text: &[u8]) -> Self {
            let mut words = vec![0u64; 2 + text.len().div_ceil(8) + 1];
            // SAFETY: the buffer holds the 16 leading bytes, the text and at
            // least one more zero byte.
            unsafe {
                std::ptr::copy_nonoverlapping(text.as_ptr(), (words.as_mut_ptr() as *mut u8).add(16), text.len())
            };
            Bare(words)
        }

        fn ptr(&self) -> *const i8 {
            // SAFETY: the buffer is at least 16 bytes long.
            unsafe { (self.0.as_ptr() as *const u8).add(16) as *const i8 }
        }
    }

    #[test]
    fn null_cstring_returns_safe_defaults() {
        let null = std::ptr::null();
        assert!(unsafe { with_cstr_bytes(null, |b| b.to_vec()) }.is_none());
        assert!(unsafe { with_cstr_str(null, str::to_owned) }.is_none());
        assert_eq!(unsafe { with_cstr_str_or_empty(null, str::to_owned) }, "");
        assert!(unsafe { cstr_string(null) }.is_none());
    }

    #[test]
    fn invalid_utf8_returns_safe_defaults() {
        let invalid = [0xffu8, 0x00];
        let bare_invalid = Bare::new(&invalid[..]);
        let ptr = bare_invalid.ptr();

        assert_eq!(unsafe { with_cstr_bytes(ptr, |b| b.to_vec()) }, Some(invalid[..1].to_vec()));
        assert!(unsafe { with_cstr_str(ptr, str::to_owned) }.is_none());
        assert_eq!(unsafe { with_cstr_str_or_empty(ptr, str::to_owned) }, "");
        assert!(unsafe { cstr_string(ptr) }.is_none());
    }

    #[test]
    fn valid_cstring_round_trips() {
        let valid = b"pith\0";
        let bare_valid = Bare::new(&valid[..]);
        let ptr = bare_valid.ptr();

        assert_eq!(unsafe { with_cstr_bytes(ptr, |b| b.to_vec()) }, Some(valid[..4].to_vec()));
        assert_eq!(unsafe { with_cstr_str(ptr, str::to_owned) }, Some("pith".to_string()));
        assert_eq!(unsafe { with_cstr_str_or_empty(ptr, str::to_owned) }, "pith");
        assert_eq!(unsafe { cstr_string(ptr) }, Some("pith".to_string()));
    }

    #[test]
    fn with_cstr_bytes_handles_null_invalid_and_valid() {
        let null = std::ptr::null();
        let invalid = [0xffu8, 0xfe, 0x00];
        let bare_invalid = Bare::new(&invalid[..]);
        let empty = b"\0";
        let bare_empty = Bare::new(&empty[..]);
        let valid = b"pith\0";
        let bare_valid = Bare::new(&valid[..]);

        let mut called = false;
        assert_eq!(
            unsafe {
                with_cstr_bytes(null, |b| {
                    called = true;
                    b.len()
                })
            },
            None
        );
        assert!(!called);

        let inv_ptr = bare_invalid.ptr();
        assert_eq!(
            unsafe { with_cstr_bytes(inv_ptr, |b| b.to_vec()) },
            Some(vec![0xff, 0xfe])
        );

        let empty_ptr = bare_empty.ptr();
        assert_eq!(unsafe { with_cstr_bytes(empty_ptr, <[u8]>::len) }, Some(0));

        let valid_ptr = bare_valid.ptr();
        assert_eq!(
            unsafe { with_cstr_bytes(valid_ptr, |b| b.to_vec()) },
            Some(b"pith".to_vec())
        );
    }

    #[test]
    fn with_cstr_bytes2_handles_null_invalid_and_valid() {
        let null = std::ptr::null();
        let a = b"hello\0";
        let bare_a = Bare::new(&a[..]);
        let b = b"world\0";
        let bare_b = Bare::new(&b[..]);
        let invalid = [0x80u8, 0x00];
        let bare_invalid = Bare::new(&invalid[..]);
        let a_ptr = bare_a.ptr();
        let b_ptr = bare_b.ptr();
        let inv_ptr = bare_invalid.ptr();

        let mut called = false;
        assert_eq!(
            unsafe {
                with_cstr_bytes2(null, b_ptr, |x, y| {
                    called = true;
                    x.len() + y.len()
                })
            },
            None
        );
        assert_eq!(
            unsafe {
                with_cstr_bytes2(a_ptr, null, |x, y| {
                    called = true;
                    x.len() + y.len()
                })
            },
            None
        );
        assert_eq!(
            unsafe {
                with_cstr_bytes2(null, null, |x, y| {
                    called = true;
                    x.len() + y.len()
                })
            },
            None
        );
        assert!(!called);

        assert_eq!(
            unsafe { with_cstr_bytes2(inv_ptr, b_ptr, |x, y| (x.to_vec(), y.to_vec())) },
            Some((vec![0x80], b"world".to_vec()))
        );
        assert_eq!(
            unsafe { with_cstr_bytes2(a_ptr, b_ptr, |x, y| (x.to_vec(), y.to_vec())) },
            Some((b"hello".to_vec(), b"world".to_vec()))
        );
    }

    #[test]
    fn with_cstr_str_handles_null_invalid_and_valid() {
        let null = std::ptr::null();
        let invalid = [0xffu8, 0x00];
        let bare_invalid = Bare::new(&invalid[..]);
        let empty = b"\0";
        let bare_empty = Bare::new(&empty[..]);
        let valid = b"pith\0";
        let bare_valid = Bare::new(&valid[..]);
        let inv_ptr = bare_invalid.ptr();
        let empty_ptr = bare_empty.ptr();
        let valid_ptr = bare_valid.ptr();

        let mut called = false;
        assert_eq!(
            unsafe {
                with_cstr_str(null, |s| {
                    called = true;
                    s.len()
                })
            },
            None
        );
        assert_eq!(
            unsafe {
                with_cstr_str(inv_ptr, |s| {
                    called = true;
                    s.len()
                })
            },
            None
        );
        assert!(!called);

        assert_eq!(
            unsafe { with_cstr_str(empty_ptr, str::to_owned) },
            Some(String::new())
        );
        assert_eq!(
            unsafe { with_cstr_str(valid_ptr, str::to_owned) },
            Some("pith".to_string())
        );
    }

    #[test]
    fn with_cstr_str2_handles_null_invalid_and_valid() {
        let null = std::ptr::null();
        let invalid = [0xffu8, 0x00];
        let bare_invalid = Bare::new(&invalid[..]);
        let a = b"foo\0";
        let bare_a = Bare::new(&a[..]);
        let b = b"bar\0";
        let bare_b = Bare::new(&b[..]);
        let inv_ptr = bare_invalid.ptr();
        let a_ptr = bare_a.ptr();
        let b_ptr = bare_b.ptr();

        let mut called = false;
        for (lhs, rhs) in [
            (null, b_ptr),
            (a_ptr, null),
            (null, null),
            (inv_ptr, b_ptr),
            (a_ptr, inv_ptr),
        ] {
            assert_eq!(
                unsafe {
                    with_cstr_str2(lhs, rhs, |x, y| {
                        called = true;
                        format!("{x}:{y}")
                    })
                },
                None
            );
        }
        assert!(!called);

        assert_eq!(
            unsafe { with_cstr_str2(a_ptr, b_ptr, |x, y| format!("{x}:{y}")) },
            Some("foo:bar".to_string())
        );
    }

    #[test]
    fn with_cstr_str_or_empty_handles_null_invalid_and_valid() {
        let null = std::ptr::null();
        let invalid = [0xffu8, 0x00];
        let bare_invalid = Bare::new(&invalid[..]);
        let empty = b"\0";
        let bare_empty = Bare::new(&empty[..]);
        let valid = b"pith\0";
        let bare_valid = Bare::new(&valid[..]);

        assert_eq!(unsafe { with_cstr_str_or_empty(null, str::to_owned) }, "");
        assert_eq!(
            unsafe { with_cstr_str_or_empty(bare_invalid.ptr(), str::to_owned) },
            ""
        );
        assert_eq!(
            unsafe { with_cstr_str_or_empty(bare_empty.ptr(), str::to_owned) },
            ""
        );
        assert_eq!(
            unsafe { with_cstr_str_or_empty(bare_valid.ptr(), str::to_owned) },
            "pith"
        );
    }
}
