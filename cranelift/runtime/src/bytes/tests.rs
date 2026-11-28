use super::buffer::*;
use super::ops::*;
use super::*;
use crate::ffi_util::unbox_result;

#[test]
fn raw_reads_and_writes_go_through_the_data_pointer() {
    unsafe {
        assert_eq!(pith_bytes_data_ptr(0), 0);
        assert_eq!(pith_bytes_data_ptr(12345), 0);
        let data = pith_bytes_from_vec(b"\"name\":\x80\xff".to_vec());
        let ptr = pith_bytes_data_ptr(data);
        assert_ne!(ptr, 0);
        assert_eq!(pith_load_u8(ptr, 0), b'"' as i64);
        assert_eq!(pith_load_u8(ptr, 8), 0xff);
        assert_eq!(pith_load_u16(ptr, 1), i64::from(u16::from_le_bytes(*b"na")));
        assert_eq!(pith_load_u32(ptr, 1), i64::from(u32::from_le_bytes(*b"name")));
        assert_eq!(pith_load_u64(ptr, 1), u64::from_le_bytes(*b"name\":\x80\xff") as i64);
        let s = pith_cstring_from_raw(ptr + 1, 4);
        assert_eq!(with_cstr_bytes(s as *const i8, <[u8]>::to_vec), Some(b"name".to_vec()));
        crate::pith_cstring_release(s as *const i8);
        let invalid = pith_cstring_from_raw(ptr + 7, 2);
        assert_eq!(
            with_cstr_bytes(invalid as *const i8, <[u8]>::to_vec),
            Some(b"\x80\xff".to_vec())
        );
        crate::pith_cstring_release(invalid as *const i8);
        let empty = pith_cstring_from_raw(ptr, 0);
        assert_eq!(with_cstr_bytes(empty as *const i8, <[u8]>::to_vec), Some(Vec::new()));
        crate::pith_cstring_release(empty as *const i8);
        let mut slots = [0i64; 3];
        pith_store_u64(slots.as_mut_ptr() as i64, 8, -5);
        assert_eq!(slots, [0, -5, 0]);
        pith_bytes_release(data);
    }
}

#[test]
fn write_word_appends_only_the_low_count_bytes() {
    unsafe {
        let buf = pith_byte_buffer_new();
        let word = i64::from_le_bytes(*b"abcdefgh");
        assert_eq!(pith_byte_buffer_write_word(buf, word, 3), 1);
        assert_eq!(pith_byte_buffer_write_word(buf, word, 8), 1);
        assert_eq!(pith_byte_buffer_write_word(buf, word, 1), 1);
        assert_eq!(pith_byte_buffer_write_word(buf, word, 0), 0);
        assert_eq!(pith_byte_buffer_write_word(buf, word, 9), 0);
        assert_eq!(pith_byte_buffer_write_word(0, word, 8), 0);
        let data = pith_byte_buffer_take_bytes(buf);
        assert_eq!(with_bytes(data, <[u8]>::to_vec).unwrap(), b"abcabcdefgha");
        pith_bytes_release(data);
        // taking the contents leaves an empty buffer that still works
        assert_eq!(pith_byte_buffer_len(buf), 0);
        assert_eq!(pith_byte_buffer_write_word(buf, word, 2), 1);
        assert_eq!(pith_byte_buffer_len(buf), 2);
        pith_byte_buffer_free(buf);
    }
}

