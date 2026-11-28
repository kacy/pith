//! Immutable refcounted `Bytes` and mutable single-owner `ByteBuffer`.

mod buffer;
mod ops;
#[cfg(test)]
mod tests;

pub use buffer::*;
pub use ops::*;

#[cfg(test)]
use crate::ffi_util::with_cstr_bytes;

#[repr(C)]
pub(crate) struct PithBytes {
    // data_ptr and data_len must stay at offsets 0 and 8: the codegen
    // inlines bytes indexing against this layout (ir_consumer.rs), so the
    // magic word lives at the end. The strict `bytes[i]` fast path also
    // reads the magic word, through BYTES_MAGIC_OFFSET below.
    pub(crate) data_ptr: *const u8,
    pub(crate) data_len: usize,
    pub(crate) data: Vec<u8>,
    pub(crate) rc: std::sync::atomic::AtomicU32,
    pub(crate) magic: u32,
}

/// Magic word for PithBytes ("PBYT")
pub const BYTES_MAGIC: u32 = 0x50425954;

/// Layout facts the codegen's inlined `bytes[i]` fast path reads directly,
/// so the two cannot drift apart: the data pointer, the length, the magic
/// word, and the alignment `pith_bytes_ref` requires of a handle.
pub const BYTES_DATA_PTR_OFFSET: i32 = std::mem::offset_of!(PithBytes, data_ptr) as i32;
pub const BYTES_DATA_LEN_OFFSET: i32 = std::mem::offset_of!(PithBytes, data_len) as i32;
pub const BYTES_MAGIC_OFFSET: i32 = std::mem::offset_of!(PithBytes, magic) as i32;
pub const BYTES_HANDLE_ALIGN: i64 = std::mem::align_of::<PithBytes>() as i64;

/// Validate `handle` as a `PithBytes` for `op` and borrow it for the duration of `f`.
///
/// The borrow cannot escape the closure.
#[inline(always)]
pub(crate) unsafe fn with_bytes_obj_for<R>(
    handle: i64,
    op: crate::quarantine::Op,
    f: impl FnOnce(&PithBytes) -> R,
) -> Option<R> {
    let ptr = handle as *const PithBytes;
    if !crate::handle_registry::plausibly_aligned::<PithBytes>(ptr as *const ()) {
        return None;
    }
    // SAFETY: `ptr` passed alignment validation; `magic_is` validates the magic word before dereferencing.
    let valid = unsafe { crate::quarantine::magic_is(std::ptr::addr_of!((*ptr).magic), BYTES_MAGIC, op) };
    if !valid {
        return None;
    }
    // SAFETY: Magic word matched `BYTES_MAGIC`; caller holds the handle live for the duration of `f`.
    let bytes = unsafe { &*ptr };
    Some(f(bytes))
}

/// Validate `handle` as a `PithBytes` and borrow the object for the duration of `f`.
#[inline(always)]
pub(crate) unsafe fn with_bytes_obj<R>(handle: i64, f: impl FnOnce(&PithBytes) -> R) -> Option<R> {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { with_bytes_obj_for(handle, crate::quarantine::Op::Use, f) }
}

/// Validate `handle` as a `PithBytes` and borrow its byte slice for the duration of `f`.
#[inline(always)]
pub(crate) unsafe fn with_bytes<R>(handle: i64, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { with_bytes_obj(handle, |b| f(b.data.as_slice())) }
}

/// Validate two `PithBytes` handles and borrow both slices for the duration of `f`.
#[inline(always)]
pub(crate) unsafe fn with_bytes2<R>(
    a: i64,
    b: i64,
    f: impl FnOnce(&[u8], &[u8]) -> R,
) -> Option<R> {
    // SAFETY: Both borrows are shared (`&[u8]`), so aliasing `a == b` is sound.
    unsafe { with_bytes(a, |a_slice| with_bytes(b, |b_slice| f(a_slice, b_slice))) }.flatten()
}

