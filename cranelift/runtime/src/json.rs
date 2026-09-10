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

// ---------------------------------------------------------------------------
// the nested-shape filler
//
// a struct whose fields are required scalars or structs of the same shape,
// at any depth, decodes here in one pass over the bytes. the caller (the
// decode lowering, self-host/ir_decode_emit.pith) allocates the whole tree
// of structs ahead of the call, each with its destructor attached and each
// sub-struct already stored in its parent's slot, so this only ever writes
// scalars into slots and recurses into a struct it can read out of one.
// nothing here knows a destructor's address, and every string minted is
// owned by the struct whose slot holds it from the moment it is written.
//
// before this filler a nested shape parsed into std.json's node pool and
// copied its fields out through the object accessors, so its values, its
// error messages and the grammar it accepted were the node parser's. this
// mirrors that parser rather than the flat filler above: string escapes are
// decoded, a colon or a comma may be missing, an object or array may run
// off the end of the input, a number is an int only when it has no `.`,
// `e` or `E`, an int past the parser's bound fails the whole parse, and a
// literal is matched by prefix. the messages name the field's kind the way
// the accessors did ("missing string field: city") and come in the
// parser's order: any malformed input is "invalid json object" before a
// field is judged, then the fields in declaration order, depth first.
//
// the spec grammar grows one field kind for this: `o<name>(<sub-spec>)`,
// a struct field whose slot holds a pre-allocated struct laid out by the
// sub-spec. the flat spec above never contains one, and the flat filler
// never sees this grammar.
// ---------------------------------------------------------------------------

/// std.json's MAX_PARSE_DEPTH: a value nested deeper than this fails the
/// parse, root value at depth 1.
const NESTED_MAX_DEPTH: usize = 128;

/// One field of the nested spec: its type char, its name, and for an `o`
/// field the sub-spec between its parentheses. `end` is the index just
/// past the field, where a `,` or the end of the spec sits. parentheses
/// rather than braces because the IR's string literals read `{{` and `}}`
/// as one escaped brace, which folds a doubly nested spec's closing pair.
struct NestedField {
    kind: u8,
    name_start: usize,
    name_end: usize,
    sub_start: usize,
    sub_end: usize,
    end: usize,
}

/// Parse the field whose type char sits at `pos`.
#[inline(always)]
fn nested_field_at(spec: &[u8], pos: usize) -> NestedField {
    let name_start = pos + 1;
    let mut i = name_start;
    while i < spec.len() && spec[i] != b',' && spec[i] != b'(' {
        i += 1;
    }
    let name_end = i;
    if i < spec.len() && spec[i] == b'(' {
        let sub_start = i + 1;
        let mut depth = 1usize;
        i += 1;
        while i < spec.len() && depth > 0 {
            match spec[i] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            i += 1;
        }
        // i is one past the closing parenthesis
        NestedField { kind: spec[pos], name_start, name_end, sub_start, sub_end: i - 1, end: i }
    } else {
        NestedField { kind: spec[pos], name_start, name_end, sub_start: 0, sub_end: 0, end: i }
    }
}

/// Where the field after `field` starts; `spec.len()` when it was the last.
#[inline(always)]
fn nested_next_pos(spec: &[u8], field: &NestedField) -> usize {
    if field.end < spec.len() {
        field.end + 1
    } else {
        spec.len()
    }
}

/// Look a key up in the nested spec: the cursor's field first, then a walk
/// from the start. Returns the field and its slot; the cursor moves to the
/// field after the hit.
#[inline(always)]
fn nested_lookup(spec: &[u8], key: &[u8], cur: &mut SpecCursor) -> Option<(usize, NestedField)> {
    if cur.pos < spec.len() {
        let field = nested_field_at(spec, cur.pos);
        if &spec[field.name_start..field.name_end] == key {
            let idx = cur.idx;
            cur.pos = nested_next_pos(spec, &field);
            cur.idx += 1;
            return Some((idx, field));
        }
    }
    let mut pos = 0;
    let mut idx = 0;
    while pos < spec.len() {
        let field = nested_field_at(spec, pos);
        if &spec[field.name_start..field.name_end] == key {
            cur.pos = nested_next_pos(spec, &field);
            cur.idx = idx + 1;
            return Some((idx, field));
        }
        pos = nested_next_pos(spec, &field);
        idx += 1;
    }
    None
}

/// The kind word the node accessors put in their messages.
fn nested_kind_word(kind: u8) -> &'static [u8] {
    match kind {
        b's' => b"string",
        b'i' => b"int",
        b'b' => b"bool",
        _ => b"object",
    }
}

fn nested_field_error(prefix: &[u8], kind: u8, name: &[u8]) -> Vec<u8> {
    let mut msg = prefix.to_vec();
    msg.extend_from_slice(nested_kind_word(kind));
    msg.extend_from_slice(b" field: ");
    msg.extend_from_slice(name);
    msg
}

