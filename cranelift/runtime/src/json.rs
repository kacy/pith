use crate::bytes::with_bytes;
use crate::ffi_util::with_cstr_bytes;

fn skip_ws(input: &[u8], mut pos: usize) -> usize {
    while pos < input.len() && matches!(input[pos], b' ' | b'\t' | b'\n' | b'\r') {
        pos += 1;
    }
    pos
}

/// The end of the number span at `pos`: the maximal run of the bytes a
/// json number is written with. The span is the unit the filler hands to
/// the float conversion and the unit a number is read by.
#[inline(always)]
fn number_span_end(input: &[u8], pos: usize) -> usize {
    let mut i = pos;
    while i < input.len() && nested_is_number_char(input[i]) {
        i += 1;
    }
    i
}

/// The f64 a json number span denotes, or None when the span is not a
/// finite number. The conversion is std's `f64::from_str`, the one
/// `parse_float` uses, so the filler, the generated flat decoder and the
/// node parser agree on the value to the last bit. The span's bytes are digits, sign, `.`, `e` and
/// `E` only, so `NaN` and `Infinity` can never reach the conversion; an
/// exponent past the type's range comes back infinite and is refused
/// here, which leaves `1.7976931348623157e308` the largest value a field
/// takes and `5e-324` the smallest positive one. An integer span (`3`)
/// converts like any other and widens to `3.0`; `-0.0` keeps its sign.
#[inline(always)]
fn float_of_span(span: &[u8]) -> Option<f64> {
    let text = std::str::from_utf8(span).ok()?;
    let value: f64 = text.parse().ok()?;
    if value.is_finite() {
        Some(value)
    } else {
        None
    }
}

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

/// The field the filler expects next: the one after the last match. An
/// encoder writes an object's keys in declaration order, so nearly every
/// key matches here on one compare and the spec is never walked.
struct SpecCursor {
    pos: usize,
    idx: usize,
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
// mirrors that parser rather than the generated flat decoder
// (self-host/ir_json_fill_emit.pith, for a struct of scalars alone),
// which keeps the grammar of the flat filler it replaced: string escapes are
// decoded, every colon and comma is required, an object or array must be
// closed and takes no trailing comma, a number is an int only when it has no `.`,
// `e` or `E`, an int past the parser's bound fails the whole parse, and a
// literal is matched by prefix. the messages name the field's kind the way
// the accessors did ("missing string field: city") and come in the
// parser's order: any malformed input is "invalid json object" before a
// field is judged, then the fields in declaration order, depth first.
//
// the spec grammar grows three field kinds for this. `o<name>(<sub-spec>)`
// is a struct field whose slot holds a pre-allocated struct laid out by the
// sub-spec. `l<name>(<elem>)` is a list field and `m<name>(<elem>)` a
// `Map[String, T]` field, an object whose every key is kept: `<elem>` is a
// scalar letter, `l(<elem>)` for a list, `m(<elem>)` for a map, or
// `o#<k>(<sub-spec>)` for a struct, so the grammar is recursive and a list
// of maps of lists of structs parses like anything else. a collection is
// what this filler builds itself: the element structs cannot be allocated
// ahead of the call because their count is the document's, so the lowering
// hands over a table of destructor addresses and `#<k>` names the entry an
// element struct gets (0 for a struct that needs none). the same
// `o<name>#<k>(<sub-spec>)` form marks a struct field inside a collection
// element, which has no pre-allocated slot either. the scalar letters are
// `s`, `i`, `b` and `f`: an `f` slot takes any json number and holds the
// f64 bit pattern, an `i` slot only an integer.
//
// ownership while a collection is built: the list or map handle is stored
// into its slot (or pushed into, or inserted under its key in, its parent)
// before the first element is read, and an element struct is pushed or
// inserted, with its destructor set, before its first field is read. so at
// every moment the root struct owns everything built so far, and the one
// release the entry point does on failure drops it all: the list or map
// releases its elements, an element struct releases its strings and
// sub-structs. there is no separate cleanup path to get wrong. the leak
// cases tests/leaks/leak_json_fill_list_fail and
// tests/leaks/leak_json_fill_collections_fail pin this for a failure at the
// first, second and last element, in the inner list of the third outer
// element, and in the second member of a map.
//
// depth: every recursion below carries the value's nesting level and
// refuses to descend past NESTED_MAX_DEPTH, the node parser's bound, so a
// hostile document a hundred thousand deep stops at the bound with a named
// error rather than on the stack.
// ---------------------------------------------------------------------------

/// std.json's MAX_PARSE_DEPTH: a value nested deeper than this fails the
/// parse, root value at depth 1. The filler counts the same way (the root
/// object is depth 1, a field's value depth 2), so a document the parser
/// takes the filler takes, and one the parser refuses the filler refuses
/// with `NESTED_TOO_DEEP`. Every recursion below carries its depth and
/// checks it before descending, so a hostile `[[[[...` a hundred thousand
/// deep stops here rather than on the stack.
const NESTED_MAX_DEPTH: usize = 128;

/// The decode error for a document nested past `NESTED_MAX_DEPTH`.
const NESTED_TOO_DEEP: &[u8] = b"json nested deeper than 128 levels";

/// Why a fill gave up on the whole document: a byte the grammar does not
/// take (`invalid json object`, what the node parser reports), or a value
/// nested past the depth bound. Either outranks any field error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NestedAbort {
    Malformed,
    TooDeep,
}

impl From<()> for NestedAbort {
    fn from(_: ()) -> Self {
        NestedAbort::Malformed
    }
}

impl NestedAbort {
    fn message(self) -> &'static [u8] {
        match self {
            NestedAbort::Malformed => b"invalid json object",
            NestedAbort::TooDeep => NESTED_TOO_DEEP,
        }
    }
}

/// One field of the nested spec: its type char, its name, for an `o`, `l`
/// or `m` field the sub-spec between its parentheses, and for a struct the
/// filler allocates itself (`o<name>#<k>(...)`) the destructor table index
/// `k`; `dtor` is -1 for a struct whose slot the lowering pre-allocated.
/// `end` is the index just past the field, where a `,` or the end of the
/// spec sits. parentheses rather than braces because the IR's string
/// literals read `{{` and `}}` as one escaped brace, which folds a doubly
/// nested spec's closing pair. an element spec (`i`, `o#2(...)`, `l(s)`,
/// `m(i)`) parses with the same function: its name is empty. a map entry,
/// `m<name>(<elem>)`, is an object with arbitrary keys whose every value is
/// one `<elem>`; the same element grammar as a list's.
struct NestedField {
    kind: u8,
    name_start: usize,
    name_end: usize,
    dtor: i64,
    sub_start: usize,
    sub_end: usize,
    end: usize,
}

