//! Single-pass JSON token scanner, UTF-8/escape decoder, and value skipper.

use super::spec::{NestedAbort, NESTED_MAX_DEPTH};

pub(super) fn skip_ws(input: &[u8], mut pos: usize) -> usize {
    while pos < input.len() && matches!(input[pos], b' ' | b'\t' | b'\n' | b'\r') {
        pos += 1;
    }
    pos
}

/// The end of the number span at `pos`: the maximal run of the bytes a
/// json number is written with. The span is the unit the filler hands to
/// the float conversion and the unit a number is read by.
#[inline(always)]
pub(super) fn number_span_end(input: &[u8], pos: usize) -> usize {
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
pub(super) fn float_of_span(span: &[u8]) -> Option<f64> {
    let text = std::str::from_utf8(span).ok()?;
    let value: f64 = text.parse().ok()?;
    if value.is_finite() {
        Some(value)
    } else {
        None
    }
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
pub(super) fn nested_read_string(input: &[u8], pos: usize) -> Result<(std::borrow::Cow<'_, [u8]>, usize), ()> {
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
pub(super) fn nested_is_number_start(b: u8) -> bool {
    b.is_ascii_digit() || b == b'-' || b == b'+'
}

#[inline(always)]
fn nested_is_number_char(b: u8) -> bool {
    b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-')
}

/// std.json's number branch with parse_integer_manual: the span of number
/// characters at `pos`; `Ok(None)` when it holds a `.`, `e` or `E` and so
/// parses as a float, `Err` when the integer is outside i64, as json.parse
/// refuses it. Signs and stray number characters inside the span are
/// skipped the way the parser skipped them, so `1-2` reads as 12 and `-`
/// alone as 0. The digits are accumulated as a negative number, since
/// i64::MIN has no positive counterpart.
pub(super) fn nested_read_number(input: &[u8], pos: usize) -> Result<(Option<i64>, usize), ()> {
    let i = number_span_end(input, pos + 1);
    if input[pos + 1..i].iter().any(|b| matches!(b, b'.' | b'e' | b'E')) {
        return Ok((None, i));
    }
    let span = &input[pos..i];
    let neg = span[0] == b'-';
    let mut value: i64 = 0;
    for &b in span {
        if b.is_ascii_digit() {
            value = value.checked_mul(10).and_then(|v| v.checked_sub((b - b'0') as i64)).ok_or(())?;
        }
    }
    let value = if neg { value } else { value.checked_neg().ok_or(())? };
    Ok((Some(value), i))
}

/// std.json's parse_value for a value nobody keeps: consumed with the
/// parser's grammar so a malformed value under an unknown key fails the
/// decode the way it always has. `depth` is the value's nesting level.
pub(super) fn nested_skip_value(input: &[u8], pos: usize, depth: usize) -> Result<usize, NestedAbort> {
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
pub(super) enum NestedSep {
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
pub(super) fn nested_after_value(input: &[u8], pos: usize, close: u8) -> Result<NestedSep, NestedAbort> {
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
pub(super) fn nested_expect_colon(input: &[u8], pos: usize) -> Result<usize, NestedAbort> {
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
