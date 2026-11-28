//! List read, pop, and string-join entrypoints.

use super::{with_list_mut, with_list_ref, ListImpl, PithList};
use crate::ffi_util::ResultPair;
use crate::runtime_core::{optional_from, optional_pair};

unsafe fn list_join_impl(impl_ref: &ListImpl, sep: &[u8]) -> *mut i8 {
    let count = impl_ref.len();
    let mut total_len = 0usize;
    for i in 0..count {
        if let Some(raw) = impl_ref.get_value(i) {
            let ptr_val = raw as *const i8;
            if !ptr_val.is_null() {
                total_len += crate::string::pith_cstring_len(ptr_val) as usize;
            }
        }
        if i + 1 < count {
            total_len += sep.len();
        }
    }

    // SAFETY: Allocates a headered C string of `total_len` bytes plus trailing NUL.
    let out = unsafe { crate::pith_alloc_cstring(total_len) };

    let mut write = 0usize;
    for i in 0..count {
        if let Some(raw) = impl_ref.get_value(i) {
            let ptr_val = raw as *const i8;
            if !ptr_val.is_null() {
                let len = crate::string::pith_cstring_len(ptr_val) as usize;
                // SAFETY: `ptr_val` is a readable C string of `len` bytes and `out + write..+len` fits in `total_len`.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        ptr_val as *const u8,
                        out.add(write) as *mut u8,
                        len,
                    );
                }
                write += len;
            }
        }
        if i + 1 < count && !sep.is_empty() {
            // SAFETY: `out + write..+sep.len()` fits in `total_len`.
            unsafe {
                std::ptr::copy_nonoverlapping(sep.as_ptr(), out.add(write) as *mut u8, sep.len());
            }
            write += sep.len();
        }
    }

    // SAFETY: `write == total_len` and `pith_alloc_cstring` reserves the trailing NUL byte.
    unsafe { *out.add(write) = 0 };
    out
}

/// Join a list of C string pointers with a separator.
/// Returns a newly allocated C string.
#[no_mangle]
pub unsafe extern "C" fn pith_list_join(list: PithList, sep: *const i8) -> *mut i8 {
    let run = |impl_ref: &ListImpl| {
        if impl_ref.len() == 0 {
            return crate::pith_cstring_empty();
        }
        if sep.is_null() {
            // SAFETY: Caller guarantees list elements are null or valid C strings.
            unsafe { list_join_impl(impl_ref, &[]) }
        } else {
            // SAFETY: `sep` is non-null C string; list elements are valid C strings.
            unsafe {
                crate::ffi_util::with_cstr_bytes(sep, |sep_bytes| {
                    list_join_impl(impl_ref, sep_bytes)
                })
            }
            .unwrap_or(std::ptr::null_mut())
        }
    };
    // SAFETY: `with_list_ref` validates `list` before reading from it.
    unsafe { with_list_ref(list, run) }.unwrap_or(std::ptr::null_mut())
}

fn list_join_int_impl(impl_ref: &ListImpl, sep_str: &str) -> *mut i8 {
    let mut out = String::new();
    for i in 0..impl_ref.len() {
        if let Some(raw) = impl_ref.get_value(i) {
            if i > 0 {
                out.push_str(sep_str);
            }
            out.push_str(&raw.to_string());
        }
    }
    crate::pith_copy_bytes_to_cstring(out.as_bytes())
}

/// Join a list of Int elements with a separator, formatting each as decimal.
/// Unlike `pith_list_join`, the elements are values rather than string pointers.
/// Returns a newly allocated C string.
#[no_mangle]
pub unsafe extern "C" fn pith_list_join_int(list: PithList, sep: *const i8) -> *mut i8 {
    let run = |impl_ref: &ListImpl| {
        // SAFETY: `sep` is null or a valid C string per caller contract.
        unsafe {
            crate::ffi_util::with_cstr_str_or_empty(sep, |sep_str| {
                list_join_int_impl(impl_ref, sep_str)
            })
        }
    };
    // SAFETY: `with_list_ref` validates `list` before reading from it.
    unsafe { with_list_ref(list, run) }.unwrap_or(std::ptr::null_mut())
}

