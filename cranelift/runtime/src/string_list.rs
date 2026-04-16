use crate::collections::list::ForgeList;

/// Get command line arguments as a Forge list of C string pointers.
#[no_mangle]
pub unsafe extern "C" fn forge_args() -> ForgeList {
    use crate::collections::list::{forge_list_new, forge_list_push_value};
    use std::alloc::{alloc, Layout};
    use std::env;

    let list = forge_list_new(8, 1);

    for arg in env::args() {
        let arg_len = arg.len();
        let arg_layout = Layout::from_size_align(arg_len + 1, 1).unwrap();
        let arg_ptr = alloc(arg_layout) as *mut i8;

        if !arg_ptr.is_null() {
            std::ptr::copy_nonoverlapping(arg.as_ptr(), arg_ptr as *mut u8, arg_len);
            *arg_ptr.add(arg_len) = 0;
            forge_list_push_value(list, arg_ptr as i64);
        }
    }

    list
}

/// Extract substring from C string (start inclusive, end exclusive)
/// Returns newly allocated C string
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_substring(s: *const i8, start: i64, end: i64) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if s.is_null() {
        return std::ptr::null_mut();
    }

    let len = crate::string::forge_cstring_len(s);
    let start = start.max(0).min(len);
    let end = end.max(start).min(len);
    let sub_len = (end - start) as usize;

    if sub_len == 0 {
        let layout = Layout::from_size_align(1, 1).unwrap();
        let ptr = alloc(layout) as *mut i8;
        if !ptr.is_null() {
            *ptr = 0;
        }
        return ptr;
    }

    let layout = Layout::from_size_align(sub_len + 1, 1).unwrap();
    let ptr = alloc(layout) as *mut i8;

    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(
            s.offset(start as isize) as *const u8,
            ptr as *mut u8,
            sub_len,
        );
        *ptr.add(sub_len) = 0;
    }
    ptr
}

/// Split a string by delimiter and return as a ForgeList of strings
///
/// # Safety
/// Both s and delim must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn forge_string_split_to_list(s: *const i8, delim: *const i8) -> ForgeList {
    use crate::collections::list::forge_list_new;
    use std::alloc::{alloc, Layout};

    if s.is_null() || delim.is_null() {
        return forge_list_new(8, 0);
    }

    let s_len = crate::string::forge_cstring_len(s) as usize;
    let delim_len = crate::string::forge_cstring_len(delim) as usize;

    if s_len == 0 {
        return forge_list_new(8, 0);
    }

    let s_slice = std::slice::from_raw_parts(s as *const u8, s_len);
    let delim_slice = if delim_len == 0 {
        &[] as &[u8]
    } else {
        std::slice::from_raw_parts(delim as *const u8, delim_len)
    };

    let list = forge_list_new(8, 0);
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
                let part_layout = Layout::from_size_align(part_len + 1, 1).unwrap();
                let part_ptr = alloc(part_layout) as *mut i8;

                if !part_ptr.is_null() {
                    std::ptr::copy_nonoverlapping(&s_slice[start], part_ptr as *mut u8, part_len);
                    *part_ptr.add(part_len) = 0;
                    crate::collections::list::forge_list_push_value(list, part_ptr as i64);
                }
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
pub unsafe extern "C" fn forge_cstring_trim(s: *const i8) -> *mut i8 {
    if s.is_null() {
        return std::ptr::null_mut();
    }

    let len = crate::string::forge_cstring_len(s) as usize;
    let slice = std::slice::from_raw_parts(s as *const u8, len);

    let mut start = 0usize;
    let mut end = len;

    while start < end && matches!(slice[start], b' ' | b'\t' | b'\n' | b'\r') {
        start += 1;
    }
    while end > start && matches!(slice[end - 1], b' ' | b'\t' | b'\n' | b'\r') {
        end -= 1;
    }

    forge_cstring_substring(s, start as i64, end as i64)
}

/// Get a single character from a C string at index as a new C string.
/// Returns a newly allocated 1-character string (or empty string if out of bounds)
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_char_at(s: *const i8, index: i64) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if s.is_null() {
        let ptr = alloc(Layout::from_size_align(1, 1).unwrap()) as *mut i8;
        if !ptr.is_null() {
            *ptr = 0;
        }
        return ptr;
    }

    let len = crate::string::forge_cstring_len(s);
    if index < 0 || index >= len {
        let ptr = alloc(Layout::from_size_align(1, 1).unwrap()) as *mut i8;
        if !ptr.is_null() {
            *ptr = 0;
        }
        return ptr;
    }

    let ptr = alloc(Layout::from_size_align(2, 1).unwrap()) as *mut i8;
    if !ptr.is_null() {
        *ptr = *s.offset(index as isize);
        *ptr.add(1) = 0;
    }
    ptr
}

/// Trim ASCII whitespace from the left side of a C string.
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_trim_left(s: *const i8) -> *mut i8 {
    if s.is_null() {
        return std::ptr::null_mut();
    }

    let len = crate::string::forge_cstring_len(s) as usize;
    let slice = std::slice::from_raw_parts(s as *const u8, len);

    let mut start = 0usize;
    while start < len && matches!(slice[start], b' ' | b'\t' | b'\n' | b'\r') {
        start += 1;
    }

    forge_cstring_substring(s, start as i64, len as i64)
}

