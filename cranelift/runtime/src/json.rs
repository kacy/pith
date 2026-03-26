//! Minimal JSON parser and builder for Forge runtime
//!
//! Uses arena-style allocation: all JSON nodes live in a global Vec,
//! and handles are indices into this Vec cast to i64.

use std::collections::HashMap;
use std::sync::Mutex;

static JSON_ARENA: std::sync::OnceLock<Mutex<Vec<JsonValue>>> = std::sync::OnceLock::new();

fn arena() -> &'static Mutex<Vec<JsonValue>> {
    JSON_ARENA.get_or_init(|| Mutex::new(Vec::new()))
}

fn alloc_node(val: JsonValue) -> i64 {
    let mut a = arena().lock().unwrap();
    let idx = a.len();
    a.push(val);
    (idx as i64) + 1 // 1-based so 0 means null/error
}

fn get_node(handle: i64) -> Option<JsonValue> {
    if handle <= 0 {
        return None;
    }
    let a = arena().lock().unwrap();
    a.get((handle - 1) as usize).cloned()
}

#[derive(Clone, Debug)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Array(Vec<i64>),           // handles
    Object(Vec<(String, i64)>), // ordered key-value pairs (handles)
}

// ---- Parser ----

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self { input, pos: 0 }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.input.len() && self.input[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        let b = self.input.get(self.pos).copied();
        self.pos += 1;
        b
    }

    fn expect(&mut self, b: u8) -> bool {
        self.skip_ws();
        if self.peek() == Some(b) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn parse_value(&mut self) -> Option<i64> {
        self.skip_ws();
        match self.peek()? {
            b'"' => self.parse_string(),
            b'{' => self.parse_object(),
            b'[' => self.parse_array(),
            b't' => self.parse_true(),
            b'f' => self.parse_false(),
            b'n' => self.parse_null(),
            _ => self.parse_number(),
        }
    }

    fn parse_string(&mut self) -> Option<i64> {
        self.advance(); // skip "
        let mut s = String::new();
        loop {
            let b = self.advance()?;
            if b == b'"' {
                break;
            }
            if b == b'\\' {
                match self.advance()? {
                    b'"' => s.push('"'),
                    b'\\' => s.push('\\'),
                    b'/' => s.push('/'),
                    b'n' => s.push('\n'),
                    b't' => s.push('\t'),
                    b'r' => s.push('\r'),
                    other => { s.push('\\'); s.push(other as char); }
                }
            } else {
                s.push(b as char);
            }
        }
        Some(alloc_node(JsonValue::Str(s)))
    }

    fn parse_number(&mut self) -> Option<i64> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.advance();
        }
        while self.pos < self.input.len() && self.input[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        let mut is_float = false;
        if self.pos < self.input.len() && self.input[self.pos] == b'.' {
            is_float = true;
            self.pos += 1;
            while self.pos < self.input.len() && self.input[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
        }
        if self.pos < self.input.len() && (self.input[self.pos] == b'e' || self.input[self.pos] == b'E') {
            is_float = true;
            self.pos += 1;
            if self.pos < self.input.len() && (self.input[self.pos] == b'+' || self.input[self.pos] == b'-') {
                self.pos += 1;
            }
            while self.pos < self.input.len() && self.input[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
        }
        let s = std::str::from_utf8(&self.input[start..self.pos]).ok()?;
        if is_float {
            let f: f64 = s.parse().ok()?;
            Some(alloc_node(JsonValue::Float(f)))
        } else {
            let n: i64 = s.parse().ok()?;
            Some(alloc_node(JsonValue::Int(n)))
        }
    }

    fn parse_object(&mut self) -> Option<i64> {
        self.advance(); // skip {
        let mut entries = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.advance();
            return Some(alloc_node(JsonValue::Object(entries)));
        }
        loop {
            self.skip_ws();
            // Parse key
            if self.peek() != Some(b'"') {
                return None;
            }
            let key_handle = self.parse_string()?;
            let key = match get_node(key_handle) {
                Some(JsonValue::Str(s)) => s,
                _ => return None,
            };
            self.skip_ws();
            if !self.expect(b':') {
                return None;
            }
            let val = self.parse_value()?;
            entries.push((key, val));
            self.skip_ws();
            if self.peek() == Some(b',') {
                self.advance();
            } else {
                break;
            }
        }
        self.expect(b'}');
        Some(alloc_node(JsonValue::Object(entries)))
    }

    fn parse_array(&mut self) -> Option<i64> {
        self.advance(); // skip [
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.advance();
            return Some(alloc_node(JsonValue::Array(items)));
        }
        loop {
            let val = self.parse_value()?;
            items.push(val);
            self.skip_ws();
            if self.peek() == Some(b',') {
                self.advance();
            } else {
                break;
            }
        }
        self.expect(b']');
        Some(alloc_node(JsonValue::Array(items)))
    }

    fn parse_true(&mut self) -> Option<i64> {
        if self.input[self.pos..].starts_with(b"true") {
            self.pos += 4;
            Some(alloc_node(JsonValue::Bool(true)))
        } else {
            None
        }
    }

    fn parse_false(&mut self) -> Option<i64> {
        if self.input[self.pos..].starts_with(b"false") {
            self.pos += 5;
            Some(alloc_node(JsonValue::Bool(false)))
        } else {
            None
        }
    }

    fn parse_null(&mut self) -> Option<i64> {
        if self.input[self.pos..].starts_with(b"null") {
            self.pos += 4;
            Some(alloc_node(JsonValue::Null))
        } else {
            None
        }
    }
}