#[inline(always)]
fn list_pop_impl(impl_ref: &mut ListImpl) -> i64 {
    if impl_ref.elem_size != 8 {
        eprintln!("pith: list element size mismatch");
        return 0;
    }

    // The list's count transfers to the caller with the element,
    // so there is no release here.
    impl_ref.pop_value().unwrap_or(0)
}

/// Pop the last 8-byte element from the list, transferring the list's
/// ownership count to the caller.
///
/// Returns the popped word, or `0` when the list is empty or invalid.
#[no_mangle]
pub unsafe extern "C" fn pith_list_pop(list: PithList) -> i64 {
    // SAFETY: `with_list_mut` validates `list` before mutating it.
    unsafe { with_list_mut(list, list_pop_impl) }.unwrap_or(0)
}

#[inline(always)]
fn list_get_opt_impl(impl_ref: &ListImpl, index: i64) -> Option<i64> {
    if index < 0 || index >= impl_ref.len() as i64 || impl_ref.elem_size != 8 {
        return None;
    }
    crate::perf_stats!(PERF_LIST_GETS += 1);
    // SAFETY: `0 <= index < impl_ref.len()` checked above.
    Some(unsafe { impl_ref.get_value_unchecked(index as usize) })
}

/// Get element at index wrapped in an Optional tuple. Returns `Some(value)`
/// when `0 <= index < len`, and `None` otherwise, so callers can
/// distinguish an out-of-bounds miss from a legitimately stored `0`.
///
/// # Safety
/// * `list` must be a valid `PithList` handle or all-zero.
#[no_mangle]
pub unsafe extern "C" fn pith_list_get_opt(list: PithList, index: i64) -> i64 {
    // SAFETY: `with_list_ref` validates `list` before reading from it.
    let found = unsafe { with_list_ref(list, |impl_ref| list_get_opt_impl(impl_ref, index)) }
        .flatten();
    optional_from(found)
}

#[inline(always)]
fn list_first_impl(impl_ref: &ListImpl) -> Option<i64> {
    if impl_ref.len() == 0 || impl_ref.elem_size != 8 {
        return None;
    }
    crate::perf_stats!(PERF_LIST_GETS += 1);
    // SAFETY: `impl_ref.len() > 0` checked above.
    Some(unsafe { impl_ref.get_value_unchecked(0) })
}

/// Return the first element of the list wrapped in an Optional tuple, or
/// `None` when the list is empty.
///
/// # Safety
/// * `list` must be a valid `PithList` handle or all-zero.
unsafe fn list_first(list: PithList) -> Option<i64> {
    // SAFETY: `with_list_ref` validates `list` before reading from it.
    unsafe { with_list_ref(list, list_first_impl) }.flatten()
}

#[no_mangle]
pub unsafe extern "C" fn pith_list_first_opt(list: PithList) -> i64 {
    // SAFETY: Preconditions forwarded from caller.
    optional_from(unsafe { list_first(list) })
}

/// `pith_list_first_opt` as a register pair; see `optional_pair`.
#[no_mangle]
pub unsafe extern "C" fn pith_list_first_opt_pair(list: PithList) -> ResultPair {
    // SAFETY: Preconditions forwarded from caller.
    optional_pair(unsafe { list_first(list) })
}

#[inline(always)]
fn list_last_impl(impl_ref: &ListImpl) -> Option<i64> {
    if impl_ref.len() == 0 || impl_ref.elem_size != 8 {
        return None;
    }
    crate::perf_stats!(PERF_LIST_GETS += 1);
    // SAFETY: `impl_ref.len() > 0` checked above.
    Some(unsafe { impl_ref.get_value_unchecked(impl_ref.len() - 1) })
}

