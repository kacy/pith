//! Mutable single-owner `PithByteBuffer` operations.

use super::{pith_bytes_from_vec, with_bytes};
use crate::ffi_util::{with_cstr_bytes, write_result, ResultPair};

/// Magic word for PithByteBuffer ("PBUF")
const BYTE_BUFFER_MAGIC: u32 = 0x50425546;

pub(crate) struct PithByteBuffer {
    pub(crate) data: Vec<u8>,
    magic: u32,
}

/// Validate `handle` as a `PithByteBuffer` and borrow it mutably for the duration of `f`.
#[inline(always)]
unsafe fn with_byte_buffer_mut<R>(
    handle: i64,
    f: impl FnOnce(&mut PithByteBuffer) -> R,
) -> Option<R> {
    let ptr = handle as *mut PithByteBuffer;
    if !crate::handle_registry::plausibly_aligned::<PithByteBuffer>(ptr as *const ()) {
        return None;
    }
    // SAFETY: Alignment validated; check magic word before mutably dereferencing.
    let valid = unsafe {
        crate::quarantine::magic_is(
            std::ptr::addr_of!((*ptr).magic),
            BYTE_BUFFER_MAGIC,
            crate::quarantine::Op::Use,
        )
    };
    if !valid {
        return None;
    }
    // SAFETY: Magic word matched `BYTE_BUFFER_MAGIC`; caller guarantees exclusive access for `f`.
    let buf = unsafe { &mut *ptr };
    Some(f(buf))
}
/// Free a byte buffer: single-owner lifecycle, no refcount. After this
/// call the handle is dead and fails the magic check.
///
/// # Safety
/// handle must be a valid PithByteBuffer or garbage.
#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_free(handle: i64) {
    // SAFETY: Validates handle and scrubs magic before freeing; borrow ends before `free_box`.
    let valid = unsafe {
        with_byte_buffer_mut(handle, |buf| {
            // scrub the magic so a stale handle fails the validity check instead
            // of touching freed memory
            buf.magic = 0;
        })
    }
    .is_some();
    if !valid {
        return;
    }
    crate::perf_count(&crate::PERF_BYTE_BUFFER_FREES, 1);
    let ptr = handle as *mut PithByteBuffer;
    // SAFETY: Handle was validated, its magic is scrubbed, and no borrow remains live.
    unsafe {
        let magic = std::ptr::addr_of_mut!((*ptr).magic);
        crate::quarantine::free_box(ptr, magic, crate::quarantine::Kind::ByteBuffer);
    }
}

/// Allocate a fresh byte buffer.
#[no_mangle]
pub extern "C" fn pith_byte_buffer_new() -> i64 {
    crate::perf_stats!(PERF_BYTE_BUFFER_NEWS += 1);
    let ptr = Box::into_raw(Box::new(PithByteBuffer {
        data: Vec::new(),
        magic: BYTE_BUFFER_MAGIC,
    }));
    ptr as i64
}

#[no_mangle]
pub extern "C" fn pith_byte_buffer_with_capacity(capacity: i64) -> i64 {
    let cap = if capacity > 0 { capacity as usize } else { 0 };
    crate::perf_stats!(PERF_BYTE_BUFFER_NEWS += 1);
    let ptr = Box::into_raw(Box::new(PithByteBuffer {
        data: Vec::with_capacity(cap),
        magic: BYTE_BUFFER_MAGIC,
    }));
    ptr as i64
}

/// append `bytes` to `buffer`, returning the count appended (`0` for an empty
/// slice, which is a success).
///
/// inlined into each entry point, with the failure text built out of line:
/// the append is a few instructions, and as a call that hands back a
/// `Result<usize, String>` it cost more than the copy it wraps.
#[inline(always)]
fn append_bytes_to_buf(buffer: &mut PithByteBuffer, bytes: &[u8]) -> ResultPair {
    crate::perf_stats!(
        PERF_BYTE_BUFFER_WRITES += 1,
        PERF_BYTE_BUFFER_WRITE_BYTES += bytes.len(),
    );
    buffer.data.extend_from_slice(bytes);
    crate::ffi_util::result_ok(bytes.len() as i64)
}

#[cold]
#[inline(never)]
fn invalid_buffer_result(builtin: &str) -> ResultPair {
    write_result(builtin, Err("invalid buffer handle".to_string()))
}

#[cold]
#[inline(never)]
fn invalid_value_result(builtin: &str, what: &str) -> ResultPair {
    write_result(builtin, Err(format!("invalid {what}")))
}

