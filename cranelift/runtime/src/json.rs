use crate::bytes::pith_bytes_ref;

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
    // a backslash takes the byte after it with it, whatever it is; a
    // backslash as the last byte runs off the end and reads as unterminated.
    while i < input.len() {
        match input[i] {
            b'"' => return Some(i),
            b'\\' => i += 2,
            _ => i += 1,
        }
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

/// Destructor for the one-field error struct `err_result` builds: the
/// message string is the struct's to drop. The struct stands in for the
/// compiler's `JsonDecodeError`, whose own destructor does the same, but
/// this one is allocated here and the compiler never attaches one to it.
unsafe extern "C" fn decode_error_dtor(ptr: i64) {
    let message = *(ptr as *const i64);
    if message != 0 {
        crate::pith_cstring_release(message as *const i8);
    }
}

unsafe fn err_result(message: &[u8]) -> i64 {
    let error = crate::pith_struct_alloc(1) as *mut i64;
    if error.is_null() {
        return 0;
    }
    *error = crate::pith_copy_bytes_to_cstring(message) as i64;
    crate::pith_struct_set_dtor(error as i64, decode_error_dtor as *const () as usize as i64);
    alloc_result(0, 0, error as i64)
}

/// The packed field spec is comma-separated fields, each `<type_char><name>`
/// (i=int, s=string, b=bool) in declaration order, so a field's position is
/// its struct slot. The emitter interns one literal per struct type.
///
/// A key is matched against the field whose type char sits at `pos` without
/// splitting the spec first: the name bytes are compared in place and the
/// byte after them must be the separator or the end of the spec.
#[inline(always)]
fn spec_field_at(spec: &[u8], pos: usize, key: &[u8]) -> bool {
    // an empty field (two adjacent separators) has no type char and cannot
    // match. the emitter never writes one; the split-based lookup this
    // replaced skipped them, and this keeps that contract.
    if spec[pos] == b',' {
        return false;
    }
    let name = &spec[pos + 1..];
    if name.len() < key.len() {
        return false;
    }
    let Some(&first) = key.first() else {
        return name.is_empty() || name[0] == b',';
    };
    // the first byte inline: on a miss it is nearly always the byte that
    // differs, so the memcmp call is left to the fields that can match.
    name[0] == first
        && name[..key.len()] == *key
        && (name.len() == key.len() || name[key.len()] == b',')
}

/// Where the field after the one at `pos`, whose name is `name_len` bytes,
/// starts; `spec.len()` when that was the last field.
#[inline(always)]
fn spec_next_pos(spec: &[u8], pos: usize, name_len: usize) -> usize {
    let end = pos + 1 + name_len;
    if end < spec.len() {
        end + 1
    } else {
        spec.len()
    }
}

/// The field the decoder expects next: the one after the last match. An
/// encoder writes an object's keys in declaration order, so nearly every
/// key matches here on one compare and the spec is never walked.
struct SpecCursor {
    pos: usize,
    idx: usize,
}

/// Look up a key in the spec. Returns (slot, type_char) or None. The
/// cursor's field is tried first; otherwise the spec is walked from the
/// start, comparing each field's name in place. Either way the cursor
/// moves to the field after the hit.
#[inline(always)]
fn spec_lookup(spec: &[u8], key: &[u8], cur: &mut SpecCursor) -> Option<(usize, u8)> {
    if cur.pos < spec.len() && spec_field_at(spec, cur.pos, key) {
        let hit = (cur.idx, spec[cur.pos]);
        cur.pos = spec_next_pos(spec, cur.pos, key.len());
        cur.idx += 1;
        return Some(hit);
    }
    let mut pos = 0;
    let mut idx = 0;
    while pos < spec.len() {
        if spec_field_at(spec, pos, key) {
            cur.pos = spec_next_pos(spec, pos, key.len());
            cur.idx = idx + 1;
            return Some((idx, spec[pos]));
        }
        while pos < spec.len() && spec[pos] != b',' {
            pos += 1;
        }
        pos += 1;
        idx += 1;
    }
    None
}

/// The end quote of the key that opens at `pos`. Keys are the one string
/// this decoder reads per field, so this is kept as a plain index loop the
/// optimizer leaves alone: inlined into the field loop, `read_string_end`
/// came out as a loop carrying eight induction variables at over twenty
/// instructions per byte.
#[inline(never)]
fn read_key_end(input: &[u8], pos: usize) -> Option<usize> {
    read_string_end(input, pos)
}

/// Decode a flat object of scalar fields straight into a pre-allocated
/// struct in a single pass. The caller allocates the struct (with its
/// destructor attached) and passes its data pointer; this writes each
/// matched field into its slot — ints and bools inline, strings as fresh
/// counted cstrings the struct then owns. Returns a bitmask of the fields
/// it filled, or -1 on a malformed object. The caller checks the mask
/// against the required set and, on any miss, releases the struct.
///
/// Per call this reads the spec's length once and otherwise touches only
/// the spec bytes of the fields it compares against. The version this
/// replaced split the whole spec byte by byte for every key, which was
/// most of its cost (docs/performance.md, typed json decoding).
#[no_mangle]
pub unsafe extern "C" fn pith_json_fill_struct(
    bytes_handle: i64,
    spec_ptr: i64,
    struct_ptr: i64,
) -> i64 {
    let Some(bytes) = pith_bytes_ref(bytes_handle) else {
        return -1;
    };
    let input = bytes.data.as_slice();
    let spec = cstr_bytes(spec_ptr);
    let obj = struct_ptr as *mut i64;
    let mut mask: i64 = 0;
    let mut cur = SpecCursor { pos: 0, idx: 0 };

    let mut pos = skip_ws(input, 0);
    if pos >= input.len() || input[pos] != b'{' {
        return -1;
    }
    pos = skip_ws(input, pos + 1);
    if pos < input.len() && input[pos] == b'}' {
        return 0;
    }

    loop {
        let key_start = pos + 1;
        let Some(key_end) = read_key_end(input, pos) else {
            return -1;
        };
        let key = &input[key_start..key_end];
        pos = skip_ws(input, key_end + 1);
        if pos >= input.len() || input[pos] != b':' {
            return -1;
        }
        pos = skip_ws(input, pos + 1);
        if pos >= input.len() {
            return -1;
        }

        if let Some((idx, field_type)) = spec_lookup(spec, key, &mut cur) {
            let bit = 1i64 << idx;
            let next = match field_type {
                b'i' => {
                    let Some((value, n)) = read_int(input, pos) else {
                        return -1;
                    };
                    *obj.add(idx) = value;
                    n
                }
                b's' => {
                    let Some(end) = read_string_end(input, pos) else {
                        return -1;
                    };
                    // a repeated key overwrites the slot. the string the
                    // first occurrence minted belongs to this struct, so
                    // it is dropped here rather than stranded.
                    if mask & bit != 0 {
                        crate::pith_cstring_release(*obj.add(idx) as *const i8);
                    }
                    *obj.add(idx) = crate::pith_copy_bytes_to_cstring(&input[pos + 1..end]) as i64;
                    end + 1
                }
                b'b' => {
                    if input[pos..].starts_with(b"true") {
                        *obj.add(idx) = 1;
                        pos + 4
                    } else if input[pos..].starts_with(b"false") {
                        *obj.add(idx) = 0;
                        pos + 5
                    } else {
                        return -1;
                    }
                }
                _ => return -1,
            };
            mask |= bit;
            pos = next;
        } else {
            let Some(next) = skip_scalar(input, pos) else {
                return -1;
            };
            pos = next;
        }

        pos = skip_ws(input, pos);
        if pos < input.len() && input[pos] == b',' {
            pos = skip_ws(input, pos + 1);
            continue;
        }
        if pos < input.len() && input[pos] == b'}' {
            break;
        }
        return -1;
    }
    mask
}

/// The Err result a caller returns when the fill mask shows a required
/// field was missing. Names the first missing field (in spec order) so
/// the message matches the field-by-field decoder's errors.
#[no_mangle]
pub unsafe extern "C" fn pith_json_decode_missing_error(mask: i64, spec_ptr: i64) -> i64 {
    let spec = cstr_bytes(spec_ptr);
    let mut idx = 0;
    for field in spec.split(|&b| b == b',') {
        if !field.is_empty() && (mask & (1i64 << idx)) == 0 {
            let mut msg = b"missing json field: ".to_vec();
            msg.extend_from_slice(&field[1..]);
            return err_result(&msg);
        }
        idx += 1;
    }
    err_result(b"missing json field")
}
