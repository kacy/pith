//! Byte/substring search, SSE2/SWAR multi-byte scanning, prefix/suffix,
//! padding, and string repetition builtins.

use crate::ffi_util::{with_cstr_bytes, with_cstr_bytes2};
use std::mem::MaybeUninit;

fn index_of_impl(h_bytes: &[u8], n_bytes: &[u8]) -> i64 {
    match crate::substring::find(h_bytes, n_bytes) {
        Some(at) => at as i64,
        None => -1,
    }
}

/// Byte offset of the first occurrence of needle in haystack, or -1. An
/// empty needle is found at 0.
///
/// # Safety
/// Both arguments must be null or valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_index_of(haystack: *const i8, needle: *const i8) -> i64 {
    // SAFETY: Both arguments are null or valid null-terminated C strings.
    unsafe { with_cstr_bytes2(haystack, needle, index_of_impl) }.unwrap_or(-1)
}

/// Offset of the first byte of `haystack` that equals any byte of `set`, or
/// `None`. Eight bytes are tested per step: xor with a byte repeated across
/// the word turns a match into a zero byte, and the usual zero-byte test
/// finds it. That test can flag a byte above a real zero byte, never one
/// below it, so the lowest flagged byte across all the set's bytes is a real
/// match. On x86_64 a haystack of sixteen bytes or more takes the sse2
/// version below. Sets longer than four bytes take a 256-bit table instead.
pub(crate) fn find_any_byte(haystack: &[u8], set: &[u8]) -> Option<usize> {
    const LOW: u64 = 0x0101_0101_0101_0101;
    const HIGH: u64 = 0x8080_8080_8080_8080;
    match set.len() {
        0 => return None,
        1 => return crate::substring::find(haystack, set),
        2..=4 => {}
        _ => {
            let mut table = [0u64; 4];
            for &b in set {
                table[(b >> 6) as usize] |= 1 << (b & 63);
            }
            return haystack.iter().position(|&b| table[(b >> 6) as usize] >> (b & 63) & 1 != 0);
        }
    }
    #[cfg(target_arch = "x86_64")]
    if haystack.len() >= 16 {
        return find_any_byte_sse2(haystack, set);
    }
    // unused slots repeat the first byte, so every set runs the same four
    // tests with no loop over its length.
    let mut splat = [LOW * set[0] as u64; 4];
    for (slot, &b) in splat.iter_mut().zip(set) {
        *slot = LOW * b as u64;
    }
    let hits = |w: u64| {
        let mut hit = 0;
        for s in splat {
            let x = w ^ s;
            hit |= x.wrapping_sub(LOW) & !x & HIGH;
        }
        hit
    };
    let mut words = haystack.chunks_exact(8);
    let mut at = 0;
    for word in &mut words {
        let hit = hits(u64::from_le_bytes(word.try_into().unwrap_or([0; 8])));
        if hit != 0 {
            return Some(at + (hit.trailing_zeros() / 8) as usize);
        }
        at += 8;
    }
    let tail = words.remainder();
    if tail.is_empty() {
        return None;
    }
    // the last one to seven bytes. a haystack of eight or more is tested
    // through its final eight bytes: the ones that overlap the word before
    // were just found clean, so the lowest flag is still a real match. a
    // shorter one is assembled into a word a byte at a time (a copy into a
    // buffer would be a call, and reading the buffer back as a word stalls),
    // and flags in the unused bytes are masked off.
    let (word, base, live) = match haystack.last_chunk::<8>() {
        Some(last) => (u64::from_le_bytes(*last), haystack.len() - 8, u64::MAX),
        None => {
            let mut word = 0;
            for (i, &b) in tail.iter().enumerate() {
                word |= (b as u64) << (8 * i);
            }
            (word, 0, u64::MAX >> (64 - 8 * tail.len()))
        }
    };
    let hit = hits(word) & live;
    if hit != 0 {
        return Some(base + (hit.trailing_zeros() / 8) as usize);
    }
    None
}