/// append a `Bytes` value to a buffer, as a result pair holding the count or
/// the failure's reason: an invalid buffer handle or an invalid bytes value.
/// the buffer handle is checked first, so an invalid one is what a call with
/// two bad arguments reports.
#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_write(handle: i64, data: i64) -> ResultPair {
    // SAFETY: `handle` (`PithByteBuffer`) and `data` (`PithBytes`) are distinct handle
    // types validated before borrowing.
    let result = unsafe {
        with_byte_buffer_mut(handle, |buffer| {
            with_bytes(data, |bytes| append_bytes_to_buf(buffer, bytes))
        })
    };
    match result {
        None => invalid_buffer_result("byte_buffer_write"),
        Some(None) => invalid_value_result("byte_buffer_write", "bytes value"),
        Some(Some(pair)) => pair,
    }
}

/// append a string's bytes to a buffer, as a result pair holding the count or
/// the failure's reason: an invalid buffer handle or an invalid string.
#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_write_string_utf8(handle: i64, s: *const i8) -> ResultPair {
    // SAFETY: `handle` (`PithByteBuffer`) and `s` (C string) are distinct and validated
    // before borrowing.
    let result = unsafe {
        with_byte_buffer_mut(handle, |buffer| {
            with_cstr_bytes(s, |bytes| append_bytes_to_buf(buffer, bytes))
        })
    };
    match result {
        None => invalid_buffer_result("byte_buffer_write_string_utf8"),
        Some(None) => invalid_value_result("byte_buffer_write_string_utf8", "string"),
        Some(Some(pair)) => pair,
    }
}

#[inline(always)]
fn byte_buffer_write_byte_impl(buffer: &mut PithByteBuffer, value: i64) -> i64 {
    if !(0..=255).contains(&value) {
        return 0;
    }
    crate::perf_stats!(PERF_BYTE_BUFFER_WRITES += 1, PERF_BYTE_BUFFER_WRITE_BYTES += 1);
    buffer.data.push(value as u8);
    1
}

#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_write_byte(handle: i64, value: i64) -> i64 {
    // SAFETY: `handle` is validated by `with_byte_buffer_mut` before mutating.
    unsafe { with_byte_buffer_mut(handle, |buf| byte_buffer_write_byte_impl(buf, value)) }
        .unwrap_or(0)
}

#[inline(always)]
fn byte_buffer_copy_within_impl(buffer: &mut PithByteBuffer, src: i64, len: i64) -> i64 {
    if src < 0 || len < 0 || (src as usize) >= buffer.data.len() {
        return 0;
    }
    let src = src as usize;
    let len = len as usize;
    crate::perf_stats!(PERF_BYTE_BUFFER_WRITES += 1, PERF_BYTE_BUFFER_WRITE_BYTES += len);
    buffer.data.reserve(len);
    // a non-overlapping range is a straight extend. an overlapping range
    // means the output repeats with period (len - src): each chunk copied
    // from src is a whole number of periods, so the region already written
    // feeds the next chunk and the chunk size doubles. a byte-long run
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

/// Append `len` bytes copied from `src` within this same buffer: the
/// back-reference every LZ-family decoder needs. The copy runs forward one
/// byte at a time on purpose: when the source range overlaps the
/// destination (offset < len), that repetition IS the encoding, which is
/// how a run of bytes costs one short match. Returns 1, or 0 on a bad
/// handle or a range that starts outside the buffer.
#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_copy_within(handle: i64, src: i64, len: i64) -> i64 {
    // SAFETY: `handle` is validated by `with_byte_buffer_mut` before mutating.
    unsafe { with_byte_buffer_mut(handle, |buf| byte_buffer_copy_within_impl(buf, src, len)) }
        .unwrap_or(0)
}
#[inline(always)]
fn byte_buffer_write_range_impl(
    buffer: &mut PithByteBuffer,
    data: &[u8],
    start: i64,
    len: i64,
) -> i64 {
    if start < 0 || len < 0 {
        return 0;
    }
    let start = start as usize;
    let len = len as usize;
    let Some(end) = start.checked_add(len) else {
        return 0;
    };
    if end > data.len() {
        return 0;
    }
    crate::perf_stats!(PERF_BYTE_BUFFER_WRITES += 1, PERF_BYTE_BUFFER_WRITE_BYTES += len);
    buffer.data.extend_from_slice(&data[start..end]);
    1
}

/// Append `len` bytes of `src` starting at `start`: a slice-then-write
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
    // SAFETY: `handle` (`PithByteBuffer`) and `src` (`PithBytes`) are distinct handle
    // types validated before borrowing.
    unsafe {
        with_byte_buffer_mut(handle, |buffer| {
            with_bytes(src, |data| byte_buffer_write_range_impl(buffer, data, start, len))
        })
    }
    .flatten()
    .unwrap_or(0)
}

