use crate::collections::list::{list_mut_from_handle, list_ref_from_handle, PithList};
use crate::ffi_util::{cstr_bytes, cstr_str};
use crate::runtime_core::optional_tuple;

/// Get command line arguments as a Pith list of C string pointers.
#[no_mangle]
pub unsafe extern "C" fn pith_args() -> PithList {
    use crate::collections::list::{pith_list_new, pith_list_push_value};
    use std::env;

    let list = pith_list_new(8, 1);

    for arg in env::args() {
        let arg_len = arg.len();
        let arg_ptr = crate::pith_copy_bytes_to_cstring(&arg.as_bytes()[..arg_len]);
        pith_list_push_value(list, arg_ptr as i64);
        crate::pith_cstring_release(arg_ptr as *const i8);
    }

    list
}

/// Report whether a byte offset lands strictly inside a well-formed multi-byte
/// utf-8 character.
///
/// The test deliberately requires the surrounding bytes to *decode*, not merely
/// to look like a continuation. A String is a byte string, and code that keeps
/// binary data in one is entitled to slice it wherever it likes; only text gets
/// a character boundary. Requiring a valid sequence means this answers "this cut
/// would corrupt a character that was really there" rather than "this byte has
/// the high bits of a continuation".
fn splits_a_character(bytes: &[u8], index: usize) -> bool {
    if index == 0 || index >= bytes.len() {
        return false;
    }
    if (bytes[index] & 0xC0) != 0x80 {
        return false;
    }

    // walk back to the byte that could have started this sequence. a utf-8
    // character is at most 4 bytes, so this stops after at most 3 steps.
    let mut lead = index;
    while lead > 0 && (bytes[lead] & 0xC0) == 0x80 && index - lead < 3 {
        lead -= 1;
    }

    let width = match bytes[lead] {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        // not a lead byte: these bytes are not a character, so there is
        // nothing here to split.
        _ => return false,
    };

    if lead + width > bytes.len() {
        return false;
    }
    if std::str::from_utf8(&bytes[lead..lead + width]).is_err() {
        return false;
    }
    index < lead + width
}

/// Abort with a diagnostic naming the cut that would have corrupted a
/// character.
fn report_split(bytes: &[u8], index: usize, start: i64, end: i64) -> ! {
    let mut lead = index;
    while lead > 0 && (bytes[lead] & 0xC0) == 0x80 {
        lead -= 1;
    }
    let width = match bytes[lead] {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    };
    let character = std::str::from_utf8(&bytes[lead..lead + width])
        .map(|s| s.to_string())
        .unwrap_or_default();

    eprintln!(
        "pith runtime error: substring({}, {}) would split the character '{}' \
         at byte {} (it occupies bytes {}..{})",
        start,
        end,
        character,
        index,
        lead,
        lead + width
    );
    eprintln!(
        "  a String is indexed in bytes, so a byte offset can land inside a \
         multi-byte character. use std.text for character-aware work:"
    );
    eprintln!("    text.slice(s, a, b)         slice by character index");
    eprintln!("    text.truncate(s, n)         keep n characters");
    eprintln!("    text.truncate_bytes(s, n)   keep n bytes, ending on a boundary");
    std::process::exit(1);
}

/// Extract substring from C string (start inclusive, end exclusive)
/// Returns newly allocated C string
///
/// Aborts when either bound would cut a well-formed utf-8 character in half.
/// That cut used to succeed and hand back a string that no longer round-trips
/// through utf-8, which is silent corruption of user text; failing loudly
/// matches `pith_cstring_char_at_strict` and the other strict accessors.
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_substring(s: *const i8, start: i64, end: i64) -> *mut i8 {
    let Some(bytes) = cstr_bytes(s) else {
        return std::ptr::null_mut();
    };

    let len = bytes.len() as i64;
    let lo = start.max(0).min(len) as usize;
    let hi = end.max(lo as i64).min(len) as usize;

    if splits_a_character(bytes, lo) {
        report_split(bytes, lo, start, end);
    }
    if splits_a_character(bytes, hi) {
        report_split(bytes, hi, start, end);
    }

    if hi - lo == 0 {
        return crate::pith_cstring_empty();
    }

    crate::pith_copy_bytes_to_cstring(&bytes[lo..hi])
}

