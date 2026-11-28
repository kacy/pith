//! Text IR parsing, literal decoding, operand validation, and ABI helpers.

use crate::CompileError;
use cranelift::prelude::*;
use std::collections::HashMap;

/// The text of a `string ID "..."` line: what lies between its first quote
/// and the closing one, with the IR's escapes decoded. `{{` and `}}` stand
/// for a literal brace, and so do `\{` and `\}`; `\n`, `\t`, `\r`, `\0`, `\\`
/// and `\"` for their characters. the lexer reports any other escape, so a
/// backslash left over here is kept as written.
pub(super) fn decode_string_literal(line: &str) -> String {
    let rest = &line[line.find('"').unwrap_or(0)..];
    // strip exactly one quote at each end; trim_matches would also eat the
    // quote of a trailing `\"`
    let raw = if rest.len() >= 2 && rest.starts_with('"') && rest.ends_with('"') {
        &rest[1..rest.len() - 1]
    } else {
        rest
    };
    // accumulated as bytes, not chars: `byte as char` maps a byte to the
    // code point of the same value, so the two bytes of a utf8 character
    // each got re-encoded and "café" came out as "cafÃ©". the escapes are
    // all ascii, and `raw` is valid utf8, so the assembled bytes are too.
    let bytes = raw.as_bytes();
    let mut content: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut j = 0;
    while j < bytes.len() {
        let next = bytes.get(j + 1).copied();
        let decoded = match (bytes[j], next) {
            (b'{', Some(b'{')) => Some(b'{'),
            (b'}', Some(b'}')) => Some(b'}'),
            (b'\\', Some(b'n')) => Some(b'\n'),
            (b'\\', Some(b't')) => Some(b'\t'),
            (b'\\', Some(b'\\')) => Some(b'\\'),
            (b'\\', Some(b'"')) => Some(b'"'),
            (b'\\', Some(b'r')) => Some(b'\r'),
            (b'\\', Some(b'0')) => Some(b'\0'),
            (b'\\', Some(b'{')) => Some(b'{'),
            (b'\\', Some(b'}')) => Some(b'}'),
            _ => None,
        };
        match decoded {
            Some(byte) => {
                content.push(byte);
                j += 2;
            }
            None => {
                content.push(bytes[j]);
                j += 1;
            }
        }
    }
    String::from_utf8(content)
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// One `func ... endfunc` block of the IR: its name, its declared return
/// kind, its parameter names in order, and its other lines.
pub(super) struct IrFunction<'a> {
    pub(super) name: &'a str,
    pub(super) retkind: &'a str,
    pub(super) params: Vec<String>,
    /// what each param line says about the parameter's kind: "float",
    /// "word" (never a Float), or "" when the emitter did not know
    pub(super) param_kinds: Vec<String>,
    pub(super) body: Vec<&'a str>,
}

/// Every function block in the stream, in order. A name defined twice (two
/// modules emitting the same helper) appears twice; the first definition is
/// the one compiled.
pub(super) fn ir_functions<'a>(lines: &[&'a str]) -> Vec<IrFunction<'a>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let mut header = lines[i].split_whitespace();
        i += 1;
        if header.next() != Some("func") {
            continue;
        }
        let Some(name) = header.next() else {
            continue;
        };
        let retkind = header.nth(1).unwrap_or("");
        let mut params: Vec<String> = Vec::new();
        let mut param_kinds: Vec<String> = Vec::new();
        let mut body: Vec<&'a str> = Vec::new();
        while i < lines.len() {
            let mut parts = lines[i].split_whitespace();
            match (parts.next(), parts.next()) {
                (Some("endfunc"), _) => {
                    i += 1;
                    break;
                }
                (Some("param"), Some(param)) => {
                    params.push(param.to_string());
                    let kind = match lines[i].split_whitespace().last() {
                        Some(k @ ("float" | "word")) => k,
                        _ => "",
                    };
                    param_kinds.push(kind.to_string());
                }
                _ => body.push(lines[i]),
            }
            i += 1;
        }
        out.push(IrFunction { name, retkind, params, param_kinds, body });
    }
    out
}



pub(super) fn is_count_mark(line: &str) -> bool {
    let line = line.trim_start();
    line == "mark" || line.starts_with("mark ")
}


/// A `-> T!` function whose ok value fits one register returns two values
/// instead of a heap-allocated result box: `(is_ok: i64, payload)`. The payload
/// carries the ok value when is_ok=1, else the error-message string pointer.
/// `result_reg` uses an i64 payload (ints, handles, pointers); `result_reg_f`
/// uses an f64 payload for float ok values. Any other retkind is the normal
/// single-i64 return. Callers and the callee agree via this retkind string.
pub(super) fn result_abi_payload_type(retkind: &str) -> Option<types::Type> {
    match retkind {
        "result_reg" => Some(types::I64),
        "result_reg_f" => Some(types::F64),
        _ => None,
    }
}

