//! Pre-lowering static scans: bit-wrapper detection, unpackable optional
//! tuple virtualization, and untyped float/string register kind inference.

use super::super::arith::plain_arith_op;
use cranelift_module::{DataId, FuncId};
use std::collections::{HashMap, HashSet};

// the native instruction behind a bit builtin, or None for anything else.
// these mirror the runtime externs exactly: pith_bit_shr is a logical shift
// (ushr), a shift count outside 0..63 stops the program on both paths, and
// a rotate takes its amount mod the width.
pub(in crate::ir_consumer) fn bits_builtin_op(builtin: &str) -> Option<&'static str> {
    match builtin {
        "bit_and" => Some("band"),
        "bit_or" => Some("bor"),
        "bit_xor" => Some("bxor"),
        "bit_shl" => Some("shl"),
        "bit_shr" => Some("shr"),
        "bit_rotl32" => Some("rotl32"),
        "bit_rotr32" => Some("rotr32"),
        "bit_rotl64" => Some("rotl64"),
        "bit_rotr64" => Some("rotr64"),
        _ => None,
    }
}

// std.bits wraps the bit builtins in one-line named functions, which made
// every `bits.band` in a hot loop a pith call into an FFI call: two call
// layers for one machine instruction. When a two-parameter function's body is
// verifiably just `return <bit builtin>(p0, p1)`, calls to it lower to that
// single instruction instead. The body is matched structurally against the
// exact shape the emitter produces (two param loads, the builtin call on
// them, the return, and the emitter's dead fallthrough tail), so an edited
// wrapper simply stops matching and keeps its call semantics.
pub(in crate::ir_consumer) fn detect_bits_wrapper(
    param_names: &[String],
    body_lines: &[&str],
    declared_funcs: &HashMap<String, FuncId>,
    global_data: &HashMap<String, DataId>,
) -> Option<&'static str> {
    if param_names.len() != 2 {
        return None;
    }
    let insns: Vec<Vec<&str>> = body_lines
        .iter()
        .map(|l| l.split_whitespace().collect::<Vec<&str>>())
        .filter(|p| !p.is_empty() && !p[0].starts_with(';'))
        .collect();
    if insns.len() < 4 {
        return None;
    }
    let (a, b, c, r) = (&insns[0], &insns[1], &insns[2], &insns[3]);
    if a.len() != 3 || a[0] != "load" || a[2] != param_names[0] {
        return None;
    }
    if b.len() != 3 || b[0] != "load" || b[2] != param_names[1] {
        return None;
    }
    if c.len() != 7
        || c[0] != "call"
        || c[3] != "int"
        || c[4] != "2"
        || c[5] != a[1]
        || c[6] != b[1]
    {
        return None;
    }
    // Never alias a builtin name that is shadowed by a user function, parameter,
    // or global closure in this module.
    if declared_funcs.contains_key(c[2])
        || global_data.contains_key(c[2])
        || param_names.iter().any(|n| n == c[2])
    {
        return None;
    }
    let op = bits_builtin_op(c[2])?;
    if r.len() != 2 || r[0] != "ret" || r[1] != c[1] {
        return None;
    }
    // the only thing allowed after the return is the emitter's unreachable
    // `iconst N 0 / ret N` tail.
    for extra in insns.iter().skip(4) {
        let dead_const = extra[0] == "iconst" && extra.len() == 3 && extra[2] == "0";
        let dead_ret = extra[0] == "ret" && extra.len() == 2;
        if !dead_const && !dead_ret {
            return None;
        }
    }
    Some(op)
}


// the runtime getters that also come as a register pair returning
// (is_some, value), for an optional the caller unpacks on the spot.
pub(in crate::ir_consumer) fn opt_getter_pair(name: &str) -> Option<&'static str> {
    Some(match name {
        "pith_list_first_opt" => "pith_list_first_opt_pair",
        "pith_list_last_opt" => "pith_list_last_opt_pair",
        "map_get_opt" => "pith_map_get_cstr_opt_pair",
        "map_get_ikey_opt" => "pith_map_get_ikey_opt_pair",
        "map_get_bkey_opt" => "pith_map_get_bkey_opt_pair",
        "pith_channel_recv" => "pith_channel_recv_pair",
        "pith_channel_try_recv" => "pith_channel_try_recv_pair",
        _ => return None,
    })
}

