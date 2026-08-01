use crate::{ensure_perf_stats_registered, perf_count, PERF_BYTES_ALLOCS, PERF_BYTES_ALLOC_BYTES};
use crate::{PERF_BYTE_BUFFER_NEWS, PERF_BYTE_BUFFER_WRITES, PERF_BYTE_BUFFER_WRITE_BYTES};
use std::io::Read;

#[repr(C)]
pub(crate) struct PithBytes {
    // data_ptr and data_len must stay at offsets 0 and 8: the codegen
    // inlines bytes indexing against this layout (ir_consumer.rs), so the
    // magic word lives at the end.
    pub(crate) data_ptr: *const u8,
    pub(crate) data_len: usize,
    pub(crate) data: Vec<u8>,
    pub(crate) rc: std::sync::atomic::AtomicU32,
    pub(crate) magic: u32,
}

/// Magic word for PithBytes ("PBYT")
pub(crate) const BYTES_MAGIC: u32 = 0x50425954;

/// Magic word for PithByteBuffer ("PBUF")
const BYTE_BUFFER_MAGIC: u32 = 0x50425546;

pub(crate) struct PithByteBuffer {
    pub(crate) data: Vec<u8>,
    magic: u32,
}

/// Fast validity checks: one memory read against a magic word instead of a
/// global registry lock per access. Bytes and buffers are currently never
/// freed, so there is no scrub-on-free here yet; the field exists so stale
/// or wild handles fail the compare instead of being dereferenced blindly.
pub(crate) unsafe fn pith_bytes_ref<'a>(handle: i64) -> Option<&'a PithBytes> {
    let ptr = handle as *const ();
    if !crate::handle_registry::plausibly_aligned::<PithBytes>(ptr)
        || (*(handle as *const PithBytes)).magic != BYTES_MAGIC
    {
        return None;
    }
    Some(&*(handle as *const PithBytes))
}

unsafe fn pith_byte_buffer_mut<'a>(handle: i64) -> Option<&'a mut PithByteBuffer> {
    let ptr = handle as *const ();
    if !crate::handle_registry::plausibly_aligned::<PithByteBuffer>(ptr)
        || (*(handle as *const PithByteBuffer)).magic != BYTE_BUFFER_MAGIC
    {
        return None;
    }
    Some(&mut *(handle as *mut PithByteBuffer))
}

pub(crate) fn pith_bytes_from_vec(data: Vec<u8>) -> i64 {
    ensure_perf_stats_registered();
    perf_count(&PERF_BYTES_ALLOCS, 1);
    perf_count(&PERF_BYTES_ALLOC_BYTES, data.len());
    let data_ptr = data.as_ptr();
    let data_len = data.len();
    let ptr = Box::into_raw(Box::new(PithBytes {
        data_ptr,
        data_len,
        data,
        rc: std::sync::atomic::AtomicU32::new(1),
        magic: BYTES_MAGIC,
    }));
    ptr as i64
}

