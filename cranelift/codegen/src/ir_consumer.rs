//! IR Consumer — translates Forge text IR to Cranelift native code
//!
//! This module parses the simple text IR emitted by self-host/ir_emitter.fg
//! and translates it to Cranelift API calls. This is the Rust-side half of
//! Stage 2: moving compilation logic from Rust to Forge.
//!
//! IR format reference:
//!   string N "content"              — string data declaration
//!   func NAME NPARAM RETTYPE        — function header
//!   param NAME                      — function parameter
//!   endfunc                         — end function
//!   iconst REG VALUE                — integer constant
//!   strref REG STRIDX              — reference to string data
//!   call REG FNAME NARGS ARG...    — function call with return
//!   callv FNAME NARGS ARG...       — void function call
//!   add REG A B                    — integer addition
//!   sub REG A B                    — integer subtraction
//!   mul REG A B                    — integer multiplication
//!   div REG A B                    — integer division
//!   mod REG A B                    — integer modulo
//!   eq REG A B                     — compare equal
//!   neq REG A B                    — compare not equal
//!   lt/gt/lte/gte REG A B          — comparisons
//!   concat REG A B                 — string concatenation
//!   store VARNAME REG              — store to variable
//!   load REG VARNAME               — load from variable
//!   ret REG                        — return value
//!   brif COND THEN ELSE            — conditional branch
//!   jmp LABEL                      — unconditional jump
//!   label NAME                     — label definition

use crate::{CodeGen, CompileError};
use cranelift::prelude::*;
use cranelift_module::{FuncId, Linkage, Module};
use std::collections::{HashMap, HashSet};

#[cfg(forge_cranelift_new_api)]
fn declare_i64_var(builder: &mut FunctionBuilder<'_>) -> Variable {
    builder.declare_var(types::I64)
}

#[cfg(not(forge_cranelift_new_api))]
fn declare_i64_var(builder: &mut FunctionBuilder<'_>, next_var_id: &mut u32) -> Variable {
    let var = Variable::new((*next_var_id) as usize);
    *next_var_id += 1;
    builder.declare_var(var, types::I64);
    var
}

/// Compile IR text to native code via Cranelift
pub fn compile_from_ir(
    codegen: &mut CodeGen,
    ir_text: &str,
    runtime_funcs: &HashMap<String, FuncId>,
) -> Result<HashMap<String, FuncId>, CompileError> {
    let lines: Vec<&str> = ir_text.lines().collect();
    let mut declared_funcs: HashMap<String, FuncId> = HashMap::new();
    let mut string_data: Vec<(String, String)> = Vec::new();
    let mut struct_layouts: HashMap<String, Vec<String>> = HashMap::new();
    let mut global_data: HashMap<String, cranelift_module::DataId> = HashMap::new();
    let mut str_globals: Vec<(String, String)> = Vec::new(); // (global_name, string_id)
    let mut string_global_names: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    // Pass 1: collect string data and declare functions
    for line in &lines {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        match parts[0] {
            "string" if parts.len() >= 3 => {
                let idx = parts[1].to_string();
                // Extract quoted string content and process escape sequences
                let rest = &line[line.find('"').unwrap_or(0)..];
                // Strip exactly one leading and one trailing quote
                // (trim_matches would eat escaped quotes like "\"")
                let raw = if rest.len() >= 2 && rest.starts_with('"') && rest.ends_with('"') {
                    &rest[1..rest.len() - 1]
                } else {
                    rest
                };
                let mut content = String::new();
                let bytes = raw.as_bytes();
                let mut j = 0;
                while j < bytes.len() {
                    if bytes[j] == b'\\' && j + 1 < bytes.len() {
                        match bytes[j + 1] {
                            b'n' => {
                                content.push('\n');
                                j += 2;
                            }
                            b't' => {
                                content.push('\t');
                                j += 2;
                            }
                            b'\\' => {
                                content.push('\\');
                                j += 2;
                            }
                            b'"' => {
                                content.push('"');
                                j += 2;
                            }
                            b'r' => {
                                content.push('\r');
                                j += 2;
                            }
                            b'0' => {
                                content.push('\0');
                                j += 2;
                            }
                            _ => {
                                content.push(bytes[j] as char);
                                j += 1;
                            }
                        }
                    } else {
                        content.push(bytes[j] as char);
                        j += 1;
                    }
                }
                string_data.push((idx, content));
            }
            "struct" if parts.len() >= 2 => {
                let name = parts[1].to_string();
                if !struct_layouts.contains_key(&name) {
                    // Filter out "pub" markers from field list
                    let fields: Vec<String> = parts[2..]
                        .iter()
                        .filter(|s| **s != "pub")
                        .map(|s| s.to_string())
                        .collect();
                    let field_pairs: Vec<(String, String)> = fields
                        .iter()
                        .map(|f| (f.clone(), "Int".to_string()))
                        .collect();
                    crate::register_struct_layout(&name, &field_pairs);
                    struct_layouts.insert(name, fields);
                }
            }
            "global" if parts.len() >= 3 => {
                let gname = parts[1].to_string();
                if !global_data.contains_key(&gname) {
                    let init_kind = parts[2];
                    use cranelift_module::DataDescription;
                    // Rename global if it conflicts with a function name
                    let data_name = if declared_funcs.contains_key(&gname) {
                        format!("__g_{}", gname)
                    } else {
                        gname.clone()
                    };
                    let data_id = codegen
                        .module
                        .declare_data(&data_name, Linkage::Local, true, false)
                        .map_err(|e| CompileError::ModuleError(e.to_string()))?;
                    let mut desc = DataDescription::new();
                    let init_val: i64 =
                        if init_kind == "list" || init_kind == "map" || init_kind == "set" || init_kind == "set_int" {
                            0
                        } else if init_kind.starts_with("str:") {
                            0 // will be patched in __init_globals
                        } else {
                            init_kind.parse().unwrap_or(0)
                        };
                    desc.define(init_val.to_le_bytes().to_vec().into_boxed_slice());
                    codegen
                        .module
                        .define_data(data_id, &desc)
                        .map_err(|e| CompileError::ModuleError(e.to_string()))?;
                    global_data.insert(gname.clone(), data_id);
                    // Track str: globals that need runtime initialization
                    if init_kind.starts_with("str:") {
                        let str_id = &init_kind[4..]; // e.g., "m0s0"
                        str_globals.push((gname.clone(), str_id.to_string()));
                        string_global_names.insert(gname);
                    }
                }
            }
            "struct_alias" if parts.len() >= 3 => {
                let alias = parts[1].to_string();
                let target = parts[2].to_string();
                crate::register_struct_alias(&alias, &target);
                if let Some(fields) = struct_layouts.get(&target).cloned() {
                    struct_layouts.insert(alias, fields);
                }
            }
            "func" if parts.len() >= 4 => {
                let name = parts[1];
                if !declared_funcs.contains_key(name) {
                    let nparam: usize = parts[2].parse().unwrap_or(0);
                    let mut sig = codegen.module.make_signature();
                    for _ in 0..nparam {
                        sig.params.push(AbiParam::new(types::I64));
                    }
                    sig.returns.push(AbiParam::new(types::I64));
                    if let Ok(func_id) =
                        codegen.module.declare_function(name, Linkage::Export, &sig)
                    {
                        declared_funcs.insert(name.to_string(), func_id);
                    }
                    // silently skip if name conflicts with runtime declaration
                }
            }
            _ => {}
        }
    }

    // Declare string data functions
    let mut string_funcs: HashMap<String, FuncId> = HashMap::new();
    for (idx, content) in &string_data {
        if !string_funcs.contains_key(idx) {
            let name = format!("__irstr_{}", idx);
            let func_id = crate::declare_string_data(&mut codegen.module, &name, content)
                .map_err(|e| CompileError::ModuleError(format!("string data: {:?}", e)))?;
            string_funcs.insert(idx.clone(), func_id);
        }
    }

    // Pass 2: compile function bodies (first definition wins for duplicates)
    let mut compiled_funcs: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut i = 0;
    while i < lines.len() {
        let parts: Vec<&str> = lines[i].split_whitespace().collect();
        if parts.is_empty() || parts[0] != "func" {
            i += 1;
            continue;
        }

        let func_name = parts[1].to_string();
        let _nparam: usize = parts[2].parse().unwrap_or(0);
        i += 1;

        // Collect function body lines until endfunc
        let mut body_lines: Vec<&str> = Vec::new();
        let mut param_names: Vec<String> = Vec::new();
        while i < lines.len() {
            let bparts: Vec<&str> = lines[i].split_whitespace().collect();
            if !bparts.is_empty() && bparts[0] == "endfunc" {
                i += 1;
                break;
            }
            if !bparts.is_empty() && bparts[0] == "param" && bparts.len() >= 2 {
                param_names.push(bparts[1].to_string());
            } else {
                body_lines.push(lines[i]);
            }
            i += 1;
        }

        // Compile this function (skip if already compiled from an earlier module)
        if compiled_funcs.contains(&func_name) {
            continue;
        }
        compiled_funcs.insert(func_name.clone());
        if let Some(&func_id) = declared_funcs.get(&func_name) {
            compile_ir_function(
                codegen,
                func_id,
                &func_name,
                &param_names,
                &body_lines,
                runtime_funcs,
                &declared_funcs,
                &string_funcs,
                &struct_layouts,
                &global_data,
                &str_globals,
                &string_global_names,
            )?;
        }
    }

    Ok(declared_funcs)
}