/// Parse the field whose type char sits at `pos`.
#[inline(always)]
fn nested_field_at(spec: &[u8], pos: usize) -> NestedField {
    let name_start = pos + 1;
    let mut i = name_start;
    while i < spec.len() && spec[i] != b',' && spec[i] != b'(' && spec[i] != b'#' {
        i += 1;
    }
    let name_end = i;
    let mut dtor: i64 = -1;
    if i < spec.len() && spec[i] == b'#' {
        dtor = 0;
        i += 1;
        while i < spec.len() && spec[i].is_ascii_digit() {
            dtor = dtor * 10 + (spec[i] - b'0') as i64;
            i += 1;
        }
    }
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
        NestedField { kind: spec[pos], name_start, name_end, dtor, sub_start, sub_end: i - 1, end: i }
    } else {
        NestedField { kind: spec[pos], name_start, name_end, dtor, sub_start: 0, sub_end: 0, end: i }
    }
}

/// How many fields a sub-spec lays out: one more than its top-level
/// separators, none for an empty spec. The slot count of a struct the
/// filler allocates for a list element.
fn nested_spec_field_count(spec: &[u8]) -> usize {
    if spec.is_empty() {
        return 0;
    }
    let mut count = 1;
    let mut depth = 0usize;
    for &b in spec {
        match b {
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => count += 1,
            _ => {}
        }
    }
    count
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
        b'f' => b"float",
        b'l' => b"list",
        b'm' => b"map",
        _ => b"object",
    }
}

/// The message for an element of the wrong kind: `expected int element:
/// xs[2]`, the field's kind word and the element's position under the
/// field, so a caller can tell which element and which field.
fn nested_element_error(kind: u8, path: &[u8], index: usize) -> Vec<u8> {
    let mut at = path.to_vec();
    nested_push_index(&mut at, index);
    nested_kind_error_at(kind, &at)
}

/// Append `[<index>]` to a path.
fn nested_push_index(out: &mut Vec<u8>, index: usize) {
    out.push(b'[');
    out.extend_from_slice(index.to_string().as_bytes());
    out.push(b']');
}

/// A map member's path: `<path>.<key>`, the dotted form `decode_path`
/// reads, so a wrong value under a key is `expected int element:
/// scores.bob` and a struct member's own error `scores.bob: missing int
/// field: x`. A key holding a dot is written as it is; the message is for
/// reading, not for parsing back.
fn nested_member_path(path: &[u8], key: &[u8]) -> Vec<u8> {
    let mut out = path.to_vec();
    out.push(b'.');
    out.extend_from_slice(key);
    out
}

/// `expected <kind> element: <path>` for a value that is not of its kind,
/// the path already naming the position (`xs[2]`, `scores.bob`).
fn nested_kind_error_at(kind: u8, path: &[u8]) -> Vec<u8> {
    let mut msg = b"expected ".to_vec();
    msg.extend_from_slice(nested_kind_word(kind));
    msg.extend_from_slice(b" element: ");
    msg.extend_from_slice(path);
    msg
}

/// `<path>: <msg>`: a struct's own error placed under its position.
fn nested_prefixed_at(path: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut out = path.to_vec();
    out.extend_from_slice(b": ");
    out.extend_from_slice(msg);
    out
}

/// A struct element's own error, placed under its position: `rows[1]:
/// missing string field: name`. A list of lists nests the same way.
fn nested_element_prefixed(path: &[u8], index: usize, msg: &[u8]) -> Vec<u8> {
    let mut at = path.to_vec();
    nested_push_index(&mut at, index);
    nested_prefixed_at(&at, msg)
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
    let i = number_span_end(input, pos + 1);
    if input[pos + 1..i].iter().any(|b| matches!(b, b'.' | b'e' | b'E')) {
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
fn nested_skip_value(input: &[u8], pos: usize, depth: usize) -> Result<usize, NestedAbort> {
    if depth > NESTED_MAX_DEPTH {
        return Err(NestedAbort::TooDeep);
    }
    let pos = skip_ws(input, pos);
    if pos >= input.len() {
        return Err(NestedAbort::Malformed);
    }
    let b = input[pos];
    if b == b'"' {
        return nested_read_string(input, pos).map(|(_, end)| end).map_err(NestedAbort::from);
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
        return nested_read_number(input, pos).map(|(_, end)| end).map_err(NestedAbort::from);
    }
    Err(NestedAbort::Malformed)
}

/// What follows a value inside an array or object: a comma with more to
/// come, or the closing byte.
enum NestedSep {
    /// a comma; the position is the first byte after it that is not space
    More(usize),
    /// the closing byte; the position is the one after it
    Closed(usize),
}

/// The separator after a value that ends at `pos`, in a container closed by
/// `close`. Anything else, the end of the input included, is malformed,
/// as it is to the node parser. The object and map fill loops spell this
/// out in place: through this helper the nested decode bench ran 4% slower,
/// while the list loop runs 6% slower without it.
#[inline(always)]
fn nested_after_value(input: &[u8], pos: usize, close: u8) -> Result<NestedSep, NestedAbort> {
    let pos = skip_ws(input, pos);
    match input.get(pos) {
        Some(&b',') => Ok(NestedSep::More(skip_ws(input, pos + 1))),
        Some(&c) if c == close => Ok(NestedSep::Closed(pos + 1)),
        _ => Err(NestedAbort::Malformed),
    }
}

/// The position after the colon that must follow an object key ending at
/// `pos`.
#[inline(always)]
fn nested_expect_colon(input: &[u8], pos: usize) -> Result<usize, NestedAbort> {
    let pos = skip_ws(input, pos);
    if input.get(pos) == Some(&b':') {
        Ok(pos + 1)
    } else {
        Err(NestedAbort::Malformed)
    }
}

/// The parser's array branch for an array nobody keeps.
fn nested_skip_array(input: &[u8], pos: usize, depth: usize) -> Result<usize, NestedAbort> {
    let mut pos = skip_ws(input, pos + 1);
    if input.get(pos) == Some(&b']') {
        return Ok(pos + 1);
    }
    loop {
        pos = nested_skip_value(input, pos, depth + 1)?;
        match nested_after_value(input, pos, b']')? {
            NestedSep::More(next) => pos = next,
            NestedSep::Closed(end) => return Ok(end),
        }
    }
}

/// The parser's object branch for an object nobody keeps.
fn nested_skip_object(input: &[u8], pos: usize, depth: usize) -> Result<usize, NestedAbort> {
    let mut pos = skip_ws(input, pos + 1);
    if input.get(pos) == Some(&b'}') {
        return Ok(pos + 1);
    }
    loop {
        if input.get(pos) != Some(&b'"') {
            return Err(NestedAbort::Malformed);
        }
        let (_, after_key) = nested_read_string(input, pos)?;
        pos = nested_expect_colon(input, after_key)?;
        pos = nested_skip_value(input, pos, depth + 1)?;
        match nested_after_value(input, pos, b'}')? {
            NestedSep::More(next) => pos = next,
            NestedSep::Closed(end) => return Ok(end),
        }
    }
}

/// The outcome of one object fill: the position after the object and, when
/// a field of it or of a struct under it was missing or of the wrong type,
/// the message for the first such field in declaration order. That error
/// waits with the caller until the whole document has parsed, because a
/// malformed byte anywhere outranks it and a field declared earlier in the
/// parent outranks it too.
type NestedFill = Result<(usize, Option<Vec<u8>>), NestedAbort>;

/// A validated destructor address taken from the lowering's destructor table
/// (0 means no destructor is needed for the struct).
#[derive(Clone, Copy)]
struct ValidDtor(i64);

impl ValidDtor {
    /// Allocate the struct a list element or a struct field under one needs:
    /// `field_count` zeroed slots, with this destructor attached when non-zero.
    /// Returns `None` when the allocation failed, which the caller treats as a
    /// malformed fill.
    #[inline(always)]
    fn alloc_object(self, spec: &[u8]) -> Option<ObjectTarget<'_>> {
        let field_count = nested_spec_field_count(spec);
        // SAFETY: Allocates a fresh zeroed struct with `field_count` slots.
        let ptr = unsafe { crate::pith_struct_alloc(field_count as i64) };
        if ptr == 0 {
            return None;
        }
        if self.0 != 0 {
            // SAFETY: `ptr` is a freshly allocated struct and `self.0` came from `DtorTable`.
            unsafe { crate::pith_struct_set_dtor(ptr, self.0) };
        }
        // SAFETY: `ptr` has `nested_spec_field_count(spec)` zeroed slots matching `spec`.
        Some(unsafe { ObjectTarget::from_raw(spec, ptr) })
    }
}

/// The table the entry points thread through the fill: the destructor
/// addresses a list element struct may name.
struct DtorTable<'a>(&'a [i64]);

