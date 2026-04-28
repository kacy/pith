use crate::bytes::pith_bytes_ref;

const TYPE_STRING: i64 = 0;
const TYPE_INT: i64 = 1;
const TYPE_BOOL: i64 = 2;

unsafe fn cstr_bytes<'a>(ptr: i64) -> &'a [u8] {
    if ptr == 0 {
        return &[];
    }
    let len = crate::string::pith_cstring_len(ptr as *const i8) as usize;
    std::slice::from_raw_parts(ptr as *const u8, len)
}

fn skip_ws(input: &[u8], mut pos: usize) -> usize {
    while pos < input.len() && matches!(input[pos], b' ' | b'\t' | b'\n' | b'\r') {
        pos += 1;
    }
    pos
}

fn read_string_end(input: &[u8], pos: usize) -> Option<usize> {
    if pos >= input.len() || input[pos] != b'"' {
        return None;
    }
    let mut i = pos + 1;
    let mut escaped = false;
    while i < input.len() {
        let b = input[i];
        if escaped {
            escaped = false;
            i += 1;
            continue;
        }
        if b == b'\\' {
            escaped = true;
            i += 1;
            continue;
        }
        if b == b'"' {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn read_int(input: &[u8], pos: usize) -> Option<(i64, usize)> {
    let mut i = pos;
    let mut neg = false;
    if i < input.len() && input[i] == b'-' {
        neg = true;
        i += 1;
    }
    let start = i;
    let mut value = 0i64;
    while i < input.len() && input[i].is_ascii_digit() {
        value = value
            .saturating_mul(10)
            .saturating_add((input[i] - b'0') as i64);
        i += 1;
    }
    if i == start {
        return None;
    }
    if neg {
        value = -value;
    }
    Some((value, i))
}

fn skip_scalar(input: &[u8], pos: usize) -> Option<usize> {
    if pos >= input.len() {
        return None;
    }
    if input[pos] == b'"' {
        return read_string_end(input, pos).map(|end| end + 1);
    }
    if input[pos] == b'-' || input[pos].is_ascii_digit() {
        return read_int(input, pos).map(|(_, end)| end);
    }
    if input[pos..].starts_with(b"true") {
        return Some(pos + 4);
    }
    if input[pos..].starts_with(b"false") {
        return Some(pos + 5);
    }
    if input[pos..].starts_with(b"null") {
        return Some(pos + 4);
    }
    None
}

unsafe fn alloc_result(is_ok: i64, ok: i64, err: i64) -> i64 {
    let tuple = crate::pith_struct_alloc(3) as *mut i64;
    if tuple.is_null() {
        return 0;
    }
    *tuple = is_ok;
    *tuple.add(1) = ok;
    *tuple.add(2) = err;
    tuple as i64
}

unsafe fn ok_result(value: i64) -> i64 {
    alloc_result(1, value, 0)
}

unsafe fn err_result(message: &[u8]) -> i64 {
    let error = crate::pith_struct_alloc(1) as *mut i64;
    if error.is_null() {
        return 0;
    }
    *error = crate::pith_copy_bytes_to_cstring(message) as i64;
    alloc_result(0, 0, error as i64)
}

fn match_field(key: &[u8], keys: &[&[u8]; 6]) -> Option<usize> {
    let mut i = 0;
    while i < keys.len() {
        if key == keys[i] {
            return Some(i);
        }
        i += 1;
    }
    None
}

unsafe fn parse_field_value(input: &[u8], pos: usize, field_type: i64) -> Option<(i64, usize)> {
    if field_type == TYPE_STRING {
        let end = read_string_end(input, pos)?;
        let value = crate::pith_copy_bytes_to_cstring(&input[pos + 1..end]) as i64;
        return Some((value, end + 1));
    }
    if field_type == TYPE_INT {
        return read_int(input, pos);
    }
    if field_type == TYPE_BOOL {
        if input[pos..].starts_with(b"true") {
            return Some((1, pos + 4));
        }
        if input[pos..].starts_with(b"false") {
            return Some((0, pos + 5));
        }
    }
    None
}

#[no_mangle]
pub unsafe extern "C" fn pith_json_decode_flat6(
    bytes_handle: i64,
    key0: i64,
    type0: i64,
    key1: i64,
    type1: i64,
    key2: i64,
    type2: i64,
    key3: i64,
    type3: i64,
    key4: i64,
    type4: i64,
    key5: i64,
    type5: i64,
) -> i64 {
    let Some(bytes) = pith_bytes_ref(bytes_handle) else {
        return err_result(b"invalid json object");
    };
    let input = bytes.data.as_slice();
    let keys = [
        cstr_bytes(key0),
        cstr_bytes(key1),
        cstr_bytes(key2),
        cstr_bytes(key3),
        cstr_bytes(key4),
        cstr_bytes(key5),
    ];
    let types = [type0, type1, type2, type3, type4, type5];
    let mut values = [0i64; 6];
    let mut seen = [false; 6];

    let mut pos = skip_ws(input, 0);
    if pos >= input.len() || input[pos] != b'{' {
        return err_result(b"invalid json object");
    }
    pos = skip_ws(input, pos + 1);
    if pos < input.len() && input[pos] == b'}' {
        return err_result(b"missing json field");
    }

    loop {
        let key_start = pos + 1;
        let Some(key_end) = read_string_end(input, pos) else {
            return err_result(b"invalid json object");
        };
        let key = &input[key_start..key_end];
        pos = skip_ws(input, key_end + 1);
        if pos >= input.len() || input[pos] != b':' {
            return err_result(b"invalid json object");
        }
        pos = skip_ws(input, pos + 1);
        if pos >= input.len() {
            return err_result(b"invalid json object");
        }

        if let Some(field_idx) = match_field(key, &keys) {
            let Some((value, next_pos)) = parse_field_value(input, pos, types[field_idx]) else {
                return err_result(b"wrong json field type");
            };
            values[field_idx] = value;
            seen[field_idx] = true;
            pos = next_pos;
        } else {
            let Some(next_pos) = skip_scalar(input, pos) else {
                return err_result(b"invalid json object");
            };
            pos = next_pos;
        }

        pos = skip_ws(input, pos);
        if pos < input.len() && input[pos] == b',' {
            pos = skip_ws(input, pos + 1);
            continue;
        }
        if pos < input.len() && input[pos] == b'}' {
            pos = skip_ws(input, pos + 1);
            if pos != input.len() {
                return err_result(b"invalid json object");
            }
            break;
        }
        return err_result(b"invalid json object");
    }

    let mut i = 0;
    while i < seen.len() {
        if !seen[i] {
            return err_result(b"missing json field");
        }
        i += 1;
    }

    let object = crate::pith_struct_alloc(6) as *mut i64;
    if object.is_null() {
        return 0;
    }
    i = 0;
    while i < values.len() {
        *object.add(i) = values[i];
        i += 1;
    }
    ok_result(object as i64)
}