/// Convert C string to uppercase
/// Returns newly allocated C string
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_to_upper(s: *const i8) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if s.is_null() {
        return std::ptr::null_mut();
    }

    let len = crate::string::forge_cstring_len(s) as usize;
    let slice = std::slice::from_raw_parts(s as *const u8, len);

    let layout = Layout::from_size_align(len + 1, 1).unwrap();
    let ptr = alloc(layout) as *mut i8;

    if !ptr.is_null() {
        for i in 0..len {
            let c = slice[i];
            *ptr.add(i) = (c as char).to_ascii_uppercase() as u8 as i8;
        }
        *ptr.add(len) = 0;
    }
    ptr
}

/// Convert C string to lowercase
/// Returns newly allocated C string
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_to_lower(s: *const i8) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if s.is_null() {
        return std::ptr::null_mut();
    }

    let len = crate::string::forge_cstring_len(s) as usize;
    let slice = std::slice::from_raw_parts(s as *const u8, len);

    let layout = Layout::from_size_align(len + 1, 1).unwrap();
    let ptr = alloc(layout) as *mut i8;

    if !ptr.is_null() {
        for i in 0..len {
            let c = slice[i];
            *ptr.add(i) = (c as char).to_ascii_lowercase() as u8 as i8;
        }
        *ptr.add(len) = 0;
    }
    ptr
}

/// Reverse a C string
/// Returns newly allocated C string
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_reverse(s: *const i8) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if s.is_null() {
        return std::ptr::null_mut();
    }

    let len = crate::string::forge_cstring_len(s) as usize;
    let slice = std::slice::from_raw_parts(s as *const u8, len);

    let layout = Layout::from_size_align(len + 1, 1).unwrap();
    let ptr = alloc(layout) as *mut i8;

    if !ptr.is_null() {
        for i in 0..len {
            *ptr.add(i) = slice[len - 1 - i] as i8;
        }
        *ptr.add(len) = 0;
    }
    ptr
}

/// Split string into a list of single-character strings (chars)
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_chars(s: *const i8) -> i64 {
    use crate::collections::list::{forge_list_new, forge_list_push_value};

    let list = forge_list_new(8, 0);
    if !s.is_null() {
        let len = crate::string::forge_cstring_len(s) as usize;
        let bytes = std::slice::from_raw_parts(s as *const u8, len);
        for &b in bytes {
            let ch_ptr = crate::forge_chr_cstr(b as i64);
            forge_list_push_value(list, ch_ptr as i64);
        }
    }
    list.ptr as i64
}

/// Sort a list of C-string pointers in-place (lexicographic order)
///
/// # Safety
/// list_ptr is i64 carrying the ForgeList's internal ptr value;
/// each 8-byte element is a *const i8 pointer to a null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn forge_list_sort_strings(list_ptr: i64) {
    let list = ForgeList {
        ptr: list_ptr as *mut (),
    };
    if list.ptr.is_null() {
        return;
    }
    let impl_ref = &mut *(list.ptr as *mut crate::collections::list::ListImpl);
    if impl_ref.elem_size != 8 {
        return;
    }
    impl_ref.values8.sort_by(|a, b| {
        let ap = *a as *const i8;
        let bp = *b as *const i8;
        if ap.is_null() && bp.is_null() {
            return std::cmp::Ordering::Equal;
        }
        if ap.is_null() {
            return std::cmp::Ordering::Less;
        }
        if bp.is_null() {
            return std::cmp::Ordering::Greater;
        }
        let a_str = std::ffi::CStr::from_ptr(ap);
        let b_str = std::ffi::CStr::from_ptr(bp);
        a_str.cmp(b_str)
    });
    impl_ref.sync_value_view();
}

/// Sort a list of i64 values in-place
///
/// # Safety
/// list_ptr is i64 carrying the ForgeList's internal ptr value
#[no_mangle]
pub unsafe extern "C" fn forge_list_sort(list_ptr: i64) {
    let list = ForgeList {
        ptr: list_ptr as *mut (),
    };
    if list.ptr.is_null() {
        return;
    }
    let impl_ref = &mut *(list.ptr as *mut crate::collections::list::ListImpl);
    if impl_ref.elem_size != 8 {
        return;
    }
    impl_ref.values8.sort();
    impl_ref.sync_value_view();
}

/// Get a sub-slice of a list
///
/// # Safety
/// list_ptr is i64 carrying the ForgeList's internal ptr value
#[no_mangle]
pub unsafe extern "C" fn forge_list_slice(list_ptr: i64, start: i64, end: i64) -> i64 {
    use crate::collections::list::{forge_list_new, forge_list_push_value};

    let new_list = forge_list_new(8, 0);
    let list = ForgeList {
        ptr: list_ptr as *mut (),
    };
    if !list.ptr.is_null() {
        let impl_ref = &*(list.ptr as *const crate::collections::list::ListImpl);
        let len = impl_ref.len() as i64;
        let s = start.max(0).min(len) as usize;
        let e = end.max(0).min(len) as usize;
        for i in s..e {
            if let Some(val) = impl_ref.get_value(i) {
                forge_list_push_value(new_list, val);
            }
        }
    }
    new_list.ptr as i64
}