/// One more owner of this bytes object.
///
/// # Safety
/// handle must be a valid PithBytes handle or garbage (the magic check
/// rejects garbage).
#[no_mangle]
pub unsafe extern "C" fn pith_bytes_retain(handle: i64) {
    if let Some(b) = pith_bytes_ref(handle) {
        b.rc.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Drop one owner; the last release frees the object and its storage.
///
/// # Safety
/// handle must be a valid PithBytes handle or garbage.
#[no_mangle]
pub unsafe extern "C" fn pith_bytes_release(handle: i64) {
    let Some(b) = pith_bytes_ref(handle) else {
        return;
    };
    let prev = b.rc.fetch_sub(1, std::sync::atomic::Ordering::Release);
    if prev > 1 {
        return;
    }
    if prev == 0 {
        // over-release: put the count back and leave the object alone
        // rather than double-freeing
        b.rc.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
    crate::perf_count(&crate::PERF_BYTES_FREES, 1);
    // scrub the magic so stale handles fail the validity check
    (*(handle as *mut PithBytes)).magic = 0;
    let _ = Box::from_raw(handle as *mut PithBytes);
}

#[no_mangle]
pub unsafe extern "C" fn pith_bytes_from_string_utf8(s: *const i8) -> i64 {
    if s.is_null() {
        return pith_bytes_from_vec(Vec::new());
    }
    let len = crate::string::pith_cstring_len(s) as usize;
    let bytes = std::slice::from_raw_parts(s as *const u8, len);
    pith_bytes_from_vec(bytes.to_vec())
}

#[no_mangle]
pub unsafe extern "C" fn pith_bytes_to_string_utf8(handle: i64) -> *mut i8 {
    let Some(bytes) = pith_bytes_ref(handle) else {
        return std::ptr::null_mut();
    };
    if std::str::from_utf8(&bytes.data).is_err() {
        return std::ptr::null_mut();
    }
    crate::pith_copy_bytes_to_cstring(&bytes.data)
}

/// Slice a byte range and decode it as UTF-8 in one step, returning a
/// pith C-string (null on invalid UTF-8). This avoids the intermediate
/// Bytes handle that `slice` then `to_string_utf8` would allocate and
/// register — a real cost in tight scanning loops (json, csv).
#[no_mangle]
pub unsafe extern "C" fn pith_bytes_substring_utf8(handle: i64, start: i64, end: i64) -> *mut i8 {
    let Some(bytes) = pith_bytes_ref(handle) else {
        return std::ptr::null_mut();
    };
    let len = bytes.data.len() as i64;
    // an inverted range is empty, not reversed: clamping `end` up to `start`
    // matches pith_cstring_substring and pith_list_slice. swapping the two
    // instead would hand back bytes the caller never asked for, which for a
    // parser walking a buffer is wrong data rather than no data.
    let start_idx = start.max(0).min(len);
    let end_idx = end.max(start_idx).min(len);
    let slice = &bytes.data[start_idx as usize..end_idx as usize];
    if std::str::from_utf8(slice).is_err() {
        return std::ptr::null_mut();
    }
    crate::pith_copy_bytes_to_cstring(slice)
}

#[no_mangle]
pub unsafe extern "C" fn pith_bytes_len(handle: i64) -> i64 {
    let Some(bytes) = pith_bytes_ref(handle) else {
        return 0;
    };
    bytes.data.len() as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_bytes_is_empty(handle: i64) -> i64 {
    let Some(bytes) = pith_bytes_ref(handle) else {
        return 1;
    };
    if bytes.data.is_empty() {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_bytes_get(handle: i64, idx: i64) -> i64 {
    let Some(bytes) = pith_bytes_ref(handle) else {
        return 0;
    };
    if idx < 0 {
        return 0;
    }
    bytes.data.get(idx as usize).copied().unwrap_or(0) as i64
}

/// Strict indexed byte access for `bytes[i]`. Aborts with a structured
/// diagnostic on out-of-bounds instead of returning silent 0.
#[no_mangle]
pub unsafe extern "C" fn pith_bytes_get_strict(handle: i64, idx: i64) -> i64 {
    let Some(bytes) = pith_bytes_ref(handle) else {
        eprintln!("pith runtime error: bytes indexing on invalid bytes handle");
        std::process::exit(1);
    };
    let len = bytes.data.len() as i64;
    if idx < 0 || idx >= len {
        eprintln!(
            "pith runtime error: bytes index out of bounds: {} for bytes of length {}",
            idx, len
        );
        std::process::exit(1);
    }
    bytes.data[idx as usize] as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_bytes_slice(handle: i64, start: i64, end: i64) -> i64 {
    let Some(bytes) = pith_bytes_ref(handle) else {
        return 0;
    };
    let len = bytes.data.len() as i64;
    // see the note in pith_bytes_substring_utf8: an inverted range is empty.
    let start_idx = start.max(0).min(len);
    let end_idx = end.max(start_idx).min(len);
    pith_bytes_from_vec(bytes.data[start_idx as usize..end_idx as usize].to_vec())
}

#[no_mangle]
pub unsafe extern "C" fn pith_bytes_concat(a: i64, b: i64) -> i64 {
    let Some(a_bytes) = pith_bytes_ref(a) else {
        return 0;
    };
    let Some(b_bytes) = pith_bytes_ref(b) else {
        return 0;
    };
    let mut out = Vec::with_capacity(a_bytes.data.len() + b_bytes.data.len());
    out.extend_from_slice(&a_bytes.data);
    out.extend_from_slice(&b_bytes.data);
    pith_bytes_from_vec(out)
}

#[no_mangle]
pub unsafe extern "C" fn pith_bytes_eq(a: i64, b: i64) -> i64 {
    if a == 0 && b == 0 {
        return 1;
    }
    let Some(a_bytes) = pith_bytes_ref(a) else {
        return 0;
    };
    let Some(b_bytes) = pith_bytes_ref(b) else {
        return 0;
    };
    if a_bytes.data == b_bytes.data {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_constant_time_eq(a: i64, b: i64) -> i64 {
    let a_bytes = pith_bytes_ref(a)
        .map(|bytes| bytes.data.as_slice())
        .unwrap_or(&[]);
    let b_bytes = pith_bytes_ref(b)
        .map(|bytes| bytes.data.as_slice())
        .unwrap_or(&[]);
    let max_len = a_bytes.len().max(b_bytes.len());
    let mut diff = (a_bytes.len() ^ b_bytes.len()) as u8;

    for i in 0..max_len {
        let left = a_bytes.get(i).copied().unwrap_or(0);
        let right = b_bytes.get(i).copied().unwrap_or(0);
        diff |= left ^ right;
    }

    if diff == 0 {
        1
    } else {
        0
    }
}

#[no_mangle]
pub extern "C" fn pith_secure_random_bytes(count: i64) -> i64 {
    let len = count.max(0) as usize;
    let mut out = vec![0_u8; len];
    if len == 0 {
        return pith_bytes_from_vec(out);
    }

    match std::fs::File::open("/dev/urandom").and_then(|mut file| file.read_exact(&mut out)) {
        Ok(()) => pith_bytes_from_vec(out),
        Err(_) => 0,
    }
}

#[no_mangle]
/// Free a byte buffer: single-owner lifecycle, no refcount. After this
/// call the handle is dead and fails the magic check.
///
/// # Safety
/// handle must be a valid PithByteBuffer or garbage.
#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_free(handle: i64) {
    if pith_byte_buffer_mut(handle).is_none() {
        return;
    }
    crate::perf_count(&crate::PERF_BYTE_BUFFER_FREES, 1);
    // scrub the magic so a stale handle fails the validity check instead
    // of touching freed memory
    (*(handle as *mut PithByteBuffer)).magic = 0;
    let _ = Box::from_raw(handle as *mut PithByteBuffer);
}

/// Allocate a fresh byte buffer.
#[no_mangle]
pub extern "C" fn pith_byte_buffer_new() -> i64 {
    ensure_perf_stats_registered();
    perf_count(&PERF_BYTE_BUFFER_NEWS, 1);
    let ptr = Box::into_raw(Box::new(PithByteBuffer {
        data: Vec::new(),
        magic: BYTE_BUFFER_MAGIC,
    }));
    ptr as i64
}

#[no_mangle]
pub extern "C" fn pith_byte_buffer_with_capacity(capacity: i64) -> i64 {
    let cap = if capacity > 0 { capacity as usize } else { 0 };
    ensure_perf_stats_registered();
    perf_count(&PERF_BYTE_BUFFER_NEWS, 1);
    let ptr = Box::into_raw(Box::new(PithByteBuffer {
        data: Vec::with_capacity(cap),
        magic: BYTE_BUFFER_MAGIC,
    }));
    ptr as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_write(handle: i64, data: i64) -> i64 {
    let Some(buffer) = pith_byte_buffer_mut(handle) else {
        return 0;
    };
    let Some(bytes) = pith_bytes_ref(data) else {
        return 0;
    };
    ensure_perf_stats_registered();
    perf_count(&PERF_BYTE_BUFFER_WRITES, 1);
    perf_count(&PERF_BYTE_BUFFER_WRITE_BYTES, bytes.data.len());
    buffer.data.extend_from_slice(&bytes.data);
    bytes.data.len() as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_write_string_utf8(handle: i64, s: *const i8) -> i64 {
    let Some(buffer) = pith_byte_buffer_mut(handle) else {
        return 0;
    };
    if s.is_null() {
        return 0;
    }
    let len = crate::string::pith_cstring_len(s) as usize;
    let bytes = std::slice::from_raw_parts(s as *const u8, len);
    ensure_perf_stats_registered();
    perf_count(&PERF_BYTE_BUFFER_WRITES, 1);
    perf_count(&PERF_BYTE_BUFFER_WRITE_BYTES, bytes.len());
    buffer.data.extend_from_slice(bytes);
    bytes.len() as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_write_byte(handle: i64, value: i64) -> i64 {
    let Some(buffer) = pith_byte_buffer_mut(handle) else {
        return 0;
    };
    if !(0..=255).contains(&value) {
        return 0;
    }
    ensure_perf_stats_registered();
    perf_count(&PERF_BYTE_BUFFER_WRITES, 1);
    perf_count(&PERF_BYTE_BUFFER_WRITE_BYTES, 1);
    buffer.data.push(value as u8);
    1
}

/// Append `len` bytes copied from `src` within this same buffer — the
/// back-reference every LZ-family decoder needs. The copy runs forward one
/// byte at a time on purpose: when the source range overlaps the
/// destination (offset < len), that repetition IS the encoding, which is
/// how a run of bytes costs one short match. Returns 1, or 0 on a bad
/// handle or a range that starts outside the buffer.
#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_copy_within(handle: i64, src: i64, len: i64) -> i64 {
    let Some(buffer) = pith_byte_buffer_mut(handle) else {
        return 0;
    };
    if src < 0 || len < 0 || (src as usize) >= buffer.data.len() {
        return 0;
    }
    let src = src as usize;
    let len = len as usize;
    ensure_perf_stats_registered();
    perf_count(&PERF_BYTE_BUFFER_WRITES, 1);
    perf_count(&PERF_BYTE_BUFFER_WRITE_BYTES, len);
    buffer.data.reserve(len);
    // a non-overlapping range is a straight extend. an overlapping range
    // means the output repeats with period (len - src): each chunk copied
    // from src is a whole number of periods, so the region already written
    // feeds the next chunk and the chunk size doubles — a byte-long run
    // costs a logarithmic number of memcpys instead of a byte loop.
    if src + len <= buffer.data.len() {
        buffer.data.extend_from_within(src..src + len);
    } else {
        let mut remaining = len;
        while remaining > 0 {
            let avail = buffer.data.len() - src;
            let n = avail.min(remaining);
            buffer.data.extend_from_within(src..src + n);
            remaining -= n;
        }
    }
    1
}

/// Read `count` bytes at `off` as one little-endian integer — the mirror
/// of `write_word`. A word-at-a-time consumer (the xxhash kernel, the
/// bitstream refill) pays one call here instead of a byte loop. Returns 0
/// on a bad handle, a count outside 1..=8, or a range outside the bytes —
/// callers needing the zero-past-the-end tolerance check bounds first.
///
/// # Safety
/// handle must be a valid PithBytes or garbage.
#[no_mangle]
pub unsafe extern "C" fn pith_bytes_read_word(handle: i64, off: i64, count: i64) -> i64 {
    let Some(bytes) = pith_bytes_ref(handle) else {
        return 0;
    };
    if off < 0 || !(1..=8).contains(&count) {
        return 0;
    }
    let off = off as usize;
    let count = count as usize;
    let Some(end) = off.checked_add(count) else {
        return 0;
    };
    if end > bytes.data.len() {
        return 0;
    }
    let mut le = [0u8; 8];
    le[..count].copy_from_slice(&bytes.data[off..end]);
    u64::from_le_bytes(le) as i64
}

/// Append `len` bytes of `src` starting at `start` — a slice-then-write
/// with the intermediate bytes allocation removed. The LZ execute loop
/// appends one literal run per sequence; allocating a Bytes object per run
/// just to feed `write` was a measurable share of decode. Returns 1, or 0
/// on a bad handle or a range outside `src`.
///
/// # Safety
/// handle must be a valid PithByteBuffer and src a valid PithBytes, or
/// garbage (the magic checks reject garbage).
#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_write_range(handle: i64, src: i64, start: i64, len: i64) -> i64 {
    let Some(buffer) = pith_byte_buffer_mut(handle) else {
        return 0;
    };
    let Some(bytes) = pith_bytes_ref(src) else {
        return 0;
    };
    if start < 0 || len < 0 {
        return 0;
    }
    let start = start as usize;
    let len = len as usize;
    let Some(end) = start.checked_add(len) else {
        return 0;
    };
    if end > bytes.data.len() {
        return 0;
    }
    ensure_perf_stats_registered();
    perf_count(&PERF_BYTE_BUFFER_WRITES, 1);
    perf_count(&PERF_BYTE_BUFFER_WRITE_BYTES, len);
    buffer.data.extend_from_slice(&bytes.data[start..end]);
    1
}

/// Append the low `count` bytes of `word`, least significant first. A hot
/// byte-at-a-time producer (the huffman literals decoder) batches up to
/// eight decoded bytes into one integer and pays one call here instead of
/// eight `write_byte` calls. Returns 1, or 0 on a bad handle or a count
/// outside 1..=8.
///
/// # Safety
/// handle must be a valid PithByteBuffer or garbage.
#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_write_word(handle: i64, word: i64, count: i64) -> i64 {
    let Some(buffer) = pith_byte_buffer_mut(handle) else {
        return 0;
    };
    if !(1..=8).contains(&count) {
        return 0;
    }
    ensure_perf_stats_registered();
    perf_count(&PERF_BYTE_BUFFER_WRITES, 1);
    perf_count(&PERF_BYTE_BUFFER_WRITE_BYTES, count as usize);
    let le = (word as u64).to_le_bytes();
    buffer.data.extend_from_slice(&le[..count as usize]);
    1
}

/// Extract the accumulated bytes (moving the storage, no copy) and free
/// the buffer in one step: the natural end of a build-then-extract
/// buffer's life.
///
/// # Safety
/// handle must be a valid PithByteBuffer or garbage.
#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_take_bytes(handle: i64) -> i64 {
    if pith_byte_buffer_mut(handle).is_none() {
        return 0;
    }
    // reclaim the whole box: the vec moves into the bytes object, the
    // buffer allocation dies here, and the scrubbed magic makes any stale
    // handle fail the validity check
    (*(handle as *mut PithByteBuffer)).magic = 0;
    let boxed = Box::from_raw(handle as *mut PithByteBuffer);
    crate::perf_count(&crate::PERF_BYTE_BUFFER_FREES, 1);
    pith_bytes_from_vec(boxed.data)
}

#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_bytes(handle: i64) -> i64 {
    let Some(buffer) = pith_byte_buffer_mut(handle) else {
        return 0;
    };
    pith_bytes_from_vec(buffer.data.clone())
}

#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_len(handle: i64) -> i64 {
    let Some(buffer) = pith_byte_buffer_mut(handle) else {
        return 0;
    };
    buffer.data.len() as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_get(handle: i64, index: i64) -> i64 {
    let Some(buffer) = pith_byte_buffer_mut(handle) else {
        return 0;
    };
    if index < 0 {
        return 0;
    }
    buffer.data.get(index as usize).copied().unwrap_or(0) as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_set(handle: i64, index: i64, value: i64) -> i64 {
    let Some(buffer) = pith_byte_buffer_mut(handle) else {
        return 0;
    };
    if index < 0 || !(0..=255).contains(&value) {
        return 0;
    }
    let idx = index as usize;
    if idx >= buffer.data.len() {
        return 0;
    }
    buffer.data[idx] = value as u8;
    1
}

#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_clear(handle: i64) {
    if let Some(buffer) = pith_byte_buffer_mut(handle) {
        buffer.data.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_bytes_handles_return_safe_defaults() {
        unsafe {
            assert_eq!(pith_bytes_len(12345), 0);
            assert_eq!(pith_bytes_is_empty(12345), 1);
            assert_eq!(pith_bytes_get(12345, 0), 0);
            assert_eq!(pith_bytes_slice(12345, 0, 1), 0);
            assert!(pith_bytes_to_string_utf8(12345).is_null());
        }
    }

    // an inverted or negative range yields nothing. this used to swap the two
    // ends, so slice(1, -5) on b"abc" came back as b"a" — a caller walking a
    // buffer got bytes it never asked for instead of an empty result.
    #[test]
    fn inverted_byte_ranges_are_empty() {
        unsafe {
            let b = pith_bytes_from_vec(b"abcdef".to_vec());

            assert_eq!(pith_bytes_len(pith_bytes_slice(b, 1, -5)), 0);
            assert_eq!(pith_bytes_len(pith_bytes_slice(b, 4, 2)), 0);
            assert_eq!(pith_bytes_len(pith_bytes_slice(b, -3, -1)), 0);

            // the ordinary cases are untouched
            assert_eq!(pith_bytes_len(pith_bytes_slice(b, 1, 4)), 3);
            assert_eq!(pith_bytes_len(pith_bytes_slice(b, 0, 99)), 6);
            assert_eq!(pith_bytes_len(pith_bytes_slice(b, -2, 3)), 3);
            assert_eq!(pith_bytes_len(pith_bytes_slice(b, 2, 2)), 0);

            // and the utf8 shortcut agrees with the slice
            let s = pith_bytes_substring_utf8(b, 4, 2);
            assert!(!s.is_null());
            assert_eq!(*s, 0, "an inverted range decodes to the empty string");
        }
    }

    #[test]
    fn invalid_byte_buffer_handles_return_safe_defaults() {
        unsafe {
            assert_eq!(pith_byte_buffer_len(12345), 0);
            assert_eq!(pith_byte_buffer_get(12345, 0), 0);
            assert_eq!(pith_byte_buffer_set(12345, 0, 1), 0);
            assert_eq!(pith_byte_buffer_bytes(12345), 0);
            pith_byte_buffer_clear(12345);
        }
    }
}
