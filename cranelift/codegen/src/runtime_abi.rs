use std::sync::OnceLock;

struct RuntimeAbi {
    list_magic: u32,
    elem_size_offset: i32,
    values8_ptr_offset: i32,
    values8_len_offset: i32,
}

static RUNTIME_ABI: OnceLock<RuntimeAbi> = OnceLock::new();

fn parse_int_field(contents: &str, key: &str) -> i32 {
    let needle = format!("\"{}\":", key);
    let start = contents.find(&needle).unwrap_or_else(|| panic!("missing abi key {}", key));
    let value = &contents[start + needle.len()..];
    let trimmed = value.trim_start();
    let end = trimmed
        .find(|ch: char| !(ch.is_ascii_digit() || ch == '-'))
        .unwrap_or(trimmed.len());
    trimmed[..end]
        .parse::<i32>()
        .unwrap_or_else(|_| panic!("invalid abi int for {}", key))
}

fn parse_hex_u32_field(contents: &str, key: &str) -> u32 {
    let needle = format!("\"{}\":", key);
    let start = contents.find(&needle).unwrap_or_else(|| panic!("missing abi key {}", key));
    let value = &contents[start + needle.len()..];
    let trimmed = value.trim_start().trim_start_matches('"');
    let end = trimmed
        .find(|ch: char| !(ch.is_ascii_hexdigit() || ch == 'x' || ch == 'X'))
        .unwrap_or(trimmed.len());
    let raw = trimmed[..end].trim_start_matches("0x").trim_start_matches("0X");
    u32::from_str_radix(raw, 16).unwrap_or_else(|_| panic!("invalid abi hex for {}", key))
}

fn load_runtime_abi() -> RuntimeAbi {
    let contents = include_str!("../../runtime-abi/list_layout.json");
    RuntimeAbi {
        list_magic: parse_hex_u32_field(contents, "list_magic"),
        elem_size_offset: parse_int_field(contents, "elem_size_offset"),
        values8_ptr_offset: parse_int_field(contents, "values8_ptr_offset"),
        values8_len_offset: parse_int_field(contents, "values8_len_offset"),
    }
}

fn runtime_abi() -> &'static RuntimeAbi {
    RUNTIME_ABI.get_or_init(load_runtime_abi)
}

pub fn list_magic() -> u32 {
    runtime_abi().list_magic
}

pub fn list_elem_size_offset() -> i32 {
    runtime_abi().elem_size_offset
}

pub fn list_values8_ptr_offset() -> i32 {
    runtime_abi().values8_ptr_offset
}

pub fn list_values8_len_offset() -> i32 {
    runtime_abi().values8_len_offset
}