/// `find_any_byte` for a haystack of sixteen bytes or more and a set of two to
/// four: sixteen bytes per step, one compare per set byte. The last partial
/// block is tested through the haystack's final sixteen bytes, and the ones
/// that overlap the block before were just found clean.
#[cfg(target_arch = "x86_64")]
fn find_any_byte_sse2(haystack: &[u8], set: &[u8]) -> Option<usize> {
    // SAFETY: SSE2 is part of the x86_64 baseline.
    unsafe { find_any_byte_sse2_impl(haystack, set) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
fn find_any_byte_sse2_impl(haystack: &[u8], set: &[u8]) -> Option<usize> {
    use std::arch::x86_64::{
        __m128i, _mm_cmpeq_epi8, _mm_loadu_si128, _mm_movemask_epi8, _mm_or_si128, _mm_set1_epi8,
    };
    let mut needles = [_mm_set1_epi8(set[0] as i8); 4];
    for (slot, &b) in needles.iter_mut().zip(set) {
        *slot = _mm_set1_epi8(b as i8);
    }
    let hits = |at: usize| {
        // SAFETY: Caller only passes `at <= haystack.len() - 16`, so 16 bytes lie inside `haystack`.
        let block = unsafe { _mm_loadu_si128(haystack.as_ptr().add(at) as *const __m128i) };
        let any = _mm_or_si128(
            _mm_or_si128(_mm_cmpeq_epi8(block, needles[0]), _mm_cmpeq_epi8(block, needles[1])),
            _mm_or_si128(_mm_cmpeq_epi8(block, needles[2]), _mm_cmpeq_epi8(block, needles[3])),
        );
        _mm_movemask_epi8(any) as u32
    };
    let last = haystack.len() - 16;
    let mut at = 0;
    while at < last {
        let hit = hits(at);
        if hit != 0 {
            return Some(at + hit.trailing_zeros() as usize);
        }
        at += 16;
    }
    let hit = hits(last);
    if hit != 0 {
        return Some(last + hit.trailing_zeros() as usize);
    }
    None
}

#[inline(always)]
fn index_of_any_impl(h_bytes: &[u8], set_bytes: &[u8]) -> i64 {
    match find_any_byte(h_bytes, set_bytes) {
        Some(at) => at as i64,
        None => -1,
    }
}

/// Byte offset of the first byte of `haystack` that is any of the bytes of
/// `set`, or -1. One pass, where a `contains` per byte walks the string once
/// for each. The set is read as bytes, so it is meant for ascii delimiters.
///
/// # Safety
/// Both arguments must be null or valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_index_of_any(haystack: *const i8, set: *const i8) -> i64 {
    // SAFETY: Both arguments are null or valid null-terminated C strings.
    unsafe { with_cstr_bytes2(haystack, set, index_of_any_impl) }.unwrap_or(-1)
}

#[inline(always)]
fn contains_impl(h_bytes: &[u8], n_bytes: &[u8]) -> i64 {
    i64::from(crate::substring::find(h_bytes, n_bytes).is_some())
}

/// 1 when needle occurs in haystack, else 0. An empty needle is contained
/// in every string.
///
/// # Safety
/// Both arguments must be null or valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_contains(haystack: *const i8, needle: *const i8) -> i64 {
    // SAFETY: Both arguments are null or valid null-terminated C strings.
    unsafe { with_cstr_bytes2(haystack, needle, contains_impl) }.unwrap_or(0)
}

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_starts_with(s: *const i8, prefix: *const i8) -> i64 {
    // SAFETY: Both arguments are null or valid null-terminated C strings.
    let res = unsafe { with_cstr_bytes2(s, prefix, |bytes, p| bytes.starts_with(p)) }.unwrap_or(false);
    i64::from(res)
}

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_ends_with(s: *const i8, suffix: *const i8) -> i64 {
    // SAFETY: Both arguments are null or valid null-terminated C strings.
    let res = unsafe { with_cstr_bytes2(s, suffix, |bytes, suf| bytes.ends_with(suf)) }.unwrap_or(false);
    i64::from(res)
}

#[inline(always)]
unsafe fn fill_byte_or_space(fill: *const i8) -> u8 {
    if fill.is_null() {
        return b' ';
    }
    // SAFETY: Caller guarantees non-null `fill` points to a readable NUL-terminated C string.
    let first = unsafe { *fill } as u8;
    if first != 0 { first } else { b' ' }
}