// ---- FFI functions ----

use crate::ffi_util::{alloc_cstring, cstr_to_str};

#[no_mangle]
pub unsafe extern "C" fn forge_json_parse_real(s: *const i8) -> i64 {
    let input = cstr_to_str(s);
    let mut parser = Parser::new(input.as_bytes());
    parser.parse_value().unwrap_or(-1)
}

#[no_mangle]
pub unsafe extern "C" fn forge_json_type_of(handle: i64) -> *mut i8 {
    // Negative handles are TOML
    if handle < -1 {
        return crate::toml::forge_toml_type_of(-handle);
    }
    let ty = match get_node(handle) {
        Some(JsonValue::Null) => "null",
        Some(JsonValue::Bool(_)) => "bool",
        Some(JsonValue::Int(_)) => "int",
        Some(JsonValue::Float(_)) => "float",
        Some(JsonValue::Str(_)) => "string",
        Some(JsonValue::Array(_)) => "array",
        Some(JsonValue::Object(_)) => "object",
        None => "null",
    };
    alloc_cstring(ty)
}

#[no_mangle]
pub unsafe extern "C" fn forge_json_get_string(handle: i64) -> *mut i8 {
    match get_node(handle) {
        Some(JsonValue::Str(s)) => alloc_cstring(&s),
        _ => alloc_cstring(""),
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_json_get_int(handle: i64) -> i64 {
    match get_node(handle) {
        Some(JsonValue::Int(n)) => n,
        Some(JsonValue::Float(f)) => f as i64,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_json_get_float(handle: i64) -> f64 {
    match get_node(handle) {
        Some(JsonValue::Float(f)) => f,
        Some(JsonValue::Int(n)) => n as f64,
        _ => 0.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_json_get_bool(handle: i64) -> i64 {
    match get_node(handle) {
        Some(JsonValue::Bool(b)) => if b { 1 } else { 0 },
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_json_array_len(handle: i64) -> i64 {
    // Negative handles are TOML
    if handle < -1 {
        return crate::toml::forge_toml_array_len(handle);
    }
    match get_node(handle) {
        Some(JsonValue::Array(items)) => items.len() as i64,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_json_array_get(handle: i64, index: i64) -> i64 {
    // Negative handles are TOML
    if handle < -1 {
        return crate::toml::forge_toml_array_get(handle, index);
    }
    match get_node(handle) {
        Some(JsonValue::Array(items)) => {
            items.get(index as usize).copied().unwrap_or(-1)
        }
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_json_object_get(handle: i64, key: *const i8) -> i64 {
    let k = cstr_to_str(key);
    match get_node(handle) {
        Some(JsonValue::Object(entries)) => {
            for (ek, ev) in &entries {
                if ek == k {
                    return *ev;
                }
            }
            -1
        }
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_json_object_has(handle: i64, key: *const i8) -> i64 {
    let k = cstr_to_str(key);
    match get_node(handle) {
        Some(JsonValue::Object(entries)) => {
            if entries.iter().any(|(ek, _)| ek == k) { 1 } else { 0 }
        }
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_json_object_keys(handle: i64) -> i64 {
    use crate::collections::list::{forge_list_new, forge_list_push_value};

    match get_node(handle) {
        Some(JsonValue::Object(entries)) => {
            let list = forge_list_new(8, 1); // string list
            for (key, _) in &entries {
                let cstr = alloc_cstring(key);
                forge_list_push_value(list, cstr as i64);
            }
            list.ptr as i64
        }
        _ => {
            let list = forge_list_new(8, 1);
            list.ptr as i64
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_json_make_object() -> i64 {
    alloc_node(JsonValue::Object(Vec::new()))
}

#[no_mangle]
pub unsafe extern "C" fn forge_json_make_array() -> i64 {
    alloc_node(JsonValue::Array(Vec::new()))
}

#[no_mangle]
pub unsafe extern "C" fn forge_json_make_int(n: i64) -> i64 {
    alloc_node(JsonValue::Int(n))
}

#[no_mangle]
pub unsafe extern "C" fn forge_json_make_string(s: *const i8) -> i64 {
    let val = cstr_to_str(s).to_string();
    alloc_node(JsonValue::Str(val))
}

#[no_mangle]
pub unsafe extern "C" fn forge_json_array_push(arr_handle: i64, val_handle: i64) {
    let mut a = arena().lock().unwrap();
    if arr_handle <= 0 {
        return;
    }
    let idx = (arr_handle - 1) as usize;
    if let Some(JsonValue::Array(ref mut items)) = a.get_mut(idx) {
        items.push(val_handle);
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_json_object_set(obj_handle: i64, key: *const i8, val_handle: i64) {
    let k = cstr_to_str(key).to_string();
    let mut a = arena().lock().unwrap();
    if obj_handle <= 0 {
        return;
    }
    let idx = (obj_handle - 1) as usize;
    if let Some(JsonValue::Object(ref mut entries)) = a.get_mut(idx) {
        // Update existing key or append
        for entry in entries.iter_mut() {
            if entry.0 == k {
                entry.1 = val_handle;
                return;
            }
        }
        entries.push((k, val_handle));
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_json_encode(handle: i64) -> *mut i8 {
    let mut buf = String::new();
    encode_value(handle, &mut buf);
    alloc_cstring(&buf)
}

fn encode_value(handle: i64, buf: &mut String) {
    match get_node(handle) {
        Some(JsonValue::Null) => buf.push_str("null"),
        Some(JsonValue::Bool(b)) => buf.push_str(if b { "true" } else { "false" }),
        Some(JsonValue::Int(n)) => buf.push_str(&n.to_string()),
        Some(JsonValue::Float(f)) => {
            if f == f.floor() && f.abs() < 1e15 {
                buf.push_str(&format!("{:.1}", f));
            } else {
                buf.push_str(&f.to_string());
            }
        }
        Some(JsonValue::Str(s)) => {
            buf.push('"');
            for c in s.chars() {
                match c {
                    '"' => buf.push_str("\\\""),
                    '\\' => buf.push_str("\\\\"),
                    '\n' => buf.push_str("\\n"),
                    '\t' => buf.push_str("\\t"),
                    '\r' => buf.push_str("\\r"),
                    _ => buf.push(c),
                }
            }
            buf.push('"');
        }
        Some(JsonValue::Array(items)) => {
            buf.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    buf.push(',');
                }
                encode_value(*item, buf);
            }
            buf.push(']');
        }
        Some(JsonValue::Object(entries)) => {
            buf.push('{');
            for (i, (key, val)) in entries.iter().enumerate() {
                if i > 0 {
                    buf.push(',');
                }
                buf.push('"');
                buf.push_str(key);
                buf.push('"');
                buf.push(':');
                encode_value(*val, buf);
            }
            buf.push('}');
        }
        None => buf.push_str("null"),
    }
}