// the defining shape of an optional the consumer can unpack without a shell:
// call R pith_list_get_opt tuple 2 LIST INDEX, which is inlined, or a call to
// a getter with a register-pair form.
pub(in crate::ir_consumer) fn is_unpackable_opt_def(parts: &[&str]) -> bool {
    if parts.len() < 5 || parts[0] != "call" || parts[3] != "tuple" {
        return false;
    }
    let Ok(nargs) = parts[4].parse::<usize>() else {
        return false;
    };
    parts.len() == 5 + nargs
        && ((parts[2] == "pith_list_get_opt" && nargs == 2) || opt_getter_pair(parts[2]).is_some())
}

// a token occurrence (parts[i]) the inline unpack understands: the defining
// call itself, a field read of tuple slot 0 or 8, an optflag or optval read,
// or the release of the throwaway tuple shell. anything else (a store, a
// return, an argument to some other call) means the tuple escapes and must
// really exist.
pub(in crate::ir_consumer) fn is_sanctioned_opt_use(parts: &[&str], i: usize) -> bool {
    if is_unpackable_opt_def(parts) {
        return i == 1;
    }
    if parts.len() == 6 && parts[0] == "field" && (parts[3] == "0" || parts[3] == "8") {
        return i == 2;
    }
    if (parts.len() == 3 && parts[0] == "optflag") || (parts.len() == 5 && parts[0] == "optval") {
        return i == 2;
    }
    if parts.len() == 6
        && parts[0] == "call"
        && parts[2] == "pith_struct_release"
        && parts[4] == "1"
    {
        return i == 5;
    }
    false
}

// registers holding an unpackable optional (see is_unpackable_opt_def) whose
// every textual use is the emitter's own unpack pattern. the scan is deliberately conservative: any
// numeric token equal to a candidate register that sits outside a sanctioned
// position disqualifies it, so an escaping optional (`.get(i)` stored in a
// variable, passed on, returned) always takes the real runtime call.
pub(in crate::ir_consumer) fn scan_unpackable_opt_regs(body_lines: &[&str]) -> HashSet<usize> {
    let mut candidates: HashMap<usize, u32> = HashMap::new();
    for line in body_lines {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if is_unpackable_opt_def(&parts) {
            if let Ok(r) = parts[1].parse::<usize>() {
                *candidates.entry(r).or_insert(0) += 1;
            }
        }
    }
    // a register defined twice is not in SSA shape; leave it alone.
    candidates.retain(|_, count| *count == 1);
    if candidates.is_empty() {
        return HashSet::new();
    }
    for line in body_lines {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() || parts[0].starts_with(';') {
            continue;
        }
        for (i, tok) in parts.iter().enumerate() {
            let Ok(r) = tok.parse::<usize>() else { continue };
            if candidates.contains_key(&r) && !is_sanctioned_opt_use(&parts, i) {
                candidates.remove(&r);
            }
        }
    }
    candidates.into_keys().collect()
}


