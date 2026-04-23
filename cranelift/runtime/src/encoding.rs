unsafe fn alloc_parse_int_result(is_ok: i64, ok: i64, err: i64) -> i64 {
    // this matches forge's heap result tuple layout: [is_ok, ok, err].
    let tuple = crate::forge_struct_alloc(3) as *mut i64;
    if tuple.is_null() {
        return 0;
    }
    *tuple = is_ok;
    *tuple.add(1) = ok;
    *tuple.add(2) = err;
    tuple as i64
}

unsafe fn parse_int_error(message: &[u8]) -> i64 {
    let err = crate::forge_copy_bytes_to_cstring(message) as i64;
    alloc_parse_int_result(0, 0, err)
}

/// Parse string to int and return a result tuple pointer.
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_parse_int(s: *const i8) -> i64 {
    if s.is_null() {
        return parse_int_error(b"invalid integer");
    }
    let len = crate::string::forge_cstring_len(s) as usize;
    let slice = std::slice::from_raw_parts(s as *const u8, len);
    let mut start = 0;
    let mut end = len;
    while start < end && slice[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && slice[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    if start == end {
        return parse_int_error(b"invalid integer");
    }
    let mut pos = start;
    let mut negative = false;
    if slice[pos] == b'-' || slice[pos] == b'+' {
        negative = slice[pos] == b'-';
        pos += 1;
        if pos == end {
            return parse_int_error(b"invalid integer");
        }
    }
    let limit = if negative { i64::MAX as u64 + 1 } else { i64::MAX as u64 };
    let mut value: u64 = 0;
    while pos < end {
        let digit = slice[pos];
        if !digit.is_ascii_digit() {
            return parse_int_error(b"invalid integer");
        }
        let digit_value = (digit - b'0') as u64;
        if value > (limit - digit_value) / 10 {
            return parse_int_error(b"integer overflow");
        }
        value = value * 10 + digit_value;
        pos += 1;
    }
    let parsed = if negative {
        if value == limit {
            i64::MIN
        } else {
            -(value as i64)
        }
    } else {
        value as i64
    };
    alloc_parse_int_result(1, parsed, 0)
}

/// Parse string to float — returns 0.0 on failure
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_parse_float(s: *const i8) -> f64 {
    if s.is_null() {
        return 0.0;
    }
    let len = crate::string::forge_cstring_len(s) as usize;
    let slice = std::slice::from_raw_parts(s as *const u8, len);
    if let Ok(str_ref) = std::str::from_utf8(slice) {
        str_ref.trim().parse::<f64>().unwrap_or(0.0)
    } else {
        0.0
    }
}

/// Base64 encode a C string — returns newly allocated C string
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_b64_encode(s: *const i8) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if s.is_null() {
        return std::ptr::null_mut();
    }

    let len = crate::string::forge_cstring_len(s) as usize;
    let input = std::slice::from_raw_parts(s as *const u8, len);

    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out: Vec<u8> = Vec::with_capacity((len + 2) / 3 * 4 + 1);
    let mut i = 0;
    while i < len {
        let b0 = input[i];
        let b1 = if i + 1 < len { input[i + 1] } else { 0 };
        let b2 = if i + 2 < len { input[i + 2] } else { 0 };

        out.push(CHARS[(b0 >> 2) as usize]);
        out.push(CHARS[((b0 & 3) << 4 | b1 >> 4) as usize]);
        out.push(if i + 1 < len {
            CHARS[((b1 & 0xf) << 2 | b2 >> 6) as usize]
        } else {
            b'='
        });
        out.push(if i + 2 < len {
            CHARS[(b2 & 0x3f) as usize]
        } else {
            b'='
        });

        i += 3;
    }
    out.push(0);

    let layout = Layout::from_size_align(out.len(), 1).unwrap();
    let ptr = alloc(layout) as *mut i8;
    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(out.as_ptr(), ptr as *mut u8, out.len());
    }
    ptr
}

/// Hex encode a C string — returns newly allocated C string
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_hex_encode(s: *const i8) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if s.is_null() {
        return std::ptr::null_mut();
    }

    let len = crate::string::forge_cstring_len(s) as usize;
    let input = std::slice::from_raw_parts(s as *const u8, len);
    let hex_len = len * 2 + 1;
    let layout = Layout::from_size_align(hex_len, 1).unwrap();
    let ptr = alloc(layout) as *mut u8;

    if !ptr.is_null() {
        for (i, &byte) in input.iter().enumerate() {
            let hi = (byte >> 4) as usize;
            let lo = (byte & 0xf) as usize;
            const HEX: &[u8] = b"0123456789abcdef";
            *ptr.add(i * 2) = HEX[hi];
            *ptr.add(i * 2 + 1) = HEX[lo];
        }
        *ptr.add(len * 2) = 0;
    }
    ptr as *mut i8
}