impl<'a> DtorTable<'a> {
    const EMPTY: DtorTable<'static> = DtorTable(&[]);

    /// # Safety
    /// When `ptr != 0` and `count > 0`, `ptr` must point to at least `count` readable
    /// destructor function addresses (or 0 for no destructor) valid for `'a`.
    #[inline(always)]
    unsafe fn from_raw(ptr: i64, count: i64) -> Self {
        if ptr == 0 || count <= 0 {
            return DtorTable(&[]);
        }
        // SAFETY: Preconditions forwarded from caller.
        DtorTable(unsafe { std::slice::from_raw_parts(ptr as *const i64, count as usize) })
    }

    /// The destructor address at index `k` of the table the lowering passed,
    /// or None when the table has no such entry (a spec naming an index the
    /// table lacks is a lowering defect, and the fill fails rather than
    /// attaching a wrong address). `k` is -1 for a pre-allocated struct, which
    /// never asks.
    #[inline(always)]
    fn get(&self, k: i64) -> Option<ValidDtor> {
        let idx = usize::try_from(k).ok()?;
        self.0.get(idx).copied().map(ValidDtor)
    }
}

/// A live Pith struct allocation paired with the spec describing its slots.
///
/// Invariant: `obj` has one `i64` slot per field in `spec`, and each pre-allocated
/// `o` field (without `#<k>`) holds either 0 or a sub-struct laid out by its sub-spec.
#[derive(Clone, Copy)]
struct ObjectTarget<'a> {
    spec: &'a [u8],
    obj: *mut i64,
}

impl<'a> ObjectTarget<'a> {
    /// # Safety
    /// `struct_ptr` must be a `pith_struct_alloc` result laid out by `spec`, with
    /// each pre-allocated `o` slot holding a struct laid out by its sub-spec.
    #[inline(always)]
    unsafe fn from_raw(spec: &'a [u8], struct_ptr: i64) -> Self {
        Self {
            spec,
            obj: struct_ptr as *mut i64,
        }
    }

    #[inline(always)]
    fn as_handle(self) -> i64 {
        self.obj as i64
    }

    #[inline(always)]
    fn read_slot(self, idx: usize) -> i64 {
        // SAFETY: `idx` came from `nested_lookup(self.spec, ...)`, so it is in bounds of `self.obj`.
        unsafe { *self.obj.add(idx) }
    }

    #[inline(always)]
    fn write_slot(self, idx: usize, value: i64) {
        // SAFETY: `idx` came from `nested_lookup(self.spec, ...)`, so it is in bounds of `self.obj`.
        unsafe { *self.obj.add(idx) = value };
    }

    #[inline(always)]
    fn replace_string(self, idx: usize, bytes: &[u8]) {
        let prev = self.read_slot(idx);
        if prev != 0 {
            // SAFETY: Slot holds an owned pith C string written by an earlier occurrence of this key.
            unsafe { crate::pith_cstring_release(prev as *const i8) };
        }
        self.write_slot(idx, crate::pith_copy_bytes_to_cstring(bytes) as i64);
    }

    #[inline(always)]
    fn has_object_slot(self, idx: usize, dtor: i64) -> bool {
        dtor >= 0 || self.read_slot(idx) != 0
    }

    #[inline(always)]
    fn child_object(self, idx: usize, sub_spec: &'a [u8]) -> ObjectTarget<'a> {
        let child = self.read_slot(idx);
        // SAFETY: By `ObjectTarget`'s invariant, slot `idx` holds a struct laid out by `sub_spec`.
        unsafe { ObjectTarget::from_raw(sub_spec, child) }
    }

    #[inline(always)]
    fn clear_struct(self, idx: usize) {
        let prev = self.read_slot(idx);
        if prev != 0 {
            // SAFETY: Slot holds an owned struct allocated by an earlier occurrence of this key.
            unsafe { crate::pith_struct_release(prev) };
            self.write_slot(idx, 0);
        }
    }

    #[inline(always)]
    fn clear_list(self, idx: usize) {
        let prev = self.read_slot(idx);
        if prev != 0 {
            // SAFETY: Slot holds an owned list allocated by an earlier occurrence of this key.
            unsafe { crate::collections::list::pith_list_release_handle(prev) };
            self.write_slot(idx, 0);
        }
    }

    #[inline(always)]
    fn clear_map(self, idx: usize) {
        let prev = self.read_slot(idx);
        if prev != 0 {
            // SAFETY: Slot holds an owned map allocated by an earlier occurrence of this key.
            unsafe { crate::collections::map::pith_map_release_handle(prev) };
            self.write_slot(idx, 0);
        }
    }
}

/// The kind of value an element spec builds, as the tag a container owns
/// its elements by: a string, a struct, a list or a map is held by count,
/// an int, bool or float by value.
fn nested_elem_tag(elem: &[u8]) -> crate::collections::list::ListTypeTag {
    use crate::collections::list::ListTypeTag;
    match elem.first() {
        Some(b's') => ListTypeTag::String,
        Some(b'o') => ListTypeTag::Struct,
        Some(b'l') => ListTypeTag::List,
        Some(b'm') => ListTypeTag::Map,
        _ => ListTypeTag::Primitive,
    }
}

/// A live list handle allocated for a decoded list field or element.
#[derive(Clone, Copy)]
struct NestedList(crate::collections::list::PithList);

impl NestedList {
    /// The list a field of element spec `elem` holds: the constructor is the
    /// element kind's, the same one the emitter picks for a list literal of
    /// that type (`ir_list_ctor_for_elem_kind`), so a list of strings, structs
    /// or lists owns its elements and its release cascades, and a list of
    /// ints, bools or floats holds them by value.
    #[inline(always)]
    fn new(elem: &[u8]) -> Option<Self> {
        use crate::collections::list;
        // SAFETY: All `pith_list_new_*` constructors allocate a fresh list with no pointer inputs.
        let handle = match elem.first() {
            Some(b's') => unsafe { list::pith_list_new_cstr() },
            Some(b'o') => unsafe { list::pith_list_new_struct() },
            Some(b'l') => unsafe { list::pith_list_new_nested_list() },
            Some(b'm') => unsafe { list::pith_list_new_nested_map() },
            _ => unsafe { list::pith_list_new_default() },
        };
        (!handle.ptr.is_null()).then_some(NestedList(handle))
    }

