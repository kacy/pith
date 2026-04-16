/// Format time as string — takes unix timestamp (ms) and format string
/// Simple implementation: returns ISO-like date string
///
/// # Safety
/// fmt must be a valid null-terminated C string (or null for default)
#[no_mangle]
pub unsafe extern "C" fn forge_format_time_fmt(timestamp_ms: i64, _fmt: *const i8) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    let secs = timestamp_ms / 1000;
    let s = format!("{}", secs);
    let bytes = s.as_bytes();
    let layout = Layout::from_size_align(bytes.len() + 1, 1).unwrap();
    let ptr = alloc(layout) as *mut i8;
    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
        *ptr.add(bytes.len()) = 0;
    }
    ptr
}

/// Write string to file path
/// Returns 1 on success, 0 on failure
///
/// # Safety
/// Both pointers must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn forge_fs_write(path: *const i8, content: *const i8) -> i64 {
    if path.is_null() || content.is_null() {
        return 0;
    }
    let path_len = crate::string::forge_cstring_len(path) as usize;
    let content_len = crate::string::forge_cstring_len(content) as usize;
    let path_slice = std::slice::from_raw_parts(path as *const u8, path_len);
    let content_slice = std::slice::from_raw_parts(content as *const u8, content_len);

    if let (Ok(path_str), Ok(content_str)) = (
        std::str::from_utf8(path_slice),
        std::str::from_utf8(content_slice),
    ) {
        match std::fs::write(path_str, content_str) {
            Ok(_) => 1,
            Err(_) => 0,
        }
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_log_info(msg: *const i8) {
    if msg.is_null() {
        eprintln!("[INFO] ");
        return;
    }
    let len = crate::string::forge_cstring_len(msg) as usize;
    let slice = std::slice::from_raw_parts(msg as *const u8, len);
    if let Ok(s) = std::str::from_utf8(slice) {
        eprintln!("[INFO] {}", s);
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_log_warn(msg: *const i8) {
    if msg.is_null() {
        eprintln!("[WARN] ");
        return;
    }
    let len = crate::string::forge_cstring_len(msg) as usize;
    let slice = std::slice::from_raw_parts(msg as *const u8, len);
    if let Ok(s) = std::str::from_utf8(slice) {
        eprintln!("[WARN] {}", s);
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_log_error(msg: *const i8) {
    if msg.is_null() {
        eprintln!("[ERROR] ");
        return;
    }
    let len = crate::string::forge_cstring_len(msg) as usize;
    let slice = std::slice::from_raw_parts(msg as *const u8, len);
    if let Ok(s) = std::str::from_utf8(slice) {
        eprintln!("[ERROR] {}", s);
    }
}

/// Execute command and capture output — returns stdout as C string
#[no_mangle]
pub unsafe extern "C" fn forge_exec_output(cmd: *const i8) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if cmd.is_null() {
        return std::ptr::null_mut();
    }
    let len = crate::string::forge_cstring_len(cmd) as usize;
    let slice = std::slice::from_raw_parts(cmd as *const u8, len);
    if let Ok(cmd_str) = std::str::from_utf8(slice) {
        let parts: Vec<&str> = cmd_str.split_whitespace().collect();
        if parts.is_empty() {
            return std::ptr::null_mut();
        }
        match std::process::Command::new(parts[0]).args(&parts[1..]).output() {
            Ok(output) => {
                let stdout = &output.stdout;
                let layout = Layout::from_size_align(stdout.len() + 1, 1).unwrap();
                let ptr = alloc(layout) as *mut i8;
                if !ptr.is_null() {
                    std::ptr::copy_nonoverlapping(stdout.as_ptr(), ptr as *mut u8, stdout.len());
                    *ptr.add(stdout.len()) = 0;
                }
                ptr
            }
            Err(_) => std::ptr::null_mut(),
        }
    } else {
        std::ptr::null_mut()
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_b64_decode(s: *const i8) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if s.is_null() {
        return std::ptr::null_mut();
    }

    let len = crate::string::forge_cstring_len(s) as usize;
    let input = std::slice::from_raw_parts(s as *const u8, len);

    const DECODE: [u8; 256] = {
        let mut t = [255u8; 256];
        let mut i = 0u8;
        while i < 26 { t[(b'A' + i) as usize] = i; i += 1; }
        i = 0;
        while i < 26 { t[(b'a' + i) as usize] = i + 26; i += 1; }
        i = 0;
        while i < 10 { t[(b'0' + i) as usize] = i + 52; i += 1; }
        t[b'+' as usize] = 62;
        t[b'/' as usize] = 63;
        t
    };

    let mut in_len = len;
    while in_len > 0 && input[in_len - 1] == b'=' {
        in_len -= 1;
    }
    let out_len = in_len * 3 / 4;
    let layout = Layout::from_size_align(out_len + 1, 1).unwrap();
    let ptr = alloc(layout) as *mut u8;

    if ptr.is_null() {
        return std::ptr::null_mut();
    }

    let mut si = 0;
    let mut di = 0;
    while si + 3 < in_len {
        let a = DECODE[input[si] as usize] as u32;
        let b = DECODE[input[si + 1] as usize] as u32;
        let c = DECODE[input[si + 2] as usize] as u32;
        let d = DECODE[input[si + 3] as usize] as u32;
        let n = (a << 18) | (b << 12) | (c << 6) | d;
        if di < out_len { *ptr.add(di) = (n >> 16) as u8; di += 1; }
        if di < out_len { *ptr.add(di) = (n >> 8) as u8; di += 1; }
        if di < out_len { *ptr.add(di) = n as u8; di += 1; }
        si += 4;
    }
    if si + 1 < in_len {
        let a = DECODE[input[si] as usize] as u32;
        let b = DECODE[input[si + 1] as usize] as u32;
        let n = (a << 18) | (b << 12);
        if di < out_len { *ptr.add(di) = (n >> 16) as u8; di += 1; }
        if si + 2 < in_len {
            let c = DECODE[input[si + 2] as usize] as u32;
            let n2 = (a << 18) | (b << 12) | (c << 6);
            if di < out_len { *ptr.add(di) = ((n2 >> 8) & 0xff) as u8; di += 1; }
        }
    }
    *ptr.add(di.min(out_len)) = 0;

    ptr as *mut i8
}

#[no_mangle]
pub unsafe extern "C" fn forge_fnv1a(s: *const i8) -> i64 {
    if s.is_null() {
        return 0;
    }
    let len = crate::string::forge_cstring_len(s) as usize;
    let bytes = std::slice::from_raw_parts(s as *const u8, len);
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash as i64
}

#[no_mangle]
pub unsafe extern "C" fn forge_cstring_index_of(haystack: *const i8, needle: *const i8) -> i64 {
    if haystack.is_null() || needle.is_null() {
        return -1;
    }
    let h_len = crate::string::forge_cstring_len(haystack) as usize;
    let n_len = crate::string::forge_cstring_len(needle) as usize;
    if n_len == 0 {
        return 0;
    }
    let h_bytes = std::slice::from_raw_parts(haystack as *const u8, h_len);
    let n_bytes = std::slice::from_raw_parts(needle as *const u8, n_len);
    for i in 0..=(h_len.saturating_sub(n_len)) {
        if &h_bytes[i..i + n_len] == n_bytes {
            return i as i64;
        }
    }
    -1
}

#[no_mangle]
pub unsafe extern "C" fn forge_cstring_contains(haystack: *const i8, needle: *const i8) -> i64 {
    if haystack.is_null() || needle.is_null() {
        return 0;
    }
    let h_len = crate::string::forge_cstring_len(haystack) as usize;
    let n_len = crate::string::forge_cstring_len(needle) as usize;
    if n_len == 0 {
        return 1;
    }
    if n_len > h_len {
        return 0;
    }
    let h_bytes = std::slice::from_raw_parts(haystack as *const u8, h_len);
    let n_bytes = std::slice::from_raw_parts(needle as *const u8, n_len);
    for i in 0..=(h_len - n_len) {
        if &h_bytes[i..i + n_len] == n_bytes {
            return 1;
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn forge_cstring_starts_with(s: *const i8, prefix: *const i8) -> i64 {
    if s.is_null() || prefix.is_null() {
        return 0;
    }
    let s_len = crate::string::forge_cstring_len(s) as usize;
    let p_len = crate::string::forge_cstring_len(prefix) as usize;
    if p_len > s_len {
        return 0;
    }
    let s_bytes = std::slice::from_raw_parts(s as *const u8, p_len);
    let p_bytes = std::slice::from_raw_parts(prefix as *const u8, p_len);
    if s_bytes == p_bytes { 1 } else { 0 }
}

#[no_mangle]
pub unsafe extern "C" fn forge_cstring_ends_with(s: *const i8, suffix: *const i8) -> i64 {
    if s.is_null() || suffix.is_null() {
        return 0;
    }
    let s_len = crate::string::forge_cstring_len(s) as usize;
    let x_len = crate::string::forge_cstring_len(suffix) as usize;
    if x_len > s_len {
        return 0;
    }
    let s_bytes = std::slice::from_raw_parts(s as *const u8, s_len);
    let x_bytes = std::slice::from_raw_parts(suffix as *const u8, x_len);
    if &s_bytes[s_len - x_len..] == x_bytes { 1 } else { 0 }
}

#[no_mangle]
pub unsafe extern "C" fn forge_cstring_pad_left(
    s: *const i8,
    width: i64,
    fill: *const i8,
) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if s.is_null() {
        return std::ptr::null_mut();
    }
    let len = crate::string::forge_cstring_len(s) as usize;
    let w = width as usize;
    if len >= w {
        return crate::forge_strdup(s);
    }
    let fill_char = if !fill.is_null() && *fill != 0 { *fill } else { b' ' as i8 };
    let pad = w - len;
    let total = w + 1;
    let layout = Layout::from_size_align(total, 1).unwrap();
    let ptr = alloc(layout) as *mut i8;
    if !ptr.is_null() {
        for i in 0..pad {
            *ptr.add(i) = fill_char;
        }
        std::ptr::copy_nonoverlapping(s, ptr.add(pad), len);
        *ptr.add(w) = 0;
    }
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn forge_cstring_pad_right(
    s: *const i8,
    width: i64,
    fill: *const i8,
) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if s.is_null() {
        return std::ptr::null_mut();
    }
    let len = crate::string::forge_cstring_len(s) as usize;
    let w = width as usize;
    if len >= w {
        return crate::forge_strdup(s);
    }
    let fill_char = if !fill.is_null() && *fill != 0 { *fill } else { b' ' as i8 };
    let total = w + 1;
    let layout = Layout::from_size_align(total, 1).unwrap();
    let ptr = alloc(layout) as *mut i8;
    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(s, ptr, len);
        for i in len..w {
            *ptr.add(i) = fill_char;
        }
        *ptr.add(w) = 0;
    }
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn forge_cstring_repeat(s: *const i8, n: i64) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if s.is_null() || n <= 0 {
        return crate::forge_cstring_empty();
    }
    let len = crate::string::forge_cstring_len(s) as usize;
    let total = len * n as usize + 1;
    let layout = Layout::from_size_align(total, 1).unwrap();
    let ptr = alloc(layout) as *mut i8;
    if !ptr.is_null() {
        for i in 0..n as usize {
            std::ptr::copy_nonoverlapping(s, ptr.add(i * len), len);
        }
        *ptr.add(len * n as usize) = 0;
    }
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn forge_float_fixed(value: f64, decimals: i64) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    let s = format!("{:.prec$}", value, prec = decimals as usize);
    let bytes = s.as_bytes();
    let layout = Layout::array::<u8>(bytes.len() + 1).unwrap();
    let ptr = alloc(layout) as *mut i8;
    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
        *ptr.add(bytes.len()) = 0;
    }
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn forge_is_dir(path: i64) -> i64 {
    let path_ptr = path as *const i8;
    if path_ptr.is_null() {
        return 0;
    }
    let len = crate::string::forge_cstring_len(path_ptr) as usize;
    let slice = std::slice::from_raw_parts(path_ptr as *const u8, len);
    if let Ok(path_str) = std::str::from_utf8(slice) {
        if std::path::Path::new(path_str).is_dir() { 1 } else { 0 }
    } else {
        0
    }
}
