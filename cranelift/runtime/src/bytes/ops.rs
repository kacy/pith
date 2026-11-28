//! Immutable `Bytes` operations, raw codegen word loads/stores, and RNG helpers.

use super::{pith_bytes_from_vec, pith_bytes_retain, with_bytes, with_bytes2};
use crate::ffi_util::with_cstr_bytes;

#[inline(always)]
fn bytes_from_slice(bytes: &[u8]) -> i64 {
    pith_bytes_from_vec(bytes.to_vec())
}

#[no_mangle]
pub unsafe extern "C" fn pith_bytes_from_string_utf8(s: *const i8) -> i64 {
    // SAFETY: `s` is null or a valid null-terminated C string for this call.
    unsafe { with_cstr_bytes(s, bytes_from_slice) }
        .unwrap_or_else(|| pith_bytes_from_vec(Vec::new()))
}

#[inline(always)]
fn bytes_to_string_utf8_impl(data: &[u8]) -> *mut i8 {
    if std::str::from_utf8(data).is_err() {
        return std::ptr::null_mut();
    }
    crate::pith_copy_bytes_to_cstring(data)
}

#[no_mangle]
pub unsafe extern "C" fn pith_bytes_to_string_utf8(handle: i64) -> *mut i8 {
    // SAFETY: `handle` is validated by `with_bytes` before borrowing its slice.
    unsafe { with_bytes(handle, bytes_to_string_utf8_impl) }.unwrap_or(std::ptr::null_mut())
}

#[inline(always)]
fn bytes_substring_utf8_impl(data: &[u8], start: i64, end: i64) -> *mut i8 {
    let len = data.len() as i64;
    // an inverted range is empty, not reversed: clamping `end` up to `start`
    // matches pith_cstring_substring and pith_list_slice. swapping the two
    // instead would hand back bytes the caller never asked for, which for a
    // parser walking a buffer is wrong data rather than no data.
    let start_idx = start.max(0).min(len);
    let end_idx = end.max(start_idx).min(len);
    let slice = &data[start_idx as usize..end_idx as usize];
    if std::str::from_utf8(slice).is_err() {
        return std::ptr::null_mut();
    }
    crate::pith_copy_bytes_to_cstring(slice)
}

