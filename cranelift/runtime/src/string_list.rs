use crate::collections::list::{with_list_mut_handle, with_list_ref_handle, ListImpl, PithList};
use crate::ffi_util::{with_cstr_bytes, with_cstr_bytes2, with_cstr_str2};
use crate::runtime_core::optional_tuple;
use std::mem::MaybeUninit;

/// Get command line arguments as a Pith list of C string pointers.
#[no_mangle]
pub unsafe extern "C" fn pith_args() -> PithList {
    use crate::collections::list::{pith_list_new, pith_list_push_value};
    use std::env;

    // SAFETY: Allocates a fresh string-tagged list.
    let list = unsafe { pith_list_new(8, 1) };

    for arg in env::args() {
        let arg_ptr = crate::pith_copy_bytes_to_cstring(arg.as_bytes());
        // SAFETY: `list` is freshly allocated and `arg_ptr` is a valid heap cstring.
        unsafe {
            pith_list_push_value(list, arg_ptr as i64);
            crate::pith_cstring_release(arg_ptr as *const i8);
        }
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

    let message = format!(
        "substring({}, {}) would split the character '{}' \
         at byte {} (it occupies bytes {}..{})\n  \
         a String is indexed in bytes, so a byte offset can land inside a \
         multi-byte character. use std.text for character-aware work:\n    \
         text.slice(s, a, b)         slice by character index\n    \
         text.truncate(s, n)         keep n characters\n    \
         text.truncate_bytes(s, n)   keep n bytes, ending on a boundary",
        start,
        end,
        character,
        index,
        lead,
        lead + width
    );
    // `fail` skips this frame, so nothing it owns may be left to drop.
    drop(character);
    crate::failure::fail(message)
}

#[inline(always)]
fn cstring_substring_impl(bytes: &[u8], start: i64, end: i64) -> *mut i8 {
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
    // SAFETY: `s` is null or a valid null-terminated C string for this call.
    unsafe { with_cstr_bytes(s, |bytes| cstring_substring_impl(bytes, start, end)) }
        .unwrap_or(std::ptr::null_mut())
}

fn split_impl(s_slice: &[u8], delim_slice: &[u8], keep_empty: bool) -> PithList {
    use crate::collections::list::{pith_list_new, pith_list_push_value_owned};

    let s_len = s_slice.len();
    let delim_len = delim_slice.len();

    if s_len == 0 && !keep_empty {
        // SAFETY: Allocates a fresh string-tagged list.
        return unsafe { pith_list_new(8, 1) };
    }

    // string-tagged: the pushes retain and the free path cascades, so the
    // list is the sole owner of its parts
    // SAFETY: Allocates a fresh string-tagged list.
    let list = unsafe { pith_list_new(8, 1) };
    let push_part = |part: &[u8]| {
        if !keep_empty && part.is_empty() {
            return;
        }
        // the fresh part's one count moves into the list
        let part_ptr = crate::pith_copy_bytes_to_cstring(part);
        // SAFETY: `list` is freshly allocated and `part_ptr` transfers its single count.
        unsafe { pith_list_push_value_owned(list, part_ptr as i64) };
    };
    // an empty delimiter never matches, so the whole string is one part.
    // each delimiter is found from the end of the previous one, so a
    // delimiter that overlaps itself ("aa" in "aaa") is consumed once and
    // the search resumes after it.
    let mut start = 0;
    if delim_len > 0 {
        while let Some(at) = crate::substring::find_from(s_slice, delim_slice, start) {
            push_part(&s_slice[start..at]);
            start = at + delim_len;
        }
    }
    push_part(&s_slice[start..]);

    list
}

/// Split a string by delimiter and return as a PithList of strings
unsafe fn split_to_list_inner(s: *const i8, delim: *const i8, keep_empty: bool) -> PithList {
    // SAFETY: Caller guarantees `s` and `delim` are null or valid C strings.
    unsafe { with_cstr_bytes2(s, delim, |sb, db| split_impl(sb, db, keep_empty)) }
        // SAFETY: Allocates a fresh empty string-tagged list on null input.
        .unwrap_or_else(|| unsafe { crate::collections::list::pith_list_new(8, 1) })
}

/// Split string into a list of strings by delimiter, dropping empty parts.
///
/// # Safety
/// Both s and delim must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn pith_string_split_to_list(s: *const i8, delim: *const i8) -> PithList {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { split_to_list_inner(s, delim, false) }
}