/// Validate three `PithBytes` handles and borrow all three slices for the duration of `f`.
#[inline(always)]
pub(crate) unsafe fn with_bytes3<R>(
    a: i64,
    b: i64,
    c: i64,
    f: impl FnOnce(&[u8], &[u8], &[u8]) -> R,
) -> Option<R> {
    // SAFETY: All borrows are shared (`&[u8]`), so any handle aliasing is sound.
    unsafe {
        with_bytes(a, |a_s| {
            with_bytes(b, |b_s| with_bytes(c, |c_s| f(a_s, b_s, c_s))).flatten()
        })
    }
    .flatten()
}

/// Validate four `PithBytes` handles and borrow all four slices for the duration of `f`.
#[inline(always)]
pub(crate) unsafe fn with_bytes4<R>(
    a: i64,
    b: i64,
    c: i64,
    d: i64,
    f: impl FnOnce(&[u8], &[u8], &[u8], &[u8]) -> R,
) -> Option<R> {
    // SAFETY: All borrows are shared (`&[u8]`), so any handle aliasing is sound.
    unsafe {
        with_bytes(a, |a_s| {
            with_bytes(b, |b_s| {
                with_bytes(c, |c_s| with_bytes(d, |d_s| f(a_s, b_s, c_s, d_s))).flatten()
            })
            .flatten()
        })
    }
    .flatten()
}
pub(crate) fn pith_bytes_from_vec(data: Vec<u8>) -> i64 {
    crate::perf_stats!(PERF_BYTES_ALLOCS += 1, PERF_BYTES_ALLOC_BYTES += data.len());
    crate::alloc_sites::record("bytes");
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

#[inline(always)]
fn release_bytes_rc(b: &PithBytes) -> bool {
    let prev = crate::runtime_core::count_release(&b.rc);
    if prev > 1 {
        return false;
    }
    if prev == 0 {
        crate::quarantine::over_release(crate::quarantine::Kind::Bytes);
        // over-release: put the count back and leave the object alone
        // rather than double-freeing
        b.rc.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return false;
    }
    acquire_last_count!(b.rc);
    true
}

/// One more owner of this bytes object.
///
/// # Safety
/// handle must be a valid PithBytes handle or garbage (the magic check
/// rejects garbage).
#[no_mangle]
pub unsafe extern "C" fn pith_bytes_retain(handle: i64) {
    crate::perf::perf_rc_op(crate::perf::RC_BYTES_RETAIN);
    // SAFETY: `handle` is validated by `with_bytes_obj_for` before bumping its refcount.
    unsafe {
        with_bytes_obj_for(handle, crate::quarantine::Op::Retain, |b| {
            b.rc.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
    }
}

/// Drop one owner; the last release frees the object and its storage.
///
/// # Safety
/// handle must be a valid PithBytes handle or garbage.
#[no_mangle]
pub unsafe extern "C" fn pith_bytes_release(handle: i64) {
    crate::perf::perf_rc_op(crate::perf::RC_BYTES_RELEASE);
    // SAFETY: `handle` is validated by `with_bytes_obj_for`; the shared borrow
    // ends before any mutation or deallocation below.
    let should_free =
        unsafe { with_bytes_obj_for(handle, crate::quarantine::Op::Release, release_bytes_rc) }
            .unwrap_or(false);
    if !should_free {
        return;
    }
    crate::perf_count(&crate::PERF_BYTES_FREES, 1);
    let ptr = handle as *mut PithBytes;
    // SAFETY: Last reference was released and no borrow of `ptr` remains live.
    unsafe {
        // scrub the magic so stale handles fail the validity check
        (*ptr).magic = 0;
        crate::quarantine::free_box(
            ptr,
            std::ptr::addr_of_mut!((*ptr).magic),
            crate::quarantine::Kind::Bytes,
        );
    }
}