/// std.json's json_utf8_encode.
fn nested_push_utf8(out: &mut Vec<u8>, code: u32) {
    if code < 0x80 {
        out.push(code as u8);
    } else if code < 0x800 {
        out.push((0xC0 + code / 64) as u8);
        out.push((0x80 + code % 64) as u8);
    } else if code < 0x10000 {
        out.push((0xE0 + code / 4096) as u8);
        out.push((0x80 + (code / 64) % 64) as u8);
        out.push((0x80 + code % 64) as u8);
    } else {
        out.push((0xF0 + code / 262144) as u8);
        out.push((0x80 + (code / 4096) % 64) as u8);
        out.push((0x80 + (code / 64) % 64) as u8);
        out.push((0x80 + code % 64) as u8);
    }
}

fn nested_hex4(raw: &[u8], at: usize) -> Option<u32> {
    let mut code = 0u32;
    for d in 0..4 {
        let digit = match raw[at + d] {
            b @ b'0'..=b'9' => (b - b'0') as u32,
            b @ b'a'..=b'f' => (b - b'a' + 10) as u32,
            b @ b'A'..=b'F' => (b - b'A' + 10) as u32,
            _ => return None,
        };
        code = code * 16 + digit;
    }
    Some(code)
}

/// std.json's json_unescape_text: rfc 8259 escapes, a surrogate pair
/// combined into one code point, anything else invalid.
fn nested_unescape(raw: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let c = raw[i];
        if c != b'\\' {
            out.push(c);
            i += 1;
            continue;
        }
        if i + 1 >= raw.len() {
            return None;
        }
        match raw[i + 1] {
            b'"' | b'\\' | b'/' => out.push(raw[i + 1]),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'n' => out.push(10),
            b'r' => out.push(13),
            b't' => out.push(9),
            b'u' => {
                if i + 5 >= raw.len() {
                    return None;
                }
                let mut code = nested_hex4(raw, i + 2)?;
                i += 6;
                if (0xD800..=0xDBFF).contains(&code) {
                    if i + 5 >= raw.len() || raw[i] != b'\\' || raw[i + 1] != b'u' {
                        return None;
                    }
                    let low = nested_hex4(raw, i + 2)?;
                    if !(0xDC00..=0xDFFF).contains(&low) {
                        return None;
                    }
                    code = 0x10000 + (code - 0xD800) * 1024 + (low - 0xDC00);
                    i += 6;
                } else if (0xDC00..=0xDFFF).contains(&code) {
                    return None;
                }
                nested_push_utf8(&mut out, code);
                continue;
            }
            _ => return None,
        }
        i += 2;
    }
    Some(out)
}

/// std.json's parse_string_raw: the content of the string opening at
/// `pos`, decoded when it carried an escape, and the position after its
/// closing quote. `Err` is an unterminated string or a bad escape.
fn nested_read_string(input: &[u8], pos: usize) -> Result<(std::borrow::Cow<'_, [u8]>, usize), ()> {
    // one scan finds the end quote and whether a backslash sat before it,
    // so a string without escapes, which is nearly every one, is looked at
    // once and borrowed.
    let mut i = pos + 1;
    let mut escaped = false;
    let end = loop {
        if i >= input.len() {
            return Err(());
        }
        match input[i] {
            b'"' => break i,
            b'\\' => {
                escaped = true;
                i += 2;
            }
            _ => i += 1,
        }
    };
    let raw = &input[pos + 1..end];
    if !escaped {
        return Ok((std::borrow::Cow::Borrowed(raw), end + 1));
    }
    match nested_unescape(raw) {
        Some(decoded) => Ok((std::borrow::Cow::Owned(decoded), end + 1)),
        None => Err(()),
    }
}

#[inline(always)]
fn nested_is_number_start(b: u8) -> bool {
    b.is_ascii_digit() || b == b'-' || b == b'+'
}

#[inline(always)]
fn nested_is_number_char(b: u8) -> bool {
    b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-')
}

/// std.json's number branch with parse_integer_manual: the span of number
/// characters at `pos`; `Ok(None)` when it holds a `.`, `e` or `E` and so
/// parses as a float, `Err` when its digits pass the parser's overflow
/// bound. Signs and stray number characters inside the span are skipped
/// the way the parser skips them, so `1-2` reads as 12 and `-` alone as 0.
fn nested_read_number(input: &[u8], pos: usize) -> Result<(Option<i64>, usize), ()> {
    let mut i = pos + 1;
    let mut is_float = false;
    while i < input.len() && nested_is_number_char(input[i]) {
        if matches!(input[i], b'.' | b'e' | b'E') {
            is_float = true;
        }
        i += 1;
    }
    if is_float {
        return Ok((None, i));
    }
    let span = &input[pos..i];
    let neg = span[0] == b'-';
    let mut value: i64 = 0;
    for &b in span {
        if b.is_ascii_digit() {
            if value > 922337203685477580 / 10 {
                return Err(());
            }
            value = value * 10 + (b - b'0') as i64;
        }
    }
    Ok((Some(if neg { -value } else { value }), i))
}

