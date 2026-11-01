//! The text codecs in encoding.rs and utility.rs (parse_int, parse_float,
//! base64, hex, the int formatters, sha-256) and the utf-8 conversions
//! between strings and bytes, each checked against a std or ring oracle.
#![no_main]

use pith_runtime::bytes::{pith_bytes_from_string_utf8, pith_bytes_substring_utf8, pith_bytes_to_string_utf8};
use pith_runtime::ffi_util::ResultPair;
use pith_runtime_fuzz::{cstr_vec, fuzz_target, take_bytes, take_cstr, Input, PBytes, PStr};

/// Take a fallible builtin's pair: the ok value, or None after releasing
/// the owned error message.
unsafe fn take_pair(pair: ResultPair) -> Option<i64> {
    if pair.is_ok == 1 {
        return Some(pair.payload);
    }
    assert_eq!(pair.is_ok, 0);
    let message = take_cstr(pair.payload as *mut i8).expect("error message");
    assert!(!message.is_empty());
    None
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(bytes.len());
    let end = bytes.iter().rposition(|b| !b.is_ascii_whitespace()).map_or(start, |i| i + 1);
    &bytes[start..end.max(start)]
}

fn hex_of(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().flat_map(|b| format!("{b:02x}").into_bytes()).collect()
}

/// The clamped byte range bytes_substring_utf8 documents.
fn clamp_range(len: usize, start: i64, end: i64) -> (usize, usize) {
    let len = len as i64;
    let lo = start.max(0).min(len);
    let hi = end.max(lo).min(len);
    (lo as usize, hi as usize)
}

fuzz_target!(|data| {
    let mut input = Input::new(data);
    let op = input.byte();
    let kind = input.byte();
    unsafe {
        match op % 10 {
            0 => {
                let text = PStr::new(input.rest(), kind);
                let content = text.content();
                let got = take_pair(pith_runtime::pith_parse_int(text.ptr()));
                let want = std::str::from_utf8(trim_ascii(&content)).ok().and_then(|t| t.parse::<i64>().ok());
                assert_eq!(got, want, "parse_int({content:?})");
            }
            1 => {
                let text = PStr::new(input.rest(), kind);
                let content = text.content();
                let got = take_pair(pith_runtime::pith_parse_float(text.ptr()));
                let want = std::str::from_utf8(&content)
                    .ok()
                    .and_then(|t| t.trim().parse::<f64>().ok())
                    .filter(|v| v.is_finite())
                    .map(|v| v.to_bits() as i64);
                assert_eq!(got, want, "parse_float({content:?})");
            }
            2 => {
                // encode then decode is the identity
                let text = PStr::new(input.rest(), kind);
                let content = text.content();
                let encoded = pith_runtime::pith_b64_encode(text.ptr());
                let encoded_bytes = cstr_vec(encoded).expect("b64_encode");
                assert_eq!(encoded_bytes.len(), content.len().div_ceil(3) * 4);
                let decoded = take_cstr(pith_runtime::pith_b64_decode(encoded)).expect("b64_decode");
                pith_runtime::pith_cstring_release(encoded);
                assert_eq!(decoded, content);
            }
            3 => {
                // decode of arbitrary text: the length is fixed by the input
                let text = PStr::new(input.rest(), kind);
                let content = text.content();
                let mut n = content.len();
                while n > 0 && content[n - 1] == b'=' {
                    n -= 1;
                }
                let decoded = take_cstr(pith_runtime::pith_b64_decode(text.ptr())).expect("b64_decode");
                assert_eq!(decoded.len(), n * 3 / 4);
            }
            4 => {
                let text = PStr::new(input.rest(), kind);
                let content = text.content();
                let encoded = pith_runtime::pith_hex_encode(text.ptr());
                assert_eq!(cstr_vec(encoded).expect("hex_encode"), hex_of(&content));
                let decoded = take_cstr(pith_runtime::pith_from_hex(encoded)).expect("from_hex");
                pith_runtime::pith_cstring_release(encoded);
                assert_eq!(decoded, content);
            }
            5 => {
                let text = PStr::new(input.rest(), kind);
                let content = text.content();
                let decoded = take_cstr(pith_runtime::pith_from_hex(text.ptr()));
                if content.len() % 2 == 1 {
                    assert!(decoded.is_none());
                } else {
                    assert_eq!(decoded.expect("from_hex").len(), content.len() / 2);
                }
            }
            6 => {
                let n = input.int();
                assert_eq!(take_cstr(pith_runtime::pith_int_to_hex(n)), Some(format!("{n:x}").into_bytes()));
                assert_eq!(take_cstr(pith_runtime::pith_int_to_oct(n)), Some(format!("{n:o}").into_bytes()));
                assert_eq!(take_cstr(pith_runtime::pith_int_to_bin(n)), Some(format!("{n:b}").into_bytes()));
            }
            7 => {
                let text = PStr::new(input.rest(), kind);
                let content = text.content();
                let got = take_cstr(pith_runtime::pith_sha256(text.ptr())).expect("sha256");
                let want = hex_of(ring::digest::digest(&ring::digest::SHA256, &content).as_ref());
                assert_eq!(got, want);
            }
            8 => {
                // string -> bytes -> string
                let text = PStr::new(input.rest(), kind);
                let content = text.content();
                let handle = pith_bytes_from_string_utf8(text.ptr());
                let back = take_cstr(pith_bytes_to_string_utf8(handle));
                assert_eq!(take_bytes(handle), Some(content.clone()));
                match std::str::from_utf8(&content) {
                    Ok(_) => assert_eq!(back, Some(content)),
                    Err(_) => assert!(back.is_none()),
                }
            }
            _ => {
                let start = input.int();
                let end = input.int();
                let raw = input.rest();
                let value = PBytes::new(raw);
                let got = take_cstr(pith_bytes_substring_utf8(value.0, start, end));
                let (lo, hi) = clamp_range(raw.len(), start, end);
                match std::str::from_utf8(&raw[lo..hi]) {
                    Ok(_) => assert_eq!(got.as_deref(), Some(&raw[lo..hi])),
                    Err(_) => assert!(got.is_none()),
                }
            }
        }
    }
});