/// Slice a byte range and decode it as UTF-8 in one step, returning a
/// pith C-string (null on invalid UTF-8). This avoids the intermediate
/// Bytes handle that `slice` then `to_string_utf8` would allocate and
/// register, a real cost in tight scanning loops (json, csv).
#[no_mangle]
pub unsafe extern "C" fn pith_bytes_substring_utf8(handle: i64, start: i64, end: i64) -> *mut i8 {
    // SAFETY: `handle` is validated by `with_bytes` before borrowing its slice.
    unsafe { with_bytes(handle, |data| bytes_substring_utf8_impl(data, start, end)) }
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub unsafe extern "C" fn pith_bytes_len(handle: i64) -> i64 {
    // SAFETY: `handle` is validated by `with_bytes` before borrowing its slice.
    unsafe { with_bytes(handle, |data| data.len() as i64) }.unwrap_or(0)
}

/// The data pointer of a bytes handle, or 0 for a handle `pith_bytes_ref`
/// refuses. The typed json decoder the compiler generates reads its input
/// through this pointer with `pith_load_u8`, checking every index against
/// the length itself, so the handle is tested once per decode rather than
/// once per byte. The codegen inlines both; these are what a build without
/// the inline fast paths (quarantine, sanitizers) calls.
///
/// # Safety
/// handle must be a valid PithBytes or garbage.
#[no_mangle]
pub unsafe extern "C" fn pith_bytes_data_ptr(handle: i64) -> i64 {
    // SAFETY: `handle` is validated by `with_bytes` before reading its data pointer.
    unsafe { with_bytes(handle, |data| data.as_ptr() as i64) }.unwrap_or(0)
}

/// The byte at `ptr + idx`, unchecked: the caller holds the bytes alive
/// and has compared `idx` against their length (see `pith_bytes_data_ptr`).
/// The wider forms read 2, 4 or 8 bytes there as one little-endian
/// integer, zero-extended, with no alignment asked of the address.
///
/// # Safety
/// The bytes `ptr + idx` up to the width must be readable.
#[no_mangle]
pub unsafe extern "C" fn pith_load_u8(ptr: i64, idx: i64) -> i64 {
    // SAFETY: Caller guarantees the byte at `ptr + idx` is readable.
    unsafe { *(ptr as *const u8).offset(idx as isize) as i64 }
}

/// # Safety
/// As `pith_load_u8`.
#[no_mangle]
pub unsafe extern "C" fn pith_load_u16(ptr: i64, idx: i64) -> i64 {
    // SAFETY: Caller guarantees 2 bytes at `ptr + idx` are readable.
    let raw = unsafe { std::ptr::read_unaligned((ptr as *const u8).offset(idx as isize) as *const u16) };
    u16::from_le(raw) as i64
}

/// # Safety
/// As `pith_load_u8`.
#[no_mangle]
pub unsafe extern "C" fn pith_load_u32(ptr: i64, idx: i64) -> i64 {
    // SAFETY: Caller guarantees 4 bytes at `ptr + idx` are readable.
    let raw = unsafe { std::ptr::read_unaligned((ptr as *const u8).offset(idx as isize) as *const u32) };
    u32::from_le(raw) as i64
}

/// # Safety
/// As `pith_load_u8`.
#[no_mangle]
pub unsafe extern "C" fn pith_load_u64(ptr: i64, idx: i64) -> i64 {
    // SAFETY: Caller guarantees 8 bytes at `ptr + idx` are readable.
    let raw = unsafe { std::ptr::read_unaligned((ptr as *const u8).offset(idx as isize) as *const u64) };
    u64::from_le(raw) as i64
}

/// Store `value` as the 8 bytes at `ptr + off`, the mirror of
/// `pith_load_u64`: the generated json decoder writes a struct slot whose
/// index it holds in a variable with it.
///
/// # Safety
/// The 8 bytes at `ptr + off` must be writable.
#[no_mangle]
pub unsafe extern "C" fn pith_store_u64(ptr: i64, off: i64, value: i64) {
    // SAFETY: Caller guarantees 8 bytes at `ptr + off` are writable.
    unsafe { std::ptr::write_unaligned((ptr as *mut u8).offset(off as isize) as *mut i64, value) };
}

/// A fresh counted string holding the `len` bytes at `ptr`, copied as
/// they are: no UTF-8 check and no escape decoding. The typed json decoder
/// mints a flat struct's string fields with it.
///
/// # Safety
/// `len` bytes at `ptr` must be readable.
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_from_raw(ptr: i64, len: i64) -> i64 {
    if len <= 0 {
        return crate::pith_copy_bytes_to_cstring(&[]) as i64;
    }
    // SAFETY: Caller guarantees `len` bytes at `ptr` are readable.
    let slice = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    crate::pith_copy_bytes_to_cstring(slice) as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_bytes_is_empty(handle: i64) -> i64 {
    // SAFETY: `handle` is validated by `with_bytes` before borrowing its slice.
    let is_empty = unsafe { with_bytes(handle, <[u8]>::is_empty) }.unwrap_or(true);
    i64::from(is_empty)
}

#[inline(always)]
fn bytes_get_impl(data: &[u8], idx: i64) -> i64 {
    if idx < 0 {
        return 0;
    }
    data.get(idx as usize).copied().unwrap_or(0) as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_bytes_get(handle: i64, idx: i64) -> i64 {
    // SAFETY: `handle` is validated by `with_bytes` before borrowing its slice.
    unsafe { with_bytes(handle, |data| bytes_get_impl(data, idx)) }.unwrap_or(0)
}

#[inline(always)]
fn bytes_get_strict_impl(data: &[u8], idx: i64) -> i64 {
    let len = data.len() as i64;
    if idx < 0 || idx >= len {
        crate::failure::fail(format!(
            "bytes index out of bounds: {} for bytes of length {}",
            idx, len
        ));
    }
    data[idx as usize] as i64
}

/// Strict indexed byte access for `bytes[i]`. Aborts with a structured
/// diagnostic on out-of-bounds instead of returning silent 0.
#[no_mangle]
pub unsafe extern "C" fn pith_bytes_get_strict(handle: i64, idx: i64) -> i64 {
    // SAFETY: `handle` is validated by `with_bytes` before borrowing its slice.
    let Some(byte) = (unsafe { with_bytes(handle, |data| bytes_get_strict_impl(data, idx)) }) else {
        eprintln!("pith runtime error: bytes indexing on invalid bytes handle");
        // panic-guard: strict bytes indexing on an invalid handle is a program bug with no value to return.
        std::process::exit(1);
    };
    byte
}

enum SliceOutcome {
    ShareWhole,
    Owned(i64),
}

#[inline(always)]
fn bytes_slice_impl(data: &[u8], start: i64, end: i64) -> SliceOutcome {
    let len = data.len() as i64;
    // see the note in pith_bytes_substring_utf8: an inverted range is empty.
    let start_idx = start.max(0).min(len);
    let end_idx = end.max(start_idx).min(len);
    // bytes values are immutable, so the whole range can share the value
    if start_idx == 0 && end_idx == len {
        return SliceOutcome::ShareWhole;
    }
    SliceOutcome::Owned(pith_bytes_from_vec(
        data[start_idx as usize..end_idx as usize].to_vec(),
    ))
}

#[no_mangle]
pub unsafe extern "C" fn pith_bytes_slice(handle: i64, start: i64, end: i64) -> i64 {
    // SAFETY: `handle` is validated by `with_bytes` before borrowing its slice.
    match unsafe { with_bytes(handle, |data| bytes_slice_impl(data, start, end)) } {
        None => 0,
        Some(SliceOutcome::ShareWhole) => {
            // SAFETY: `handle` was just validated as a live `PithBytes`.
            unsafe { pith_bytes_retain(handle) };
            handle
        }
        Some(SliceOutcome::Owned(new_handle)) => new_handle,
    }
}

enum ConcatOutcome {
    ShareLeft,
    ShareRight,
    Owned(i64),
}

#[inline(always)]
fn bytes_concat_impl(a_data: &[u8], b_data: &[u8]) -> ConcatOutcome {
    // bytes values are immutable, so a concat with an empty side can share
    // the other one. a buffered reader concatenates each read onto what it
    // has cached, which is usually nothing.
    if a_data.is_empty() {
        return ConcatOutcome::ShareRight;
    }
    if b_data.is_empty() {
        return ConcatOutcome::ShareLeft;
    }
    let mut out = Vec::with_capacity(a_data.len() + b_data.len());
    out.extend_from_slice(a_data);
    out.extend_from_slice(b_data);
    ConcatOutcome::Owned(pith_bytes_from_vec(out))
}

#[no_mangle]
pub unsafe extern "C" fn pith_bytes_concat(a: i64, b: i64) -> i64 {
    // SAFETY: Both handles are validated by `with_bytes2` before borrowing their slices.
    match unsafe { with_bytes2(a, b, bytes_concat_impl) } {
        None => 0,
        Some(ConcatOutcome::ShareLeft) => {
            // SAFETY: `a` was just validated as a live `PithBytes`.
            unsafe { pith_bytes_retain(a) };
            a
        }
        Some(ConcatOutcome::ShareRight) => {
            // SAFETY: `b` was just validated as a live `PithBytes`.
            unsafe { pith_bytes_retain(b) };
            b
        }
        Some(ConcatOutcome::Owned(new_handle)) => new_handle,
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_bytes_eq(a: i64, b: i64) -> i64 {
    if a == 0 && b == 0 {
        return 1;
    }
    // SAFETY: Both handles are validated by `with_bytes2` before comparing slices.
    let eq = unsafe { with_bytes2(a, b, |left, right| left == right) }.unwrap_or(false);
    i64::from(eq)
}

#[inline(always)]
fn constant_time_eq_impl(a_bytes: &[u8], b_bytes: &[u8]) -> i64 {
    let max_len = a_bytes.len().max(b_bytes.len());
    // fold the length difference in without narrowing it: casting the xor to
    // u8 dropped the high bits, so two inputs whose lengths differ by an exact
    // multiple of 256 (e.g. 16 vs 272) started the accumulator at 0 and could
    // then compare equal if the longer input's extra bytes were all zero.
    let mut diff = ((a_bytes.len() ^ b_bytes.len() != 0) as u8) * 0xff;

    for i in 0..max_len {
        let left = a_bytes.get(i).copied().unwrap_or(0);
        let right = b_bytes.get(i).copied().unwrap_or(0);
        diff |= left ^ right;
    }

    i64::from(diff == 0)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_constant_time_eq(a: i64, b: i64) -> i64 {
    // an invalid handle (null, or a freed/fabricated id) must never read as a
    // match: this is the primitive that gates every MAC, tag, and password
    // comparison. degrading a bad handle to an empty slice made two invalid
    // handles compare EQUAL (both len 0). fail closed instead. this branch is
    // on handle validity, not on secret content, so it leaks no timing about
    // the compared values.
    // SAFETY: Both handles are validated by `with_bytes2` before borrowing their slices.
    unsafe { with_bytes2(a, b, constant_time_eq_impl) }.unwrap_or(0)
}

/// Maximum single secure-random draw (64 MiB): prevents an untrusted or buggy
/// caller from triggering an unrecoverable OOM abort in `vec![0; len]`.
pub(super) const MAX_SECURE_RANDOM_BYTES: i64 = 64 * 1024 * 1024;

#[no_mangle]
pub extern "C" fn pith_secure_random_bytes(count: i64) -> i64 {
    if !(0..=MAX_SECURE_RANDOM_BYTES).contains(&count) {
        return 0;
    }
    let len = count as usize;
    let mut out = Vec::new();
    if out.try_reserve_exact(len).is_err() {
        return 0;
    }
    out.resize(len, 0_u8);
    if len == 0 {
        return pith_bytes_from_vec(out);
    }

    if fill_secure_random(&mut out) {
        pith_bytes_from_vec(out)
    } else {
        0
    }
}

/// fill `out` from the kernel's random source. on linux that is one
/// `getrandom` call: opening `/dev/urandom` per request cost an open, a read
/// and a close, and the open and close contend on the process's fd table, so
/// 32 tasks minting trace ids spent most of their time there.
#[cfg(target_os = "linux")]
fn fill_secure_random(out: &mut [u8]) -> bool {
    let mut filled = 0;
    while filled < out.len() {
        // SAFETY: the pointer and length name the unfilled tail of `out`.
        let n = unsafe {
            libc::getrandom(
                out[filled..].as_mut_ptr() as *mut libc::c_void,
                out.len() - filled,
                0,
            )
        };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return false;
        }
        filled += n as usize;
    }
    true
}

#[cfg(not(target_os = "linux"))]
fn fill_secure_random(out: &mut [u8]) -> bool {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(out))
        .is_ok()
}
#[inline(always)]
fn bytes_read_word_impl(data: &[u8], off: i64, count: i64) -> i64 {
    if off < 0 || !(1..=8).contains(&count) {
        return 0;
    }
    let off = off as usize;
    let count = count as usize;
    let Some(end) = off.checked_add(count) else {
        return 0;
    };
    if end > data.len() {
        return 0;
    }
    let mut le = [0u8; 8];
    le[..count].copy_from_slice(&data[off..end]);
    u64::from_le_bytes(le) as i64
}

/// Read `count` bytes at `off` as one little-endian integer, the mirror
/// of `write_word`. A word-at-a-time consumer (the xxhash kernel, the
/// bitstream refill) pays one call here instead of a byte loop. Returns 0
/// on a bad handle, a count outside 1..=8, or a range outside the bytes;
/// callers needing the zero-past-the-end tolerance check bounds first.
///
/// # Safety
/// handle must be a valid PithBytes or garbage.
#[no_mangle]
pub unsafe extern "C" fn pith_bytes_read_word(handle: i64, off: i64, count: i64) -> i64 {
    // SAFETY: `handle` is validated by `with_bytes` before borrowing its slice.
    unsafe { with_bytes(handle, |data| bytes_read_word_impl(data, off, count)) }.unwrap_or(0)
}
