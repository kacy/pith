use crate::ffi_util::{cstr_bytes, cstr_str};

/// Format time as string — takes unix timestamp (ms) and format string
/// Simple implementation: returns ISO-like date string
///
/// # Safety
/// fmt must be a valid null-terminated C string (or null for default)
#[no_mangle]
pub unsafe extern "C" fn pith_format_time_fmt(timestamp_ms: i64, _fmt: *const i8) -> *mut i8 {
    let secs = timestamp_ms / 1000;
    let s = format!("{}", secs);
    crate::pith_copy_bytes_to_cstring(s.as_bytes())
}

/// Write string to file path
/// Returns 1 on success, 0 on failure
///
/// # Safety
/// Both pointers must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn pith_fs_write(path: *const i8, content: *const i8) -> i64 {
    if let (Some(path_str), Some(content_str)) = (cstr_str(path), cstr_str(content)) {
        match std::fs::write(path_str, content_str) {
            Ok(_) => 1,
            Err(_) => 0,
        }
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_log_info(msg: *const i8) {
    eprintln!("[INFO] {}", cstr_str(msg).unwrap_or(""));
}

#[no_mangle]
pub unsafe extern "C" fn pith_log_warn(msg: *const i8) {
    eprintln!("[WARN] {}", cstr_str(msg).unwrap_or(""));
}

#[no_mangle]
pub unsafe extern "C" fn pith_log_error(msg: *const i8) {
    eprintln!("[ERROR] {}", cstr_str(msg).unwrap_or(""));
}

/// Execute command and capture output — returns stdout as C string
///
/// the child runs to completion via the process pool (see `process`), so a
/// green worker is not held for however long the command takes.
#[no_mangle]
pub unsafe extern "C" fn pith_exec_output(cmd: *const i8) -> *mut i8 {
    let Some(cmd_str) = cstr_str(cmd) else {
        return std::ptr::null_mut();
    };
    let parts: Vec<&str> = cmd_str.split_whitespace().collect();
    if parts.is_empty() {
        return std::ptr::null_mut();
    }
    let mut command = std::process::Command::new(parts[0]);
    crate::env_overlay::apply(&mut command);
    command.args(&parts[1..]);
    match crate::process::command_output(command) {
        Some(output) => crate::pith_copy_bytes_to_cstring(&output.stdout),
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_b64_decode(s: *const i8) -> *mut i8 {
    let Some(input) = cstr_bytes(s) else {
        return std::ptr::null_mut();
    };

    const DECODE: [u8; 256] = {
        let mut t = [255u8; 256];
        let mut i = 0u8;
        while i < 26 {
            t[(b'A' + i) as usize] = i;
            i += 1;
        }
        i = 0;
        while i < 26 {
            t[(b'a' + i) as usize] = i + 26;
            i += 1;
        }
        i = 0;
        while i < 10 {
            t[(b'0' + i) as usize] = i + 52;
            i += 1;
        }
        t[b'+' as usize] = 62;
        t[b'/' as usize] = 63;
        t
    };

    let mut in_len = input.len();
    while in_len > 0 && input[in_len - 1] == b'=' {
        in_len -= 1;
    }
    let out_len = in_len * 3 / 4;
    let ptr = crate::pith_alloc_cstring(out_len) as *mut u8;

    let mut si = 0;
    let mut di = 0;
    while si + 3 < in_len {
        let a = DECODE[input[si] as usize] as u32;
        let b = DECODE[input[si + 1] as usize] as u32;
        let c = DECODE[input[si + 2] as usize] as u32;
        let d = DECODE[input[si + 3] as usize] as u32;
        let n = (a << 18) | (b << 12) | (c << 6) | d;
        if di < out_len {
            *ptr.add(di) = (n >> 16) as u8;
            di += 1;
        }
        if di < out_len {
            *ptr.add(di) = (n >> 8) as u8;
            di += 1;
        }
        if di < out_len {
            *ptr.add(di) = n as u8;
            di += 1;
        }
        si += 4;
    }
    if si + 1 < in_len {
        let a = DECODE[input[si] as usize] as u32;
        let b = DECODE[input[si + 1] as usize] as u32;
        let n = (a << 18) | (b << 12);
        if di < out_len {
            *ptr.add(di) = (n >> 16) as u8;
            di += 1;
        }
        if si + 2 < in_len {
            let c = DECODE[input[si + 2] as usize] as u32;
            let n2 = (a << 18) | (b << 12) | (c << 6);
            if di < out_len {
                *ptr.add(di) = ((n2 >> 8) & 0xff) as u8;
                di += 1;
            }
        }
    }
    *ptr.add(di.min(out_len)) = 0;

    ptr as *mut i8
}

#[no_mangle]
pub unsafe extern "C" fn pith_fnv1a(s: *const i8) -> i64 {
    let Some(bytes) = cstr_bytes(s) else {
        return 0;
    };
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash as i64
}

/// Byte offset of the first occurrence of needle in haystack, or -1. An
/// empty needle is found at 0.
///
/// # Safety
/// Both arguments must be null or valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_index_of(haystack: *const i8, needle: *const i8) -> i64 {
    let (Some(h_bytes), Some(n_bytes)) = (cstr_bytes(haystack), cstr_bytes(needle)) else {
        return -1;
    };
    match crate::substring::find(h_bytes, n_bytes) {
        Some(at) => at as i64,
        None => -1,
    }
}

/// 1 when needle occurs in haystack, else 0. An empty needle is contained
/// in every string.
///
/// # Safety
/// Both arguments must be null or valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_contains(haystack: *const i8, needle: *const i8) -> i64 {
    let (Some(h_bytes), Some(n_bytes)) = (cstr_bytes(haystack), cstr_bytes(needle)) else {
        return 0;
    };
    if crate::substring::find(h_bytes, n_bytes).is_some() {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_starts_with(s: *const i8, prefix: *const i8) -> i64 {
    let (Some(bytes), Some(prefix_bytes)) = (cstr_bytes(s), cstr_bytes(prefix)) else {
        return 0;
    };
    if bytes.starts_with(prefix_bytes) {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_ends_with(s: *const i8, suffix: *const i8) -> i64 {
    let (Some(bytes), Some(suffix_bytes)) = (cstr_bytes(s), cstr_bytes(suffix)) else {
        return 0;
    };
    if bytes.ends_with(suffix_bytes) {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_pad_left(
    s: *const i8,
    width: i64,
    fill: *const i8,
) -> *mut i8 {
    let Some(bytes) = cstr_bytes(s) else {
        return std::ptr::null_mut();
    };
    let len = bytes.len();
    if width <= 0 {
        return crate::pith_strdup(s);
    }
    let w = width as usize;
    if len >= w {
        return crate::pith_strdup(s);
    }
    let fill_char = if !fill.is_null() && *fill != 0 {
        *fill
    } else {
        b' ' as i8
    };
    let pad = w - len;
    let ptr = crate::pith_alloc_cstring(w);
    for i in 0..pad {
        *ptr.add(i) = fill_char;
    }
    std::ptr::copy_nonoverlapping(s, ptr.add(pad), len);
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_pad_right(
    s: *const i8,
    width: i64,
    fill: *const i8,
) -> *mut i8 {
    let Some(bytes) = cstr_bytes(s) else {
        return std::ptr::null_mut();
    };
    let len = bytes.len();
    if width <= 0 {
        return crate::pith_strdup(s);
    }
    let w = width as usize;
    if len >= w {
        return crate::pith_strdup(s);
    }
    let fill_char = if !fill.is_null() && *fill != 0 {
        *fill
    } else {
        b' ' as i8
    };
    let ptr = crate::pith_alloc_cstring(w);
    std::ptr::copy_nonoverlapping(s, ptr, len);
    for i in len..w {
        *ptr.add(i) = fill_char;
    }
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_repeat(s: *const i8, n: i64) -> *mut i8 {
    let Some(bytes) = cstr_bytes(s) else {
        return crate::pith_cstring_empty();
    };
    if n <= 0 {
        return crate::pith_cstring_empty();
    }
    let len = bytes.len();
    let Some(total_len) = len.checked_mul(n as usize) else {
        return crate::pith_cstring_empty();
    };
    let ptr = crate::pith_alloc_cstring(total_len);
    for i in 0..n as usize {
        std::ptr::copy_nonoverlapping(s, ptr.add(i * len), len);
    }
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn pith_float_fixed(value: f64, decimals: i64) -> *mut i8 {
    let precision = decimals.max(0) as usize;
    let s = format!("{:.prec$}", value, prec = precision);
    crate::pith_copy_bytes_to_cstring(s.as_bytes())
}

#[no_mangle]
pub unsafe extern "C" fn pith_is_dir(path: i64) -> i64 {
    if let Some(path_str) = cstr_str(path as *const i8) {
        if std::path::Path::new(path_str).is_dir() {
            1
        } else {
            0
        }
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
