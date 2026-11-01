//! The string entry points in string_list.rs, utility.rs, string.rs and the
//! substring search behind them, called with arbitrary bytes and arbitrary
//! (negative, huge, off-by-one) indexes, each checked against a plain
//! slice-based model.
#![no_main]

use pith_runtime::collections::list::{pith_list_get_value, pith_list_len, pith_list_release_handle, PithList};
use pith_runtime::string::{
    pith_string_contains, pith_string_ends_with, pith_string_len, pith_string_new, pith_string_release,
    pith_string_starts_with, pith_string_substring, pith_string_trim, PithString,
};
use pith_runtime_fuzz::{cstr_vec, fuzz_target, take_cstr, Input, PStr};

/// The most bytes a call may be asked to produce. Past this a padding or a
/// repeat is an allocation the program asked for, not a runtime defect.
const MAX_OUT: usize = 1 << 16;

fn find_from(h: &[u8], n: &[u8], start: usize) -> Option<usize> {
    if start > h.len() {
        return None;
    }
    if n.is_empty() {
        return Some(start);
    }
    h[start..].windows(n.len()).position(|w| w == n).map(|at| start + at)
}

fn model_split(s: &[u8], d: &[u8]) -> Vec<Vec<u8>> {
    let mut parts = Vec::new();
    let mut start = 0;
    if !d.is_empty() {
        while let Some(at) = find_from(s, d, start) {
            parts.push(s[start..at].to_vec());
            start = at + d.len();
        }
    }
    parts.push(s[start..].to_vec());
    parts.retain(|p| !p.is_empty());
    parts
}

