use crate::collections::list::{list_mut_from_handle, list_ref_from_handle, PithList};
use crate::ffi_util::{cstr_bytes, cstr_str};

unsafe fn copy_bytes(bytes: &[u8]) -> *mut i8 {
    crate::runtime_core::pith_try_copy_bytes_to_cstring(bytes).unwrap_or(std::ptr::null_mut())
}

unsafe fn alloc_cstring(len: usize) -> *mut i8 {
    crate::runtime_core::pith_try_alloc_cstring(len).unwrap_or(std::ptr::null_mut())
}

/// Get command line arguments as a Pith list of C string pointers.
#[no_mangle]
pub unsafe extern "C" fn pith_args() -> PithList {
    use crate::collections::list::{pith_list_new, pith_list_push_value};
    use std::env;

    let list = pith_list_new(8, 1);

    for arg in env::args() {
        let arg_len = arg.len();
        let arg_ptr = copy_bytes(&arg.as_bytes()[..arg_len]);
        pith_list_push_value(list, arg_ptr as i64);
    }

    list
}

/// Extract substring from C string (start inclusive, end exclusive)
/// Returns newly allocated C string
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_substring(s: *const i8, start: i64, end: i64) -> *mut i8 {
    let Some(bytes) = cstr_bytes(s) else {
        return std::ptr::null_mut();
    };

    let len = bytes.len() as i64;
    let start = start.max(0).min(len) as usize;
    let end = end.max(start as i64).min(len) as usize;
    let sub_len = end - start;

    if sub_len == 0 {
        return crate::pith_cstring_empty();
    }

    copy_bytes(&bytes[start..end])
}

/// Split a string by delimiter and return as a PithList of strings
///
/// # Safety
/// Both s and delim must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn pith_string_split_to_list(s: *const i8, delim: *const i8) -> PithList {
    use crate::collections::list::pith_list_new;

    let (Some(s_slice), Some(delim_slice)) = (cstr_bytes(s), cstr_bytes(delim)) else {
        return pith_list_new(8, 0);
    };
    let s_len = s_slice.len();
    let delim_len = delim_slice.len();

    if s_len == 0 {
        return pith_list_new(8, 0);
    }

    let list = pith_list_new(8, 0);
    let mut start = 0;
    for i in 0..=s_len {
        let is_delim = if delim_len == 0 {
            false
        } else if i + delim_len <= s_len {
            &s_slice[i..i + delim_len] == delim_slice
        } else {
            false
        };

        if is_delim || i == s_len {
            let part_len = i - start;
            if part_len > 0 {
                let part_ptr = copy_bytes(&s_slice[start..i]);
                crate::collections::list::pith_list_push_value(list, part_ptr as i64);
            }

            if delim_len > 0 {
                start = i + delim_len;
            } else {
                start = i + 1;
            }
        }
    }

    list
}

/// Trim ASCII whitespace from both ends of a C string.
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_trim(s: *const i8) -> *mut i8 {
    let Some(slice) = cstr_bytes(s) else {
        return std::ptr::null_mut();
    };

    let mut start = 0usize;
    let mut end = slice.len();

    while start < end && matches!(slice[start], b' ' | b'\t' | b'\n' | b'\r') {
        start += 1;
    }
    while end > start && matches!(slice[end - 1], b' ' | b'\t' | b'\n' | b'\r') {
        end -= 1;
    }

    copy_bytes(&slice[start..end])
}

/// Get a single character from a C string at index as a new C string.
/// Returns a newly allocated 1-character string (or empty string if out of bounds)
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_char_at(s: *const i8, index: i64) -> *mut i8 {
    let Some(bytes) = cstr_bytes(s) else {
        return crate::pith_cstring_empty();
    };

    if index < 0 || index as usize >= bytes.len() {
        return crate::pith_cstring_empty();
    }

    let ptr = alloc_cstring(1);
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    *ptr = bytes[index as usize] as i8;
    *ptr.add(1) = 0;
    ptr
}

/// Trim ASCII whitespace from the left side of a C string.
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_trim_left(s: *const i8) -> *mut i8 {
    let Some(slice) = cstr_bytes(s) else {
        return std::ptr::null_mut();
    };

    let mut start = 0usize;
    while start < slice.len() && matches!(slice[start], b' ' | b'\t' | b'\n' | b'\r') {
        start += 1;
    }

    copy_bytes(&slice[start..])
}