#[test]
fn constant_time_eq_fails_closed_on_invalid_handles() {
    unsafe {
        // this gates every mac/tag/password compare: an invalid handle
        // must NOT read as equal. two invalid handles used to both degrade
        // to empty slices and compare equal.
        assert_eq!(pith_crypto_constant_time_eq(12345, 67890), 0);
        let real = pith_bytes_from_vec(b"secret".to_vec());
        assert_eq!(pith_crypto_constant_time_eq(0, real), 0);
        assert_eq!(pith_crypto_constant_time_eq(real, 0), 0);
        // valid equal and unequal comparisons still work
        let same = pith_bytes_from_vec(b"secret".to_vec());
        let other = pith_bytes_from_vec(b"secreu".to_vec());
        assert_eq!(pith_crypto_constant_time_eq(real, same), 1);
        assert_eq!(pith_crypto_constant_time_eq(real, other), 0);
        // two genuinely-empty byte values are still equal
        let e1 = pith_bytes_from_vec(Vec::new());
        let e2 = pith_bytes_from_vec(Vec::new());
        assert_eq!(pith_crypto_constant_time_eq(e1, e2), 1);
        // lengths differing by an exact multiple of 256 must NOT read as
        // equal even when the longer input's extra bytes are all zero: the
        // length term used to be truncated to u8, so 0x100 vanished.
        let short = pith_bytes_from_vec(vec![0u8; 16]);
        let long = pith_bytes_from_vec(vec![0u8; 272]);
        assert_eq!(pith_crypto_constant_time_eq(short, long), 0);
    }
}

fn rc_of(handle: i64) -> u32 {
    unsafe {
        with_bytes_obj(handle, |b| b.rc.load(std::sync::atomic::Ordering::Relaxed)).unwrap_or(0)
    }
}

#[test]
fn a_concat_with_an_empty_side_shares_the_other_value() {
    unsafe {
        let data = pith_bytes_from_vec(b"abc".to_vec());
        let empty = pith_bytes_from_vec(Vec::new());
        let left = pith_bytes_concat(empty, data);
        let right = pith_bytes_concat(data, empty);
        assert_eq!((left, right), (data, data));
        assert_eq!(rc_of(data), 3);
        pith_bytes_release(left);
        pith_bytes_release(right);
        assert_eq!(rc_of(data), 1);
        let both = pith_bytes_concat(data, data);
        assert_ne!(both, data);
        assert_eq!(with_bytes(both, <[u8]>::to_vec), Some(b"abcabc".to_vec()));
        pith_bytes_release(both);
        pith_bytes_release(data);
        pith_bytes_release(empty);
    }
}

#[test]
fn a_slice_of_the_whole_value_shares_it_and_a_part_copies() {
    unsafe {
        let data = pith_bytes_from_vec(b"abcdef".to_vec());
        let whole = pith_bytes_slice(data, 0, 6);
        assert_eq!(whole, data);
        assert_eq!(rc_of(data), 2);
        pith_bytes_release(whole);
        let clamped = pith_bytes_slice(data, -3, 99);
        assert_eq!(clamped, data);
        pith_bytes_release(clamped);
        let part = pith_bytes_slice(data, 1, 3);
        assert_ne!(part, data);
        assert_eq!(with_bytes(part, <[u8]>::to_vec), Some(b"bc".to_vec()));
        pith_bytes_release(part);
        assert_eq!(rc_of(data), 1);
        pith_bytes_release(data);
    }
}

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
// ends, so slice(1, -5) on b"abc" came back as b"a", and a caller walking a
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
        assert_eq!(crate::string::pith_cstring_len(pith_byte_buffer_string(12345)), 0);
    }
}