/// Split string into a list of strings by delimiter, preserving empty parts on
/// leading, trailing, and consecutive delimiters.
///
/// # Safety
/// Both s and delim must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn pith_string_split_exact_to_list(s: *const i8, delim: *const i8) -> PithList {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { split_to_list_inner(s, delim, true) }
}

fn trim_impl(slice: &[u8]) -> *mut i8 {
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

/// Trim ASCII whitespace from both ends of a C string.
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_trim(s: *const i8) -> *mut i8 {
    // SAFETY: `s` is null or a valid null-terminated C string for this call.
    unsafe { with_cstr_bytes(s, trim_impl) }.unwrap_or(std::ptr::null_mut())
}

#[inline(always)]
fn char_at_impl(bytes: &[u8], index: i64) -> *mut i8 {
    if index < 0 || index as usize >= bytes.len() {
        return crate::pith_cstring_empty();
    }
    crate::pith_copy_bytes_to_cstring(&[bytes[index as usize]])
}

/// Get a single character from a C string at index as a new C string.
/// Returns a newly allocated 1-character string (or empty string if out of bounds)
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_char_at(s: *const i8, index: i64) -> *mut i8 {
    // SAFETY: `s` is null or a valid null-terminated C string for this call.
    unsafe { with_cstr_bytes(s, |bytes| char_at_impl(bytes, index)) }
        .unwrap_or_else(crate::pith_cstring_empty)
}

#[inline(always)]
fn char_at_strict_impl(bytes: &[u8], index: i64) -> *mut i8 {
    let len = bytes.len() as i64;
    if index < 0 || index >= len {
        crate::failure::fail(format!(
            "string index out of bounds: {} for string of length {} (use .get(i) for Optional access)",
            index, len
        ));
    }
    crate::pith_copy_bytes_to_cstring(&[bytes[index as usize]])
}

/// Strict indexed character access for `s[i]` on a String. Aborts with a
/// structured diagnostic on out-of-bounds instead of returning silent "".
/// Callers who want the checked shape have `.get(i)` (returns String?).
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_char_at_strict(s: *const i8, index: i64) -> *mut i8 {
    // SAFETY: `s` is null or a valid null-terminated C string for this call.
    let Some(ptr) = (unsafe { with_cstr_bytes(s, |bytes| char_at_strict_impl(bytes, index)) }) else {
        eprintln!("pith runtime error: string indexing on invalid string handle");
        // panic-guard: strict string indexing on an invalid handle is a program bug with no value to return.
        std::process::exit(1);
    };
    ptr
}

#[inline(always)]
fn char_at_opt_impl(bytes: &[u8], index: i64) -> i64 {
    if index < 0 || index as usize >= bytes.len() {
        return optional_tuple(false, 0);
    }
    let ptr = crate::pith_copy_bytes_to_cstring(&[bytes[index as usize]]);
    optional_tuple(true, ptr as i64)
}

/// Get a single character at index wrapped in an Optional tuple. Returns
/// `Some(char_string)` when `0 <= index < len`, and `None` otherwise, so
/// callers of `s.get(i)` can distinguish an out-of-bounds miss from an
/// intentional empty result.
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_char_at_opt(s: *const i8, index: i64) -> i64 {
    // SAFETY: `s` is null or a valid null-terminated C string for this call.
    unsafe { with_cstr_bytes(s, |bytes| char_at_opt_impl(bytes, index)) }
        .unwrap_or_else(|| optional_tuple(false, 0))
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

fn format_pad_impl(bytes: &[u8], width: i64, align: i64, fill: i64) -> i64 {
    let text = String::from_utf8_lossy(bytes);
    let width = width.max(0) as usize;
    let fill_ch = (fill as u8) as char;
    let char_count = text.chars().count();
    if char_count >= width {
        return crate::runtime_core::pith_copy_bytes_to_cstring(text.as_bytes()) as i64;
    }
    let pad = width - char_count;
    let out = match align as u8 {
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
    };
    crate::runtime_core::pith_copy_bytes_to_cstring(out.as_bytes()) as i64
}

/// Pad a string to a width: align is '<', '>', or '^' as a byte, fill
/// is the pad byte (space or '0'). Strings already at or over the
/// width pass through as a fresh copy.
///
/// # Safety
/// handle must be a valid cstring pointer or 0.
#[no_mangle]
pub unsafe extern "C" fn pith_format_pad(handle: i64, width: i64, align: i64, fill: i64) -> i64 {
    // SAFETY: `handle` is 0 or a valid null-terminated C string for this call.
    unsafe {
        crate::ffi_util::with_cstr_bytes_or_empty(handle as *const i8, |bytes| {
            format_pad_impl(bytes, width, align, fill)
        })
    }
}

#[inline(always)]
fn byte_at_impl(bytes: &[u8], index: i64) -> i64 {
    if index < 0 || index as usize >= bytes.len() {
        return -1;
    }
    bytes[index as usize] as i64
}

/// Read one byte of a string as an integer: the allocation-free form of
/// s[i] for comparisons. Out of range returns -1, which matches no byte,
/// mirroring char_at's empty-string-on-out-of-range semantics under ==.
///
/// # Safety
/// s must be a valid null-terminated C string or garbage (checked).
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_byte_at(s: *const i8, index: i64) -> i64 {
    // SAFETY: `s` is null or a valid null-terminated C string for this call.
    unsafe { with_cstr_bytes(s, |bytes| byte_at_impl(bytes, index)) }.unwrap_or(-1)
}

fn trim_left_impl(slice: &[u8]) -> *mut i8 {
    let mut start = 0usize;
    while start < slice.len() && matches!(slice[start], b' ' | b'\t' | b'\n' | b'\r') {
        start += 1;
    }

    crate::pith_copy_bytes_to_cstring(&slice[start..])
}

/// Trim ASCII whitespace from the left side of a C string.
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_trim_left(s: *const i8) -> *mut i8 {
    // SAFETY: `s` is null or a valid null-terminated C string for this call.
    unsafe { with_cstr_bytes(s, trim_left_impl) }.unwrap_or(std::ptr::null_mut())
}

fn to_upper_impl(slice: &[u8]) -> *mut i8 {
    let len = slice.len();
    // SAFETY: `pith_alloc_cstring(len)` returns a fresh writable buffer of `len` bytes plus NUL.
    let ptr = unsafe { crate::pith_alloc_cstring(len) };
    // SAFETY: `ptr` points to `len` writable (uninitialized) bytes.
    let out = unsafe { std::slice::from_raw_parts_mut(ptr as *mut MaybeUninit<u8>, len) };
    for (dst, &b) in out.iter_mut().zip(slice) {
        dst.write(b.to_ascii_uppercase());
    }
    ptr
}

/// Convert C string to uppercase
/// Returns newly allocated C string
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_to_upper(s: *const i8) -> *mut i8 {
    // SAFETY: `s` is null or a valid null-terminated C string for this call.
    unsafe { with_cstr_bytes(s, to_upper_impl) }.unwrap_or(std::ptr::null_mut())
}

fn to_lower_impl(slice: &[u8]) -> *mut i8 {
    let len = slice.len();
    // SAFETY: `pith_alloc_cstring(len)` returns a fresh writable buffer of `len` bytes plus NUL.
    let ptr = unsafe { crate::pith_alloc_cstring(len) };
    // SAFETY: `ptr` points to `len` writable (uninitialized) bytes.
    let out = unsafe { std::slice::from_raw_parts_mut(ptr as *mut MaybeUninit<u8>, len) };
    for (dst, &b) in out.iter_mut().zip(slice) {
        dst.write(b.to_ascii_lowercase());
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
    // SAFETY: `s` is null or a valid null-terminated C string for this call.
    unsafe { with_cstr_bytes(s, to_lower_impl) }.unwrap_or(std::ptr::null_mut())
}

fn reverse_impl(slice: &[u8]) -> *mut i8 {
    let len = slice.len();
    // SAFETY: `pith_alloc_cstring(len)` returns a fresh writable buffer of `len` bytes plus NUL.
    let ptr = unsafe { crate::pith_alloc_cstring(len) };
    // SAFETY: `ptr` points to `len` writable (uninitialized) bytes.
    let out = unsafe { std::slice::from_raw_parts_mut(ptr as *mut MaybeUninit<u8>, len) };
    for (dst, &b) in out.iter_mut().zip(slice.iter().rev()) {
        dst.write(b);
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
    // SAFETY: `s` is null or a valid null-terminated C string for this call.
    unsafe { with_cstr_bytes(s, reverse_impl) }.unwrap_or(std::ptr::null_mut())
}

fn chars_impl(list: PithList, bytes: &[u8]) {
    use crate::collections::list::pith_list_push_value;

    for &b in bytes {
        // SAFETY: `b` is a valid byte; `list` is a freshly allocated string-tagged list.
        unsafe {
            let ch_ptr = crate::pith_chr_cstr(b as i64);
            pith_list_push_value(list, ch_ptr as i64);
            crate::pith_cstring_release(ch_ptr as *const i8);
        }
    }
}

/// Split string into a list of single-character strings (chars)
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_chars(s: *const i8) -> i64 {
    // SAFETY: Allocates a fresh string-tagged list.
    let list = unsafe { crate::collections::list::pith_list_new(8, 1) };
    // SAFETY: `s` is null or a valid null-terminated C string for this call.
    unsafe { with_cstr_bytes(s, |bytes| chars_impl(list, bytes)) };
    list.ptr as i64
}

/// Compare two nullable C-string pointers lexicographically by raw bytes.
///
/// # Safety
/// Both `a` and `b` must be 0 or valid pointers to null-terminated C strings.
#[inline(always)]
unsafe fn compare_cstr_handles(a: i64, b: i64) -> std::cmp::Ordering {
    let ap = a as *const i8;
    let bp = b as *const i8;
    if ap.is_null() {
        return if bp.is_null() {
            std::cmp::Ordering::Equal
        } else {
            std::cmp::Ordering::Less
        };
    }
    if bp.is_null() {
        return std::cmp::Ordering::Greater;
    }
    // SAFETY: Caller guarantees both non-null pointers are valid C strings.
    unsafe { with_cstr_bytes2(ap, bp, |ab, bb| ab.cmp(bb)) }.unwrap_or(std::cmp::Ordering::Equal)
}

unsafe fn sort_strings_impl(impl_ref: &mut ListImpl) {
    if impl_ref.elem_size != 8 {
        return;
    }
    impl_ref.values8.sort_by(|&a, &b| {
        // SAFETY: Caller guarantees every 8-byte element is null or a valid C string.
        unsafe { compare_cstr_handles(a, b) }
    });
    impl_ref.sync_value_view();
}

/// Sort a list of C-string pointers in-place (lexicographic order)
///
/// # Safety
/// list_ptr is i64 carrying the PithList's internal ptr value;
/// each 8-byte element is a *const i8 pointer to a null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn pith_list_sort_strings(list_ptr: i64) {
    // SAFETY: `list_ptr` is validated by `with_list_mut_handle` and caller guarantees
    // every element is null or a valid C string.
    unsafe { with_list_mut_handle(list_ptr, |list| sort_strings_impl(list)) };
}

fn sort_i64_impl(impl_ref: &mut ListImpl) {
    if impl_ref.elem_size != 8 {
        return;
    }
    impl_ref.values8.sort();
    impl_ref.sync_value_view();
}

/// Sort a list of i64 values in-place
///
/// # Safety
/// list_ptr is i64 carrying the PithList's internal ptr value
#[no_mangle]
pub unsafe extern "C" fn pith_list_sort(list_ptr: i64) {
    // SAFETY: `list_ptr` is validated by `with_list_mut_handle` before mutating.
    unsafe { with_list_mut_handle(list_ptr, sort_i64_impl) };
}

fn list_slice_impl(impl_ref: &ListImpl, start: i64, end: i64) -> PithList {
    use crate::collections::list::{pith_list_new, pith_list_push_value};

    // SAFETY: Allocates a fresh list with the source list's element tag.
    let new_list = unsafe { pith_list_new(8, impl_ref.type_tag as i32) };
    let len = impl_ref.len() as i64;
    let s = start.max(0).min(len) as usize;
    let e = end.max(0).min(len) as usize;
    for i in s..e {
        if let Some(val) = impl_ref.get_value(i) {
            // SAFETY: `new_list` is freshly allocated and `val` matches its element tag.
            unsafe { pith_list_push_value(new_list, val) };
        }
    }
    new_list
}

fn clone_list_elements(impl_ref: &ListImpl) -> PithList {
    use crate::collections::list::{pith_list_new, pith_list_push_value};

    // SAFETY: Allocates a fresh list with the source list's element tag.
    let new_list = unsafe { pith_list_new(8, impl_ref.type_tag as i32) };
    for i in 0..impl_ref.len() {
        if let Some(val) = impl_ref.get_value(i) {
            // SAFETY: `new_list` is freshly allocated and `val` matches its element tag.
            unsafe { pith_list_push_value(new_list, val) };
        }
    }
    new_list
}

/// Get a sub-slice of a list
///
/// # Safety
/// list_ptr is i64 carrying the PithList's internal ptr value
#[no_mangle]
pub unsafe extern "C" fn pith_list_slice(list_ptr: i64, start: i64, end: i64) -> i64 {
    // SAFETY: `list_ptr` is validated by `with_list_ref_handle` before reading.
    let new_list =
        unsafe { with_list_ref_handle(list_ptr, |impl_ref| list_slice_impl(impl_ref, start, end)) }
            // SAFETY: Allocates an empty primitive list when `list_ptr` is invalid.
            .unwrap_or_else(|| unsafe { crate::collections::list::pith_list_new(8, 0) });
    new_list.ptr as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_list_sort_copy(list_ptr: i64) -> i64 {
    // SAFETY: `list_ptr` is validated by `with_list_ref_handle` before reading.
    let new_list = unsafe { with_list_ref_handle(list_ptr, clone_list_elements) }
        // SAFETY: Allocates an empty primitive list when `list_ptr` is invalid.
        .unwrap_or_else(|| unsafe { crate::collections::list::pith_list_new(8, 0) });
    // SAFETY: `new_list` is a freshly allocated valid list.
    unsafe { pith_list_sort(new_list.ptr as i64) };
    new_list.ptr as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_list_sort_strings_copy(list_ptr: i64) -> i64 {
    // SAFETY: `list_ptr` is validated by `with_list_ref_handle` before reading.
    let new_list = unsafe { with_list_ref_handle(list_ptr, clone_list_elements) }
        // SAFETY: Allocates an empty primitive list when `list_ptr` is invalid.
        .unwrap_or_else(|| unsafe { crate::collections::list::pith_list_new(8, 0) });
    // SAFETY: `new_list` is a freshly allocated valid string list.
    unsafe { pith_list_sort_strings(new_list.ptr as i64) };
    new_list.ptr as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_list_slice_copy(list_ptr: i64, start: i64, end: i64) -> i64 {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { pith_list_slice(list_ptr, start, end) }
}

fn replace_impl(s_bytes: &[u8], from_bytes: &[u8], to_bytes: &[u8]) -> *mut i8 {
    let s_len = s_bytes.len();
    let from_len = from_bytes.len();

    if from_len == 0 {
        return crate::pith_copy_bytes_to_cstring(s_bytes);
    }

    let mut result: Vec<u8> = Vec::with_capacity(s_len);
    let mut copied = 0;
    // each match is found from the end of the previous one, so matches never
    // overlap and the bytes between them are copied in one piece.
    while let Some(at) = crate::substring::find_from(s_bytes, from_bytes, copied) {
        result.extend_from_slice(&s_bytes[copied..at]);
        result.extend_from_slice(to_bytes);
        copied = at + from_len;
    }
    result.extend_from_slice(&s_bytes[copied..]);

    crate::pith_copy_bytes_to_cstring(&result)
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
    // SAFETY: All non-null pointers are valid null-terminated C strings for this call.
    unsafe {
        with_cstr_bytes(s, |s_bytes| {
            crate::ffi_util::with_cstr_bytes_or_empty(from, |from_bytes| {
                crate::ffi_util::with_cstr_bytes_or_empty(to, |to_bytes| replace_impl(s_bytes, from_bytes, to_bytes))
            })
        })
    }
    .unwrap_or(std::ptr::null_mut())
}

/// Check if a C string is empty (null or zero-length)
///
/// # Safety
/// s must be null or a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_is_empty(s: *const i8) -> i64 {
    // SAFETY: `s` is null or a valid null-terminated C string for this call.
    let is_empty = unsafe { with_cstr_bytes(s, <[u8]>::is_empty) }.unwrap_or(true);
    i64::from(is_empty)
}

#[inline(always)]
fn last_index_of_impl(h: &str, n: &str) -> i64 {
    match h.rfind(n) {
        Some(idx) => idx as i64,
        None => -1,
    }
}

/// Find last index of needle in haystack, returns -1 if not found
///
/// # Safety
/// Both arguments must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_last_index_of(haystack: *const i8, needle: *const i8) -> i64 {
    // SAFETY: Both arguments are null or valid null-terminated C strings.
    unsafe { with_cstr_str2(haystack, needle, last_index_of_impl) }.unwrap_or(-1)
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
            assert_eq!(with_cstr_bytes(ch, <[u8]>::to_vec), Some(Vec::new()));
            assert_eq!(pith_cstring_is_empty(std::ptr::null()), 1);
            assert_eq!(pith_cstring_is_empty(ptr), 0);
            assert_eq!(
                pith_cstring_last_index_of(ptr, b"x\0".as_ptr() as *const i8),
                -1
            );
        }
    }

    fn replace(text: &str, from: &str, to: &str) -> String {
        let t = std::ffi::CString::new(text).unwrap();
        let f = std::ffi::CString::new(from).unwrap();
        let o = std::ffi::CString::new(to).unwrap();
        unsafe {
            let out = pith_cstring_replace(t.as_ptr(), f.as_ptr(), o.as_ptr());
            let text = String::from_utf8(with_cstr_bytes(out, <[u8]>::to_vec).unwrap()).unwrap();
            crate::pith_cstring_release(out);
            text
        }
    }

    fn split(text: &str, delim: &str) -> Vec<String> {
        use crate::collections::list::{pith_list_get_value, pith_list_len, pith_list_release};
        let t = std::ffi::CString::new(text).unwrap();
        let d = std::ffi::CString::new(delim).unwrap();
        unsafe {
            let list = pith_string_split_to_list(t.as_ptr(), d.as_ptr());
            let mut parts = Vec::new();
            for i in 0..pith_list_len(list) {
                let part = pith_list_get_value(list, i) as *const i8;
                parts.push(
                    String::from_utf8(with_cstr_bytes(part, <[u8]>::to_vec).unwrap()).unwrap(),
                );
            }
            pith_list_release(list);
            parts
        }
    }

    #[test]
    fn replace_edge_cases() {
        // empty needle: the text is copied unchanged
        assert_eq!(replace("abc", "", "x"), "abc");
        assert_eq!(replace("", "", "x"), "");
        // needle longer than the text
        assert_eq!(replace("ab", "abc", "x"), "ab");
        assert_eq!(replace("", "a", "x"), "");
        // needle at either end, growing and shrinking
        assert_eq!(replace("abcXYabc", "abc", "-"), "-XY-");
        assert_eq!(replace("abc", "abc", ""), "");
        assert_eq!(replace("abc", "c", "cccc"), "abcccc");
        // repeated first bytes: matches are consumed left to right and
        // never overlap
        assert_eq!(replace("aaaaaaab", "ab", "!"), "aaaaaa!");
        assert_eq!(replace("aaaa", "a", "bb"), "bbbbbbbb");
        assert_eq!(replace("aaaa", "aa", "b"), "bb");
        assert_eq!(replace("aaaaa", "aa", "b"), "bba");
        assert_eq!(replace("abab", "aba", "X"), "Xb");
        // a needle sharing a prefix with an earlier non-match
        assert_eq!(replace("abcabdabcabd", "abd", "_"), "abc_abc_");
        assert_eq!(replace("ababac", "abac", "Y"), "abY");
        // bytes above 0x7f
        assert_eq!(replace("héllo wörld", "ö", "oe"), "héllo woerld");
    }

    #[test]
    fn split_edge_cases() {
        let none: Vec<String> = Vec::new();
        // an empty delimiter never matches, so the text is one part; an
        // empty text has no parts at all
        assert_eq!(split("abc", ""), vec!["abc"]);
        assert_eq!(split("", ""), none);
        assert_eq!(split("", ","), none);
        // delimiter longer than the text
        assert_eq!(split("ab", "abc"), vec!["ab"]);
        // delimiters at either end and back to back add no empty parts
        assert_eq!(split(",a,b,", ","), vec!["a", "b"]);
        assert_eq!(split("a,,b,,,c", ","), vec!["a", "b", "c"]);
        assert_eq!(split("abcXYabc", "abc"), vec!["XY"]);
        assert_eq!(split("abc", "abc"), none);
        // repeated first bytes
        assert_eq!(split("aaXaaXaa", "aa"), vec!["X", "X"]);
        assert_eq!(split("aaaaaaab", "ab"), vec!["aaaaaa"]);
        // a self-overlapping delimiter is consumed once per match and the
        // search resumes after it. the old loop kept scanning inside the
        // match it had just consumed and crashed on the second one.
        assert_eq!(split("aaa", "aa"), vec!["a"]);
        assert_eq!(split("aaaa", "aa"), none);
        assert_eq!(split("aaaaa", "aa"), vec!["a"]);
        // a delimiter sharing a prefix with an earlier non-match
        assert_eq!(split("abcabdabcabd", "abd"), vec!["abc", "abc"]);
        // bytes above 0x7f
        assert_eq!(split("日本語のテキスト", "の"), vec!["日本語", "テキスト"]);
        // the path shape
        assert_eq!(
            split("data//north/./users/../users/17.json", "/"),
            vec!["data", "north", ".", "users", "..", "users", "17.json"]
        );
    }

    fn split_exact(text: &str, delim: &str) -> Vec<String> {
        use crate::collections::list::{pith_list_get_value, pith_list_len, pith_list_release};
        let t = std::ffi::CString::new(text).unwrap();
        let d = std::ffi::CString::new(delim).unwrap();
        unsafe {
            let list = pith_string_split_exact_to_list(t.as_ptr(), d.as_ptr());
            let mut parts = Vec::new();
            for i in 0..pith_list_len(list) {
                let part = pith_list_get_value(list, i) as *const i8;
                parts.push(
                    String::from_utf8(with_cstr_bytes(part, <[u8]>::to_vec).unwrap()).unwrap(),
                );
            }
            pith_list_release(list);
            parts
        }
    }

    #[test]
    fn split_exact_edge_cases() {
        assert_eq!(split_exact("abc", ""), vec!["abc"]);
        assert_eq!(split_exact("", ""), vec![""]);
        assert_eq!(split_exact("", ","), vec![""]);
        assert_eq!(split_exact(",", ","), vec!["", ""]);
        assert_eq!(split_exact(",a,b,", ","), vec!["", "a", "b", ""]);
        assert_eq!(split_exact("a,,b", ","), vec!["a", "", "b"]);
        assert_eq!(split_exact("a\n\nb", "\n"), vec!["a", "", "b"]);
        assert_eq!(split_exact("a----b", "--"), vec!["a", "", "b"]);
        assert_eq!(split_exact("aaa", "aa"), vec!["", "a"]);
    }
}