/// std.json's parse_value for a value nobody keeps: consumed with the
/// parser's grammar so a malformed value under an unknown key fails the
/// decode the way it always has. `depth` is the value's nesting level.
fn nested_skip_value(input: &[u8], pos: usize, depth: usize) -> Result<usize, ()> {
    if depth > NESTED_MAX_DEPTH {
        return Err(());
    }
    let pos = skip_ws(input, pos);
    if pos >= input.len() {
        return Err(());
    }
    let b = input[pos];
    if b == b'"' {
        return nested_read_string(input, pos).map(|(_, end)| end);
    }
    if input[pos..].starts_with(b"true") || input[pos..].starts_with(b"null") {
        return Ok(pos + 4);
    }
    if input[pos..].starts_with(b"false") {
        return Ok(pos + 5);
    }
    if b == b'[' {
        return nested_skip_array(input, pos, depth);
    }
    if b == b'{' {
        return nested_skip_object(input, pos, depth);
    }
    if nested_is_number_start(b) {
        return nested_read_number(input, pos).map(|(_, end)| end);
    }
    Err(())
}

/// The parser's array branch: elements separated by optional commas, the
/// closing bracket optional at the end of the input.
fn nested_skip_array(input: &[u8], pos: usize, depth: usize) -> Result<usize, ()> {
    let mut pos = skip_ws(input, pos + 1);
    if pos < input.len() && input[pos] == b']' {
        return Ok(pos + 1);
    }
    while pos < input.len() && input[pos] != b']' {
        pos = nested_skip_value(input, pos, depth + 1)?;
        pos = skip_ws(input, pos);
        if pos < input.len() && input[pos] == b',' {
            pos += 1;
        }
        pos = skip_ws(input, pos);
    }
    if pos < input.len() {
        pos += 1;
    }
    Ok(pos)
}

/// The parser's object branch for an object nobody keeps.
fn nested_skip_object(input: &[u8], pos: usize, depth: usize) -> Result<usize, ()> {
    let mut pos = skip_ws(input, pos + 1);
    if pos < input.len() && input[pos] == b'}' {
        return Ok(pos + 1);
    }
    let mut done = false;
    while pos < input.len() && !done {
        pos = skip_ws(input, pos);
        if pos >= input.len() || input[pos] != b'"' {
            return Err(());
        }
        let (_, after_key) = nested_read_string(input, pos)?;
        pos = skip_ws(input, after_key);
        if pos < input.len() && input[pos] == b':' {
            pos += 1;
        }
        pos = nested_skip_value(input, pos, depth + 1)?;
        pos = skip_ws(input, pos);
        if pos < input.len() && input[pos] == b',' {
            pos += 1;
        }
        pos = skip_ws(input, pos);
        if pos < input.len() && input[pos] == b'}' {
            done = true;
        }
    }
    if pos < input.len() && input[pos] == b'}' {
        pos += 1;
    }
    Ok(pos)
}

/// The outcome of one object fill: the position after the object and, when
/// a field of it or of a struct under it was missing or of the wrong type,
/// the message for the first such field in declaration order. That error
/// waits with the caller until the whole document has parsed, because a
/// malformed byte anywhere outranks it and a field declared earlier in the
/// parent outranks it too.
type NestedFill = Result<(usize, Option<Vec<u8>>), ()>;