fn model_replace(s: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    if from.is_empty() {
        return s.to_vec();
    }
    let mut out = Vec::new();
    let mut copied = 0;
    while let Some(at) = find_from(s, from, copied) {
        out.extend_from_slice(&s[copied..at]);
        out.extend_from_slice(to);
        copied = at + from.len();
    }
    out.extend_from_slice(&s[copied..]);
    out
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// Whether `index` falls strictly inside a well-formed utf-8 character, the
/// cut pith_cstring_substring refuses by exiting the program. A copy of the
/// runtime's rule, so the harness only makes the calls that return.
fn splits_a_character(bytes: &[u8], index: usize) -> bool {
    if index == 0 || index >= bytes.len() || (bytes[index] & 0xC0) != 0x80 {
        return false;
    }
    let mut lead = index;
    while lead > 0 && (bytes[lead] & 0xC0) == 0x80 && index - lead < 3 {
        lead -= 1;
    }
    let width = match bytes[lead] {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return false,
    };
    lead + width <= bytes.len() && std::str::from_utf8(&bytes[lead..lead + width]).is_ok() && index < lead + width
}

fn clamp(len: usize, start: i64, end: i64) -> (usize, usize) {
    let len = len as i64;
    let lo = start.max(0).min(len);
    let hi = end.max(lo).min(len);
    (lo as usize, hi as usize)
}

/// The strings in a string list, read in place.
unsafe fn list_strings(handle: i64) -> Vec<Vec<u8>> {
    let list = PithList { ptr: handle as *mut () };
    (0..pith_list_len(list))
        .map(|i| cstr_vec(pith_list_get_value(list, i) as *const i8).expect("list element"))
        .collect()
}

unsafe fn take_list(handle: i64) -> Vec<Vec<u8>> {
    let strings = list_strings(handle);
    pith_list_release_handle(handle);
    strings
}

unsafe fn pstring_bytes(s: PithString) -> Vec<u8> {
    let len = pith_string_len(s);
    if len <= 0 {
        return Vec::new();
    }
    std::slice::from_raw_parts(s.ptr, len as usize).to_vec()
}

unsafe fn take_pstring(s: PithString) -> Vec<u8> {
    let bytes = pstring_bytes(s);
    pith_string_release(s);
    bytes
}

fuzz_target!(|data| {
    let mut input = Input::new(data);
    let op = input.byte();
    let kind = input.byte();
    unsafe {
        match op % 13 {
            0 => {
                let text = input.chunk();
                let a = input.index(text.len());
                let b = input.index(text.len());
                let s = PStr::new(text, kind);
                let content = s.content();
                let (lo, hi) = clamp(content.len(), a, b);
                if splits_a_character(&content, lo) || splits_a_character(&content, hi) {
                    return 0;
                }
                let got = take_cstr(pith_runtime::pith_cstring_substring(s.ptr(), a, b));
                assert_eq!(got.as_deref(), Some(&content[lo..hi]), "substring({a}, {b})");
            }
            1 => {
                let text = input.chunk();
                let i = input.index(text.len());
                let s = PStr::new(text, kind);
                let content = s.content();
                let want = usize::try_from(i).ok().and_then(|i| content.get(i)).copied();
                let got = take_cstr(pith_runtime::pith_cstring_char_at(s.ptr(), i)).expect("char_at");
                assert_eq!(got, want.map(|b| vec![b]).unwrap_or_default());
                assert_eq!(pith_runtime::pith_cstring_byte_at(s.ptr(), i), want.map_or(-1, |b| b as i64));
                let opt = pith_runtime::pith_cstring_char_at_opt(s.ptr(), i);
                if let Some(b) = want {
                    let tuple = opt as *const i64;
                    assert_eq!(*tuple, 1);
                    assert_eq!(take_cstr(*tuple.add(1) as *mut i8), Some(vec![b]));
                    pith_runtime::pith_struct_release(opt);
                    let strict = pith_runtime::pith_cstring_char_at_strict(s.ptr(), i);
                    assert_eq!(take_cstr(strict), Some(vec![b]));
                } else {
                    assert_eq!(opt, 0);
                }
            }
            2 => {
                let text = input.chunk();
                let delim = input.chunk();
                let a = input.int();
                let b = input.int();
                let s = PStr::new(text, kind);
                let d = PStr::new(delim, kind >> 2);
                let want = model_split(&s.content(), &d.content());
                let list = pith_runtime::pith_string_split_to_list(s.ptr(), d.ptr());
                let handle = list.ptr as i64;
                assert_eq!(list_strings(handle), want);
                // a slice clamps both ends into the list
                let n = want.len() as i64;
                let (lo, hi) = (a.max(0).min(n) as usize, b.max(0).min(n) as usize);
                let slice = take_list(pith_runtime::pith_list_slice(handle, a, b));
                assert_eq!(slice, if lo < hi { want[lo..hi].to_vec() } else { Vec::new() });
                let mut sorted = want.clone();
                sorted.sort();
                assert_eq!(take_list(pith_runtime::pith_list_sort_strings_copy(handle)), sorted);
                pith_runtime::pith_list_sort_strings(handle);
                assert_eq!(take_list(handle), sorted);
            }
            3 => {
                let s = PStr::new(input.chunk(), kind);
                let from = PStr::new(input.chunk(), kind >> 2);
                let to = PStr::new(input.rest(), kind >> 4);
                let want = model_replace(&s.content(), &from.content(), &to.content());
                if want.len() > MAX_OUT {
                    return 0;
                }
                let got = take_cstr(pith_runtime::pith_cstring_replace(s.ptr(), from.ptr(), to.ptr()));
                assert_eq!(got, Some(want));
            }
            4 => {
                let h = PStr::new(input.chunk(), kind);
                let n = PStr::new(input.rest(), kind >> 2);
                let (hc, nc) = (h.content(), n.content());
                let first = find_from(&hc, &nc, 0);
                assert_eq!(pith_runtime::pith_cstring_index_of(h.ptr(), n.ptr()), first.map_or(-1, |i| i as i64));
                assert_eq!(pith_runtime::pith_cstring_contains(h.ptr(), n.ptr()), first.is_some() as i64);
                assert_eq!(pith_runtime::pith_cstring_starts_with(h.ptr(), n.ptr()), hc.starts_with(&nc) as i64);
                assert_eq!(pith_runtime::pith_cstring_ends_with(h.ptr(), n.ptr()), hc.ends_with(&nc) as i64);
                let last = match (std::str::from_utf8(&hc), std::str::from_utf8(&nc)) {
                    (Ok(hs), Ok(ns)) => hs.rfind(ns).map_or(-1, |i| i as i64),
                    _ => -1,
                };
                assert_eq!(pith_runtime::pith_cstring_last_index_of(h.ptr(), n.ptr()), last);
            }
            5 => {
                let s = PStr::new(input.rest(), kind);
                let c = s.content();
                let start = c.iter().position(|&b| !is_ws(b)).unwrap_or(c.len());
                let end = c.iter().rposition(|&b| !is_ws(b)).map_or(start, |i| i + 1);
                assert_eq!(take_cstr(pith_runtime::pith_cstring_trim(s.ptr())), Some(c[start..end].to_vec()));
                assert_eq!(take_cstr(pith_runtime::pith_cstring_trim_left(s.ptr())), Some(c[start..].to_vec()));
                assert_eq!(pith_runtime::pith_cstring_is_empty(s.ptr()), c.is_empty() as i64);
            }
            6 => {
                let s = PStr::new(input.rest(), kind);
                let c = s.content();
                assert_eq!(take_cstr(pith_runtime::pith_cstring_to_upper(s.ptr())), Some(c.to_ascii_uppercase()));
                assert_eq!(take_cstr(pith_runtime::pith_cstring_to_lower(s.ptr())), Some(c.to_ascii_lowercase()));
                let mut rev = c.clone();
                rev.reverse();
                assert_eq!(take_cstr(pith_runtime::pith_cstring_reverse(s.ptr())), Some(rev));
                let chars = take_list(pith_runtime::pith_cstring_chars(s.ptr()));
                assert_eq!(chars, c.iter().map(|&b| vec![b]).collect::<Vec<_>>());
            }
            7 => {
                let text = input.chunk();
                let width = input.index(text.len());
                let fill = PStr::new(input.rest(), kind >> 2);
                let s = PStr::new(text, kind);
                let c = s.content();
                if width > MAX_OUT as i64 {
                    return 0;
                }
                let fill_byte = match fill.content().first() {
                    Some(&b) if b != 0 => b,
                    _ => b' ',
                };
                let pad = (width.max(0) as usize).saturating_sub(c.len());
                let mut left = vec![fill_byte; pad];
                left.extend_from_slice(&c);
                let mut right = c.clone();
                right.extend(std::iter::repeat_n(fill_byte, pad));
                assert_eq!(take_cstr(pith_runtime::pith_cstring_pad_left(s.ptr(), width, fill.ptr())), Some(left));
                assert_eq!(take_cstr(pith_runtime::pith_cstring_pad_right(s.ptr(), width, fill.ptr())), Some(right));
            }
            8 => {
                let n = input.int();
                let s = PStr::new(input.rest(), kind);
                let c = s.content();
                let want = match c.len().checked_mul(n.max(0) as usize) {
                    Some(total) if total > MAX_OUT => return 0,
                    Some(_) if n > 0 => c.repeat(n as usize),
                    _ => Vec::new(),
                };
                assert_eq!(take_cstr(pith_runtime::pith_cstring_repeat(s.ptr(), n)), Some(want));
            }
            9 => {
                let a = PStr::new(input.chunk(), kind);
                let b = PStr::new(input.rest(), kind >> 2);
                let (ac, bc) = (a.content(), b.content());
                let mut joined = ac.clone();
                joined.extend_from_slice(&bc);
                assert_eq!(take_cstr(pith_runtime::pith_concat_cstr(a.ptr(), b.ptr())), Some(joined));
                assert_eq!(pith_runtime::pith_cstring_eq(a.ptr(), b.ptr()), (ac == bc) as i64);
                let order = pith_runtime::pith_cstring_compare(a.ptr(), b.ptr());
                assert_eq!(order.signum(), ac.cmp(&bc) as i64);
            }
            10 => {
                let width = input.int();
                let align = input.byte();
                let fill = input.byte();
                let s = PStr::new(input.rest(), kind);
                if width > MAX_OUT as i64 {
                    return 0;
                }
                // format_pad reads its argument as a C string, to the first NUL
                let c = s.content();
                let text = c.split(|&b| b == 0).next().unwrap_or(&[]);
                let lossy = String::from_utf8_lossy(text).into_owned();
                let got = pith_runtime::pith_format_pad(s.ptr() as i64, width, align as i64, fill as i64);
                let got = String::from_utf8(take_cstr(got as *mut i8).expect("format_pad")).expect("utf-8");
                let width = width.max(0) as usize;
                if lossy.chars().count() >= width {
                    assert_eq!(got, lossy);
                } else {
                    assert_eq!(got.chars().count(), width);
                }
            }
            11 => {
                // the struct-shaped strings of string.rs
                let text = input.chunk();
                let a = input.index(text.len());
                let b = input.index(text.len());
                let needle = input.rest();
                let s = pith_string_new(text.as_ptr(), text.len() as i64);
                let n = pith_string_new(needle.as_ptr(), needle.len() as i64);
                let sc = pstring_bytes(s);
                let nc = pstring_bytes(n);
                match std::str::from_utf8(text) {
                    Ok(_) => assert_eq!(sc, text),
                    Err(_) => assert!(sc.is_empty()),
                }
                let sub = take_pstring(pith_string_substring(s, a, b));
                let want = std::str::from_utf8(&sc)
                    .ok()
                    .filter(|_| a >= 0 && b <= sc.len() as i64 && a < b)
                    .and_then(|t| t.get(a as usize..b as usize))
                    .map(|t| t.as_bytes().to_vec())
                    .unwrap_or_default();
                assert_eq!(sub, want);
                let trimmed = take_pstring(pith_string_trim(s));
                assert_eq!(trimmed, std::str::from_utf8(&sc).unwrap_or("").trim().as_bytes());
                assert_eq!(pith_string_contains(s, n), find_from(&sc, &nc, 0).is_some());
                assert_eq!(pith_string_starts_with(s, n), sc.starts_with(&nc));
                assert_eq!(pith_string_ends_with(s, n), sc.ends_with(&nc));
                pith_string_release(s);
                pith_string_release(n);
            }
            _ => {
                let s = PStr::new(input.rest(), kind);
                let c = s.content();
                assert_eq!(pith_runtime::string::pith_cstring_len(s.ptr()), c.len() as i64);
                let copy = pith_runtime::pith_strdup(s.ptr());
                assert_eq!(take_cstr(copy), Some(c));
            }
        }
    }
});
