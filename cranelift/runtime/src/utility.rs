//! Lightweight utility builtins (`pith_log_*`, `pith_fnv1a`) and legacy alias
//! shims (`pith_fs_write`, `pith_float_fixed`, `pith_is_dir`).

use crate::ffi_util::{with_cstr_bytes, with_cstr_str_or_empty};

pub use crate::encoding::pith_b64_decode;
pub use crate::platform::pith_format_time_fmt;
pub use crate::process::pith_exec_output;
pub use crate::string_search::{
    pith_cstring_contains, pith_cstring_ends_with, pith_cstring_index_of,
    pith_cstring_index_of_any, pith_cstring_pad_left, pith_cstring_pad_right, pith_cstring_repeat,
    pith_cstring_starts_with,
};

/// Write string to file path
/// Returns 1 on success, 0 on failure
///
/// # Safety
/// Both pointers must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn pith_fs_write(path: *const i8, content: *const i8) -> i64 {
    // SAFETY: Both pointers are null or valid null-terminated C strings.
    unsafe { crate::host_fs::pith_write_file(path, content) }
}

#[no_mangle]
pub unsafe extern "C" fn pith_log_info(msg: *const i8) {
    // SAFETY: `msg` is null or a valid null-terminated C string.
    unsafe { with_cstr_str_or_empty(msg, |s| eprintln!("[INFO] {s}")) };
}

#[no_mangle]
pub unsafe extern "C" fn pith_log_warn(msg: *const i8) {
    // SAFETY: `msg` is null or a valid null-terminated C string.
    unsafe { with_cstr_str_or_empty(msg, |s| eprintln!("[WARN] {s}")) };
}

#[no_mangle]
pub unsafe extern "C" fn pith_log_error(msg: *const i8) {
    // SAFETY: `msg` is null or a valid null-terminated C string.
    unsafe { with_cstr_str_or_empty(msg, |s| eprintln!("[ERROR] {s}")) };
}

fn fnv1a_impl(bytes: &[u8]) -> i64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_fnv1a(s: *const i8) -> i64 {
    // SAFETY: `s` is null or a valid null-terminated C string for this call.
    unsafe { with_cstr_bytes(s, fnv1a_impl) }.unwrap_or(0)
}

#[no_mangle]
pub unsafe extern "C" fn pith_float_fixed(value: f64, decimals: i64) -> *mut i8 {
    // SAFETY: Delegates directly to `pith_fmt_float`.
    unsafe { crate::platform::pith_fmt_float(value, decimals) }
}