/// Convert C string to uppercase
/// Returns newly allocated C string
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_to_upper(s: *const i8) -> *mut i8 {
    let Some(slice) = cstr_bytes(s) else {
        return std::ptr::null_mut();
    };
    let len = slice.len();

    let ptr = alloc_cstring(len);
    if ptr.is_null() {
        return std::ptr::null_mut();
    }

    for i in 0..len {
        let c = slice[i];
        *ptr.add(i) = (c as char).to_ascii_uppercase() as u8 as i8;
    }
    *ptr.add(len) = 0;
    ptr
}

/// Convert C string to lowercase
/// Returns newly allocated C string
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_to_lower(s: *const i8) -> *mut i8 {
    let Some(slice) = cstr_bytes(s) else {
        return std::ptr::null_mut();
    };
    let len = slice.len();

    let ptr = alloc_cstring(len);
    if ptr.is_null() {
        return std::ptr::null_mut();
    }

    for i in 0..len {
        let c = slice[i];
        *ptr.add(i) = (c as char).to_ascii_lowercase() as u8 as i8;
    }
    *ptr.add(len) = 0;
    ptr
}

/// Reverse a C string
/// Returns newly allocated C string
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_reverse(s: *const i8) -> *mut i8 {
    let Some(slice) = cstr_bytes(s) else {
        return std::ptr::null_mut();
    };
    let len = slice.len();

    let ptr = alloc_cstring(len);
    if ptr.is_null() {
        return std::ptr::null_mut();
    }

    for i in 0..len {
        *ptr.add(i) = slice[len - 1 - i] as i8;
    }
    *ptr.add(len) = 0;
    ptr
}

/// Split string into a list of single-character strings (chars)
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_chars(s: *const i8) -> i64 {
    use crate::collections::list::{pith_list_new, pith_list_push_value};

    let list = pith_list_new(8, 0);
    if let Some(bytes) = cstr_bytes(s) {
        for &b in bytes {
            let ch_ptr = crate::pith_chr_cstr(b as i64);
            pith_list_push_value(list, ch_ptr as i64);
        }
    }
    list.ptr as i64
}

/// Sort a list of C-string pointers in-place (lexicographic order)
///
/// # Safety
/// list_ptr is i64 carrying the PithList's internal ptr value;
/// each 8-byte element is a *const i8 pointer to a null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn pith_list_sort_strings(list_ptr: i64) {
    let Some(impl_ref) = list_mut_from_handle(list_ptr) else {
        return;
    };
    if impl_ref.elem_size != 8 {
        return;
    }
    impl_ref.values8.sort_by(|a, b| {
        let ap = *a as *const i8;
        let bp = *b as *const i8;
        match (cstr_bytes(ap), cstr_bytes(bp)) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, Some(_)) => std::cmp::Ordering::Less,
            (Some(_), None) => std::cmp::Ordering::Greater,
            (Some(a_bytes), Some(b_bytes)) => a_bytes.cmp(b_bytes),
        }
    });
    impl_ref.sync_value_view();
}

/// Sort a list of i64 values in-place
///
/// # Safety
/// list_ptr is i64 carrying the PithList's internal ptr value
#[no_mangle]
pub unsafe extern "C" fn pith_list_sort(list_ptr: i64) {
    let Some(impl_ref) = list_mut_from_handle(list_ptr) else {
        return;
    };
    if impl_ref.elem_size != 8 {
        return;
    }
    impl_ref.values8.sort();
    impl_ref.sync_value_view();
}

