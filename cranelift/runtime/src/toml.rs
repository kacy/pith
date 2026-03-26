//! Minimal TOML parser for Forge runtime
//!
//! Supports: bare keys, strings, integers, floats, booleans, arrays, tables.
//! Uses arena allocation like the JSON module.

use std::sync::Mutex;

static TOML_ARENA: std::sync::OnceLock<Mutex<Vec<TomlValue>>> = std::sync::OnceLock::new();

fn arena() -> &'static Mutex<Vec<TomlValue>> {
    TOML_ARENA.get_or_init(|| Mutex::new(Vec::new()))
}

fn alloc_node(val: TomlValue) -> i64 {
    let mut a = arena().lock().unwrap();
    let idx = a.len();
    a.push(val);
    (idx as i64) + 1 // 1-based
}

fn get_node(handle: i64) -> Option<TomlValue> {
    if handle <= 0 { return None; }
    let a = arena().lock().unwrap();
    a.get((handle - 1) as usize).cloned()
}

#[derive(Clone, Debug)]
pub enum TomlValue {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Array(Vec<i64>),
    Table(Vec<(String, i64)>),
}

// ---- Parser ----

pub fn parse_toml(input: &str) -> i64 {
    let mut root_entries: Vec<(String, i64)> = Vec::new();
    let mut current_table: Option<String> = None;
    let mut tables: std::collections::HashMap<String, Vec<(String, i64)>> = std::collections::HashMap::new();

    for line in input.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Table header: [name]
        if line.starts_with('[') && line.ends_with(']') && !line.starts_with("[[") {
            let name = line[1..line.len()-1].trim().to_string();
            current_table = Some(name.clone());
            tables.entry(name).or_insert_with(Vec::new);
            continue;
        }
        // Key = Value
        if let Some(eq_pos) = line.find('=') {
            let key = line[..eq_pos].trim().to_string();
            let val_str = line[eq_pos+1..].trim();
            let val_handle = parse_value(val_str);
            if let Some(ref table_name) = current_table {
                tables.entry(table_name.clone()).or_insert_with(Vec::new).push((key, val_handle));
            } else {
                root_entries.push((key, val_handle));
            }
        }
    }

    // Add sub-tables to root
    for (name, entries) in &tables {
        let table_handle = alloc_node(TomlValue::Table(entries.clone()));
        root_entries.push((name.clone(), table_handle));
    }

    alloc_node(TomlValue::Table(root_entries))
}

fn parse_value(s: &str) -> i64 {
    let s = s.trim();
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        let inner = &s[1..s.len()-1];
        alloc_node(TomlValue::String(inner.to_string()))
    } else if s == "true" {
        alloc_node(TomlValue::Bool(true))
    } else if s == "false" {
        alloc_node(TomlValue::Bool(false))
    } else if s.starts_with('[') && s.ends_with(']') {
        parse_array(&s[1..s.len()-1])
    } else if s.contains('.') {
        if let Ok(f) = s.parse::<f64>() {
            alloc_node(TomlValue::Float(f))
        } else {
            alloc_node(TomlValue::String(s.to_string()))
        }
    } else if let Ok(n) = s.parse::<i64>() {
        alloc_node(TomlValue::Int(n))
    } else {
        alloc_node(TomlValue::String(s.to_string()))
    }
}

fn parse_array(s: &str) -> i64 {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    let mut depth = 0;

    for c in s.chars() {
        match c {
            '"' => { in_string = !in_string; current.push(c); }
            '[' if !in_string => { depth += 1; current.push(c); }
            ']' if !in_string => { depth -= 1; current.push(c); }
            ',' if !in_string && depth == 0 => {
                let trimmed = current.trim();
                if !trimmed.is_empty() {
                    items.push(parse_value(trimmed));
                }
                current.clear();
            }
            _ => current.push(c),
        }
    }
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        items.push(parse_value(trimmed));
    }
    alloc_node(TomlValue::Array(items))
}

// ---- FFI ----

use crate::ffi_util::{alloc_cstring, cstr_to_str};

#[no_mangle]
pub unsafe extern "C" fn forge_toml_parse(s: *const i8) -> i64 {
    parse_toml(cstr_to_str(s))
}