    #[inline(always)]
    fn as_handle(self) -> i64 {
        self.0.ptr as i64
    }

    #[inline(always)]
    fn push_value(self, value: i64) {
        // SAFETY: `self.0` is a live list built by `NestedList::new` for primitive elements.
        unsafe { crate::collections::list::pith_list_push_value(self.0, value) };
    }

    #[inline(always)]
    fn push_owned(self, value: i64) {
        // SAFETY: `self.0` is a live list built by `NestedList::new` and `value` transfers one count.
        unsafe { crate::collections::list::pith_list_push_value_owned(self.0, value) };
    }
}

/// A live string-keyed map handle allocated for a decoded map field or element.
#[derive(Clone, Copy)]
struct NestedMap(i64);

impl NestedMap {
    /// The map a `Map[String, T]` field holds: string-keyed, owning its values
    /// by the element kind's tag, as the map a literal of that type builds
    /// does, so a map of strings, structs, lists or maps releases its values
    /// with itself and a map of ints, bools or floats holds them by value.
    #[inline(always)]
    fn new(elem: &[u8]) -> Option<Self> {
        // SAFETY: Allocates a fresh string-keyed map with value tag `nested_elem_tag(elem)`.
        let handle = unsafe { crate::collections::map::new_string_keyed_handle(nested_elem_tag(elem)) };
        (handle != 0).then_some(NestedMap(handle))
    }

    #[inline(always)]
    fn as_handle(self) -> i64 {
        self.0
    }