/// Fill the struct at `obj`, laid out by `spec`, from the object opening at
/// `pos` (the byte is `{`). A key the spec knows writes its slot; one it
/// does not is consumed with the parser's grammar. A key that repeats has
/// its last value win, as the node accessors read the last entry: a string
/// slot already holding a value is released before it is overwritten, and
/// a struct slot is filled again in place with a fresh mask.
///
/// # Safety
/// `obj` must have a slot per spec field; each `o` field's slot must hold a
/// struct laid out by that field's sub-spec.
unsafe fn nested_fill_object(input: &[u8], pos: usize, spec: &[u8], obj: *mut i64, depth: usize) -> NestedFill {
    let mut filled: u64 = 0;
    let mut wrong: u64 = 0;
    let mut deferred: Vec<(usize, Vec<u8>)> = Vec::new();
    let mut cur = SpecCursor { pos: 0, idx: 0 };
    let inner_depth = depth + 1;

    let mut pos = skip_ws(input, pos + 1);
    let mut done = pos < input.len() && input[pos] == b'}';
    if done {
        pos += 1;
    }
    while pos < input.len() && !done {
        pos = skip_ws(input, pos);
        if pos >= input.len() || input[pos] != b'"' {
            return Err(());
        }
        let (key, after_key) = nested_read_string(input, pos)?;
        pos = skip_ws(input, after_key);
        if pos < input.len() && input[pos] == b':' {
            pos += 1;
        }
        if inner_depth > NESTED_MAX_DEPTH {
            return Err(());
        }
        pos = skip_ws(input, pos);
        if pos >= input.len() {
            return Err(());
        }
        let b = input[pos];
        match nested_lookup(spec, &key, &mut cur) {
            None => {
                pos = nested_skip_value(input, pos, inner_depth)?;
            }
            Some((idx, field)) => {
                let bit = 1u64 << idx;
                let slot = obj.add(idx);
                let mut ok = false;
                match field.kind {
                    b's' if b == b'"' => {
                        let (value, end) = nested_read_string(input, pos)?;
                        if *slot != 0 {
                            crate::pith_cstring_release(*slot as *const i8);
                        }
                        *slot = crate::pith_copy_bytes_to_cstring(&value) as i64;
                        pos = end;
                        ok = true;
                    }
                    b'i' if nested_is_number_start(b) => {
                        let (value, end) = nested_read_number(input, pos)?;
                        if let Some(value) = value {
                            *slot = value;
                            ok = true;
                        }
                        pos = end;
                    }
                    b'b' if input[pos..].starts_with(b"true") => {
                        *slot = 1;
                        pos += 4;
                        ok = true;
                    }
                    b'b' if input[pos..].starts_with(b"false") => {
                        *slot = 0;
                        pos += 5;
                        ok = true;
                    }
                    b'o' if b == b'{' && *slot != 0 => {
                        let sub_spec = &spec[field.sub_start..field.sub_end];
                        let (end, sub_err) = nested_fill_object(input, pos, sub_spec, *slot as *mut i64, inner_depth)?;
                        deferred.retain(|(slot_idx, _)| *slot_idx != idx);
                        if let Some(msg) = sub_err {
                            deferred.push((idx, msg));
                        }
                        pos = end;
                        ok = true;
                    }
                    _ => {
                        pos = nested_skip_value(input, pos, inner_depth)?;
                    }
                }
                if ok {
                    filled |= bit;
                    wrong &= !bit;
                } else {
                    filled &= !bit;
                    wrong |= bit;
                    deferred.retain(|(slot_idx, _)| *slot_idx != idx);
                }
            }
        }
        pos = skip_ws(input, pos);
        if pos < input.len() && input[pos] == b',' {
            pos += 1;
        }
        pos = skip_ws(input, pos);
        if pos < input.len() && input[pos] == b'}' {
            done = true;
            pos += 1;
        }
    }

    // the fields in declaration order: a wrong type, then a miss, then
    // whatever a nested struct reported, the first of which is the error.
    let mut spec_pos = 0;
    let mut idx = 0;
    while spec_pos < spec.len() {
        let field = nested_field_at(spec, spec_pos);
        let bit = 1u64 << idx;
        let name = &spec[field.name_start..field.name_end];
        if wrong & bit != 0 {
            return Ok((pos, Some(nested_field_error(b"expected ", field.kind, name))));
        }
        if filled & bit == 0 {
            return Ok((pos, Some(nested_field_error(b"missing ", field.kind, name))));
        }
        if field.kind == b'o' {
            if let Some((_, msg)) = deferred.iter().find(|(slot_idx, _)| *slot_idx == idx) {
                return Ok((pos, Some(msg.clone())));
            }
        }
        spec_pos = nested_next_pos(spec, &field);
        idx += 1;
    }
    Ok((pos, None))
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
    let spec = cstr_bytes(spec_ptr);
    let outcome: Result<(), Vec<u8>> = (|| {
        let Some(bytes) = pith_bytes_ref(bytes_handle) else {
            return Err(b"invalid json object".to_vec());
        };
        let input = bytes.data.as_slice();
        if check_utf8 != 0 && std::str::from_utf8(input).is_err() {
            return Err(b"bytes_to_string_utf8 failed".to_vec());
        }
        let pos = skip_ws(input, 0);
        if pos >= input.len() || input[pos] != b'{' {
            return Err(b"invalid json object".to_vec());
        }
        let Ok((end, field_error)) = nested_fill_object(input, pos, spec, struct_ptr as *mut i64, 1) else {
            return Err(b"invalid json object".to_vec());
        };
        if skip_ws(input, end) < input.len() {
            return Err(b"invalid json object".to_vec());
        }
        match field_error {
            Some(msg) => Err(msg),
            None => Ok(()),
        }
    })();
    match outcome {
        Ok(()) => alloc_result(1, struct_ptr, 0),
        Err(msg) => {
            crate::pith_struct_release(struct_ptr);
            err_result(&msg)
        }
    }
}