#[no_mangle]
pub unsafe extern "C" fn forge_list_sort_copy(list_ptr: i64) -> i64 {
    use crate::collections::list::{forge_list_new, forge_list_push_value};

    let new_list = forge_list_new(8, 0);
    let list = ForgeList {
        ptr: list_ptr as *mut (),
    };
    if list.ptr.is_null() {
        return new_list.ptr as i64;
    }

    let impl_ref = &*(list.ptr as *const crate::collections::list::ListImpl);
    let mut i = 0usize;
    while i < impl_ref.len() {
        if let Some(val) = impl_ref.get_value(i) {
            forge_list_push_value(new_list, val);
        }
        i += 1;
    }

    forge_list_sort(new_list.ptr as i64);
    new_list.ptr as i64
}

#[no_mangle]
pub unsafe extern "C" fn forge_list_sort_strings_copy(list_ptr: i64) -> i64 {
    use crate::collections::list::{forge_list_new, forge_list_push_value};

    let new_list = forge_list_new(8, 0);
    let list = ForgeList {
        ptr: list_ptr as *mut (),
    };
    if list.ptr.is_null() {
        return new_list.ptr as i64;
    }

    let impl_ref = &*(list.ptr as *const crate::collections::list::ListImpl);
    let mut i = 0usize;
    while i < impl_ref.len() {
        if let Some(val) = impl_ref.get_value(i) {
            forge_list_push_value(new_list, val);
        }
        i += 1;
    }

    forge_list_sort_strings(new_list.ptr as i64);
    new_list.ptr as i64
}

#[no_mangle]
pub unsafe extern "C" fn forge_list_slice_copy(list_ptr: i64, start: i64, end: i64) -> i64 {
    forge_list_slice(list_ptr, start, end)
}

/// Replace all occurrences of `from` with `to` in `s`
/// Returns newly allocated C string
///
/// # Safety
/// All pointers must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_replace(
    s: *const i8,
    from: *const i8,
    to: *const i8,
) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if s.is_null() {
        return std::ptr::null_mut();
    }

    let s_len = crate::string::forge_cstring_len(s) as usize;
    let from_len = if from.is_null() {
        0
    } else {
        crate::string::forge_cstring_len(from) as usize
    };
    let to_len = if to.is_null() {
        0
    } else {
        crate::string::forge_cstring_len(to) as usize
    };

    if from_len == 0 {
        let layout = Layout::from_size_align(s_len + 1, 1).unwrap();
        let out = alloc(layout) as *mut i8;
        if !out.is_null() {
            std::ptr::copy_nonoverlapping(s, out, s_len);
            *out.add(s_len) = 0;
        }
        return out;
    }

    let s_bytes = std::slice::from_raw_parts(s as *const u8, s_len);
    let from_bytes = std::slice::from_raw_parts(from as *const u8, from_len);
    let to_bytes = if to.is_null() {
        &[][..]
    } else {
        std::slice::from_raw_parts(to as *const u8, to_len)
    };

    let mut result: Vec<u8> = Vec::with_capacity(s_len);
    let mut i = 0;
    while i <= s_len.saturating_sub(from_len) {
        if &s_bytes[i..i + from_len] == from_bytes {
            result.extend_from_slice(to_bytes);
            i += from_len;
        } else {
            result.push(s_bytes[i]);
            i += 1;
        }
    }
    while i < s_len {
        result.push(s_bytes[i]);
        i += 1;
    }

    let out_len = result.len();
    let layout = Layout::from_size_align(out_len + 1, 1).unwrap();
    let out = alloc(layout) as *mut i8;
    if !out.is_null() {
        std::ptr::copy_nonoverlapping(result.as_ptr(), out as *mut u8, out_len);
        *out.add(out_len) = 0;
    }
    out
}

/// Check if a C string is empty (null or zero-length)
///
/// # Safety
/// s must be null or a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_is_empty(s: *const i8) -> i64 {
    if s.is_null() {
        return 1;
    }
    let len = crate::string::forge_cstring_len(s);
    if len == 0 { 1 } else { 0 }
}

/// Find last index of needle in haystack, returns -1 if not found
///
/// # Safety
/// Both arguments must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_last_index_of(
    haystack: *const i8,
    needle: *const i8,
) -> i64 {
    if haystack.is_null() || needle.is_null() {
        return -1;
    }
    let h_len = crate::string::forge_cstring_len(haystack) as usize;
    let n_len = crate::string::forge_cstring_len(needle) as usize;
    let h_slice = std::slice::from_raw_parts(haystack as *const u8, h_len);
    let n_slice = std::slice::from_raw_parts(needle as *const u8, n_len);
    if let (Ok(h), Ok(n)) = (std::str::from_utf8(h_slice), std::str::from_utf8(n_slice)) {
        match h.rfind(n) {
            Some(idx) => idx as i64,
            None => -1,
        }
    } else {
        -1
    }
}
