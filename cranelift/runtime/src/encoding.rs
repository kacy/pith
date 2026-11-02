use crate::ffi_util::{result_err, result_ok, ResultPair};

unsafe fn parse_error(message: &[u8]) -> ResultPair {
    result_err(message)
}

/// Parse a string to an int, as the (is_ok, payload) register pair: the value,
/// or an owned message (`invalid integer`, `integer overflow`).
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_parse_int(s: *const i8) -> ResultPair {
    if s.is_null() {
        return parse_error(b"invalid integer");
    }
    let len = crate::string::pith_cstring_len(s) as usize;
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
        return parse_error(b"invalid integer");
    }
    let mut pos = start;
    let mut negative = false;
    if slice[pos] == b'-' || slice[pos] == b'+' {
        negative = slice[pos] == b'-';
        pos += 1;
        if pos == end {
            return parse_error(b"invalid integer");
        }
    }
    let limit = if negative {
        i64::MAX as u64 + 1
    } else {
        i64::MAX as u64
    };
    let mut value: u64 = 0;
    while pos < end {
        let digit = slice[pos];
        if !digit.is_ascii_digit() {
            return parse_error(b"invalid integer");
        }
        let digit_value = (digit - b'0') as u64;
        if value > (limit - digit_value) / 10 {
            return parse_error(b"integer overflow");
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
    result_ok(parsed)
}

/// Parse a string to a float, as the (is_ok, payload) register pair that
/// parse_int returns, with the payload holding the f64's bit pattern so every
/// value, a signed zero included, round-trips.
///
/// The text is trimmed of whitespace and converted by std's
/// `f64::from_str`: an optional sign, digits with an optional fraction
/// (`5`, `5.`, `.5`, `5.25`) and an optional exponent. A value too small
/// for the type rounds to a zero of the text's sign and is a success. The
/// failures are text that is not a number (`invalid float`), an exponent
/// past the type's range (`float out of range`), and the spellings of a
/// NaN or an infinity (`float is not finite`), because a parsed float is
/// always finite.
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_parse_float(s: *const i8) -> ResultPair {
    if s.is_null() {
        return parse_error(b"invalid float");
    }
    let len = crate::string::pith_cstring_len(s) as usize;
    let slice = std::slice::from_raw_parts(s as *const u8, len);
    let Ok(text) = std::str::from_utf8(slice) else {
        return parse_error(b"invalid float");
    };
    let text = text.trim();
    let Ok(value) = text.parse::<f64>() else {
        return parse_error(b"invalid float");
    };
    if !value.is_finite() {
        // from_str spells a NaN or an infinity with letters, so text with
        // no letter but the exponent's that comes back infinite overflowed
        if text
            .bytes()
            .any(|b| b.is_ascii_alphabetic() && b != b'e' && b != b'E')
        {
            return parse_error(b"float is not finite");
        }
        return parse_error(b"float out of range");
    }
    result_ok(value.to_bits() as i64)
}

/// Base64-encode a C string into a new C string.
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_b64_encode(s: *const i8) -> *mut i8 {
    if s.is_null() {
        return std::ptr::null_mut();
    }

    let len = crate::string::pith_cstring_len(s) as usize;
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

    crate::pith_copy_bytes_to_cstring(&out[..out.len() - 1])
}

/// Hex-encode a C string into a new C string.
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_hex_encode(s: *const i8) -> *mut i8 {
    if s.is_null() {
        return std::ptr::null_mut();
    }

    let len = crate::string::pith_cstring_len(s) as usize;
    let input = std::slice::from_raw_parts(s as *const u8, len);
    let ptr = crate::pith_alloc_cstring(len * 2) as *mut u8;

    for (i, &byte) in input.iter().enumerate() {
        let hi = (byte >> 4) as usize;
        let lo = (byte & 0xf) as usize;
        const HEX: &[u8] = b"0123456789abcdef";
        *ptr.add(i * 2) = HEX[hi];
        *ptr.add(i * 2 + 1) = HEX[lo];
    }
    ptr as *mut i8
}

/// Decode a hex string back to the original string
///
/// # Safety
/// s must be a valid null-terminated C string of hex digits
#[no_mangle]
pub unsafe extern "C" fn pith_from_hex(s: *const i8) -> *mut i8 {
    if s.is_null() {
        return std::ptr::null_mut();
    }

    let len = crate::string::pith_cstring_len(s) as usize;
    if len % 2 != 0 {
        return std::ptr::null_mut();
    }
    let input = std::slice::from_raw_parts(s as *const u8, len);
    let out_len = len / 2;
    let ptr = crate::pith_alloc_cstring(out_len) as *mut u8;

    for i in 0..out_len {
        let hi = hex_digit(input[i * 2]);
        let lo = hex_digit(input[i * 2 + 1]);
        *ptr.add(i) = (hi << 4) | lo;
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
pub unsafe extern "C" fn pith_int_to_hex(n: i64) -> *mut i8 {
    let s = format!("{:x}", n);
    crate::pith_copy_bytes_to_cstring(s.as_bytes())
}

/// Convert an integer to octal string (e.g., 8 → "10")
///
/// # Safety
/// Returns heap-allocated null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_int_to_oct(n: i64) -> *mut i8 {
    let s = format!("{:o}", n);
    crate::pith_copy_bytes_to_cstring(s.as_bytes())
}

/// Convert an integer to binary string (e.g., 10 → "1010")
///
/// # Safety
/// Returns heap-allocated null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_int_to_bin(n: i64) -> *mut i8 {
    let s = format!("{:b}", n);
    crate::pith_copy_bytes_to_cstring(s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::{pith_parse_float, pith_parse_int};
    use crate::collections::alloc_probe::allocations_during;
    use crate::ffi_util::unbox_result;
    use std::ffi::CString;

    // (is_ok, the value) with 0 standing in for the value of a failure
    fn parse(input: &str) -> (bool, i64) {
        let c_input = CString::new(input).unwrap();
        match unsafe { unbox_result(pith_parse_int(c_input.as_ptr())) } {
            Ok(value) => (true, value),
            Err(_) => (false, 0),
        }
    }

    // a successful parse hands its value back in two registers: no result box
    // and no allocation of any kind. more calls than the struct pool can hold
    // blocks for, with nothing released in between, so a box would have to
    // come from the allocator on most of them.
    #[test]
    fn a_successful_parse_allocates_nothing() {
        let int_text = CString::new("12345").unwrap();
        let float_text = CString::new("2.5").unwrap();
        let count = allocations_during(|| {
            for _ in 0..2000 {
                let int_pair = unsafe { pith_parse_int(int_text.as_ptr()) };
                assert_eq!((int_pair.is_ok, int_pair.payload), (1, 12345));
                let float_pair = unsafe { pith_parse_float(float_text.as_ptr()) };
                assert_eq!(
                    (float_pair.is_ok, float_pair.payload),
                    (1, 2.5f64.to_bits() as i64)
                );
            }
        });
        assert_eq!(count, 0);
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

    // the bits of the ok value, or the error message
    fn parse_f(input: &str) -> Result<u64, String> {
        let c_input = CString::new(input).unwrap();
        unsafe { unbox_result(pith_parse_float(c_input.as_ptr())) }.map(|bits| bits as u64)
    }

    #[test]
    fn parse_float_accepts_zeros_with_their_sign() {
        assert_eq!(parse_f("0"), Ok(0.0f64.to_bits()));
        assert_eq!(parse_f("0.0"), Ok(0.0f64.to_bits()));
        assert_eq!(parse_f("-0.0"), Ok((-0.0f64).to_bits()));
        assert_eq!(parse_f("1e-400"), Ok(0.0f64.to_bits()));
        assert_eq!(parse_f("-1e-400"), Ok((-0.0f64).to_bits()));
    }

    #[test]
    fn parse_float_accepts_trimmed_values() {
        assert_eq!(parse_f("1.5"), Ok(1.5f64.to_bits()));
        assert_eq!(parse_f(" -2.5e3\n"), Ok((-2500.0f64).to_bits()));
        assert_eq!(parse_f(".5"), Ok(0.5f64.to_bits()));
        assert_eq!(parse_f("1.7976931348623157e308"), Ok(f64::MAX.to_bits()));
    }

    #[test]
    fn parse_float_rejects_malformed_and_non_finite_text() {
        let err = |m: &str| Err(m.to_string());
        assert_eq!(parse_f(""), err("invalid float"));
        assert_eq!(parse_f("abc"), err("invalid float"));
        assert_eq!(parse_f("1.5x"), err("invalid float"));
        assert_eq!(parse_f("1e400"), err("float out of range"));
        assert_eq!(parse_f("-1e400"), err("float out of range"));
        assert_eq!(parse_f("inf"), err("float is not finite"));
        assert_eq!(parse_f("-Infinity"), err("float is not finite"));
        assert_eq!(parse_f("NaN"), err("float is not finite"));
    }
}