/// The named variables of a function body that hold a float. The IR does
/// not type its locals, so this infers them: a variable stored from the
/// result of a float instruction is a float. A parameter is a float when its
/// param line says `float`; one whose line names no kind (e.g. a lambda
/// parameter) is inferred as float only when a register loaded from it is
/// directly consumed by a float operation or a float-taking runtime call, so
/// an Int parameter in a lambda that also touches Floats is never poisoned.
/// Then three rounds propagate the kind through loads, float-typed consumers,
/// integer-spelled arithmetic on a float operand, and stores.
pub(in crate::ir_consumer) fn scan_float_vars(
    param_names: &[String],
    param_kinds: &[String],
    body_lines: &[&str],
) -> HashSet<String> {
    let mut float_vars: HashSet<String> = HashSet::new();
    let mut float_source_regs: HashSet<usize> = HashSet::new();
    let mut unannotated_params: HashSet<&str> = HashSet::new();
    for (i, name) in param_names.iter().enumerate() {
        match param_kinds.get(i).map(String::as_str) {
            Some("float") => {
                float_vars.insert(name.clone());
            }
            Some("word") => {}
            _ => {
                if name != "__closure" {
                    unannotated_params.insert(name.as_str());
                }
            }
        }
    }

    let parsed: Vec<Vec<&str>> = body_lines
        .iter()
        .map(|l| l.split_whitespace().collect())
        .collect();
    let mut reg_from_param: HashMap<usize, &str> = HashMap::new();
    for p in &parsed {
        if p.is_empty() {
            continue;
        }
        match p[0] {
            "fconst" | "fadd" | "fsub" | "fmul" | "fdiv" if p.len() >= 2 => {
                if let Ok(r) = p[1].parse::<usize>() {
                    float_source_regs.insert(r);
                }
            }
            "call" if p.len() >= 5 => {
                if p[3] == "float"
                    || matches!(
                        p[2],
                        "int_to_float"
                            | "pith_int_to_float"
                            | "bits_to_float"
                            | "pith_bits_to_float"
                    )
                {
                    if let Ok(r) = p[1].parse::<usize>() {
                        float_source_regs.insert(r);
                    }
                }
            }
            "rcall" if p.len() >= 6 && p[4] == "float" => {
                if let Ok(r) = p[2].parse::<usize>() {
                    float_source_regs.insert(r);
                }
            }
            "field" if p.len() >= 6 && p[4] == "float" => {
                if let Ok(r) = p[1].parse::<usize>() {
                    float_source_regs.insert(r);
                }
            }
            "load" if p.len() >= 3 => {
                if let Ok(r) = p[1].parse::<usize>() {
                    if unannotated_params.contains(p[2]) {
                        reg_from_param.insert(r, p[2]);
                    }
                }
            }
            "store" if p.len() >= 3 => {
                if let Ok(r) = p[2].parse::<usize>() {
                    if float_source_regs.contains(&r) {
                        float_vars.insert(p[1].to_string());
                    }
                }
            }
            _ => {}
        }
    }

    let takes_float_arg = |fname: &str, arg_pos: usize| -> bool {
        matches!(
            (fname, arg_pos),
            (
                "float_to_string"
                    | "pith_float_to_cstr"
                    | "float_format"
                    | "pith_float_format"
                    | "float_to_int"
                    | "pith_float_to_int"
                    | "float_to_bits"
                    | "pith_float_to_bits"
                    | "sqrt"
                    | "floor"
                    | "ceil"
                    | "round"
                    | "sin"
                    | "cos"
                    | "tan"
                    | "asin"
                    | "acos"
                    | "atan"
                    | "log"
                    | "log2"
                    | "log10"
                    | "exp"
                    | "fabs"
                    | "is_nan"
                    | "is_inf",
                0
            ) | ("pow" | "atan2" | "fmin" | "fmax", 0 | 1)
        )
    };

    for _ in 0..3 {
        // a load of a float variable yields a float
        for p in &parsed {
            if p.len() >= 3 && p[0] == "load" {
                if let Ok(r) = p[1].parse::<usize>() {
                    if float_vars.contains(p[2]) {
                        float_source_regs.insert(r);
                    }
                }
            }
        }
        // Infer unannotated params from direct float consumers, and propagate
        // float kind through integer-spelled arithmetic on a float operand.
        for p in &parsed {
            if p.is_empty() {
                continue;
            }
            match p[0] {
                "fadd" | "fsub" | "fmul" | "fdiv" if p.len() >= 4 => {
                    for tok in [p[2], p[3]] {
                        if let Ok(r) = tok.parse::<usize>() {
                            if let Some(&pname) = reg_from_param.get(&r) {
                                float_vars.insert(pname.to_string());
                                float_source_regs.insert(r);
                            }
                        }
                    }
                }
                op if p.len() >= 4
                    && matches!(
                        plain_arith_op(op),
                        "mul" | "div" | "add" | "sub" | "eq" | "neq" | "lt" | "gt" | "lte" | "gte"
                    ) =>
                {
                    let a_reg = p[2].parse::<usize>().ok();
                    let b_reg = p[3].parse::<usize>().ok();
                    let a_float = a_reg.is_some_and(|r| float_source_regs.contains(&r));
                    let b_float = b_reg.is_some_and(|r| float_source_regs.contains(&r));
                    if a_float && !b_float {
                        if let Some(r) = b_reg {
                            if let Some(&pname) = reg_from_param.get(&r) {
                                float_vars.insert(pname.to_string());
                                float_source_regs.insert(r);
                            }
                        }
                    } else if b_float && !a_float {
                        if let Some(r) = a_reg {
                            if let Some(&pname) = reg_from_param.get(&r) {
                                float_vars.insert(pname.to_string());
                                float_source_regs.insert(r);
                            }
                        }
                    }
                    if (a_float || b_float)
                        && matches!(plain_arith_op(op), "mul" | "div" | "add" | "sub")
                    {
                        if let Ok(r) = p[1].parse::<usize>() {
                            float_source_regs.insert(r);
                        }
                    }
                }
                "call" if p.len() >= 6 => {
                    let fname = p[2];
                    if let Ok(nargs) = p[4].parse::<usize>() {
                        for j in 0..nargs {
                            if let Some(tok) = p.get(5 + j) {
                                if takes_float_arg(fname, j) {
                                    if let Ok(r) = tok.parse::<usize>() {
                                        if let Some(&pname) = reg_from_param.get(&r) {
                                            float_vars.insert(pname.to_string());
                                            float_source_regs.insert(r);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        // Store propagation
        for p in &parsed {
            if p.len() >= 3 && p[0] == "store" {
                if let Ok(r) = p[2].parse::<usize>() {
                    if float_source_regs.contains(&r) {
                        float_vars.insert(p[1].to_string());
                    }
                }
            }
        }
    }

    float_vars
}


/// The registers known to hold a string or a float. The IR is untyped, and
/// these two kinds change how an instruction lowers: a string operand turns
/// `add` into a concatenation and an ordered compare into a runtime string
/// compare, and a float operand (held as its i64 bit pattern) turns
/// arithmetic and compares into their float forms. Every other value is a
/// plain i64.
#[derive(Default)]
pub(in crate::ir_consumer) struct RegKinds {
    pub(in crate::ir_consumer) strings: HashSet<usize>,
    pub(in crate::ir_consumer) floats: HashSet<usize>,
}

impl RegKinds {
    /// `reg` now holds a plain i64.
    pub(in crate::ir_consumer) fn clear(&mut self, reg: usize) {
        self.strings.remove(&reg);
        self.floats.remove(&reg);
    }

    /// `reg` now holds a value of `kind`, a retkind or field-kind token such
    /// as `string`, `float` or `struct:Name`; any other token clears it.
    pub(in crate::ir_consumer) fn set(&mut self, reg: usize, kind: &str) {
        self.clear(reg);
        match kind {
            "string" => {
                self.strings.insert(reg);
            }
            "float" => {
                self.floats.insert(reg);
            }
            _ => {}
        }
    }

    /// Whether the register an operand token names holds a string.
    pub(in crate::ir_consumer) fn is_string(&self, operand: &str) -> bool {
        operand
            .parse::<usize>()
            .is_ok_and(|r| self.strings.contains(&r))
    }

    /// Whether the register an operand token names holds a float.
    pub(in crate::ir_consumer) fn is_float(&self, operand: &str) -> bool {
        operand
            .parse::<usize>()
            .is_ok_and(|r| self.floats.contains(&r))
    }
}