    /// Store a member into a decode-built map: a heap value hands the map its
    /// one count (the allocation's, or the fresh string's), a scalar is copied.
    /// A key already present has its earlier value released by the map, so a
    /// repeated key leaves one value, the last, and nothing stranded.
    #[inline(always)]
    fn insert(self, elem: &[u8], key: &[u8], value: i64) {
        use crate::collections::list::ListTypeTag;
        let owned = nested_elem_tag(elem) != ListTypeTag::Primitive;
        // SAFETY: `self.0` is a live string-keyed map built for `elem`; `value` matches `elem`.
        unsafe { crate::collections::map::insert_bytes_key(self.0, key, value, owned) };
    }
}

/// Fill the struct behind `target` from the object opening at `pos` (the byte
/// is `{`). A key the spec knows writes its slot; one it does not is consumed
/// with the parser's grammar. A key that repeats has its last value win, as the
/// node accessors read the last entry: a string slot already holding a value is
/// released before it is overwritten, a pre-allocated struct slot is filled
/// again in place with a fresh mask, and a list slot or a filler-allocated
/// struct slot has its first value released before the second is built.
fn nested_fill_object<'a>(
    input: &[u8],
    pos: usize,
    target: ObjectTarget<'a>,
    dtors: &DtorTable<'_>,
    depth: usize,
) -> NestedFill {
/// Compact bitset for field presence/mismatch tracking: inline `u64` for the
/// common <=64-field struct (zero heap allocation), spilling into `Vec<u64>`
/// for structs with 65+ fields so `1u64 << idx` never overflows.
struct FieldBits {
    lo: u64,
    hi: Vec<u64>,
}

impl FieldBits {
    #[inline]
    fn new() -> Self {
        FieldBits {
            lo: 0,
            hi: Vec::new(),
        }
    }

    #[inline]
    fn set(&mut self, idx: usize) {
        if idx < 64 {
            self.lo |= 1u64 << idx;
        } else {
            let word = (idx - 64) / 64;
            let bit = (idx - 64) % 64;
            if self.hi.len() <= word {
                self.hi.resize(word + 1, 0);
            }
            self.hi[word] |= 1u64 << bit;
        }
    }

    #[inline]
    fn clear(&mut self, idx: usize) {
        if idx < 64 {
            self.lo &= !(1u64 << idx);
        } else {
            let word = (idx - 64) / 64;
            let bit = (idx - 64) % 64;
            if let Some(slot) = self.hi.get_mut(word) {
                *slot &= !(1u64 << bit);
            }
        }
    }

    #[inline]
    fn contains(&self, idx: usize) -> bool {
        if idx < 64 {
            (self.lo & (1u64 << idx)) != 0
        } else {
            let word = (idx - 64) / 64;
            let bit = (idx - 64) % 64;
            self.hi
                .get(word)
                .is_some_and(|w| (*w & (1u64 << bit)) != 0)
        }
    }
}

    let spec = target.spec;
    let mut filled = FieldBits::new();
    let mut wrong = FieldBits::new();
    let mut deferred: Vec<(usize, Vec<u8>)> = Vec::new();
    let mut cur = SpecCursor { pos: 0, idx: 0 };
    let inner_depth = depth + 1;

    let mut pos = skip_ws(input, pos + 1);
    let mut done = pos < input.len() && input[pos] == b'}';
    if done {
        pos += 1;
    }
    while !done {
        if input.get(pos) != Some(&b'"') {
            return Err(NestedAbort::Malformed);
        }
        let (key, after_key) = nested_read_string(input, pos)?;
        pos = nested_expect_colon(input, after_key)?;
        if inner_depth > NESTED_MAX_DEPTH {
            return Err(NestedAbort::TooDeep);
        }
        pos = skip_ws(input, pos);
        if pos >= input.len() {
            return Err(NestedAbort::Malformed);
        }
        let b = input[pos];
        match nested_lookup(spec, &key, &mut cur) {
            None => {
                pos = nested_skip_value(input, pos, inner_depth)?;
            }
            Some((idx, field)) => {
                let mut ok = false;
                match field.kind {
                    b's' if b == b'"' => {
                        let (value, end) = nested_read_string(input, pos)?;
                        target.replace_string(idx, &value);
                        pos = end;
                        ok = true;
                    }
                    b'i' if nested_is_number_start(b) => {
                        let (value, end) = nested_read_number(input, pos)?;
                        if let Some(value) = value {
                            target.write_slot(idx, value);
                            ok = true;
                        }
                        pos = end;
                    }
                    b'f' if nested_is_number_start(b) => {
                        // the node parser's float branch: the whole number
                        // span, an integer one included, converted by the
                        // runtime. a span that is not a finite number fails
                        // the parse the way the parser's float branch does.
                        let end = number_span_end(input, pos + 1);
                        let Some(value) = float_of_span(&input[pos..end]) else {
                            return Err(NestedAbort::Malformed);
                        };
                        target.write_slot(idx, value.to_bits() as i64);
                        pos = end;
                        ok = true;
                    }
                    b'b' if input[pos..].starts_with(b"true") => {
                        target.write_slot(idx, 1);
                        pos += 4;
                        ok = true;
                    }
                    b'b' if input[pos..].starts_with(b"false") => {
                        target.write_slot(idx, 0);
                        pos += 5;
                        ok = true;
                    }
                    b'o' if b == b'{' && target.has_object_slot(idx, field.dtor) => {
                        let sub_spec = &spec[field.sub_start..field.sub_end];
                        let child = if field.dtor >= 0 {
                            // a struct field under a list element: no
                            // pre-allocated slot, so allocate it here and
                            // store it before it is filled, and drop what a
                            // repeated key built before.
                            let Some(dtor) = dtors.get(field.dtor) else {
                                return Err(NestedAbort::Malformed);
                            };
                            target.clear_struct(idx);
                            let Some(sub) = dtor.alloc_object(sub_spec) else {
                                return Err(NestedAbort::Malformed);
                            };
                            target.write_slot(idx, sub.as_handle());
                            sub
                        } else {
                            target.child_object(idx, sub_spec)
                        };
                        let (end, sub_err) = nested_fill_object(input, pos, child, dtors, inner_depth)?;
                        deferred.retain(|(slot_idx, _)| *slot_idx != idx);
                        if let Some(msg) = sub_err {
                            deferred.push((idx, msg));
                        }
                        pos = end;
                        ok = true;
                    }
                    b'l' if b == b'[' => {
                        // a repeated list key: the first list and everything
                        // in it go before the second is built.
                        target.clear_list(idx);
                        let elem = &spec[field.sub_start..field.sub_end];
                        let Some(list) = NestedList::new(elem) else {
                            return Err(NestedAbort::Malformed);
                        };
                        // owned by the struct from here: a failure anywhere
                        // below releases the root and the list with it.
                        target.write_slot(idx, list.as_handle());
                        let name = &spec[field.name_start..field.name_end];
                        let (end, elem_err) = nested_fill_list(input, pos, elem, name, list, dtors, inner_depth)?;
                        deferred.retain(|(slot_idx, _)| *slot_idx != idx);
                        if let Some(msg) = elem_err {
                            deferred.push((idx, msg));
                        }
                        pos = end;
                        ok = true;
                    }
                    b'm' if b == b'{' => {
                        // a repeated map key: the first map and its values
                        // go before the second is built, as for a list.
                        target.clear_map(idx);
                        let elem = &spec[field.sub_start..field.sub_end];
                        let Some(map) = NestedMap::new(elem) else {
                            return Err(NestedAbort::Malformed);
                        };
                        // owned by the struct before the first member is
                        // read, so a failure below cascades from the root.
                        target.write_slot(idx, map.as_handle());
                        let name = &spec[field.name_start..field.name_end];
                        let (end, member_err) = nested_fill_map(input, pos, elem, name, map, dtors, inner_depth)?;
                        deferred.retain(|(slot_idx, _)| *slot_idx != idx);
                        if let Some(msg) = member_err {
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
                    filled.set(idx);
                    wrong.clear(idx);
                } else {
                    filled.clear(idx);
                    wrong.set(idx);
                    deferred.retain(|(slot_idx, _)| *slot_idx != idx);
                }
            }
        }
        pos = skip_ws(input, pos);
        match input.get(pos) {
            Some(&b',') => pos = skip_ws(input, pos + 1),
            Some(&b'}') => {
                pos += 1;
                done = true;
            }
            _ => return Err(NestedAbort::Malformed),
        }
    }

    // the fields in declaration order: a wrong type, then a miss, then
    // whatever a nested struct reported, the first of which is the error.
    let mut spec_pos = 0;
    let mut idx = 0;
    while spec_pos < spec.len() {
        let field = nested_field_at(spec, spec_pos);
        let name = &spec[field.name_start..field.name_end];
        if wrong.contains(idx) {
            return Ok((pos, Some(nested_field_error(b"expected ", field.kind, name))));
        }
        if !filled.contains(idx) {
            return Ok((pos, Some(nested_field_error(b"missing ", field.kind, name))));
        }
        if field.kind == b'o' || field.kind == b'l' || field.kind == b'm' {
            if let Some((_, msg)) = deferred.iter().find(|(slot_idx, _)| *slot_idx == idx) {
                return Ok((pos, Some(msg.clone())));
            }
        }
        spec_pos = nested_next_pos(spec, &field);
        idx += 1;
    }
    Ok((pos, None))
}

/// Fill `list` from the array opening at `pos` (the byte is `[`), one
/// element per `elem` spec, pushing each as it is built. The grammar is
/// the node parser's array branch: elements separated by commas, with no
/// trailing comma, and the closing bracket required. Returns the
/// position after the array and the first element error by position, if
/// any; like a field error it waits with the caller, because a malformed
/// byte anywhere in the document outranks it. Elements after a failed one
/// are still read, so the grammar check covers the whole array.
fn nested_fill_list(
    input: &[u8],
    pos: usize,
    elem: &[u8],
    path: &[u8],
    list: NestedList,
    dtors: &DtorTable<'_>,
    depth: usize,
) -> NestedFill {
    let mut first_err: Option<Vec<u8>> = None;
    let elem_depth = depth + 1;
    let mut pos = skip_ws(input, pos + 1);
    if pos < input.len() && input[pos] == b']' {
        return Ok((pos + 1, None));
    }
    let mut index = 0usize;
    loop {
        if pos >= input.len() {
            return Err(NestedAbort::Malformed);
        }
        if elem_depth > NESTED_MAX_DEPTH {
            return Err(NestedAbort::TooDeep);
        }
        let (end, elem_err) = nested_fill_element(input, pos, elem, path, index, list, dtors, elem_depth)?;
        if first_err.is_none() {
            first_err = elem_err;
        }
        match nested_after_value(input, end, b']')? {
            NestedSep::More(next) => pos = next,
            NestedSep::Closed(after) => return Ok((after, first_err)),
        }
        index += 1;
    }
}

/// Read one array element at `pos` (not whitespace, not the end) by the
/// element spec and push it into `list`. A scalar of the right kind is
/// pushed by value or as a fresh string the list owns; a struct element is
/// allocated with the destructor its `#<k>` names, pushed, and then filled
/// in place, so the list owns it before its first field is read; a list
/// element is built the same way through `nested_fill_list`. An element of
/// the wrong kind is consumed with the parser's grammar and reported as
/// `expected <kind> element: <path>[<index>]`; a struct element's own
/// field error comes back as `<path>[<index>]: <its message>`.
#[allow(clippy::too_many_arguments)]
fn nested_fill_element(
    input: &[u8],
    pos: usize,
    elem: &[u8],
    path: &[u8],
    index: usize,
    list: NestedList,
    dtors: &DtorTable<'_>,
    depth: usize,
) -> NestedFill {
    let b = input[pos];
    let field = nested_field_at(elem, 0);
    match field.kind {
        b's' if b == b'"' => {
            let (value, end) = nested_read_string(input, pos)?;
            let s = crate::pith_copy_bytes_to_cstring(&value) as i64;
            list.push_owned(s);
            Ok((end, None))
        }
        b'i' if nested_is_number_start(b) => {
            let (value, end) = nested_read_number(input, pos)?;
            match value {
                Some(value) => {
                    list.push_value(value);
                    Ok((end, None))
                }
                None => Ok((end, Some(nested_element_error(b'i', path, index)))),
            }
        }
        b'f' if nested_is_number_start(b) => {
            let end = number_span_end(input, pos + 1);
            let Some(value) = float_of_span(&input[pos..end]) else {
                return Err(NestedAbort::Malformed);
            };
            list.push_value(value.to_bits() as i64);
            Ok((end, None))
        }
        b'b' if input[pos..].starts_with(b"true") => {
            list.push_value(1);
            Ok((pos + 4, None))
        }
        b'b' if input[pos..].starts_with(b"false") => {
            list.push_value(0);
            Ok((pos + 5, None))
        }
        b'o' if b == b'{' => {
            let sub_spec = &elem[field.sub_start..field.sub_end];
            let Some(dtor) = dtors.get(field.dtor) else {
                return Err(NestedAbort::Malformed);
            };
            let Some(sub) = dtor.alloc_object(sub_spec) else {
                return Err(NestedAbort::Malformed);
            };
            // the list takes the one count the allocation minted, before
            // any field of the element is read
            list.push_owned(sub.as_handle());
            let (end, sub_err) = nested_fill_object(input, pos, sub, dtors, depth)?;
            Ok((end, sub_err.map(|msg| nested_element_prefixed(path, index, &msg))))
        }
        b'l' if b == b'[' => {
            let inner_elem = &elem[field.sub_start..field.sub_end];
            let Some(inner) = NestedList::new(inner_elem) else {
                return Err(NestedAbort::Malformed);
            };
            list.push_owned(inner.as_handle());
            let mut inner_path = path.to_vec();
            nested_push_index(&mut inner_path, index);
            nested_fill_list(input, pos, inner_elem, &inner_path, inner, dtors, depth)
        }
        b'm' if b == b'{' => {
            let inner_elem = &elem[field.sub_start..field.sub_end];
            let Some(inner) = NestedMap::new(inner_elem) else {
                return Err(NestedAbort::Malformed);
            };
            list.push_owned(inner.as_handle());
            let mut inner_path = path.to_vec();
            nested_push_index(&mut inner_path, index);
            nested_fill_map(input, pos, inner_elem, &inner_path, inner, dtors, depth)
        }
        kind => {
            let end = nested_skip_value(input, pos, depth)?;
            Ok((end, Some(nested_element_error(kind, path, index))))
        }
    }
}

/// Fill `map` from the object opening at `pos` (the byte is `{`), one
/// member per key, each value by the `elem` spec, inserting each as it is
/// built. The grammar is the node parser's object branch, as in
/// `nested_fill_object`; every key is kept, none is looked up in a spec. A
/// key that repeats has its last value win and the map releases the
/// earlier one at the insert, as the node accessors read the last entry.
/// Returns the position after the object and the first member error in
/// document order, if any, for a key whose last value carried it; like an
/// element error it waits with the caller.
fn nested_fill_map(
    input: &[u8],
    pos: usize,
    elem: &[u8],
    path: &[u8],
    map: NestedMap,
    dtors: &DtorTable<'_>,
    depth: usize,
) -> NestedFill {
    let mut deferred: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let member_depth = depth + 1;
    let mut pos = skip_ws(input, pos + 1);
    let mut done = pos < input.len() && input[pos] == b'}';
    if done {
        pos += 1;
    }
    while !done {
        if input.get(pos) != Some(&b'"') {
            return Err(NestedAbort::Malformed);
        }
        let (key, after_key) = nested_read_string(input, pos)?;
        pos = nested_expect_colon(input, after_key)?;
        if member_depth > NESTED_MAX_DEPTH {
            return Err(NestedAbort::TooDeep);
        }
        pos = skip_ws(input, pos);
        if pos >= input.len() {
            return Err(NestedAbort::Malformed);
        }
        let (end, member_err) = nested_fill_member(input, pos, elem, path, &key, map, dtors, member_depth)?;
        // the last value under a key decides its error, as it decides its value
        deferred.retain(|(k, _)| k.as_slice() != key.as_ref());
        if let Some(msg) = member_err {
            deferred.push((key.into_owned(), msg));
        }
        pos = skip_ws(input, end);
        match input.get(pos) {
            Some(&b',') => pos = skip_ws(input, pos + 1),
            Some(&b'}') => {
                pos += 1;
                done = true;
            }
            _ => return Err(NestedAbort::Malformed),
        }
    }
    Ok((pos, deferred.into_iter().next().map(|(_, msg)| msg)))
}

/// Read one map member's value at `pos` (not whitespace, not the end) by
/// the element spec and insert it under `key`. A scalar of the right kind
/// is inserted by value or as a fresh string the map owns; a struct is
/// allocated with the destructor its `#<k>` names, inserted, then filled
/// in place; a list or a map value is built the same way through its
/// filler. So the map owns every value before the value's first byte is
/// read, and a failure anywhere below cascades from the root. A value of
/// the wrong kind is consumed with the parser's grammar and reported as
/// `expected <kind> element: <path>.<key>`; a struct's own field error
/// comes back as `<path>.<key>: <its message>`.
#[allow(clippy::too_many_arguments)]
fn nested_fill_member(
    input: &[u8],
    pos: usize,
    elem: &[u8],
    path: &[u8],
    key: &[u8],
    map: NestedMap,
    dtors: &DtorTable<'_>,
    depth: usize,
) -> NestedFill {
    let b = input[pos];
    let field = nested_field_at(elem, 0);
    match field.kind {
        b's' if b == b'"' => {
            let (value, end) = nested_read_string(input, pos)?;
            let s = crate::pith_copy_bytes_to_cstring(&value) as i64;
            map.insert(elem, key, s);
            Ok((end, None))
        }
        b'i' if nested_is_number_start(b) => {
            let (value, end) = nested_read_number(input, pos)?;
            match value {
                Some(value) => {
                    map.insert(elem, key, value);
                    Ok((end, None))
                }
                None => Ok((end, Some(nested_kind_error_at(b'i', &nested_member_path(path, key))))),
            }
        }
        b'f' if nested_is_number_start(b) => {
            let end = number_span_end(input, pos + 1);
            let Some(value) = float_of_span(&input[pos..end]) else {
                return Err(NestedAbort::Malformed);
            };
            map.insert(elem, key, value.to_bits() as i64);
            Ok((end, None))
        }
        b'b' if input[pos..].starts_with(b"true") => {
            map.insert(elem, key, 1);
            Ok((pos + 4, None))
        }
        b'b' if input[pos..].starts_with(b"false") => {
            map.insert(elem, key, 0);
            Ok((pos + 5, None))
        }
        b'o' if b == b'{' => {
            let sub_spec = &elem[field.sub_start..field.sub_end];
            let Some(dtor) = dtors.get(field.dtor) else {
                return Err(NestedAbort::Malformed);
            };
            let Some(sub) = dtor.alloc_object(sub_spec) else {
                return Err(NestedAbort::Malformed);
            };
            // the map takes the one count the allocation minted, before
            // any field of the member is read
            map.insert(elem, key, sub.as_handle());
            let (end, sub_err) = nested_fill_object(input, pos, sub, dtors, depth)?;
            Ok((end, sub_err.map(|msg| nested_prefixed_at(&nested_member_path(path, key), &msg))))
        }
        b'l' if b == b'[' => {
            let inner_elem = &elem[field.sub_start..field.sub_end];
            let Some(inner) = NestedList::new(inner_elem) else {
                return Err(NestedAbort::Malformed);
            };
            map.insert(elem, key, inner.as_handle());
            nested_fill_list(input, pos, inner_elem, &nested_member_path(path, key), inner, dtors, depth)
        }
        b'm' if b == b'{' => {
            let inner_elem = &elem[field.sub_start..field.sub_end];
            let Some(inner) = NestedMap::new(inner_elem) else {
                return Err(NestedAbort::Malformed);
            };
            map.insert(elem, key, inner.as_handle());
            nested_fill_map(input, pos, inner_elem, &nested_member_path(path, key), inner, dtors, depth)
        }
        kind => {
            let end = nested_skip_value(input, pos, depth)?;
            Ok((end, Some(nested_kind_error_at(kind, &nested_member_path(path, key)))))
        }
    }
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

#[cfg(test)]
mod tests {
    use super::{
        float_of_span, nested_elem_tag, nested_element_error, nested_element_prefixed, nested_field_at,
        nested_fill_from_input, nested_kind_error_at, nested_member_path, nested_prefixed_at, nested_spec_field_count,
        pith_json_float_bits, DtorTable, NestedAbort, ObjectTarget, JSON_FLOAT_REFUSED, NESTED_TOO_DEEP,
    };

    #[test]
    fn spec_field_reads_a_list_entry_and_a_destructor_index() {
        let spec = b"stitle,lrows(o#12(iid,slabel)),icount";
        let title = nested_field_at(spec, 0);
        assert_eq!((title.kind, &spec[title.name_start..title.name_end], title.dtor), (b's', &b"title"[..], -1));
        let rows = nested_field_at(spec, title.end + 1);
        assert_eq!((rows.kind, &spec[rows.name_start..rows.name_end], rows.dtor), (b'l', &b"rows"[..], -1));
        let elem = &spec[rows.sub_start..rows.sub_end];
        assert_eq!(elem, b"o#12(iid,slabel)");
        let elem_field = nested_field_at(elem, 0);
        assert_eq!((elem_field.kind, elem_field.name_start, elem_field.name_end, elem_field.dtor), (b'o', 1, 1, 12));
        assert_eq!(&elem[elem_field.sub_start..elem_field.sub_end], b"iid,slabel");
        let count = nested_field_at(spec, rows.end + 1);
        assert_eq!((count.kind, &spec[count.name_start..count.name_end], count.end), (b'i', &b"count"[..], spec.len()));
    }

    #[test]
    fn spec_field_reads_a_struct_under_a_list_with_its_index() {
        // a struct field inside a list element carries `#<k>` after its name
        let spec = b"scode,ogeo#3(ilat,slabel),ltags(s)";
        let geo = nested_field_at(spec, 6);
        assert_eq!((geo.kind, &spec[geo.name_start..geo.name_end], geo.dtor), (b'o', &b"geo"[..], 3));
        assert_eq!(&spec[geo.sub_start..geo.sub_end], b"ilat,slabel");
        let tags = nested_field_at(spec, geo.end + 1);
        assert_eq!((tags.kind, &spec[tags.sub_start..tags.sub_end]), (b'l', &b"s"[..]));
        // a list of lists nests the element form
        let grid = nested_field_at(b"lcells(l(i))", 0);
        assert_eq!(&b"lcells(l(i))"[grid.sub_start..grid.sub_end], b"l(i)");
    }

    #[test]
    fn spec_field_reads_a_map_entry_and_a_map_element() {
        let spec = b"stitle,mscores(i),mrows(o#0(iid,slabel)),lgroups(m(i)),mnested(m(s))";
        let scores = nested_field_at(spec, 7);
        assert_eq!((scores.kind, &spec[scores.name_start..scores.name_end], &spec[scores.sub_start..scores.sub_end]), (b'm', &b"scores"[..], &b"i"[..]));
        let rows = nested_field_at(spec, scores.end + 1);
        assert_eq!((rows.kind, &spec[rows.sub_start..rows.sub_end]), (b'm', &b"o#0(iid,slabel)"[..]));
        let groups = nested_field_at(spec, rows.end + 1);
        assert_eq!((groups.kind, &spec[groups.sub_start..groups.sub_end]), (b'l', &b"m(i)"[..]));
        let elem = nested_field_at(&spec[groups.sub_start..groups.sub_end], 0);
        assert_eq!((elem.kind, elem.name_start, elem.name_end), (b'm', 1, 1));
        let nested = nested_field_at(spec, groups.end + 1);
        assert_eq!((nested.kind, &spec[nested.sub_start..nested.sub_end], nested.end), (b'm', &b"m(s)"[..], spec.len()));
        assert_eq!(nested_spec_field_count(spec), 5);
    }

    #[test]
    fn map_member_paths_and_kind_words() {
        assert_eq!(nested_member_path(b"scores", b"bob"), b"scores.bob".to_vec());
        assert_eq!(nested_member_path(b"rows[1]", b"with space"), b"rows[1].with space".to_vec());
        assert_eq!(nested_kind_error_at(b'i', b"scores.bob"), b"expected int element: scores.bob".to_vec());
        assert_eq!(nested_kind_error_at(b'm', b"groups[1]"), b"expected map element: groups[1]".to_vec());
        assert_eq!(nested_prefixed_at(b"rows.b", b"missing string field: label"), b"rows.b: missing string field: label".to_vec());
        assert!(nested_elem_tag(b"m(i)") == crate::collections::list::ListTypeTag::Map);
        assert!(nested_elem_tag(b"o#2(iid)") == crate::collections::list::ListTypeTag::Struct);
        assert!(nested_elem_tag(b"l(s)") == crate::collections::list::ListTypeTag::List);
        assert!(nested_elem_tag(b"s") == crate::collections::list::ListTypeTag::String);
        assert!(nested_elem_tag(b"f") == crate::collections::list::ListTypeTag::Primitive);
        assert_eq!(NestedAbort::TooDeep.message(), NESTED_TOO_DEEP);
        assert_eq!(NestedAbort::Malformed.message(), b"invalid json object");
    }

    #[test]
    fn spec_field_count_is_the_top_level_separators_plus_one() {
        assert_eq!(nested_spec_field_count(b""), 0);
        assert_eq!(nested_spec_field_count(b"iid"), 1);
        assert_eq!(nested_spec_field_count(b"iid,slabel"), 2);
        assert_eq!(nested_spec_field_count(b"scode,ogeo#3(ilat,slabel),ltags(s)"), 3);
        assert_eq!(nested_spec_field_count(b"lrows(o#0(iid,slabel))"), 1);
    }

    #[test]
    fn element_errors_name_the_position_under_the_field() {
        assert_eq!(nested_element_error(b'i', b"xs", 2), b"expected int element: xs[2]".to_vec());
        assert_eq!(nested_element_error(b'o', b"rows", 0), b"expected object element: rows[0]".to_vec());
        assert_eq!(nested_element_prefixed(b"rows", 1, b"missing string field: name"), b"rows[1]: missing string field: name".to_vec());
        assert_eq!(nested_element_error(b'i', b"grid[1]", 3), b"expected int element: grid[1][3]".to_vec());
    }

    #[test]
    fn float_span_takes_every_json_number_form() {
        assert_eq!(float_of_span(b"1.5"), Some(1.5));
        assert_eq!(float_of_span(b"-2.25"), Some(-2.25));
        assert_eq!(float_of_span(b"1e10"), Some(1e10));
        assert_eq!(float_of_span(b"2.5E-3"), Some(2.5e-3));
        assert_eq!(float_of_span(b"3"), Some(3.0));
        assert_eq!(float_of_span(b"-7"), Some(-7.0));
        assert_eq!(float_of_span(b"1.7976931348623157e308"), Some(f64::MAX));
        assert_eq!(float_of_span(b"5e-324"), Some(f64::from_bits(1)));
    }

    #[test]
    fn negative_zero_keeps_its_sign() {
        let value = float_of_span(b"-0.0").unwrap();
        assert_eq!(value, 0.0);
        assert!(value.is_sign_negative());
    }

    #[test]
    fn float_span_refuses_what_is_not_a_finite_number() {
        assert_eq!(float_of_span(b"1e309"), None);
        assert_eq!(float_of_span(b"-1e309"), None);
        assert_eq!(float_of_span(b"1-2"), None);
        assert_eq!(float_of_span(b"1e"), None);
        assert_eq!(float_of_span(b"1.2.3"), None);
        assert_eq!(float_of_span(b"-"), None);
        assert_eq!(float_of_span(b""), None);
    }

    #[test]
    fn float_bits_converts_a_span_or_refuses_it() {
        let doc = b"x:-0.0,1e309,2.5";
        let ptr = doc.as_ptr() as i64;
        unsafe {
            assert_eq!(pith_json_float_bits(ptr, 2, 6), (-0.0f64).to_bits() as i64);
            assert_eq!(pith_json_float_bits(ptr, 7, 12), JSON_FLOAT_REFUSED);
            assert_eq!(pith_json_float_bits(ptr, 13, 16), 2.5f64.to_bits() as i64);
            assert_eq!(pith_json_float_bits(ptr, 13, 13), JSON_FLOAT_REFUSED);
            assert_eq!(pith_json_float_bits(0, 0, 1), JSON_FLOAT_REFUSED);
        }
        assert!(f64::from_bits(JSON_FLOAT_REFUSED as u64).is_nan());
    }

    #[test]
    fn nested_decode_handles_structs_with_more_than_64_fields() {
        // 70 integer fields: previously `1u64 << idx` wrapped/panicked past 63.
        let mut spec = Vec::new();
        let mut doc = Vec::from(&b"{"[..]);
        for i in 0..70usize {
            if i > 0 {
                spec.push(b',');
                doc.push(b',');
            }
            spec.extend_from_slice(format!("if{i}").as_bytes());
            doc.extend_from_slice(format!("\"f{i}\":{i}").as_bytes());
        }
        doc.push(b'}');
        let dtors = DtorTable(&[]);
        unsafe {
            let struct_ptr = crate::runtime_core::pith_struct_alloc(70);
            assert_ne!(struct_ptr, 0);
            let target = ObjectTarget::from_raw(&spec, struct_ptr);
            assert_eq!(nested_fill_from_input(&doc, target, false, &dtors), Ok(()));
            let base = struct_ptr as *const i64;
            for i in 0..70usize {
                assert_eq!(*base.add(i), i as i64);
            }
            // Missing field #69 must be reported, not aliased onto field #5.
            let mut missing_doc = Vec::from(&b"{"[..]);
            for i in 0..69usize {
                if i > 0 {
                    missing_doc.push(b',');
                }
                missing_doc.extend_from_slice(format!("\"f{i}\":{i}").as_bytes());
            }
            missing_doc.push(b'}');
            let target2 = ObjectTarget::from_raw(&spec, struct_ptr);
            let err = nested_fill_from_input(&missing_doc, target2, false, &dtors).unwrap_err();
            assert_eq!(err, b"missing int field: f69");
            crate::pith_struct_release(struct_ptr);
        }
    }
}