/// Return the last element of the list wrapped in an Optional tuple, or
/// `None` when the list is empty.
///
/// # Safety
/// * `list` must be a valid `PithList` handle or all-zero.
unsafe fn list_last(list: PithList) -> Option<i64> {
    // SAFETY: `with_list_ref` validates `list` before reading from it.
    unsafe { with_list_ref(list, list_last_impl) }.flatten()
}

#[no_mangle]
pub unsafe extern "C" fn pith_list_last_opt(list: PithList) -> i64 {
    // SAFETY: Preconditions forwarded from caller.
    optional_from(unsafe { list_last(list) })
}

/// `pith_list_last_opt` as a register pair; see `optional_pair`.
#[no_mangle]
pub unsafe extern "C" fn pith_list_last_opt_pair(list: PithList) -> ResultPair {
    // SAFETY: Preconditions forwarded from caller.
    optional_pair(unsafe { list_last(list) })
}

/// Strict pointer-sized indexed access for `list[i]`. Aborts with a
/// structured diagnostic on out-of-bounds instead of returning a silent
/// zero. Callers that want a checked shape have `.get(i)` (returns T?).
///
/// # Safety
/// * `list` must be a valid `PithList` handle or all-zero.
// The three ways `list[i]` can fail, each behind a `#[cold]` call that never
// returns. Formatting a diagnostic needs stack for the format arguments, and
// leaving that inline made every successful index (the overwhelming majority,
// hundreds of millions per compiler run) build a stack frame for a message it
// was never going to print.
#[cold]
#[inline(never)]
pub(super) fn strict_index_bad_handle() -> ! {
    eprintln!("pith runtime error: list indexing on invalid list handle");
    // panic-guard: strict list indexing on an invalid handle is a program bug with no value to return.
    std::process::exit(1);
}

#[cold]
#[inline(never)]
fn strict_index_out_of_bounds(index: i64, len: i64) -> ! {
    crate::failure::fail(format!(
        "list index out of bounds: {} for list of length {} (use .get(i) for Optional access)",
        index, len
    ))
}

#[cold]
#[inline(never)]
fn strict_index_bad_elem_size(elem_size: usize) -> ! {
    eprintln!(
        "pith runtime error: list index requires 8-byte element (elem_size={})",
        elem_size
    );
    // panic-guard: strict list indexing needs an 8-byte element; anything else is a compiler bug.
    std::process::exit(1);
}

#[inline(always)]
fn list_get_value_strict_impl(impl_ref: &ListImpl, index: i64) -> i64 {
    crate::perf_stats!(PERF_LIST_GETS += 1);
    let len = impl_ref.len() as i64;
    if index < 0 || index >= len {
        strict_index_out_of_bounds(index, len);
    }
    if impl_ref.elem_size != 8 {
        strict_index_bad_elem_size(impl_ref.elem_size);
    }
    // SAFETY: `0 <= index < len` checked above.
    unsafe { impl_ref.get_value_unchecked(index as usize) }
}

#[no_mangle]
pub unsafe extern "C" fn pith_list_get_value_strict(list: PithList, index: i64) -> i64 {
    // SAFETY: `with_list_ref` validates `list` before reading from it.
    match unsafe { with_list_ref(list, |impl_ref| list_get_value_strict_impl(impl_ref, index)) } {
        Some(v) => v,
        None => strict_index_bad_handle(),
    }
}

#[inline(always)]
fn list_get_value_impl(impl_ref: &ListImpl, index: i64) -> i64 {
    crate::perf_stats!(
        PERF_LIST_GETS += 1,
        PERF_LIST_GET_VALUE_CALLS += 1,
        PERF_LIST_GET_VALUE_CHECKED_CALLS += 1,
    );

    if index < 0 || index >= impl_ref.len() as i64 {
        return 0;
    }

    if impl_ref.elem_size != 8 {
        crate::perf_count(&crate::PERF_LIST_GET_ELEM_OTHER, 1);
        return 0;
    }

    crate::perf_count(&crate::PERF_LIST_GET_ELEM8, 1);
    // SAFETY: `0 <= index < impl_ref.len()` checked above.
    unsafe { impl_ref.get_value_unchecked(index as usize) }
}