#[no_mangle]
pub unsafe extern "C" fn pith_is_dir(path: i64) -> i64 {
    // SAFETY: `path` is null or a valid null-terminated C string.
    unsafe { crate::host_fs::pith_dir_exists(path as *const i8) }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::format_time_utc;
    use crate::string_search::find_any_byte;

    #[test]
    fn format_time_handles_the_strftime_subset() {
        let f = |ms: i64, fmt: &str| format_time_utc(ms, fmt.as_bytes());
        assert_eq!(f(0, "%Y-%m-%d %H:%M:%S"), "1970-01-01 00:00:00");
        // 2024-02-29 13:45:07.089 utc, a leap day
        let ms = 1_709_214_307_089;
        assert_eq!(f(ms, "%Y-%m-%d %H:%M:%S"), "2024-02-29 13:45:07");
        assert_eq!(f(ms, "%FT%T.%3fZ"), "2024-02-29T13:45:07.089Z");
        assert_eq!(f(ms, "100%% at %H"), "100% at 13");
        assert_eq!(f(ms, ""), "");
        assert_eq!(f(ms, "%q %"), "%q %");
        assert_eq!(f(ms, "zeit é %Y"), "zeit é 2024");
        // before the epoch: one millisecond earlier is the last moment of 1969
        assert_eq!(f(-1, "%F %T.%3f"), "1969-12-31 23:59:59.999");
        assert_eq!(f(951_782_400_000, "%F"), "2000-02-29");
        assert_eq!(f(4_107_542_400_000, "%F"), "2100-03-01");
        unsafe {
            let p = pith_format_time_fmt(0, b"%F\0".as_ptr() as *const i8);
            assert_eq!(std::ffi::CStr::from_ptr(p).to_str().unwrap(), "1970-01-01");
            let p = pith_format_time_fmt(0, std::ptr::null());
            assert_eq!(std::ffi::CStr::from_ptr(p).to_str().unwrap(), "");
        }
    }

    /// The word-at-a-time search has to agree with the obvious loop for every
    /// set size, match position and tail length, including the bytes whose
    /// neighbours make the zero-byte test over-report (a 0x01 above a match).
    #[test]
    fn find_any_byte_matches_a_byte_loop() {
        let naive = |h: &[u8], set: &[u8]| h.iter().position(|b| set.contains(b));
        let sets: [&[u8]; 6] = [b"", b",", b",\"", b",\"\n", b"\x00\x01\x80\xff", b",;:|\t\n"];
        let mut state = 0x2545_f491_4f6c_dd1du64;
        for len in 0..70 {
            for _ in 0..200 {
                let hay: Vec<u8> = (0..len)
                    .map(|_| {
                        state ^= state << 13;
                        state ^= state >> 7;
                        state ^= state << 17;
                        // a small alphabet, so the sets match often
                        b"ab,\"\n\x00\x01\x02\x80\xff;x"[(state % 12) as usize]
                    })
                    .collect();
                for set in sets {
                    assert_eq!(find_any_byte(&hay, set), naive(&hay, set), "{hay:?} {set:?}");
                }
            }
        }
        unsafe {
            let hay = b"plain,field\0".as_ptr() as *const i8;
            assert_eq!(pith_cstring_index_of_any(hay, b"\"\n,\0".as_ptr() as *const i8), 5);
            assert_eq!(pith_cstring_index_of_any(hay, b";\0".as_ptr() as *const i8), -1);
            assert_eq!(pith_cstring_index_of_any(hay, b"\0".as_ptr() as *const i8), -1);
            assert_eq!(pith_cstring_index_of_any(std::ptr::null(), hay), -1);
        }
    }

    #[test]
    fn utility_cstring_callers_handle_null_and_invalid_utf8() {
        let invalid = [0xffu8, 0x00];
        let ptr = invalid.as_ptr() as *const i8;

        unsafe {
            assert_eq!(pith_fs_write(ptr, b"x\0".as_ptr() as *const i8), 0);
            assert!(pith_exec_output(ptr).is_null());
            assert!(pith_b64_decode(std::ptr::null()).is_null());
            assert_eq!(pith_cstring_index_of(std::ptr::null(), ptr), -1);
            assert_eq!(pith_cstring_contains(ptr, b"x\0".as_ptr() as *const i8), 0);
            assert_eq!(pith_is_dir(ptr as i64), 0);
        }
    }

    fn index_of(haystack: &str, needle: &str) -> i64 {
        let h = std::ffi::CString::new(haystack).unwrap();
        let n = std::ffi::CString::new(needle).unwrap();
        unsafe { pith_cstring_index_of(h.as_ptr(), n.as_ptr()) }
    }

    fn contains(haystack: &str, needle: &str) -> i64 {
        let h = std::ffi::CString::new(haystack).unwrap();
        let n = std::ffi::CString::new(needle).unwrap();
        unsafe { pith_cstring_contains(h.as_ptr(), n.as_ptr()) }
    }

    #[test]
    fn index_of_and_contains_edge_cases() {
        // empty needle: found at 0, contained in everything
        assert_eq!(index_of("", ""), 0);
        assert_eq!(index_of("abc", ""), 0);
        assert_eq!(contains("", ""), 1);
        assert_eq!(contains("abc", ""), 1);
        // needle longer than the haystack
        assert_eq!(index_of("", "a"), -1);
        assert_eq!(index_of("ab", "abc"), -1);
        assert_eq!(contains("ab", "abc"), 0);
        // needle at either end
        assert_eq!(index_of("needle in a haystack", "needle"), 0);
        assert_eq!(index_of("a haystack ends in a needle", "needle"), 21);
        assert_eq!(index_of("abc", "abc"), 0);
        assert_eq!(index_of("abc", "c"), 2);
        assert_eq!(contains("abc", "c"), 1);
        // repeated first bytes
        assert_eq!(index_of("aaaaaaab", "ab"), 6);
        assert_eq!(index_of("aaaaaaaa", "ab"), -1);
        assert_eq!(index_of("aaaa", "aa"), 0);
        assert_eq!(contains("aaaaaaaa", "ab"), 0);
        // a needle sharing a prefix with an earlier non-match
        assert_eq!(index_of("abcabd", "abd"), 3);
        assert_eq!(index_of("ababac", "abac"), 2);
        assert_eq!(index_of("ababab", "abac"), -1);
        assert_eq!(contains("xxabcxxabcdxx", "abcd"), 1);
        // bytes above 0x7f, offsets in bytes
        assert_eq!(index_of("héllo wörld", "ö"), 8);
        assert_eq!(index_of("héllo wörld", "wörld"), 7);
        assert_eq!(contains("héllo wörld", "ü"), 0);
    }

    fn repeat(text: &str, n: i64) -> Vec<u8> {
        let t = std::ffi::CString::new(text).unwrap();
        unsafe {
            let out = pith_cstring_repeat(t.as_ptr(), n);
            let bytes = with_cstr_bytes(out, <[u8]>::to_vec).unwrap();
            crate::pith_cstring_release(out);
            bytes
        }
    }

    #[test]
    fn repeat_of_the_empty_string_returns_at_once() {
        // this used to loop i64::MAX times copying nothing
        assert_eq!(repeat("", i64::MAX), b"");
        assert_eq!(repeat("", 3), b"");
        assert_eq!(repeat("ab", 3), b"ababab");
        assert_eq!(repeat("ab", 0), b"");
        assert_eq!(repeat("ab", -1), b"");
        // a total past the address space (or valid layout limit) is refused, not wrapped
        assert_eq!(repeat("abc", i64::MAX), b"");
        assert_eq!(repeat("ab", i64::MAX), b"");
        assert_eq!(repeat("a", i64::MAX), b"");
    }
}
