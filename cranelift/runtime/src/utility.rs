use crate::ffi_util::{cstr_bytes, cstr_str};

const MAX_FLOAT_FIXED_DECIMALS: usize = 308;

fn b64_decoded_len(input_len: usize) -> Option<usize> {
    input_len.checked_mul(3)?.checked_div(4)
}

/// Format time as string — takes unix timestamp (ms) and format string
/// Simple implementation: returns ISO-like date string
///
/// # Safety
/// fmt must be a valid null-terminated C string (or null for default)
#[no_mangle]
pub unsafe extern "C" fn pith_format_time_fmt(timestamp_ms: i64, _fmt: *const i8) -> *mut i8 {
    let secs = timestamp_ms / 1000;
    let s = format!("{}", secs);
    crate::runtime_core::pith_try_copy_bytes_to_cstring(s.as_bytes())
        .unwrap_or(std::ptr::null_mut())
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
#[no_mangle]
pub unsafe extern "C" fn pith_exec_output(cmd: *const i8) -> *mut i8 {
    let Some(cmd_str) = cstr_str(cmd) else {
        return std::ptr::null_mut();
    };
    let parts: Vec<&str> = cmd_str.split_whitespace().collect();
    if parts.is_empty() {
        std::ptr::null_mut()
    } else {
        match std::process::Command::new(parts[0])
            .args(&parts[1..])
            .output()
        {
            Ok(output) => crate::runtime_core::pith_try_copy_bytes_to_cstring(&output.stdout)
                .unwrap_or(std::ptr::null_mut()),
            Err(_) => std::ptr::null_mut(),
        }
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
    let Some(out_len) = b64_decoded_len(in_len) else {
        eprintln!("pith runtime error: base64 decode output size overflow");
        return std::ptr::null_mut();
    };
    let Some(ptr) = crate::runtime_core::pith_try_alloc_cstring(out_len) else {
        eprintln!("pith runtime error: allocation failed");
        return std::ptr::null_mut();
    };

    let mut si = 0;
    let mut di = 0;
    while si + 3 < in_len {
        let a = DECODE[input[si] as usize] as u32;
        let b = DECODE[input[si + 1] as usize] as u32;
        let c = DECODE[input[si + 2] as usize] as u32;
        let d = DECODE[input[si + 3] as usize] as u32;
        let n = (a << 18) | (b << 12) | (c << 6) | d;
        if di < out_len {
            *ptr.add(di) = (n >> 16) as u8 as i8;
            di += 1;
        }
        if di < out_len {
            *ptr.add(di) = (n >> 8) as u8 as i8;
            di += 1;
        }
        if di < out_len {
            *ptr.add(di) = n as u8 as i8;
            di += 1;
        }
        si += 4;
    }
    if si + 1 < in_len {
        let a = DECODE[input[si] as usize] as u32;
        let b = DECODE[input[si + 1] as usize] as u32;
        let n = (a << 18) | (b << 12);
        if di < out_len {
            *ptr.add(di) = (n >> 16) as u8 as i8;
            di += 1;
        }
        if si + 2 < in_len {
            let c = DECODE[input[si + 2] as usize] as u32;
            let n2 = (a << 18) | (b << 12) | (c << 6);
            if di < out_len {
                *ptr.add(di) = ((n2 >> 8) & 0xff) as u8 as i8;
                di += 1;
            }
        }
    }
    *ptr.add(di.min(out_len)) = 0;

    ptr
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

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_index_of(haystack: *const i8, needle: *const i8) -> i64 {
    let (Some(h_bytes), Some(n_bytes)) = (cstr_bytes(haystack), cstr_bytes(needle)) else {
        return -1;
    };
    let h_len = h_bytes.len();
    let n_len = n_bytes.len();
    if n_len == 0 {
        return 0;
    }
    for i in 0..=(h_len.saturating_sub(n_len)) {
        if &h_bytes[i..i + n_len] == n_bytes {
            return i as i64;
        }
    }
    -1
}

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_contains(haystack: *const i8, needle: *const i8) -> i64 {
    let (Some(h_bytes), Some(n_bytes)) = (cstr_bytes(haystack), cstr_bytes(needle)) else {
        return 0;
    };
    let h_len = h_bytes.len();
    let n_len = n_bytes.len();
    if n_len == 0 {
        return 1;
    }
    if n_len > h_len {
        return 0;
    }
    for i in 0..=(h_len - n_len) {
        if &h_bytes[i..i + n_len] == n_bytes {
            return 1;
        }
    }
    0
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
    let Some(ptr) = crate::runtime_core::pith_try_alloc_cstring(w) else {
        return std::ptr::null_mut();
    };
    for i in 0..pad {
        *ptr.add(i) = fill_char;
    }
    std::ptr::copy_nonoverlapping(s, ptr.add(pad), len);
    *ptr.add(w) = 0;
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
    let Some(ptr) = crate::runtime_core::pith_try_alloc_cstring(w) else {
        return std::ptr::null_mut();
    };
    std::ptr::copy_nonoverlapping(s, ptr, len);
    for i in len..w {
        *ptr.add(i) = fill_char;
    }
    *ptr.add(w) = 0;
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
    let Some(ptr) = crate::runtime_core::pith_try_alloc_cstring(total_len) else {
        return crate::pith_cstring_empty();
    };
    for i in 0..n as usize {
        std::ptr::copy_nonoverlapping(s, ptr.add(i * len), len);
    }
    *ptr.add(total_len) = 0;
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn pith_float_fixed(value: f64, decimals: i64) -> *mut i8 {
    let precision = if decimals <= 0 {
        0
    } else {
        (decimals as usize).min(MAX_FLOAT_FIXED_DECIMALS)
    };
    let s = format!("{:.prec$}", value, prec = precision);
    crate::runtime_core::pith_try_copy_bytes_to_cstring(s.as_bytes())
        .unwrap_or(std::ptr::null_mut())
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
    use std::ffi::CString;

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

    unsafe fn cstring_bytes(ptr: *const i8) -> Vec<u8> {
        assert!(!ptr.is_null());
        let len = crate::string::pith_cstring_len(ptr) as usize;
        std::slice::from_raw_parts(ptr as *const u8, len).to_vec()
    }

    #[test]
    fn base64_decode_length_rejects_overflow() {
        assert_eq!(b64_decoded_len(4), Some(3));
        assert_eq!(b64_decoded_len(usize::MAX), None);
    }

    #[test]
    fn base64_decode_and_repeat_use_checked_allocations() {
        let Ok(encoded) = CString::new("cGl0aA==") else {
            assert!(false);
            return;
        };
        let Ok(text) = CString::new("ha") else {
            assert!(false);
            return;
        };

        unsafe {
            let decoded = pith_b64_decode(encoded.as_ptr());
            assert_eq!(cstring_bytes(decoded), b"pith");

            let repeated = pith_cstring_repeat(text.as_ptr(), 3);
            assert_eq!(cstring_bytes(repeated), b"hahaha");

            let padded_left = pith_cstring_pad_left(text.as_ptr(), 4, b".\0".as_ptr() as *const i8);
            assert_eq!(cstring_bytes(padded_left), b"..ha");

            let padded_right =
                pith_cstring_pad_right(text.as_ptr(), 4, b".\0".as_ptr() as *const i8);
            assert_eq!(cstring_bytes(padded_right), b"ha..");

            crate::pith_free(decoded);
            crate::pith_free(repeated);
            crate::pith_free(padded_left);
            crate::pith_free(padded_right);
        }
    }

    #[test]
    fn utility_formatters_use_checked_allocations() {
        unsafe {
            let timestamp = pith_format_time_fmt(123_000, std::ptr::null());
            assert_eq!(cstring_bytes(timestamp), b"123");

            let fixed = pith_float_fixed(1.25, 2);
            assert_eq!(cstring_bytes(fixed), b"1.25");

            let capped = pith_float_fixed(1.0, i64::MAX);
            assert!(!capped.is_null());
            assert!(cstring_bytes(capped).len() <= MAX_FLOAT_FIXED_DECIMALS + 2);

            crate::pith_free(timestamp);
            crate::pith_free(fixed);
            crate::pith_free(capped);
        }
    }
}