/// Split a string by delimiter and return as a PithList of strings
///
/// # Safety
/// Both s and delim must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn pith_string_split_to_list(s: *const i8, delim: *const i8) -> PithList {
    use crate::collections::list::pith_list_new;

    let (Some(s_slice), Some(delim_slice)) = (cstr_bytes(s), cstr_bytes(delim)) else {
        return pith_list_new(8, 1);
    };
    let s_len = s_slice.len();
    let delim_len = delim_slice.len();

    if s_len == 0 {
        return pith_list_new(8, 1);
    }

    // string-tagged: the pushes retain and the free path cascades, so the
    // list is the sole owner of its parts
    let list = pith_list_new(8, 1);
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
                let part_ptr = crate::pith_copy_bytes_to_cstring(&s_slice[start..i]);
                crate::collections::list::pith_list_push_value(list, part_ptr as i64);
                crate::pith_cstring_release(part_ptr as *const i8);
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

    crate::pith_copy_bytes_to_cstring(&slice[start..end])
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

    let ptr = crate::pith_alloc_cstring(1);
    *ptr = bytes[index as usize] as i8;
    ptr
}

/// Strict indexed character access for `s[i]` on a String. Aborts with a
/// structured diagnostic on out-of-bounds instead of returning silent "".
/// Callers who want the checked shape have `.get(i)` (returns String?).
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_char_at_strict(s: *const i8, index: i64) -> *mut i8 {
    let Some(bytes) = cstr_bytes(s) else {
        eprintln!("pith runtime error: string indexing on invalid string handle");
        std::process::exit(1);
    };
    let len = bytes.len() as i64;
    if index < 0 || index >= len {
        eprintln!(
            "pith runtime error: string index out of bounds: {} for string of length {} (use .get(i) for Optional access)",
            index, len
        );
        std::process::exit(1);
    }
    let ptr = crate::pith_alloc_cstring(1);
    *ptr = bytes[index as usize] as i8;
    ptr
}

/// Get a single character at index wrapped in an Optional tuple. Returns
/// `Some(char_string)` when `0 <= index < len`, and `None` otherwise —
/// callers of `s.get(i)` can distinguish an out-of-bounds miss from an
/// intentional empty result.
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_char_at_opt(s: *const i8, index: i64) -> i64 {
    let Some(bytes) = cstr_bytes(s) else {
        return optional_tuple(false, 0);
    };
    if index < 0 || index as usize >= bytes.len() {
        return optional_tuple(false, 0);
    }
    let ptr = crate::pith_alloc_cstring(1);
    *ptr = bytes[index as usize] as i8;
    optional_tuple(true, ptr as i64)
}

/// Format a float with a fixed number of decimal places.
///
/// # Safety
/// Pure math in, fresh allocation out.
#[no_mangle]
pub unsafe extern "C" fn pith_float_format(value: f64, precision: i64) -> i64 {
    let prec = precision.clamp(0, 17) as usize;
    let text = format!("{:.*}", prec, value);
    crate::runtime_core::pith_copy_bytes_to_cstring(text.as_bytes()) as i64
}

