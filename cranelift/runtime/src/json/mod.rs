//! Direct JSON-to-struct decoding and float parsing helpers.

mod fill;
mod scanner;
mod spec;
mod targets;
#[cfg(test)]
mod tests;

use crate::bytes::with_bytes;
use crate::ffi_util::with_cstr_bytes;
use fill::nested_fill_object;
use scanner::{float_of_span, skip_ws};
use targets::{DtorTable, ObjectTarget};

/// What `pith_json_float_bits` answers for a span that is not a finite
/// number: a NaN, which no finite value's bit pattern can be.
pub const JSON_FLOAT_REFUSED: i64 = 0x7ff8_0000_0000_0001;

/// The f64 bit pattern of the json number span `ptr[start..end]`, through
/// `float_of_span`, or `JSON_FLOAT_REFUSED`. The flat decoder the compiler
/// generates scans the span itself and hands it here, so a Float field
/// gets the value `parse_float` and the node parser give the same text.
///
/// # Safety
/// The bytes `ptr[start..end]` must be readable.
#[no_mangle]
pub unsafe extern "C" fn pith_json_float_bits(ptr: i64, start: i64, end: i64) -> i64 {
    if ptr == 0 || start < 0 || end < start {
        return JSON_FLOAT_REFUSED;
    }
    // SAFETY: Caller guarantees the bytes `ptr[start..end]` are readable.
    let span = unsafe {
        std::slice::from_raw_parts((ptr as *const u8).add(start as usize), (end - start) as usize)
    };
    match float_of_span(span) {
        Some(value) => value.to_bits() as i64,
        None => JSON_FLOAT_REFUSED,
    }
}

fn alloc_result(is_ok: i64, ok: i64, err: i64) -> i64 {
    // SAFETY: Allocates a fresh 3-slot struct; null is checked before writing slots 0..3.
    let tuple = unsafe { crate::pith_struct_alloc(3) } as *mut i64;
    if tuple.is_null() {
        return 0;
    }
    // SAFETY: `tuple` is a freshly allocated, non-null 3-slot struct.
    unsafe {
        *tuple = is_ok;
        *tuple.add(1) = ok;
        *tuple.add(2) = err;
    }
    tuple as i64
}

/// Destructor for the one-field error struct `err_result` builds: the
/// message string is the struct's to drop. The struct stands in for the
/// compiler's `JsonDecodeError`, whose own destructor does the same, but
/// this one is allocated here and the compiler never attaches one to it.
unsafe extern "C" fn decode_error_dtor(ptr: i64) {
    // SAFETY: `ptr` is the 1-slot error struct allocated by `err_result`.
    let message = unsafe { *(ptr as *const i64) };
    if message != 0 {
        // SAFETY: Slot 0 holds an owned pith C string written by `err_result`.
        unsafe { crate::pith_cstring_release(message as *const i8) };
    }
}

fn err_result(message: &[u8]) -> i64 {
    // SAFETY: Allocates a fresh 1-slot error struct; null is checked before writing slot 0.
    let error = unsafe { crate::pith_struct_alloc(1) } as *mut i64;
    if error.is_null() {
        return 0;
    }
    let msg = crate::pith_copy_bytes_to_cstring(message) as i64;
    // SAFETY: `error` is a freshly allocated, non-null 1-slot struct; `decode_error_dtor` matches its layout.
    unsafe {
        *error = msg;
        crate::pith_struct_set_dtor(error as i64, decode_error_dtor as *const () as usize as i64);
    }
    alloc_result(0, 0, error as i64)
}
/// Decode an object with nested struct fields straight into a pre-allocated
/// tree of structs in a single pass, returning the decode's result box: the
/// ok box around `struct_ptr`, or an error box after `struct_ptr` has been
/// released (its destructor drops every sub-struct and string filled so far).
/// `check_utf8` is set for a bytes input, which the node path validated
/// before parsing; text input is taken as it is.
///
/// # Safety
/// `struct_ptr` must be a pith_struct_alloc result laid out by `spec`, with
/// a struct laid out by the sub-spec already stored in every `o` slot and a
/// destructor attached wherever a slot holds a string or a struct.
#[no_mangle]
pub unsafe extern "C" fn pith_json_fill_struct_nested(
    bytes_handle: i64,
    spec_ptr: i64,
    struct_ptr: i64,
    check_utf8: i64,
) -> i64 {
    // SAFETY: Preconditions forwarded from caller; no destructor table is used.
    unsafe { nested_fill_root(bytes_handle, spec_ptr, struct_ptr, check_utf8, &DtorTable::EMPTY) }
}