fn normalize_runtime_result(
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

fn compile_ir_function(
    codegen: &mut CodeGen,
    func_id: FuncId,
    func_name: &str,
    param_names: &[String],
    body_lines: &[&str],
    runtime_funcs: &HashMap<String, FuncId>,
    declared_funcs: &HashMap<String, FuncId>,
    string_funcs: &HashMap<String, FuncId>,
    struct_layouts: &HashMap<String, Vec<String>>,
    global_data: &HashMap<String, cranelift_module::DataId>,
    str_globals: &[(String, String)],
    string_global_names: &std::collections::HashSet<String>,
) -> Result<(), CompileError> {
    let mut ctx = codegen.module.make_context();

    // Build signature
    for _ in param_names {
        ctx.func.signature.params.push(AbiParam::new(types::I64));
    }
    ctx.func.signature.returns.push(AbiParam::new(types::I64));

    let mut builder_ctx = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
    // Cache function references to avoid duplicate declarations
    let mut func_ref_cache: HashMap<FuncId, cranelift::codegen::ir::FuncRef> = HashMap::new();

    let entry_block = builder.create_block();
    builder.append_block_params_for_function_params(entry_block);
    builder.switch_to_block(entry_block);

    // Map param names to block params
    let block_params: Vec<Value> = builder.block_params(entry_block).to_vec();
    let mut regs: HashMap<usize, Value> = HashMap::new();
    let mut string_regs: HashSet<usize> = HashSet::new();
    let mut string_vars: HashSet<String> = HashSet::new();
    let mut bytes_regs: HashSet<usize> = HashSet::new();
    let mut bytes_vars: HashSet<String> = HashSet::new();
    let mut float_regs: HashSet<usize> = HashSet::new();
    let mut float_vars: HashSet<String> = HashSet::new();
    let mut reg_source_vars: HashMap<usize, String> = HashMap::new();
    let mut struct_regs: HashMap<usize, String> = HashMap::new();
    let mut struct_vars: HashMap<String, String> = HashMap::new();
    let mut named_vars: HashMap<String, Variable> = HashMap::new();
    let mut labels: HashMap<String, Block> = HashMap::new();
    #[cfg(not(forge_cranelift_new_api))]
    let mut next_var_id: u32 = 0;

    // Call __init_globals (and module-specific __init_globals_N) at the start of main
    if func_name == "main" {
        // Call module-specific initializers first (imported modules)
        for (name, &fid) in declared_funcs.iter() {
            if name.starts_with("__init_globals_") {
                let init_ref = codegen.module.declare_func_in_func(fid, builder.func);
                builder.ins().call(init_ref, &[]);
            }
        }
        // Then the main module's __init_globals
        if let Some(&init_id) = declared_funcs.get("__init_globals") {
            let init_ref = codegen.module.declare_func_in_func(init_id, builder.func);
            builder.ins().call(init_ref, &[]);
        }
        // Initialize str: globals — call string function and store result
        for (gname, str_id) in str_globals.iter() {
            if let (Some(&data_id), Some(&sfunc_id)) = (
                global_data.get(gname.as_str()),
                string_funcs.get(str_id.as_str()),
            ) {
                let sf_ref = codegen.module.declare_func_in_func(sfunc_id, builder.func);
                let str_val = builder.ins().call(sf_ref, &[]);
                let str_result = builder.func.dfg.first_result(str_val);
                let gv = codegen.module.declare_data_in_func(data_id, builder.func);
                let addr = builder.ins().global_value(types::I64, gv);
                builder
                    .ins()
                    .store(cranelift::codegen::ir::MemFlags::new(), str_result, addr, 0);
            }
        }
    }

    for (i, name) in param_names.iter().enumerate() {
        if i < block_params.len() {
            #[cfg(forge_cranelift_new_api)]
            let var = declare_i64_var(&mut builder);
            #[cfg(not(forge_cranelift_new_api))]
            let var = declare_i64_var(&mut builder, &mut next_var_id);
            builder.def_var(var, block_params[i]);
            named_vars.insert(name.clone(), var);
            regs.insert(i, block_params[i]);
        }
    }

    // Pre-scan: detect float-typed variables by finding `store VAR REG`
    // where REG was assigned by fconst/fmul/fadd/fsub/fdiv
    {
        let mut float_source_regs: std::collections::HashSet<usize> =
            std::collections::HashSet::new();
        for line in body_lines {
            let p: Vec<&str> = line.split_whitespace().collect();
            if p.is_empty() {
                continue;
            }
            match p[0] {
                "fconst" | "fadd" | "fsub" | "fmul" | "fdiv" if p.len() >= 2 => {
                    if let Ok(r) = p[1].parse::<usize>() {
                        float_source_regs.insert(r);
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
        // If function has any float operations, mark all params as float.
        // This is conservative but correct for math functions.
        let has_float_ops = body_lines.iter().any(|line| {
            let p: Vec<&str> = line.split_whitespace().collect();
            !p.is_empty() && matches!(p[0], "fconst" | "fmul" | "fadd" | "fsub" | "fdiv")
        });
        if has_float_ops {
            for name in param_names {
                float_vars.insert(name.clone());
            }
        }
        // Iterative propagation: if a variable is stored from a register
        // that was loaded from a float variable, mark it as float too.
        // Also mark registers from loads of float vars.
        for _ in 0..3 {
            let mut new_float_regs: Vec<usize> = Vec::new();
            for line in body_lines.iter() {
                let p: Vec<&str> = line.split_whitespace().collect();
                if p.len() >= 3 && p[0] == "load" {
                    if let Ok(r) = p[1].parse::<usize>() {
                        if float_vars.contains(p[2]) {
                            new_float_regs.push(r);
                        }
                    }
                }
            }
            for r in &new_float_regs {
                float_source_regs.insert(*r);
            }
            // Propagate: if mul/div/add/sub uses a float reg, its result is float
            for line in body_lines.iter() {
                let p: Vec<&str> = line.split_whitespace().collect();
                if p.len() >= 4 && matches!(p[0], "mul" | "div" | "add" | "sub") {
                    let a_float = p[2]
                        .parse::<usize>()
                        .map_or(false, |r| float_source_regs.contains(&r));
                    let b_float = p[3]
                        .parse::<usize>()
                        .map_or(false, |r| float_source_regs.contains(&r));
                    if a_float || b_float {
                        if let Ok(r) = p[1].parse::<usize>() {
                            float_source_regs.insert(r);
                        }
                    }
                }
            }
            // Store propagation
            for line in body_lines.iter() {
                let p: Vec<&str> = line.split_whitespace().collect();
                if p.len() >= 3 && p[0] == "store" {
                    if let Ok(r) = p[2].parse::<usize>() {
                        if float_source_regs.contains(&r) {
                            float_vars.insert(p[1].to_string());
                        }
                    }
                }
            }
        }
    }

    // Pre-scan for labels and create blocks
    for line in body_lines {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if !parts.is_empty() && parts[0] == "label" && parts.len() >= 2 {
            let block = builder.create_block();
            labels.insert(parts[1].to_string(), block);
        }
    }

    // Older emitters briefly lowered `break` in `while true` loops through an
    // extra join label. The current self-hosted emitter already jumps straight
    // to the loop exit, so redirecting labels here now corrupts valid nested
    // `if`/`result` joins into early loop exits.
    let break_redirects: HashMap<String, String> = HashMap::new();

    // Compile instructions
    let mut terminated = false;
    for line in body_lines {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() || parts[0].starts_with(';') {
            continue;
        }

        // If current block is terminated, skip until next label
        if terminated {
            if parts[0] == "label" && parts.len() >= 2 {
                let block = labels[parts[1]];
                builder.switch_to_block(block);
                terminated = false;
            }
            continue;
        }

        match parts[0] {
            "iconst" if parts.len() >= 3 => {
                let reg: usize = parts[1].parse().unwrap_or(0);
                let s = parts[2];
                let val: i64 = if s.starts_with("0x") || s.starts_with("0X") {
                    i64::from_str_radix(&s[2..], 16).unwrap_or(0)
                } else if s.starts_with("0b") || s.starts_with("0B") {
                    i64::from_str_radix(&s[2..], 2).unwrap_or(0)
                } else if s.starts_with("0o") || s.starts_with("0O") {
                    i64::from_str_radix(&s[2..], 8).unwrap_or(0)
                } else {
                    s.parse().unwrap_or(0)
                };
                let v = builder.ins().iconst(types::I64, val);
                regs.insert(reg, v);
                reg_source_vars.remove(&reg);
                struct_regs.remove(&reg);
                string_regs.remove(&reg);
                bytes_regs.remove(&reg);
                float_regs.remove(&reg);
            }

            "fconst" if parts.len() >= 3 => {
                let reg: usize = parts[1].parse().unwrap_or(0);
                let fval: f64 = parts[2].parse().unwrap_or(0.0);
                let fv = builder.ins().f64const(fval);
                let v =
                    builder
                        .ins()
                        .bitcast(types::I64, cranelift::codegen::ir::MemFlags::new(), fv);
                regs.insert(reg, v);
                reg_source_vars.remove(&reg);
                struct_regs.remove(&reg);
                float_regs.insert(reg);
                string_regs.remove(&reg);
                bytes_regs.remove(&reg);
            }

            "strref" if parts.len() >= 3 => {
                let reg: usize = parts[1].parse().unwrap_or(0);
                let str_idx = parts[2].to_string();
                if let Some(&sf_id) = string_funcs.get(&str_idx) {
                    let sf_ref = codegen.module.declare_func_in_func(sf_id, builder.func);
                    let call = builder.ins().call(sf_ref, &[]);
                    let v = builder.func.dfg.first_result(call);
                    regs.insert(reg, v);
                } else {
                    regs.insert(reg, builder.ins().iconst(types::I64, 0));
                }
                reg_source_vars.remove(&reg);
                struct_regs.remove(&reg);
                string_regs.insert(reg);
                bytes_regs.remove(&reg);
                float_regs.remove(&reg);
            }

            "band" | "bor" | "bxor" | "shl" | "shr" if parts.len() >= 4 => {
                let reg: usize = parts[1].parse().unwrap_or(0);
                let a = get_reg(&regs, parts[2]);
                let b = get_reg(&regs, parts[3]);
                let v = match parts[0] {
                    "band" => builder.ins().band(a, b),
                    "bor" => builder.ins().bor(a, b),
                    "bxor" => builder.ins().bxor(a, b),
                    "shl" => builder.ins().ishl(a, b),
                    "shr" => builder.ins().ushr(a, b),
                    _ => unreachable!(),
                };
                regs.insert(reg, v);
                reg_source_vars.remove(&reg);
                struct_regs.remove(&reg);
                string_regs.remove(&reg);
                bytes_regs.remove(&reg);
                float_regs.remove(&reg);
            }

            "bnot" if parts.len() >= 3 => {
                let reg: usize = parts[1].parse().unwrap_or(0);
                let a = get_reg(&regs, parts[2]);
                let v = builder.ins().bnot(a);
                regs.insert(reg, v);
                reg_source_vars.remove(&reg);
                struct_regs.remove(&reg);
                string_regs.remove(&reg);
                bytes_regs.remove(&reg);
                float_regs.remove(&reg);
            }

            "and" | "or" if parts.len() >= 4 => {
                let reg: usize = parts[1].parse().unwrap_or(0);
                let a = get_reg(&regs, parts[2]);
                let b = get_reg(&regs, parts[3]);
                let v = match parts[0] {
                    "and" => builder.ins().band(a, b),
                    "or" => builder.ins().bor(a, b),
                    _ => unreachable!(),
                };
                regs.insert(reg, v);
                reg_source_vars.remove(&reg);
                struct_regs.remove(&reg);
                string_regs.remove(&reg);
                bytes_regs.remove(&reg);
                float_regs.remove(&reg);
            }

            "fadd" | "fsub" | "fmul" | "fdiv" if parts.len() >= 4 => {
                let reg: usize = parts[1].parse().unwrap_or(0);
                let a = get_reg(&regs, parts[2]);
                let b = get_reg(&regs, parts[3]);
                // Bitcast i64 → f64
                let fa =
                    builder
                        .ins()
                        .bitcast(types::F64, cranelift::codegen::ir::MemFlags::new(), a);
                let fb =
                    builder
                        .ins()
                        .bitcast(types::F64, cranelift::codegen::ir::MemFlags::new(), b);
                let fv = match parts[0] {
                    "fadd" => builder.ins().fadd(fa, fb),
                    "fsub" => builder.ins().fsub(fa, fb),
                    "fmul" => builder.ins().fmul(fa, fb),
                    "fdiv" => builder.ins().fdiv(fa, fb),
                    _ => unreachable!(),
                };
                // Bitcast f64 → i64
                let v =
                    builder
                        .ins()
                        .bitcast(types::I64, cranelift::codegen::ir::MemFlags::new(), fv);
                regs.insert(reg, v);
                reg_source_vars.remove(&reg);
                struct_regs.remove(&reg);
                float_regs.insert(reg);
                string_regs.remove(&reg);
                bytes_regs.remove(&reg);
            }

            "add" | "sub" | "mul" | "div" | "mod" if parts.len() >= 4 => {
                let reg: usize = parts[1].parse().unwrap_or(0);
                let a_reg = parts[2].parse::<usize>().ok();
                let b_reg = parts[3].parse::<usize>().ok();
                let a = get_reg(&regs, parts[2]);
                let b = get_reg(&regs, parts[3]);
                reg_source_vars.remove(&reg);
                struct_regs.remove(&reg);
                // If `add` has a string operand, treat as concat (IR emitter
                // sometimes emits `add` instead of `concat` when variable types
                // aren't tracked across function boundaries)
                if parts[0] == "add"
                    && a_reg.is_some_and(|r| string_regs.contains(&r))
                    && b_reg.is_some_and(|r| string_regs.contains(&r))
                {
                    let concat_name = if runtime_funcs.contains_key("forge_concat_cstr") {
                        "forge_concat_cstr"
                    } else {
                        "forge_string_concat"
                    };
                    if let Some(&concat_id) = runtime_funcs.get(concat_name) {
                        let concat_ref = *func_ref_cache.entry(concat_id).or_insert_with(|| {
                            codegen.module.declare_func_in_func(concat_id, builder.func)
                        });
                        let call = builder.ins().call(concat_ref, &[a, b]);
                        if !builder.func.dfg.inst_results(call).is_empty() {
                            regs.insert(reg, builder.func.dfg.first_result(call));
                        } else {
                            regs.insert(reg, a);
                        }
                    } else {
                        regs.insert(reg, builder.ins().iadd(a, b));
                    }
                    string_regs.insert(reg);
                    bytes_regs.remove(&reg);
                    float_regs.remove(&reg);
                // If operands are known floats, promote to float operation
                } else if matches!(parts[0], "add" | "sub" | "mul" | "div")
                    && (a_reg.is_some_and(|r| float_regs.contains(&r))
                        || b_reg.is_some_and(|r| float_regs.contains(&r)))
                {
                    let fa = builder.ins().bitcast(
                        types::F64,
                        cranelift::codegen::ir::MemFlags::new(),
                        a,
                    );
                    let fb = builder.ins().bitcast(
                        types::F64,
                        cranelift::codegen::ir::MemFlags::new(),
                        b,
                    );
                    let fv = match parts[0] {
                        "add" => builder.ins().fadd(fa, fb),
                        "sub" => builder.ins().fsub(fa, fb),
                        "mul" => builder.ins().fmul(fa, fb),
                        "div" => builder.ins().fdiv(fa, fb),
                        _ => unreachable!(),
                    };
                    let v = builder.ins().bitcast(
                        types::I64,
                        cranelift::codegen::ir::MemFlags::new(),
                        fv,
                    );
                    regs.insert(reg, v);
                    float_regs.insert(reg);
                    string_regs.remove(&reg);
                    bytes_regs.remove(&reg);
                } else {
                    let v = match parts[0] {
                        "add" => builder.ins().iadd(a, b),
                        "sub" => builder.ins().isub(a, b),
                        "mul" => builder.ins().imul(a, b),
                        "div" => builder.ins().sdiv(a, b),
                        "mod" => builder.ins().srem(a, b),
                        _ => unreachable!(),
                    };
                    regs.insert(reg, v);
                    string_regs.remove(&reg);
                    bytes_regs.remove(&reg);
                    float_regs.remove(&reg);
                }
            }

            "eq" | "neq" | "lt" | "gt" | "lte" | "gte" if parts.len() >= 4 => {
                let reg: usize = parts[1].parse().unwrap_or(0);
                let a_reg = parts[2].parse::<usize>().ok();
                let b_reg = parts[3].parse::<usize>().ok();
                let a = get_reg(&regs, parts[2]);
                let b = get_reg(&regs, parts[3]);
                reg_source_vars.remove(&reg);
                struct_regs.remove(&reg);
                // For lt/gt/lte/gte on strings, call runtime comparison
                let is_str_cmp = matches!(parts[0], "lt" | "gt" | "lte" | "gte")
                    && (a_reg.is_some_and(|r| string_regs.contains(&r))
                        || b_reg.is_some_and(|r| string_regs.contains(&r)));
                if is_str_cmp {
                    let cmp_name = match parts[0] {
                        "lt" => "forge_cstring_lt",
                        "gt" => "forge_cstring_gt",
                        "lte" => "forge_cstring_lte",
                        "gte" => "forge_cstring_gte",
                        _ => "forge_cstring_lt",
                    };
                    if let Some(&fid) = runtime_funcs.get(cmp_name) {
                        let fref = *func_ref_cache.entry(fid).or_insert_with(|| {
                            codegen.module.declare_func_in_func(fid, builder.func)
                        });
                        let call = builder.ins().call(fref, &[a, b]);
                        regs.insert(reg, builder.func.dfg.first_result(call));
                    } else {
                        let cmp = builder.ins().icmp(IntCC::SignedLessThan, a, b);
                        regs.insert(reg, builder.ins().uextend(types::I64, cmp));
                    }
                } else if a_reg.is_some_and(|r| float_regs.contains(&r))
                    || b_reg.is_some_and(|r| float_regs.contains(&r))
                {
                    // Float comparison
                    let fa = builder.ins().bitcast(
                        types::F64,
                        cranelift::codegen::ir::MemFlags::new(),
                        a,
                    );
                    let fb = builder.ins().bitcast(
                        types::F64,
                        cranelift::codegen::ir::MemFlags::new(),
                        b,
                    );
                    use cranelift::codegen::ir::condcodes::FloatCC;
                    let fcc = match parts[0] {
                        "eq" => FloatCC::Equal,
                        "neq" => FloatCC::NotEqual,
                        "lt" => FloatCC::LessThan,
                        "gt" => FloatCC::GreaterThan,
                        "lte" => FloatCC::LessThanOrEqual,
                        "gte" => FloatCC::GreaterThanOrEqual,
                        _ => FloatCC::Equal,
                    };
                    let cmp = builder.ins().fcmp(fcc, fa, fb);
                    let v = builder.ins().uextend(types::I64, cmp);
                    regs.insert(reg, v);
                } else {
                    let cc = match parts[0] {
                        "eq" => IntCC::Equal,
                        "neq" => IntCC::NotEqual,
                        "lt" => IntCC::SignedLessThan,
                        "gt" => IntCC::SignedGreaterThan,
                        "lte" => IntCC::SignedLessThanOrEqual,
                        "gte" => IntCC::SignedGreaterThanOrEqual,
                        _ => IntCC::Equal,
                    };
                    let cmp = builder.ins().icmp(cc, a, b);
                    let v = builder.ins().uextend(types::I64, cmp);
                    regs.insert(reg, v);
                }
                string_regs.remove(&reg);
                bytes_regs.remove(&reg);
                float_regs.remove(&reg);
            }

            "concat" if parts.len() >= 4 => {
                let reg: usize = parts[1].parse().unwrap_or(0);
                let a = get_reg(&regs, parts[2]);
                let b = get_reg(&regs, parts[3]);
                reg_source_vars.remove(&reg);
                struct_regs.remove(&reg);
                let concat_name = if runtime_funcs.contains_key("forge_concat_cstr") {
                    "forge_concat_cstr"
                } else {
                    "forge_string_concat"
                };
                if let Some(&concat_id) = runtime_funcs.get(concat_name) {
                    let concat_ref = *func_ref_cache.entry(concat_id).or_insert_with(|| {
                        codegen.module.declare_func_in_func(concat_id, builder.func)
                    });
                    let call = builder.ins().call(concat_ref, &[a, b]);
                    if !builder.func.dfg.inst_results(call).is_empty() {
                        regs.insert(reg, builder.func.dfg.first_result(call));
                        string_regs.insert(reg);
                    } else {
                        regs.insert(reg, a);
                    }
                } else {
                    regs.insert(reg, a);
                }
                string_regs.insert(reg);
                bytes_regs.remove(&reg);
                float_regs.remove(&reg);
            }

            "call" if parts.len() >= 4 => {
                let reg: usize = parts[1].parse().unwrap_or(0);
                let (mut fname, retkind, nargs, arg_start) = parse_call_shape(&parts)
                    .ok_or_else(|| {
                        CompileError::ModuleError(format!(
                            "ir consumer: malformed call instruction in {}: {}",
                            func_name, line
                        ))
                    })?;
                reg_source_vars.remove(&reg);
                struct_regs.remove(&reg);

                // Struct constructor: call REG StructName N args...
                // If fname is a known struct, emit __struct_alloc + sstore
                if struct_layouts.contains_key(fname) {
                    let mut args: Vec<Value> = Vec::new();
                    for j in 0..nargs {
                        if j + arg_start < parts.len() {
                            args.push(get_reg(&regs, parts[j + arg_start]));
                        }
                    }
                    // Allocate struct
                    if let Some(&alloc_id) = runtime_funcs.get("forge_struct_alloc") {
                        let alloc_ref = *func_ref_cache.entry(alloc_id).or_insert_with(|| {
                            codegen.module.declare_func_in_func(alloc_id, builder.func)
                        });
                        let nfields = builder.ins().iconst(types::I64, nargs as i64);
                        let alloc_call = builder.ins().call(alloc_ref, &[nfields]);
                        let ptr = builder.func.dfg.first_result(alloc_call);
                        // Store each field
                        for (i, arg) in args.iter().enumerate() {
                            let offset = (i * 8) as i32;
                            builder.ins().store(
                                cranelift::codegen::ir::MemFlags::new(),
                                *arg,
                                ptr,
                                offset,
                            );
                        }
                        regs.insert(reg, ptr);
                        struct_regs.insert(reg, fname.to_string());
                        string_regs.remove(&reg);
                        bytes_regs.remove(&reg);
                        float_regs.remove(&reg);
                    } else {
                        regs.insert(reg, builder.ins().iconst(types::I64, 0));
                        struct_regs.remove(&reg);
                        string_regs.remove(&reg);
                        bytes_regs.remove(&reg);
                        float_regs.remove(&reg);
                    }
                } else {
                    // tcp_read with 2 args → tcp_read2 (different runtime function)
                    if fname == "tcp_read" && nargs == 2 {
                        fname = "tcp_read2";
                    }
                    // __list_get on a string → char_at (string indexing)
                    if (fname == "__list_get" || fname == "__index")
                        && nargs >= 1
                        && parts.len() > arg_start
                    {
                        if let Ok(arg_reg) = parts[arg_start].parse::<usize>() {
                            if string_regs.contains(&arg_reg) {
                                fname = "char_at";
                            }
                        }
                    }
                    let mut args: Vec<Value> = Vec::new();
                    for j in 0..nargs {
                        if j + arg_start < parts.len() {
                            args.push(get_reg(&regs, parts[j + arg_start]));
                        }
                    }
                    // Note: `len` maps to forge_auto_len which handles both
                    // strings and lists at runtime via magic number check.
                    // Look up function: user-defined first, then runtime with name resolution.
                    let resolved_name = resolve_func_name(fname);
                    let mut runtime_call = false;
                    let fid = if let Some(&fid) = declared_funcs.get(fname) {
                        Some(fid)
                    } else if let Some(&fid) = runtime_funcs.get(resolved_name) {
                        runtime_call = true;
                        Some(fid)
                    } else if let Some(&fid) = runtime_funcs.get(fname) {
                        runtime_call = true;
                        Some(fid)
                    } else {
                        None
                    };

                    if let Some(fid) = fid {
                        let fref = *func_ref_cache.entry(fid).or_insert_with(|| {
                            codegen.module.declare_func_in_func(fid, builder.func)
                        });
                        // Check function signature for f64 params and bitcast as needed
                        let sig_ref = builder.func.dfg.ext_funcs[fref].signature;
                        let sig = &builder.func.dfg.signatures[sig_ref];
                        let param_types: Vec<types::Type> =
                            sig.params.iter().map(|p| p.value_type).collect();
                        let mut typed_args = args.clone();
                        for (i, arg) in typed_args.iter_mut().enumerate() {
                            if i < param_types.len() && param_types[i] == types::F64 {
                                // Bitcast i64 → f64 for float params
                                *arg = builder.ins().bitcast(
                                    types::F64,
                                    cranelift::codegen::ir::MemFlags::new(),
                                    *arg,
                                );
                            }
                        }
                        let call = builder.ins().call(fref, &typed_args);
                        let mut returns_float = false;
                        if !builder.func.dfg.inst_results(call).is_empty() {
                            let result = builder.func.dfg.first_result(call);
                            let result_ty = builder.func.dfg.value_type(result);
                            if result_ty == types::F64 {
                                // Bitcast f64 → i64 for uniform handling
                                let cast = builder.ins().bitcast(
                                    types::I64,
                                    cranelift::codegen::ir::MemFlags::new(),
                                    result,
                                );
                                regs.insert(reg, cast);
                                returns_float = true;
                            } else {
                                // Normalize i64 results: iadd 0 works around a Cranelift
                                // register state issue with struct-from-list returns
                                let zero = builder.ins().iconst(types::I64, 0);
                                let mut normalized = builder.ins().iadd(result, zero);
                                if runtime_call {
                                    normalized =
                                        normalize_runtime_result(&mut builder, normalized, retkind);
                                }
                                regs.insert(reg, normalized);
                            }
                        } else {
                            regs.insert(reg, builder.ins().iconst(types::I64, 0));
                        }
                        if retkind == "string" {
                            string_regs.insert(reg);
                        } else {
                            string_regs.remove(&reg);
                        }
                        if retkind == "bytes" {
                            bytes_regs.insert(reg);
                        } else {
                            bytes_regs.remove(&reg);
                        }
                        if retkind == "float" || returns_float {
                            float_regs.insert(reg);
                        } else {
                            float_regs.remove(&reg);
                        }
                        if let Some(struct_name) = explicit_struct_name_from_retkind(retkind) {
                            struct_regs.insert(reg, struct_name.to_string());
                        } else {
                            struct_regs.remove(&reg);
                        }
                    } else if let Some(&var) = named_vars.get(fname) {
                        // Indirect call through function pointer variable
                        let fn_ptr = builder.use_var(var);
                        let mut sig = codegen.module.make_signature();
                        for _ in &args {
                            sig.params.push(AbiParam::new(types::I64));
                        }
                        sig.returns.push(AbiParam::new(types::I64));
                        let sig_ref = builder.import_signature(sig);
                        let call = builder.ins().call_indirect(sig_ref, fn_ptr, &args);
                        regs.insert(reg, builder.func.dfg.first_result(call));
                        struct_regs.remove(&reg);
                        string_regs.remove(&reg);
                        bytes_regs.remove(&reg);
                        float_regs.remove(&reg);
                    } else {
                        regs.insert(reg, builder.ins().iconst(types::I64, 0));
                        struct_regs.remove(&reg);
                        string_regs.remove(&reg);
                        bytes_regs.remove(&reg);
                        float_regs.remove(&reg);
                    }
                } // end struct constructor else
            }

            "store" if parts.len() >= 3 => {
                let name = parts[1].to_string();
                let val = get_reg(&regs, parts[2]);
                // Propagate types through store
                if let Ok(src_reg) = parts[2].parse::<usize>() {
                    if let Some(struct_name) = struct_regs.get(&src_reg) {
                        struct_vars.insert(name.clone(), struct_name.clone());
                    } else {
                        struct_vars.remove(&name);
                    }
                    if string_regs.contains(&src_reg) {
                        string_vars.insert(name.clone());
                    } else {
                        string_vars.remove(&name);
                    }
                    if bytes_regs.contains(&src_reg) {
                        bytes_vars.insert(name.clone());
                    } else {
                        bytes_vars.remove(&name);
                    }
                    if float_regs.contains(&src_reg) {
                        float_vars.insert(name.clone());
                    } else {
                        float_vars.remove(&name);
                    }
                }
                // Check if this is a global variable
                if let Some(&data_id) = global_data.get(&name) {
                    let gv = codegen.module.declare_data_in_func(data_id, builder.func);
                    let addr = builder.ins().global_value(types::I64, gv);
                    builder
                        .ins()
                        .store(cranelift::codegen::ir::MemFlags::new(), val, addr, 0);
                } else {
                    let var = if let Some(&v) = named_vars.get(&name) {
                        v
                    } else {
                        #[cfg(forge_cranelift_new_api)]
                        let v = declare_i64_var(&mut builder);
                        #[cfg(not(forge_cranelift_new_api))]
                        let v = declare_i64_var(&mut builder, &mut next_var_id);
                        named_vars.insert(name, v);
                        v
                    };
                    builder.def_var(var, val);
                }
            }

            "load" if parts.len() >= 3 => {
                let reg: usize = parts[1].parse().unwrap_or(0);
                let name = parts[2];
                reg_source_vars.insert(reg, name.to_string());
                if let Some(struct_name) = struct_vars.get(name) {
                    struct_regs.insert(reg, struct_name.clone());
                } else if struct_layouts.contains_key(name) {
                    struct_regs.insert(reg, name.to_string());
                } else {
                    struct_regs.remove(&reg);
                }
                // Check if this is a global variable
                if let Some(&data_id) = global_data.get(name) {
                    let gv = codegen.module.declare_data_in_func(data_id, builder.func);
                    let addr = builder.ins().global_value(types::I64, gv);
                    let val = builder.ins().load(
                        types::I64,
                        cranelift::codegen::ir::MemFlags::new(),
                        addr,
                        0,
                    );
                    regs.insert(reg, val);
                } else if let Some(&var) = named_vars.get(name) {
                    let val = builder.use_var(var);
                    regs.insert(reg, val);
                } else if struct_layouts.contains_key(name) {
                    regs.insert(reg, builder.ins().iconst(types::I64, 0));
                } else {
                    return Err(CompileError::ModuleError(format!(
                        "ir consumer: unknown load source '{}' in {}",
                        name, func_name
                    )));
                }
                // Propagate types through load
                if string_vars.contains(name) || string_global_names.contains(name) {
                    string_regs.insert(reg);
                } else {
                    string_regs.remove(&reg);
                }
                if bytes_vars.contains(name) {
                    bytes_regs.insert(reg);
                } else {
                    bytes_regs.remove(&reg);
                }
                if float_vars.contains(name) {
                    float_regs.insert(reg);
                } else {
                    float_regs.remove(&reg);
                }
            }

            "field" if parts.len() >= 4 => {
                let reg: usize = parts[1].parse().unwrap_or(0);
                let obj_reg: usize = parts[2].parse().unwrap_or(usize::MAX);
                let obj_struct_name = struct_regs.get(&obj_reg).cloned();
                let obj = get_reg(&regs, parts[2]);
                let (offset, field_retkind) = if parts.len() >= 6 && parts[3].parse::<i32>().is_ok() {
                    (parts[3].parse::<i32>().unwrap_or(0), Some(parts[4]))
                } else if parts.len() == 4 {
                    let field_name = parts[3];
                    if let Ok(idx) = field_name.parse::<usize>() {
                        ((idx * 8) as i32, None)
                    } else if let Some(struct_name) = obj_struct_name.as_deref() {
                        let offset = field_offset_in_struct(struct_layouts, struct_name, field_name)
                            .ok_or_else(|| {
                                CompileError::ModuleError(format!(
                                    "ir consumer: unknown field {} on {} in {}",
                                    field_name, struct_name, func_name
                                ))
                            })?;
                        (offset, None)
                    } else {
                        let offset = unique_field_offset_for_name(struct_layouts, field_name)
                            .ok_or_else(|| {
                                CompileError::ModuleError(format!(
                                    "ir consumer: ambiguous field instruction in {}: {}",
                                    func_name, line
                                ))
                            })?;
                        (offset, None)
                    }
                } else {
                    return Err(CompileError::ModuleError(format!(
                        "ir consumer: malformed field instruction in {}: {}",
                        func_name, line
                    )));
                };
                let raw = builder.ins().load(
                    types::I64,
                    cranelift::codegen::ir::MemFlags::new(),
                    obj,
                    offset,
                );
                let zero = builder.ins().iconst(types::I64, 0);
                let v = builder.ins().iadd(raw, zero);
                regs.insert(reg, v);
                reg_source_vars.remove(&reg);
                struct_regs.remove(&reg);
                if let Some(retkind) = field_retkind {
                    if retkind == "string" {
                        string_regs.insert(reg);
                    } else {
                        string_regs.remove(&reg);
                    }
                    if retkind == "bytes" {
                        bytes_regs.insert(reg);
                    } else {
                        bytes_regs.remove(&reg);
                    }
                    if retkind == "float" {
                        float_regs.insert(reg);
                    } else {
                        float_regs.remove(&reg);
                    }
                    if let Some(struct_name) = explicit_struct_name_from_retkind(retkind) {
                        struct_regs.insert(reg, struct_name.to_string());
                    }
                } else {
                    string_regs.remove(&reg);
                    bytes_regs.remove(&reg);
                    float_regs.remove(&reg);
                }
            }

            "funcref" if parts.len() >= 3 => {
                let reg: usize = parts[1].parse().unwrap_or(0);
                let fname = parts[2];
                if let Some(&fid) = declared_funcs.get(fname) {
                    let fref = *func_ref_cache
                        .entry(fid)
                        .or_insert_with(|| codegen.module.declare_func_in_func(fid, builder.func));
                    let addr = builder.ins().func_addr(types::I64, fref);
                    regs.insert(reg, addr);
                } else {
                    return Err(CompileError::ModuleError(format!(
                        "ir consumer: unknown function reference '{}' in {}",
                        fname, func_name
                    )));
                }
                reg_source_vars.remove(&reg);
                struct_regs.remove(&reg);
                string_regs.remove(&reg);
                bytes_regs.remove(&reg);
                float_regs.remove(&reg);
            }

            "sstore" if parts.len() >= 4 => {
                // Store field in struct: sstore struct_reg field_idx value_reg
                let struct_val = get_reg(&regs, parts[1]);
                let field_idx: i32 = parts[2].parse().unwrap_or(0);
                let val = get_reg(&regs, parts[3]);
                let offset = field_idx * 8;
                builder.ins().store(
                    cranelift::codegen::ir::MemFlags::new(),
                    val,
                    struct_val,
                    offset,
                );
            }

            "ret" if parts.len() >= 2 => {
                if func_name == "main" {
                    let zero = builder.ins().iconst(types::I64, 0);
                    builder.ins().return_(&[zero]);
                } else {
                    let val = get_reg(&regs, parts[1]);
                    builder.ins().return_(&[val]);
                }
                terminated = true;
            }

            "brif" if parts.len() >= 4 => {
                let cond = get_reg(&regs, parts[1]);
                let then_label = parts[2];
                let else_label = parts[3];
                let cond_bool = builder.ins().icmp_imm(IntCC::NotEqual, cond, 0);
                let then_block = labels.get(then_label).copied().unwrap_or(entry_block);
                let else_block = labels.get(else_label).copied().unwrap_or(entry_block);
                builder
                    .ins()
                    .brif(cond_bool, then_block, &[], else_block, &[]);
                terminated = true;
            }

            "jmp" if parts.len() >= 2 => {
                let target = parts[1];
                // Redirect break targets that incorrectly loop back
                let actual_target = break_redirects
                    .get(target)
                    .map(|s| s.as_str())
                    .unwrap_or(target);
                let block = labels.get(actual_target).copied().unwrap_or(entry_block);
                builder.ins().jump(block, &[]);
                terminated = true;
            }

            "label" if parts.len() >= 2 => {
                let block = labels[parts[1]];
                if !terminated {
                    builder.ins().jump(block, &[]);
                }
                builder.switch_to_block(block);
                terminated = false;
            }

            _ => {
                // Skip unknown instructions (comments, etc.)
            }
        }
    }

    // Default return if not terminated
    if !terminated {
        let zero = builder.ins().iconst(types::I64, 0);
        builder.ins().return_(&[zero]);
    }

    builder.seal_all_blocks();
    builder.finalize();

    codegen
        .module
        .define_function(func_id, &mut ctx)
        .map_err(|e| {
            eprintln!(
                "IR consumer verifier error in '{}': {}\nIR:\n{}",
                func_name,
                e,
                ctx.func.display()
            );
            CompileError::ModuleError(format!("IR consumer: {}", e))
        })?;

    Ok(())
}

fn parse_call_shape<'a>(parts: &'a [&'a str]) -> Option<(&'a str, &'a str, usize, usize)> {
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

fn explicit_struct_name_from_retkind(retkind: &str) -> Option<&str> {
    if let Some(name) = retkind.strip_prefix("struct:") {
        return Some(name);
    }
    None
}

fn field_offset_in_struct(
    struct_layouts: &HashMap<String, Vec<String>>,
    struct_name: &str,
    field_name: &str,
) -> Option<i32> {
    let fields = struct_layouts.get(struct_name)?;
    let idx = fields.iter().position(|field| field == field_name)?;
    Some((idx * 8) as i32)
}

fn unique_field_offset_for_name(
    struct_layouts: &HashMap<String, Vec<String>>,
    field_name: &str,
) -> Option<i32> {
    let mut matching_positions: Vec<usize> = struct_layouts
        .values()
        .filter_map(|fields| fields.iter().position(|field| field == field_name))
        .collect();
    matching_positions.sort_unstable();
    matching_positions.dedup();
    if matching_positions.len() == 1 {
        return Some((matching_positions[0] * 8) as i32);
    }
    None
}


/// Map IR function names to runtime_funcs keys.
/// Returns the key that exists in the runtime_funcs HashMap.
fn resolve_func_name(name: &str) -> &str {
    match name {
        // String operations
        "print" => "forge_smart_print",
        "print_err" => "print_err",
        "to_string" | "int_to_string" => "to_string",
        "bool_to_string" => "bool_to_string",
        "float_to_string" => "float_to_string",
        "smart_to_string" => "smart_to_string",
        "identity" => "identity",
        "chr" => "chr",
        "ord" => "ord",
        "string_len" => "string_len",
        "substring" => "substring",
        "contains" | "string_contains" => "contains",
        "starts_with" => "starts_with",
        "ends_with" => "ends_with",
        "trim" => "trim",
        "split" => "split",
        "to_upper" => "to_upper",
        "to_lower" => "to_lower",
        "replace" => "replace",
        "repeat" => "repeat",
        "index_of" | "string_index_of" => "index_of",
        "last_index_of" | "string_last_index_of" => "last_index_of",
        "string_is_empty" => "string_is_empty",
        "pad_left" => "pad_left",
        "pad_right" => "pad_right",
        "char_at" => "char_at",
        "chars" => "chars",
        "reverse" => "reverse",
        // List operations
        "len" => "forge_auto_len",
        "join" => "forge_list_join",
        "list_join" => "list_join",
        "push" => "forge_list_push_value",
        "pop" => "forge_list_pop",
        "remove" | "list_remove" => "list_remove",
        "is_empty" | "list_is_empty" => "list_is_empty",
        "clear" | "list_clear" => "list_clear",
        "list_reverse" => "list_reverse",
        "list_contains" => "list_contains",
        "list_contains_string" => "list_contains_string",
        "list_index_of" => "list_index_of",
        "list_index_of_string" => "list_index_of_string",
        "set" => "forge_list_set_value",
        "map" | "list_map" => "forge_list_map",
        "filter" | "list_filter" => "forge_list_filter",
        "reduce" | "list_reduce" => "forge_list_reduce",
        "each" | "list_each" => "forge_list_each",
        "sort" => "forge_list_sort",
        "sort_strings" => "forge_list_sort_strings",
        "slice" => "forge_list_slice",
        // Map operations
        "insert" => "insert",
        "map_insert" => "map_insert",
        "map_insert_ikey" => "map_insert_ikey",
        "map_get" => "map_get",
        "map_get_ikey" => "map_get_ikey",
        "get_default" | "map_get_default" => "get_default",
        "map_get_default_ikey" => "map_get_default_ikey",
        "contains_key" | "map_contains_key" => "contains_key",
        "map_contains_ikey" => "map_contains_ikey",
        "keys" | "map_keys" => "keys",
        "map_values" => "map_values",
        "map_remove" => "map_remove",
        "map_remove_ikey" => "map_remove_ikey",
        "map_clear" => "map_clear",
        "map_is_empty" => "map_is_empty",
        "map_len" => "map_len",
        // Set operations
        "set_add" => "set_add",
        "set_add_int" => "forge_set_add_int_handle",
        "set_contains" => "set_contains",
        "set_contains_int" => "forge_set_contains_int_handle",
        "set_remove" => "set_remove",
        "set_remove_int" => "forge_set_remove_int_handle",
        "set_clear" => "set_clear",
        "set_is_empty" => "set_is_empty",
        "set_len" => "set_len",
        // Internal IR instructions
        "__list_get" | "__index" => "forge_list_get_value",
        "__list_new" => "forge_list_new_default",
        "__list_push" => "forge_list_push_value",
        "__map_new" => "forge_map_new_default",
        "__map_new_int" => "forge_map_new_int",
        "__set_new" => "forge_set_new_default",
        "__set_new_int" => "forge_set_new_int",
        "__struct_alloc" => "forge_struct_alloc",
        "__closure_set_env" => "forge_closure_set_env",
        "__closure_get_env" => "forge_closure_get_env",
        "__str_eq" => "forge_cstring_eq",
        "bytes_from_string_utf8" => "bytes_from_string_utf8",
        "bytes_to_string_utf8" => "bytes_to_string_utf8",
        "bytes_len" => "bytes_len",
        "bytes_is_empty" => "bytes_is_empty",
        "bytes_get" => "bytes_get",
        "bytes_slice" => "bytes_slice",
        "bytes_concat" => "bytes_concat",
        "bytes_eq" => "bytes_eq",
        "byte_buffer_new" => "byte_buffer_new",
        "byte_buffer_with_capacity" => "byte_buffer_with_capacity",
        "byte_buffer_write" => "byte_buffer_write",
        "byte_buffer_write_byte" => "byte_buffer_write_byte",
        "byte_buffer_bytes" => "byte_buffer_bytes",
        "byte_buffer_clear" => "byte_buffer_clear",
        // Numeric / math
        "abs" => "forge_abs",
        "min" => "forge_min",
        "max" => "forge_max",
        "clamp" => "forge_clamp",
        "pow" => "forge_pow",
        "sqrt" => "forge_sqrt",
        "floor" => "forge_floor",
        "ceil" => "forge_ceil",
        "round" => "forge_round",
        "sin" => "forge_sin",
        "cos" => "forge_cos",
        "tan" => "forge_tan",
        "asin" => "forge_asin",
        "acos" => "forge_acos",
        "atan" => "forge_atan",
        "atan2" => "forge_atan2",
        "math_log" => "forge_log",
        "math_log10" => "forge_log10",
        "math_log2" => "forge_log2",
        "math_exp" => "forge_exp",
        "math_abs_float" => "forge_abs_float",
        "to_float" => "forge_int_to_float",
        "to_int" => "forge_float_to_int",
        "random_int" => "forge_random_int",
        "random_seed" => "forge_random_seed",
        "random_string" => "forge_random_string",
        "random_float" => "forge_random_float",
        "int_to_hex" => "forge_int_to_hex",
        "int_to_oct" => "forge_int_to_oct",
        "int_to_bin" => "forge_int_to_bin",
        "fmt_float" => "forge_fmt_float",
        "format_int" => "forge_format_int",
        // Bitwise
        "bit_and" => "forge_bit_and",
        "bit_or" => "forge_bit_or",
        "bit_xor" => "forge_bit_xor",
        "bit_not" => "forge_bit_not",
        "bit_shl" => "forge_bit_shl",
        "bit_shr" => "forge_bit_shr",
        // IO / system
        "read_file" => "read_file",
        "read_file_bytes" => "read_file_bytes",
        "write_file" => "write_file",
        "append_file" => "append_file",
        "write_file_bytes" => "write_file_bytes",
        "append_file_bytes" => "append_file_bytes",
        "file_open_read" => "file_open_read",
        "file_open_write" => "file_open_write",
        "file_open_append" => "file_open_append",
        "file_read" => "file_read",
        "file_write" => "file_write",
        "file_read_bytes" => "file_read_bytes",
        "file_write_bytes" => "file_write_bytes",
        "file_close" => "file_close",
        "file_exists" => "file_exists",
        "dir_exists" => "dir_exists",
        "exec" => "exec",
        "exec_output" => "exec_output",
        "exit" => "exit",
        "env" => "env",
        "args" => "args",
        "dns_resolve" => "dns_resolve",
        "tcp_listen" => "tcp_listen",
        "tcp_connect" => "tcp_connect",
        "tcp_accept" => "tcp_accept",
        "tcp_read" => "tcp_read",
        "tcp_read2" => "tcp_read2",
        "tcp_write" => "tcp_write",
        "tcp_set_timeout" => "tcp_set_timeout",
        "tcp_close" => "tcp_close",
        "process_spawn" => "process_spawn",
        "process_write" => "process_write",
        "process_read" => "process_read",
        "process_read_err" => "process_read_err",
        "process_write_bytes" => "process_write_bytes",
        "process_read_bytes" => "process_read_bytes",
        "process_read_err_bytes" => "process_read_err_bytes",
        "process_wait" => "process_wait",
        "process_kill" => "process_kill",
        "process_close" => "process_close",
        // Crypto / encoding
        "sha256" => "forge_sha256",
        "fnv1a" => "forge_fnv1a",
        "b64_encode" => "forge_b64_encode",
        "b64_decode" => "forge_b64_decode",
        "to_hex" => "forge_hex_encode",
        "from_hex" => "forge_from_hex",
        // Path
        "path_join" | "join_path" => "forge_path_join",
        "path_dir" | "dir" => "forge_path_dir",
        "path_base" | "base" => "forge_path_basename",
        "path_ext" | "ext" => "forge_path_ext",
        "path_stem" | "stem" => "forge_path_stem",
        // Logging
        // JSON
        // URL
        "url_parse" => "forge_url_parse",
        "url_scheme" => "forge_url_scheme",
        "url_host" => "forge_url_host",
        "url_port" => "forge_url_port",
        "url_path" => "forge_url_path",
        "url_query" => "forge_url_query",
        "url_fragment" => "forge_url_fragment",
        "url_to_string" => "forge_url_to_string",
        "url_encode" => "forge_url_encode",
        "url_decode" => "forge_url_decode",
        "tcp_read_bytes" => "tcp_read_bytes",
        "tcp_write_bytes" => "tcp_write_bytes",
        // Concurrency
        "spawn" => "forge_spawn",
        "await" => "forge_await",
        "Mutex" => "forge_mutex_new",
        "WaitGroup" => "forge_waitgroup_new",
        "Semaphore" => "forge_semaphore_new",
        "lock" => "forge_mutex_lock",
        "unlock" => "forge_mutex_unlock",
        "wait" => "forge_waitgroup_wait",
        "done" => "forge_waitgroup_done",
        "add" => "forge_waitgroup_add",
        "acquire" => "forge_semaphore_acquire",
        "release" => "forge_semaphore_release",
        _ => name,
    }
}

fn get_reg(regs: &HashMap<usize, Value>, s: &str) -> Value {
    let reg: usize = s
        .parse()
        .unwrap_or_else(|_| panic!("IR consumer: invalid register reference '{}'", s));
    regs.get(&reg)
        .copied()
        .unwrap_or_else(|| panic!("IR consumer: missing register {}", reg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_call_shape_requires_explicit_retkind() {
        let old = vec!["call", "7", "print", "1", "3"];
        let new = vec!["call", "8", "char_at", "string", "2", "1", "2"];
        let imported_struct = vec!["call", "9", "advance_token", "struct:Token", "0"];

        assert_eq!(parse_call_shape(&old), None);
        assert_eq!(parse_call_shape(&new), Some(("char_at", "string", 2, 5)));
        assert_eq!(
            parse_call_shape(&imported_struct),
            Some(("advance_token", "struct:Token", 0, 5))
        );
    }

    #[test]
    fn explicit_struct_name_from_retkind_requires_struct_prefix() {
        assert_eq!(explicit_struct_name_from_retkind("struct:Token"), Some("Token"));
        assert_eq!(explicit_struct_name_from_retkind("Token"), None);
        assert_eq!(explicit_struct_name_from_retkind("string"), None);
        assert_eq!(explicit_struct_name_from_retkind("unknown"), None);
    }
}