/// Push the return AbiParams for a function's retkind: two values for the
/// register result ABI, one i64 otherwise.
pub(super) fn push_return_types(returns: &mut Vec<AbiParam>, retkind: &str) {
    match result_abi_payload_type(retkind) {
        Some(payload) => {
            returns.push(AbiParam::new(types::I64));
            returns.push(AbiParam::new(payload));
        }
        None => returns.push(AbiParam::new(types::I64)),
    }
}

pub(super) fn normalize_runtime_result(
    builder: &mut FunctionBuilder<'_>,
    value: Value,
    retkind: &str,
) -> Value {
    if retkind != "result_int" && retkind != "result_bool" {
        return value;
    }

    let zero = builder.ins().iconst(types::I64, 0);
    let one = builder.ins().iconst(types::I64, 1);
    let is_error = builder.ins().icmp(IntCC::Equal, value, zero);
    let encoded = builder.ins().iadd(value, one);
    builder.ins().select(is_error, zero, encoded)
}


pub(super) fn parse_call_shape<'a>(parts: &'a [&'a str]) -> Option<(&'a str, &'a str, usize, usize)> {
    if parts.len() < 5 {
        return None;
    }

    let fname = parts[2];
    if parts[3].parse::<usize>().is_ok() {
        return None;
    }
    let nargs = parts[4].parse::<usize>().ok()?;
    Some((fname, parts[3], nargs, 5))
}

pub(super) fn parse_reg(s: &str, instruction: &str, func_name: &str) -> Result<usize, CompileError> {
    s.parse::<usize>().map_err(|_| {
        CompileError::ModuleError(format!(
            "IR consumer: invalid destination register '{}' in {}: {}",
            s, func_name, instruction
        ))
    })
}

// An integer literal operand, with the 0x / 0b / 0o prefixes the emitter uses.
// A token that does not parse is malformed IR, so this reports it rather than
// coercing to 0 and miscompiling silently.
pub(super) fn parse_i64_operand(
    s: &str,
    what: &str,
    instruction: &str,
    func_name: &str,
) -> Result<i64, CompileError> {
    let parse_unsigned = |raw: &str| -> Result<u64, std::num::ParseIntError> {
        if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
            u64::from_str_radix(hex, 16)
        } else if let Some(bin) = raw.strip_prefix("0b").or_else(|| raw.strip_prefix("0B")) {
            u64::from_str_radix(bin, 2)
        } else if let Some(oct) = raw.strip_prefix("0o").or_else(|| raw.strip_prefix("0O")) {
            u64::from_str_radix(oct, 8)
        } else {
            raw.parse::<u64>()
        }
    };
    let parsed: Result<i64, ()> = if let Some(rest) = s.strip_prefix('-') {
        if rest.starts_with("0x")
            || rest.starts_with("0X")
            || rest.starts_with("0b")
            || rest.starts_with("0B")
            || rest.starts_with("0o")
            || rest.starts_with("0O")
        {
            parse_unsigned(rest)
                .ok()
                .filter(|&v| v <= (i64::MAX as u64) + 1)
                .map(|v| v.wrapping_neg() as i64)
                .ok_or(())
        } else {
            s.parse::<i64>().map_err(|_| ())
        }
    } else {
        // accept full 64-bit unsigned bit patterns (UInt64 globals/constants >= 2^63)
        parse_unsigned(s).map(|v| v as i64).map_err(|_| ())
    };
    parsed.map_err(|_| {
        CompileError::ModuleError(format!(
            "IR consumer: invalid {} '{}' in {}: {}",
            what, s, func_name, instruction
        ))
    })
}

// whether a call's retkind promises the destination register a real value.
// `void` says the register is never read, and `unknown` is the emitter
// admitting it does not know, so neither can contradict the callee.
pub(super) fn retkind_expects_a_value(retkind: &str) -> bool {
    retkind != "void" && retkind != "unknown"
}


pub(super) fn get_reg(regs: &HashMap<usize, Value>, s: &str) -> Result<Value, CompileError> {
    let reg = s.parse::<usize>().map_err(|_| {
        CompileError::ModuleError(format!("IR consumer: invalid register reference '{}'", s))
    })?;
    regs.get(&reg)
        .copied()
        .ok_or_else(|| CompileError::ModuleError(format!("IR consumer: missing register {}", reg)))
}

pub(super) fn get_label(
    labels: &HashMap<String, Block>,
    name: &str,
    func_name: &str,
) -> Result<Block, CompileError> {
    labels.get(name).copied().ok_or_else(|| {
        CompileError::ModuleError(format!(
            "IR consumer: unknown label '{}' in {}",
            name, func_name
        ))
    })
}