/// Get element at index for pointer-sized elements (returns i64 directly)
/// Returns 0 if out of bounds or on error
#[no_mangle]
pub unsafe extern "C" fn pith_list_get_value(list: PithList, index: i64) -> i64 {
    // SAFETY: `with_list_ref` validates `list` before reading from it.
    unsafe { with_list_ref(list, |impl_ref| list_get_value_impl(impl_ref, index)) }.unwrap_or(0)
}

#[inline(always)]
fn list_get_value_unchecked_impl(impl_ref: &ListImpl, index: i64) -> i64 {
    crate::perf_stats!(
        PERF_LIST_GETS += 1,
        PERF_LIST_GET_VALUE_CALLS += 1,
        PERF_LIST_GET_VALUE_UNCHECKED_CALLS += 1,
    );

    if impl_ref.elem_size != 8 {
        crate::perf_count(&crate::PERF_LIST_GET_ELEM_OTHER, 1);
        return 0;
    }
    if index >= impl_ref.len() as i64 {
        return 0;
    }

    crate::perf_count(&crate::PERF_LIST_GET_ELEM8, 1);
    // SAFETY: `0 <= index < impl_ref.len()` checked by caller and above.
    unsafe { impl_ref.get_value_unchecked(index as usize) }
}

/// Get element at index for pointer-sized elements without an upper-bound check.
/// This is used only in compiler-generated loops that already guard the index.
#[no_mangle]
pub unsafe extern "C" fn pith_list_get_value_unchecked(list: PithList, index: i64) -> i64 {
    if index < 0 {
        return 0;
    }

    // SAFETY: `with_list_ref` validates `list` before reading from it.
    unsafe { with_list_ref(list, |impl_ref| list_get_value_unchecked_impl(impl_ref, index)) }
        .unwrap_or(0)
}

#[inline(always)]
unsafe fn list_get_bytes_impl(
    impl_ref: &ListImpl,
    index: i64,
    elem_size: i64,
    out: *mut u8,
) -> bool {
    crate::perf_stats!(PERF_LIST_GETS += 1, PERF_LIST_GET_BYTES_CALLS += 1);

    if index < 0 || index >= impl_ref.len() as i64 {
        return false;
    }

    if impl_ref.elem_size != elem_size as usize {
        eprintln!("pith: list element size mismatch");
        return false;
    }
    if elem_size == 8 {
        crate::perf_count(&crate::PERF_LIST_GET_ELEM8, 1);
    } else {
        crate::perf_count(&crate::PERF_LIST_GET_ELEM_OTHER, 1);
    }

    match impl_ref.get_value(index as usize) {
        Some(value) => {
            if elem_size == 8 {
                // SAFETY: Caller guarantees `out` is writable for 8 bytes.
                unsafe { std::ptr::copy_nonoverlapping(value.to_ne_bytes().as_ptr(), out, 8) };
            } else if let Some(elem_data) = impl_ref.elements.get(index as usize) {
                // SAFETY: Caller guarantees `out` is writable for `elem_size == elem_data.len()` bytes.
                unsafe { std::ptr::copy_nonoverlapping(elem_data.as_ptr(), out, elem_data.len()) };
            } else {
                return false;
            }

            // Reads borrow: the element stays in the list with its count,
            // and the caller retains only if it keeps the value.
            true
        }
        None => false,
    }
}

/// Get element at index (copies to out buffer)
///
/// Returns true if successful, false if index out of bounds
#[no_mangle]
pub unsafe extern "C" fn pith_list_get(
    list: PithList,
    index: i64,
    elem_size: i64,
    out: *mut u8,
) -> bool {
    if out.is_null() {
        return false;
    }

    // SAFETY: `with_list_ref` validates `list`; `out` is writable for `elem_size` bytes.
    unsafe {
        with_list_ref(list, |impl_ref| {
            list_get_bytes_impl(impl_ref, index, elem_size, out)
        })
    }
    .unwrap_or(false)
}