/// an empty append succeeds with a count of 0, where it used to share
/// that return value with an invalid handle (#1153).
#[test]
fn an_empty_append_succeeds_and_an_invalid_handle_is_an_error() {
    unsafe {
        let buffer = pith_byte_buffer_new();
        let empty = pith_bytes_from_vec(Vec::new());
        assert_eq!(unbox_result(pith_byte_buffer_write(buffer, empty)), Ok(0));
        assert_eq!(
            unbox_result(pith_byte_buffer_write_string_utf8(buffer, c"".as_ptr())),
            Ok(0)
        );
        assert_eq!(
            unbox_result(pith_byte_buffer_write_string_utf8(buffer, c"ab".as_ptr())),
            Ok(2)
        );
        assert_eq!(
            unbox_result(pith_byte_buffer_write(12345, empty)),
            Err("byte_buffer_write failed: invalid buffer handle".to_string())
        );
        assert_eq!(
            unbox_result(pith_byte_buffer_write(buffer, 12345)),
            Err("byte_buffer_write failed: invalid bytes value".to_string())
        );
        assert_eq!(
            unbox_result(pith_byte_buffer_write_string_utf8(0, c"".as_ptr())),
            Err("byte_buffer_write_string_utf8 failed: invalid buffer handle".to_string())
        );
        assert_eq!(
            unbox_result(pith_byte_buffer_write_string_utf8(buffer, std::ptr::null())),
            Err("byte_buffer_write_string_utf8 failed: invalid string".to_string())
        );
        assert_eq!(pith_byte_buffer_len(buffer), 2);
        pith_bytes_release(empty);
        pith_byte_buffer_free(buffer);
    }
}

/// a successful append hands its count back in two registers: once the
/// buffer has the room, a write allocates nothing, where each one used to
/// allocate a result box (#1162). more writes than the struct pool holds
/// blocks for, so a box would have to come from the allocator.
#[test]
fn a_successful_append_allocates_nothing() {
    unsafe {
        let buffer = pith_byte_buffer_with_capacity(1 << 16);
        let pair = pith_bytes_from_vec(b"cd".to_vec());
        let count = crate::collections::alloc_probe::allocations_during(|| {
            for _ in 0..2000 {
                let wrote = pith_byte_buffer_write_string_utf8(buffer, c"ab".as_ptr());
                assert_eq!((wrote.is_ok, wrote.payload), (1, 2));
                let wrote = pith_byte_buffer_write(buffer, pair);
                assert_eq!((wrote.is_ok, wrote.payload), (1, 2));
            }
        });
        assert_eq!(count, 0);
        assert_eq!(pith_byte_buffer_len(buffer), 8000);
        pith_bytes_release(pair);
        pith_byte_buffer_free(buffer);
    }
}

#[test]
fn byte_buffer_string_copies_every_byte_verbatim() {
    unsafe {
        let buffer = pith_byte_buffer_new();
        // not utf-8, and a NUL in the middle: the string must carry both
        let raw = pith_bytes_from_vec(vec![0xC8, 0xFF, 0x00, b'z']);
        assert_eq!(unbox_result(pith_byte_buffer_write(buffer, raw)), Ok(4));
        let text = pith_byte_buffer_string(buffer);
        assert_eq!(crate::string::pith_cstring_len(text), 4);
        let copied = std::slice::from_raw_parts(text as *const u8, 4);
        assert_eq!(copied, &[0xC8, 0xFF, 0x00, b'z']);
        // the buffer is untouched and a later write does not reach the copy
        assert_eq!(pith_byte_buffer_len(buffer), 4);
        assert_eq!(unbox_result(pith_byte_buffer_write(buffer, raw)), Ok(4));
        assert_eq!(crate::string::pith_cstring_len(text), 4);
        crate::pith_cstring_release(text);
        pith_bytes_release(raw);
        pith_byte_buffer_free(buffer);
        // an empty buffer reads back as the empty string
        let empty = pith_byte_buffer_new();
        assert_eq!(crate::string::pith_cstring_len(pith_byte_buffer_string(empty)), 0);
        pith_byte_buffer_free(empty);
    }
}

#[test]
fn secure_random_rejects_negative_and_oversized_counts() {
    assert_eq!(pith_secure_random_bytes(-1), 0);
    assert_eq!(pith_secure_random_bytes(i64::MAX), 0);
    assert_eq!(pith_secure_random_bytes(MAX_SECURE_RANDOM_BYTES + 1), 0);
    let ok = pith_secure_random_bytes(32);
    assert_ne!(ok, 0);
    unsafe {
        assert_eq!(pith_bytes_len(ok), 32);
        pith_bytes_release(ok);
    }
}