/// Resolve a handle: if negative (from universal parse), negate to get TOML handle
fn resolve_handle(h: i64) -> i64 {
    if h < 0 { -h } else { h }
}

#[no_mangle]
pub unsafe extern "C" fn forge_toml_type_of(handle: i64) -> *mut i8 {
    let ty = match get_node(resolve_handle(handle)) {
        Some(TomlValue::String(_)) => "string",
        Some(TomlValue::Int(_)) => "int",
        Some(TomlValue::Float(_)) => "float",
        Some(TomlValue::Bool(_)) => "bool",
        Some(TomlValue::Array(_)) => "array",
        Some(TomlValue::Table(_)) => "table",
        None => "null",
    };
    alloc_cstring(ty)
}

#[no_mangle]
pub unsafe extern "C" fn forge_toml_get_string(handle: i64, key: *const i8) -> *mut i8 {
    let k = cstr_to_str(key);
    if let Some(TomlValue::Table(entries)) = get_node(resolve_handle(handle)) {
        for (ek, ev) in &entries {
            if ek == k {
                if let Some(TomlValue::String(s)) = get_node(*ev) {
                    return alloc_cstring(&s);
                }
            }
        }
    }
    alloc_cstring("")
}

#[no_mangle]
pub unsafe extern "C" fn forge_toml_get_int(handle: i64, key: *const i8) -> i64 {
    let k = cstr_to_str(key);
    if let Some(TomlValue::Table(entries)) = get_node(resolve_handle(handle)) {
        for (ek, ev) in &entries {
            if ek == k {
                if let Some(TomlValue::Int(n)) = get_node(*ev) {
                    return n;
                }
            }
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn forge_toml_get_float(handle: i64, key: *const i8) -> f64 {
    let k = cstr_to_str(key);
    if let Some(TomlValue::Table(entries)) = get_node(resolve_handle(handle)) {
        for (ek, ev) in &entries {
            if ek == k {
                if let Some(TomlValue::Float(f)) = get_node(*ev) {
                    return f;
                }
            }
        }
    }
    0.0
}

#[no_mangle]
pub unsafe extern "C" fn forge_toml_get_bool(handle: i64, key: *const i8) -> i64 {
    let k = cstr_to_str(key);
    if let Some(TomlValue::Table(entries)) = get_node(resolve_handle(handle)) {
        for (ek, ev) in &entries {
            if ek == k {
                if let Some(TomlValue::Bool(b)) = get_node(*ev) {
                    return if b { 1 } else { 0 };
                }
            }
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn forge_toml_has(handle: i64, key: *const i8) -> i64 {
    let k = cstr_to_str(key);
    if let Some(TomlValue::Table(entries)) = get_node(resolve_handle(handle)) {
        if entries.iter().any(|(ek, _)| ek == k) { 1 } else { 0 }
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_toml_get_array(handle: i64, key: *const i8) -> i64 {
    let k = cstr_to_str(key);
    if let Some(TomlValue::Table(entries)) = get_node(resolve_handle(handle)) {
        for (ek, ev) in &entries {
            if ek == k {
                return -(*ev); // Negate to tag as TOML handle
            }
        }
    }
    -1
}

#[no_mangle]
pub unsafe extern "C" fn forge_toml_array_len(handle: i64) -> i64 {
    match get_node(resolve_handle(handle)) {
        Some(TomlValue::Array(items)) => items.len() as i64,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_toml_array_get(handle: i64, index: i64) -> i64 {
    match get_node(resolve_handle(handle)) {
        Some(TomlValue::Array(items)) => {
            items.get(index as usize).map(|h| -(*h)).unwrap_or(-1) // Negate to tag as TOML
        }
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_toml_get_table(handle: i64, key: *const i8) -> i64 {
    let k = cstr_to_str(key);
    if let Some(TomlValue::Table(entries)) = get_node(resolve_handle(handle)) {
        for (ek, ev) in &entries {
            if ek == k {
                return -(*ev); // Negate to tag as TOML handle
            }
        }
    }
    -1
}

#[no_mangle]
pub unsafe extern "C" fn forge_toml_keys(handle: i64) -> i64 {
    use crate::collections::list::{forge_list_new, forge_list_push_value};

    match get_node(resolve_handle(handle)) {
        Some(TomlValue::Table(entries)) => {
            let list = forge_list_new(8, 1);
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