#[inline(always)]
fn byte_buffer_write_word_impl(buffer: &mut PithByteBuffer, word: i64, count: i64) -> i64 {
    if !(1..=8).contains(&count) {
        return 0;
    }
    crate::perf_stats!(
        PERF_BYTE_BUFFER_WRITES += 1,
        PERF_BYTE_BUFFER_WRITE_BYTES += count as usize,
    );
    // store all eight bytes in one unaligned write and count only the low
    // `count`; extend_from_slice with a run-time length is a memcpy call.
    let data = &mut buffer.data;
    data.reserve(8);
    let len = data.len();
    // SAFETY: `reserve(8)` ensures 8 bytes of spare capacity at `len`, and
    // `1 <= count <= 8`, so `len + count` bytes are initialized.
    unsafe {
        std::ptr::write_unaligned(
            data.as_mut_ptr().add(len).cast::<[u8; 8]>(),
            (word as u64).to_le_bytes(),
        );
        data.set_len(len + count as usize);
    }
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
    // SAFETY: `handle` is validated by `with_byte_buffer_mut` before mutating.
    unsafe { with_byte_buffer_mut(handle, |buf| byte_buffer_write_word_impl(buf, word, count)) }
        .unwrap_or(0)
}

/// Extract the accumulated bytes (moving the storage, no copy) and free
/// the buffer in one step: the natural end of a build-then-extract
/// buffer's life.
///
/// # Safety
/// handle must be a valid PithByteBuffer or garbage.
#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_take_bytes(handle: i64) -> i64 {
    // the vec moves into the bytes object and the buffer stays alive and empty.
    // only the destructor of the value that owns the handle frees the buffer, so
    // no handle a program can still name ever points at freed memory.
    // SAFETY: `handle` is validated by `with_byte_buffer_mut` before borrowing.
    unsafe { with_byte_buffer_mut(handle, |buf| pith_bytes_from_vec(std::mem::take(&mut buf.data))) }
        .unwrap_or(0)
}

#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_bytes(handle: i64) -> i64 {
    // SAFETY: `handle` is validated by `with_byte_buffer_mut` before cloning its bytes.
    unsafe { with_byte_buffer_mut(handle, |buf| pith_bytes_from_vec(buf.data.clone())) }
        .unwrap_or(0)
}

#[inline(always)]
fn byte_buffer_string_impl(buffer: &mut PithByteBuffer) -> *mut i8 {
    if buffer.data.is_empty() {
        return crate::pith_cstring_empty();
    }
    crate::pith_copy_bytes_to_cstring(&buffer.data)
}

/// Copy the accumulated bytes into a fresh pith string, verbatim.
///
/// This is the byte buffer as string storage: `std.io`'s StringBuffer
/// appends each write's bytes with `pith_byte_buffer_write_string_utf8`
/// and reads the whole thing back through here. A pith string carries
/// whatever bytes were put into it (`chr(200)` is a one-byte string that
/// is not UTF-8, and its header length lets it hold a NUL), so the read
/// back must not validate: `pith_bytes_to_string_utf8` would refuse
/// content the writer accepted. It is also one copy and no intermediate
/// bytes object, where `pith_byte_buffer_bytes` followed by a decode is
/// two. The buffer is left as it was; the string is independent of it.
///
/// # Safety
/// handle must be a valid PithByteBuffer or garbage (returns an empty
/// string for garbage).
#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_string(handle: i64) -> *mut i8 {
    // SAFETY: `handle` is validated by `with_byte_buffer_mut` before reading.
    unsafe { with_byte_buffer_mut(handle, byte_buffer_string_impl) }
        .unwrap_or_else(crate::pith_cstring_empty)
}

#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_len(handle: i64) -> i64 {
    // SAFETY: `handle` is validated by `with_byte_buffer_mut` before reading its length.
    unsafe { with_byte_buffer_mut(handle, |buf| buf.data.len() as i64) }.unwrap_or(0)
}

#[inline(always)]
fn byte_buffer_get_impl(buffer: &mut PithByteBuffer, index: i64) -> i64 {
    if index < 0 {
        return 0;
    }
    buffer.data.get(index as usize).copied().unwrap_or(0) as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_get(handle: i64, index: i64) -> i64 {
    // SAFETY: `handle` is validated by `with_byte_buffer_mut` before reading.
    unsafe { with_byte_buffer_mut(handle, |buf| byte_buffer_get_impl(buf, index)) }.unwrap_or(0)
}

#[inline(always)]
fn byte_buffer_set_impl(buffer: &mut PithByteBuffer, index: i64, value: i64) -> i64 {
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
pub unsafe extern "C" fn pith_byte_buffer_set(handle: i64, index: i64, value: i64) -> i64 {
    // SAFETY: `handle` is validated by `with_byte_buffer_mut` before mutating.
    unsafe { with_byte_buffer_mut(handle, |buf| byte_buffer_set_impl(buf, index, value)) }
        .unwrap_or(0)
}

#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_clear(handle: i64) {
    // SAFETY: `handle` is validated by `with_byte_buffer_mut` before clearing.
    unsafe { with_byte_buffer_mut(handle, |buf| buf.data.clear()) };
}