/// Get a sub-slice of a list
///
/// # Safety
/// list_ptr is i64 carrying the PithList's internal ptr value
#[no_mangle]
pub unsafe extern "C" fn pith_list_slice(list_ptr: i64, start: i64, end: i64) -> i64 {
    use crate::collections::list::{pith_list_new, pith_list_push_value};

    let new_list = pith_list_new(8, 0);
    if let Some(impl_ref) = list_ref_from_handle(list_ptr) {
        let len = impl_ref.len() as i64;
        let s = start.max(0).min(len) as usize;
        let e = end.max(0).min(len) as usize;
        for i in s..e {
            if let Some(val) = impl_ref.get_value(i) {
                pith_list_push_value(new_list, val);
            }
        }
    }
    new_list.ptr as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_list_sort_copy(list_ptr: i64) -> i64 {
    use crate::collections::list::{pith_list_new, pith_list_push_value};

    let new_list = pith_list_new(8, 0);
    let Some(impl_ref) = list_ref_from_handle(list_ptr) else {
        return new_list.ptr as i64;
    };
    let mut i = 0usize;
    while i < impl_ref.len() {
        if let Some(val) = impl_ref.get_value(i) {
            pith_list_push_value(new_list, val);
        }
        i += 1;
    }

    pith_list_sort(new_list.ptr as i64);
    new_list.ptr as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_list_sort_strings_copy(list_ptr: i64) -> i64 {
    use crate::collections::list::{pith_list_new, pith_list_push_value};

    let new_list = pith_list_new(8, 0);
    let Some(impl_ref) = list_ref_from_handle(list_ptr) else {
        return new_list.ptr as i64;
    };
    let mut i = 0usize;
    while i < impl_ref.len() {
        if let Some(val) = impl_ref.get_value(i) {
            pith_list_push_value(new_list, val);
        }
        i += 1;
    }

    pith_list_sort_strings(new_list.ptr as i64);
    new_list.ptr as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_list_slice_copy(list_ptr: i64, start: i64, end: i64) -> i64 {
    pith_list_slice(list_ptr, start, end)
}

/// Replace all occurrences of `from` with `to` in `s`
/// Returns newly allocated C string
///
/// # Safety
/// All pointers must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_replace(
    s: *const i8,
    from: *const i8,
    to: *const i8,
) -> *mut i8 {
    let Some(s_bytes) = cstr_bytes(s) else {
        return std::ptr::null_mut();
    };
    let from_bytes = cstr_bytes(from).unwrap_or(&[]);
    let to_bytes = cstr_bytes(to).unwrap_or(&[]);
    let s_len = s_bytes.len();
    let from_len = from_bytes.len();

    if from_len == 0 {
        return copy_bytes(s_bytes);
    }

    let mut result: Vec<u8> = Vec::new();
    if result.try_reserve(s_len).is_err() {
        return std::ptr::null_mut();
    }
    let mut i = 0;
    while i <= s_len.saturating_sub(from_len) {
        if &s_bytes[i..i + from_len] == from_bytes {
            if result.try_reserve(to_bytes.len()).is_err() {
                return std::ptr::null_mut();
            }
            result.extend_from_slice(to_bytes);
            i += from_len;
        } else {
            if result.try_reserve(1).is_err() {
                return std::ptr::null_mut();
            }
            result.push(s_bytes[i]);
            i += 1;
        }
    }
    while i < s_len {
        if result.try_reserve(1).is_err() {
            return std::ptr::null_mut();
        }
        result.push(s_bytes[i]);
        i += 1;
    }

    copy_bytes(&result)
}

/// Check if a C string is empty (null or zero-length)
///
/// # Safety
/// s must be null or a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_is_empty(s: *const i8) -> i64 {
    if matches!(cstr_bytes(s), None | Some([])) {
        1
    } else {
        0
    }
}

/// Find last index of needle in haystack, returns -1 if not found
///
/// # Safety
/// Both arguments must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_last_index_of(haystack: *const i8, needle: *const i8) -> i64 {
    let (Some(h), Some(n)) = (cstr_str(haystack), cstr_str(needle)) else {
        return -1;
    };
    match h.rfind(n) {
        Some(idx) => idx as i64,
        None => -1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_helpers_handle_null_and_invalid_utf8() {
        let invalid = [0xffu8, 0x00];
        let ptr = invalid.as_ptr() as *const i8;

        unsafe {
            assert!(pith_cstring_substring(std::ptr::null(), 0, 1).is_null());
            let ch = pith_cstring_char_at(std::ptr::null(), 0);
            assert_eq!(crate::ffi_util::cstr_bytes(ch), Some(&[][..]));
            assert_eq!(pith_cstring_is_empty(std::ptr::null()), 1);
            assert_eq!(pith_cstring_is_empty(ptr), 0);
            assert_eq!(
                pith_cstring_last_index_of(ptr, b"x\0".as_ptr() as *const i8),
                -1
            );
        }
    }

    #[test]
    fn string_helpers_use_checked_allocations() {
        unsafe {
            let text = b" Pith \0";
            let ptr = text.as_ptr() as *const i8;

            let trimmed = pith_cstring_trim(ptr);
            assert_eq!(crate::ffi_util::cstr_bytes(trimmed), Some(&b"Pith"[..]));

            let upper = pith_cstring_to_upper(trimmed);
            assert_eq!(crate::ffi_util::cstr_bytes(upper), Some(&b"PITH"[..]));

            let lower = pith_cstring_to_lower(upper);
            assert_eq!(crate::ffi_util::cstr_bytes(lower), Some(&b"pith"[..]));

            let reversed = pith_cstring_reverse(lower);
            assert_eq!(crate::ffi_util::cstr_bytes(reversed), Some(&b"htip"[..]));

            let ch = pith_cstring_char_at(reversed, 1);
            assert_eq!(crate::ffi_util::cstr_bytes(ch), Some(&b"t"[..]));

            crate::pith_free(trimmed);
            crate::pith_free(upper);
            crate::pith_free(lower);
            crate::pith_free(reversed);
            crate::pith_free(ch);
        }
    }
}