fn pad_left_impl(bytes: &[u8], width: i64, fill_byte: u8) -> *mut i8 {
    if width <= 0 || bytes.len() >= width as usize {
        return crate::pith_copy_bytes_to_cstring(bytes);
    }
    let w = width as usize;
    if !crate::runtime_core::cstring_len_valid(w) {
        return crate::pith_copy_bytes_to_cstring(bytes);
    }
    let pad = w - bytes.len();
    // SAFETY: `pith_alloc_cstring(w)` returns a fresh writable buffer of `w` bytes plus NUL.
    let ptr = unsafe { crate::pith_alloc_cstring(w) };
    // SAFETY: `ptr` points to `w` writable (uninitialized) bytes.
    let out = unsafe { std::slice::from_raw_parts_mut(ptr as *mut MaybeUninit<u8>, w) };
    out[..pad].fill(MaybeUninit::new(fill_byte));
    for (dst, &b) in out[pad..].iter_mut().zip(bytes) {
        dst.write(b);
    }
    ptr
}

fn pad_right_impl(bytes: &[u8], width: i64, fill_byte: u8) -> *mut i8 {
    if width <= 0 || bytes.len() >= width as usize {
        return crate::pith_copy_bytes_to_cstring(bytes);
    }
    let w = width as usize;
    if !crate::runtime_core::cstring_len_valid(w) {
        return crate::pith_copy_bytes_to_cstring(bytes);
    }
    // SAFETY: `pith_alloc_cstring(w)` returns a fresh writable buffer of `w` bytes plus NUL.
    let ptr = unsafe { crate::pith_alloc_cstring(w) };
    // SAFETY: `ptr` points to `w` writable (uninitialized) bytes.
    let out = unsafe { std::slice::from_raw_parts_mut(ptr as *mut MaybeUninit<u8>, w) };
    for (dst, &b) in out[..bytes.len()].iter_mut().zip(bytes) {
        dst.write(b);
    }
    out[bytes.len()..].fill(MaybeUninit::new(fill_byte));
    ptr
}

fn repeat_impl(bytes: &[u8], n: i64) -> *mut i8 {
    let len = bytes.len();
    // an empty string repeats to the empty string at any count. the copy
    // loop below runs once per repetition, so a huge count of nothing used
    // to spin for as long as the count said.
    if n <= 0 || len == 0 {
        return crate::pith_cstring_empty();
    }
    let Some(total_len) = len
        .checked_mul(n as usize)
        .filter(|&t| crate::runtime_core::cstring_len_valid(t))
    else {
        return crate::pith_cstring_empty();
    };
    // SAFETY: `pith_alloc_cstring(total_len)` returns a fresh writable buffer of
    // `total_len` bytes plus NUL.
    let ptr = unsafe { crate::pith_alloc_cstring(total_len) };
    for i in 0..n as usize {
        // SAFETY: `i * len + len <= total_len`; source and fresh dest do not alias.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr() as *const i8, ptr.add(i * len), len);
        }
    }
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_pad_left(
    s: *const i8,
    width: i64,
    fill: *const i8,
) -> *mut i8 {
    // SAFETY: `fill` is null or a valid null-terminated C string.
    let fill_byte = if width > 0 { unsafe { fill_byte_or_space(fill) } } else { b' ' };
    // SAFETY: `s` is null or a valid null-terminated C string for this call.
    unsafe { with_cstr_bytes(s, |bytes| pad_left_impl(bytes, width, fill_byte)) }
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_pad_right(
    s: *const i8,
    width: i64,
    fill: *const i8,
) -> *mut i8 {
    // SAFETY: `fill` is null or a valid null-terminated C string.
    let fill_byte = if width > 0 { unsafe { fill_byte_or_space(fill) } } else { b' ' };
    // SAFETY: `s` is null or a valid null-terminated C string for this call.
    unsafe { with_cstr_bytes(s, |bytes| pad_right_impl(bytes, width, fill_byte)) }
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_repeat(s: *const i8, n: i64) -> *mut i8 {
    // SAFETY: `s` is null or a valid null-terminated C string for this call.
    unsafe { with_cstr_bytes(s, |bytes| repeat_impl(bytes, n)) }
        .unwrap_or_else(crate::pith_cstring_empty)
}