/// `pith_json_fill_struct_nested` for a spec with a list field: `dtors_ptr`
/// is a struct of `dtor_count` slots the lowering filled with the address
/// of the destructor each `#<k>` in the spec names (0 for a struct that
/// needs none), or 0 when no list element is a struct. The table belongs
/// to the caller, which releases it after the call. Every list and every
/// element struct the fill builds is owned by the root struct from the
/// moment it exists, so the release on failure drops it all.
///
/// # Safety
/// As `pith_json_fill_struct_nested`, and `dtors_ptr` must be a
/// pith_struct_alloc result of at least `dtor_count` slots or 0.
#[no_mangle]
pub unsafe extern "C" fn pith_json_fill_struct_lists(
    bytes_handle: i64,
    spec_ptr: i64,
    struct_ptr: i64,
    check_utf8: i64,
    dtors_ptr: i64,
    dtor_count: i64,
) -> i64 {
    // SAFETY: Caller guarantees `dtors_ptr` is 0 or holds at least `dtor_count` destructor addresses.
    let dtors = unsafe { DtorTable::from_raw(dtors_ptr, dtor_count) };
    // SAFETY: Preconditions forwarded from caller.
    unsafe { nested_fill_root(bytes_handle, spec_ptr, struct_ptr, check_utf8, &dtors) }
}

fn nested_fill_from_input(
    input: &[u8],
    target: ObjectTarget<'_>,
    check_utf8: bool,
    dtors: &DtorTable<'_>,
) -> Result<(), Vec<u8>> {
    if check_utf8 && std::str::from_utf8(input).is_err() {
        return Err(b"bytes_to_string_utf8 failed".to_vec());
    }
    let pos = skip_ws(input, 0);
    if pos >= input.len() || input[pos] != b'{' {
        return Err(b"invalid json object".to_vec());
    }
    let (end, field_error) = match nested_fill_object(input, pos, target, dtors, 1) {
        Ok(filled) => filled,
        Err(abort) => return Err(abort.message().to_vec()),
    };
    if skip_ws(input, end) < input.len() {
        return Err(b"invalid json object".to_vec());
    }
    match field_error {
        Some(msg) => Err(msg),
        None => Ok(()),
    }
}

unsafe fn nested_fill_with_spec(
    bytes_handle: i64,
    spec: &[u8],
    struct_ptr: i64,
    check_utf8: bool,
    dtors: &DtorTable<'_>,
) -> Result<(), Vec<u8>> {
    // SAFETY: Caller guarantees `struct_ptr` is laid out by `spec`.
    let target = unsafe { ObjectTarget::from_raw(spec, struct_ptr) };
    // SAFETY: `with_bytes` validates `bytes_handle` before borrowing its slice.
    unsafe {
        with_bytes(bytes_handle, |input| {
            nested_fill_from_input(input, target, check_utf8, dtors)
        })
    }
    .unwrap_or_else(|| Err(b"invalid json object".to_vec()))
}

unsafe fn nested_fill_root(
    bytes_handle: i64,
    spec_ptr: i64,
    struct_ptr: i64,
    check_utf8: i64,
    dtors: &DtorTable<'_>,
) -> i64 {
    let check_utf8 = check_utf8 != 0;
    // SAFETY: Caller guarantees `spec_ptr` is null or a valid null-terminated C string.
    let outcome = unsafe {
        with_cstr_bytes(spec_ptr as *const i8, |spec| {
            nested_fill_with_spec(bytes_handle, spec, struct_ptr, check_utf8, dtors)
        })
    }
    .unwrap_or_else(|| {
        // SAFETY: Preconditions forwarded from caller for an empty spec when `spec_ptr` is null.
        unsafe { nested_fill_with_spec(bytes_handle, &[], struct_ptr, check_utf8, dtors) }
    });
    match outcome {
        Ok(()) => alloc_result(1, struct_ptr, 0),
        Err(msg) => {
            // SAFETY: Caller transferred ownership of `struct_ptr`; release it on decode failure.
            unsafe { crate::pith_struct_release(struct_ptr) };
            err_result(&msg)
        }
    }
}