/// Pad a string to a width: align is '<', '>', or '^' as a byte, fill
/// is the pad byte (space or '0'). Strings already at or over the
/// width pass through as a fresh copy.
///
/// # Safety
/// handle must be a valid cstring pointer or 0.
#[no_mangle]
pub unsafe extern "C" fn pith_format_pad(handle: i64, width: i64, align: i64, fill: i64) -> i64 {
    let text = if handle == 0 {
        String::new()
    } else {
        std::ffi::CStr::from_ptr(handle as *const i8)
            .to_string_lossy()
            .into_owned()
    };
    let width = width.max(0) as usize;
    let fill_ch = (fill as u8) as char;
    let out = if text.chars().count() >= width {
        text
    } else {
        let pad = width - text.chars().count();
        match align as u8 {
            b'<' => format!("{}{}", text, fill_ch.to_string().repeat(pad)),
            b'^' => {
                let left = pad / 2;
                let right = pad - left;
                format!(
                    "{}{}{}",
                    fill_ch.to_string().repeat(left),
                    text,
                    fill_ch.to_string().repeat(right)
                )
            }
            _ => {
                // right alignment; zero-fill tucks behind a leading sign
                if fill_ch == '0' && (text.starts_with('-') || text.starts_with('+')) {
                    format!("{}{}{}", &text[..1], "0".repeat(pad), &text[1..])
                } else {
                    format!("{}{}", fill_ch.to_string().repeat(pad), text)
                }
            }
        }
    };
    crate::runtime_core::pith_copy_bytes_to_cstring(out.as_bytes()) as i64
}

/// Read one byte of a string as an integer: the allocation-free form of
/// s[i] for comparisons. Out of range returns -1, which matches no byte,
/// mirroring char_at's empty-string-on-out-of-range semantics under ==.
///
/// # Safety

/// s must be a valid null-terminated C string or garbage (checked).
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_byte_at(s: *const i8, index: i64) -> i64 {
    let Some(bytes) = cstr_bytes(s) else {
        return -1;
    };
    if index < 0 || index as usize >= bytes.len() {
        return -1;
    }
    bytes[index as usize] as i64
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

    crate::pith_copy_bytes_to_cstring(&slice[start..])
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

    let ptr = crate::pith_alloc_cstring(len);

    for i in 0..len {
        let c = slice[i];
        *ptr.add(i) = (c as char).to_ascii_uppercase() as u8 as i8;
    }
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

    let ptr = crate::pith_alloc_cstring(len);

    for i in 0..len {
        let c = slice[i];
        *ptr.add(i) = (c as char).to_ascii_lowercase() as u8 as i8;
    }
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

    let ptr = crate::pith_alloc_cstring(len);

    for i in 0..len {
        *ptr.add(i) = slice[len - 1 - i] as i8;
    }
    ptr
}

/// Split string into a list of single-character strings (chars)
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_chars(s: *const i8) -> i64 {
    use crate::collections::list::{pith_list_new, pith_list_push_value};

    let list = pith_list_new(8, 1);
    if let Some(bytes) = cstr_bytes(s) {
        for &b in bytes {
            let ch_ptr = crate::pith_chr_cstr(b as i64);
            pith_list_push_value(list, ch_ptr as i64);
            crate::pith_cstring_release(ch_ptr as *const i8);
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

    let src_tag = list_ref_from_handle(list_ptr).map(|r| r.type_tag as i32).unwrap_or(0);
    let new_list = pith_list_new(8, src_tag);
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

    let src_tag = list_ref_from_handle(list_ptr).map(|r| r.type_tag as i32).unwrap_or(0);
    let new_list = pith_list_new(8, src_tag);
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

    let src_tag = list_ref_from_handle(list_ptr).map(|r| r.type_tag as i32).unwrap_or(0);
    let new_list = pith_list_new(8, src_tag);
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
        return crate::pith_copy_bytes_to_cstring(s_bytes);
    }

    let mut result: Vec<u8> = Vec::with_capacity(s_len);
    let mut i = 0;
    // `i + from_len <= s_len`, not `i <= s_len - from_len`: the saturating
    // form yields 0 when the needle is longer than the haystack, which let
    // the empty-haystack case slice past the end.
    while i + from_len <= s_len {
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
    crate::pith_copy_bytes_to_cstring(&result[..out_len])
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
}