/// Decode a hex string back to the original string
///
/// # Safety
/// s must be a valid null-terminated C string of hex digits
#[no_mangle]
pub unsafe extern "C" fn forge_from_hex(s: *const i8) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if s.is_null() {
        return std::ptr::null_mut();
    }

    let len = crate::string::forge_cstring_len(s) as usize;
    if len % 2 != 0 {
        return std::ptr::null_mut();
    }
    let input = std::slice::from_raw_parts(s as *const u8, len);
    let out_len = len / 2;
    let layout = Layout::from_size_align(out_len + 1, 1).unwrap();
    let ptr = alloc(layout) as *mut u8;

    if !ptr.is_null() {
        for i in 0..out_len {
            let hi = hex_digit(input[i * 2]);
            let lo = hex_digit(input[i * 2 + 1]);
            *ptr.add(i) = (hi << 4) | lo;
        }
        *ptr.add(out_len) = 0;
    }
    ptr as *mut i8
}

fn hex_digit(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

/// Convert an integer to hex string (e.g., 255 → "ff")
///
/// # Safety
/// Returns heap-allocated null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_int_to_hex(n: i64) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    let s = format!("{:x}", n);
    let bytes = s.as_bytes();
    let layout = Layout::array::<u8>(bytes.len() + 1).unwrap();
    let ptr = alloc(layout) as *mut i8;
    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
        *ptr.add(bytes.len()) = 0;
    }
    ptr
}

/// Convert an integer to octal string (e.g., 8 → "10")
///
/// # Safety
/// Returns heap-allocated null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_int_to_oct(n: i64) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    let s = format!("{:o}", n);
    let bytes = s.as_bytes();
    let layout = Layout::array::<u8>(bytes.len() + 1).unwrap();
    let ptr = alloc(layout) as *mut i8;
    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
        *ptr.add(bytes.len()) = 0;
    }
    ptr
}

/// Convert an integer to binary string (e.g., 10 → "1010")
///
/// # Safety
/// Returns heap-allocated null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_int_to_bin(n: i64) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    let s = format!("{:b}", n);
    let bytes = s.as_bytes();
    let layout = Layout::array::<u8>(bytes.len() + 1).unwrap();
    let ptr = alloc(layout) as *mut i8;
    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
        *ptr.add(bytes.len()) = 0;
    }
    ptr
}

/// SHA-256 hash of a C string — returns hex-encoded C string
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_sha256(s: *const i8) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if s.is_null() {
        let placeholder =
            b"0000000000000000000000000000000000000000000000000000000000000000\0";
        let layout = Layout::from_size_align(placeholder.len(), 1).unwrap();
        let ptr = alloc(layout) as *mut i8;
        if !ptr.is_null() {
            std::ptr::copy_nonoverlapping(placeholder.as_ptr(), ptr as *mut u8, placeholder.len());
        }
        return ptr;
    }

    let len = crate::string::forge_cstring_len(s) as usize;
    let input = std::slice::from_raw_parts(s as *const u8, len);

    let hash = sha256_compute(input);
    let mut hex = Vec::with_capacity(65);
    const HEX: &[u8] = b"0123456789abcdef";
    for &byte in &hash {
        hex.push(HEX[(byte >> 4) as usize]);
        hex.push(HEX[(byte & 0xf) as usize]);
    }
    hex.push(0);

    let layout = Layout::from_size_align(hex.len(), 1).unwrap();
    let ptr = alloc(layout) as *mut i8;
    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(hex.as_ptr(), ptr as *mut u8, hex.len());
    }
    ptr
}

fn sha256_compute(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let orig_len = data.len();
    let mut msg: Vec<u8> = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    let bit_len = (orig_len as u64) * 8;
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] =
            [h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut result = [0u8; 32];
    for (i, &word) in h.iter().enumerate() {
        result[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::forge_parse_int;
    use std::ffi::CString;

    fn parse(input: &str) -> (bool, i64) {
        let c_input = CString::new(input).unwrap();
        let tuple = unsafe { forge_parse_int(c_input.as_ptr()) as *const i64 };
        assert!(!tuple.is_null());
        let is_ok = unsafe { *tuple } != 0;
        let value = unsafe { *tuple.add(1) };
        (is_ok, value)
    }

    #[test]
    fn parse_int_accepts_trimmed_signed_values() {
        assert_eq!(parse("42"), (true, 42));
        assert_eq!(parse("  -17\n"), (true, -17));
        assert_eq!(parse("+5"), (true, 5));
        assert_eq!(parse("-1"), (true, -1));
        assert_eq!(parse("0"), (true, 0));
    }

    #[test]
    fn parse_int_rejects_invalid_or_overflowing_values() {
        assert_eq!(parse(""), (false, 0));
        assert_eq!(parse("12x"), (false, 0));
        assert_eq!(parse("9223372036854775808"), (false, 0));
        assert_eq!(parse("-9223372036854775809"), (false, 0));
    }

    #[test]
    fn parse_int_handles_i64_bounds() {
        assert_eq!(parse("9223372036854775807"), (true, i64::MAX));
        assert_eq!(parse("-9223372036854775808"), (true, i64::MIN));
    }
}
