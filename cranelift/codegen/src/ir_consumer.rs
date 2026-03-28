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
use std::collections::HashMap;

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
                let raw = rest.trim_matches('"');
                let mut content = String::new();
                let bytes = raw.as_bytes();
                let mut j = 0;
                while j < bytes.len() {
                    if bytes[j] == b'\\' && j + 1 < bytes.len() {
                        match bytes[j + 1] {
                            b'n' => { content.push('\n'); j += 2; }
                            b't' => { content.push('\t'); j += 2; }
                            b'\\' => { content.push('\\'); j += 2; }
                            b'"' => { content.push('"'); j += 2; }
                            b'r' => { content.push('\r'); j += 2; }
                            b'0' => { content.push('\0'); j += 2; }
                            _ => { content.push(bytes[j] as char); j += 1; }
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
                    let fields: Vec<String> = parts[2..].iter().map(|s| s.to_string()).collect();
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
                    let data_id = codegen.module
                        .declare_data(&gname, Linkage::Local, true, false)
                        .map_err(|e| CompileError::ModuleError(e.to_string()))?;
                    let mut desc = DataDescription::new();
                    let init_val: i64 = if init_kind == "list" || init_kind == "map" || init_kind == "set" {
                        0
                    } else if init_kind.starts_with("str:") {
                        0 // will be patched in __init_globals
                    } else {
                        init_kind.parse().unwrap_or(0)
                    };
                    desc.define(init_val.to_le_bytes().to_vec().into_boxed_slice());
                    codegen.module.define_data(data_id, &desc)
                        .map_err(|e| CompileError::ModuleError(e.to_string()))?;
                    global_data.insert(gname.clone(), data_id);
                    // Track str: globals that need runtime initialization
                    if init_kind.starts_with("str:") {
                        let str_id = &init_kind[4..]; // e.g., "m0s0"
                        str_globals.push((gname, str_id.to_string()));
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
                    let func_id = codegen
                        .module
                        .declare_function(name, Linkage::Export, &sig)
                        .map_err(|e| CompileError::ModuleError(e.to_string()))?;
                    declared_funcs.insert(name.to_string(), func_id);
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
            let func_id =
                crate::declare_string_data(&mut codegen.module, &name, content)
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
        let nparam: usize = parts[2].parse().unwrap_or(0);
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
            )?;
        }
    }

    Ok(declared_funcs)
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
    let mut named_vars: HashMap<String, Variable> = HashMap::new();
    let mut labels: HashMap<String, Block> = HashMap::new();
    let mut next_var_id: u32 = 0;

    // Create a zero constant that can be used as fallback for undefined registers
    let zero_val = builder.ins().iconst(types::I64, 0);
    regs.insert(usize::MAX, zero_val); // sentinel

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
            if let (Some(&data_id), Some(&sfunc_id)) = (global_data.get(gname.as_str()), string_funcs.get(str_id.as_str())) {
                let sf_ref = codegen.module.declare_func_in_func(sfunc_id, builder.func);
                let str_val = builder.ins().call(sf_ref, &[]);
                let str_result = builder.func.dfg.first_result(str_val);
                let gv = codegen.module.declare_data_in_func(data_id, builder.func);
                let addr = builder.ins().global_value(types::I64, gv);
                builder.ins().store(cranelift::codegen::ir::MemFlags::new(), str_result, addr, 0);
            }
        }
    }

    for (i, name) in param_names.iter().enumerate() {
        if i < block_params.len() {
            let var = Variable::from_u32(next_var_id);
            next_var_id += 1;
            builder.declare_var(var, types::I64);
            builder.def_var(var, block_params[i]);
            named_vars.insert(name.clone(), var);
            regs.insert(i, block_params[i]);
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
            }

            "fconst" if parts.len() >= 3 => {
                let reg: usize = parts[1].parse().unwrap_or(0);
                let fval: f64 = parts[2].parse().unwrap_or(0.0);
                // Create actual f64 constant, then bitcast to i64 for uniform handling
                let fv = builder.ins().f64const(fval);
                let v = builder.ins().bitcast(types::I64, cranelift::codegen::ir::MemFlags::new(), fv);
                regs.insert(reg, v);
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
            }

            "bnot" if parts.len() >= 3 => {
                let reg: usize = parts[1].parse().unwrap_or(0);
                let a = get_reg(&regs, parts[2]);
                let v = builder.ins().bnot(a);
                regs.insert(reg, v);
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
            }

            "fadd" | "fsub" | "fmul" | "fdiv" if parts.len() >= 4 => {
                let reg: usize = parts[1].parse().unwrap_or(0);
                let a = get_reg(&regs, parts[2]);
                let b = get_reg(&regs, parts[3]);
                // Bitcast i64 → f64
                let fa = builder.ins().bitcast(types::F64, cranelift::codegen::ir::MemFlags::new(), a);
                let fb = builder.ins().bitcast(types::F64, cranelift::codegen::ir::MemFlags::new(), b);
                let fv = match parts[0] {
                    "fadd" => builder.ins().fadd(fa, fb),
                    "fsub" => builder.ins().fsub(fa, fb),
                    "fmul" => builder.ins().fmul(fa, fb),
                    "fdiv" => builder.ins().fdiv(fa, fb),
                    _ => unreachable!(),
                };
                // Bitcast f64 → i64
                let v = builder.ins().bitcast(types::I64, cranelift::codegen::ir::MemFlags::new(), fv);
                regs.insert(reg, v);
            }

            "add" | "sub" | "mul" | "div" | "mod" if parts.len() >= 4 => {
                let reg: usize = parts[1].parse().unwrap_or(0);
                let a = get_reg(&regs, parts[2]);
                let b = get_reg(&regs, parts[3]);
                let v = match parts[0] {
                    "add" => builder.ins().iadd(a, b),
                    "sub" => builder.ins().isub(a, b),
                    "mul" => builder.ins().imul(a, b),
                    "div" => builder.ins().sdiv(a, b),
                    "mod" => builder.ins().srem(a, b),
                    _ => unreachable!(),
                };
                regs.insert(reg, v);
            }

            "eq" | "neq" | "lt" | "gt" | "lte" | "gte" if parts.len() >= 4 => {
                let reg: usize = parts[1].parse().unwrap_or(0);
                let a = get_reg(&regs, parts[2]);
                let b = get_reg(&regs, parts[3]);
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

            "concat" if parts.len() >= 4 => {
                let reg: usize = parts[1].parse().unwrap_or(0);
                let a = get_reg(&regs, parts[2]);
                let b = get_reg(&regs, parts[3]);
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
                    regs.insert(reg, a);
                }
            }

            "call" if parts.len() >= 4 => {
                let reg: usize = parts[1].parse().unwrap_or(0);
                let fname = parts[2];
                let nargs: usize = parts[3].parse().unwrap_or(0);
                let mut args: Vec<Value> = Vec::new();
                for j in 0..nargs {
                    if j + 4 < parts.len() {
                        args.push(get_reg(&regs, parts[j + 4]));
                    }
                }
                // Look up function: user-defined first, then runtime with name resolution
                let resolved_name = resolve_func_name(fname);
                let fid = declared_funcs
                    .get(fname)
                    .or_else(|| runtime_funcs.get(resolved_name))
                    .or_else(|| runtime_funcs.get(fname))
                    .or_else(|| runtime_funcs.get(&format!("forge_{}", fname)))
                    .copied();

                if let Some(fid) = fid {
                    let fref = *func_ref_cache.entry(fid).or_insert_with(|| {
                        codegen.module.declare_func_in_func(fid, builder.func)
                    });
                    // Check function signature for f64 params and bitcast as needed
                    let sig_ref = builder.func.dfg.ext_funcs[fref].signature;
                    let sig = &builder.func.dfg.signatures[sig_ref];
                    let param_types: Vec<types::Type> = sig.params.iter().map(|p| p.value_type).collect();
                    let ret_types: Vec<types::Type> = sig.returns.iter().map(|r| r.value_type).collect();
                    let mut typed_args = args.clone();
                    for (i, arg) in typed_args.iter_mut().enumerate() {
                        if i < param_types.len() && param_types[i] == types::F64 {
                            // Bitcast i64 → f64 for float params
                            *arg = builder.ins().bitcast(types::F64, cranelift::codegen::ir::MemFlags::new(), *arg);
                        }
                    }
                    let call = builder.ins().call(fref, &typed_args);
                    if !builder.func.dfg.inst_results(call).is_empty() {
                        let result = builder.func.dfg.first_result(call);
                        let result_ty = builder.func.dfg.value_type(result);
                        if result_ty == types::F64 {
                            // Bitcast f64 → i64 for uniform handling
                            let cast = builder.ins().bitcast(types::I64, cranelift::codegen::ir::MemFlags::new(), result);
                            regs.insert(reg, cast);
                        } else {
                            regs.insert(reg, result);
                        }
                    } else {
                        regs.insert(reg, builder.ins().iconst(types::I64, 0));
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
                } else {
                    regs.insert(reg, builder.ins().iconst(types::I64, 0));
                }
            }

            "store" if parts.len() >= 3 => {
                let name = parts[1].to_string();
                let val = get_reg(&regs, parts[2]);
                // Check if this is a global variable
                if let Some(&data_id) = global_data.get(&name) {
                    let gv = codegen.module.declare_data_in_func(data_id, builder.func);
                    let addr = builder.ins().global_value(types::I64, gv);
                    builder.ins().store(cranelift::codegen::ir::MemFlags::new(), val, addr, 0);
                } else {
                    let var = if let Some(&v) = named_vars.get(&name) {
                        v
                    } else {
                        let v = Variable::from_u32(next_var_id);
                        next_var_id += 1;
                        builder.declare_var(v, types::I64);
                        named_vars.insert(name, v);
                        v
                    };
                    builder.def_var(var, val);
                }
            }

            "load" if parts.len() >= 3 => {
                let reg: usize = parts[1].parse().unwrap_or(0);
                let name = parts[2];
                // Check if this is a global variable
                if let Some(&data_id) = global_data.get(name) {
                    let gv = codegen.module.declare_data_in_func(data_id, builder.func);
                    let addr = builder.ins().global_value(types::I64, gv);
                    let val = builder.ins().load(types::I64, cranelift::codegen::ir::MemFlags::new(), addr, 0);
                    regs.insert(reg, val);
                } else if let Some(&var) = named_vars.get(name) {
                    let val = builder.use_var(var);
                    regs.insert(reg, val);
                } else {
                    regs.insert(reg, builder.ins().iconst(types::I64, 0));
                }
            }

            "field" if parts.len() >= 4 => {
                let reg: usize = parts[1].parse().unwrap_or(0);
                let obj = get_reg(&regs, parts[2]);
                let field_name = parts[3];
                // Try numeric field index first (for tuples: .0, .1)
                let offset = if let Ok(idx) = field_name.parse::<usize>() {
                    (idx * 8) as i32
                } else {
                    // Look up field offset from struct layouts
                    struct_layouts
                        .values()
                        .find_map(|fields| {
                            fields.iter().position(|f| f == field_name).map(|i| (i * 8) as i32)
                        })
                        .unwrap_or(0)
                };
                let v = builder.ins().load(
                    types::I64,
                    cranelift::codegen::ir::MemFlags::new(),
                    obj,
                    offset,
                );
                regs.insert(reg, v);
            }

            "funcref" if parts.len() >= 3 => {
                let reg: usize = parts[1].parse().unwrap_or(0);
                let fname = parts[2];
                if let Some(&fid) = declared_funcs.get(fname) {
                    let fref = *func_ref_cache.entry(fid).or_insert_with(|| {
                        codegen.module.declare_func_in_func(fid, builder.func)
                    });
                    let addr = builder.ins().func_addr(types::I64, fref);
                    regs.insert(reg, addr);
                } else {
                    regs.insert(reg, builder.ins().iconst(types::I64, 0));
                }
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
                let val = get_reg(&regs, parts[1]);
                builder.ins().return_(&[val]);
                terminated = true;
            }

            "brif" if parts.len() >= 4 => {
                let cond = get_reg(&regs, parts[1]);
                let then_label = parts[2];
                let else_label = parts[3];
                let cond_bool = builder.ins().icmp_imm(IntCC::NotEqual, cond, 0);
                let then_block = labels.get(then_label).copied().unwrap_or(entry_block);
                let else_block = labels.get(else_label).copied().unwrap_or(entry_block);
                builder.ins().brif(cond_bool, then_block, &[], else_block, &[]);
                terminated = true;
            }

            "jmp" if parts.len() >= 2 => {
                let target = parts[1];
                let block = labels.get(target).copied().unwrap_or(entry_block);
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
            eprintln!("IR consumer verifier error in '{}': {}\nIR:\n{}", func_name, e, ctx.func.display());
            CompileError::ModuleError(format!("IR consumer: {}", e))
        })?;

    Ok(())
}

/// Map IR function names to runtime_funcs keys.
/// Returns the key that exists in the runtime_funcs HashMap.
fn resolve_func_name(name: &str) -> &str {
    match name {
        // String operations
        "print" => "forge_print_cstr",
        "print_err" => "forge_print_err",
        "to_string" | "int_to_string" => "forge_int_to_cstr",
        "bool_to_string" => "forge_bool_to_cstr",
        "float_to_string" => "forge_float_to_cstr",
        "smart_to_string" => "forge_smart_to_string",
        "identity" => "forge_identity",
        "chr" => "forge_chr",
        "ord" => "forge_ord",
        "string_len" => "forge_cstring_len",
        "substring" => "forge_cstring_substring",
        "contains" | "string_contains" => "forge_cstring_contains",
        "starts_with" => "forge_cstring_starts_with",
        "ends_with" => "forge_cstring_ends_with",
        "trim" => "forge_cstring_trim",
        "split" => "forge_cstring_split",
        "to_upper" => "forge_cstring_to_upper",
        "to_lower" => "forge_cstring_to_lower",
        "replace" => "forge_cstring_replace",
        "repeat" => "forge_cstring_repeat",
        "index_of" | "string_index_of" => "forge_cstring_index_of",
        "last_index_of" | "string_last_index_of" => "forge_cstring_last_index_of",
        "string_is_empty" => "forge_cstring_is_empty",
        "pad_left" => "forge_cstring_pad_left",
        "pad_right" => "forge_cstring_pad_right",
        "char_at" => "forge_cstring_char_at",
        "chars" => "forge_cstring_chars",
        "reverse" => "forge_cstring_reverse",
        // List operations
        "len" => "forge_list_len",
        "join" => "forge_list_join",
        "push" => "forge_list_push_value",
        "pop" => "forge_list_pop",
        "remove" | "list_remove" => "forge_list_remove_value",
        "is_empty" | "list_is_empty" => "forge_list_is_empty",
        "clear" | "list_clear" => "forge_list_clear_value",
        "list_reverse" => "forge_list_reverse_value",
        "list_contains" => "forge_list_contains_int",
        "list_index_of" => "forge_list_index_of_int",
        "set" => "forge_list_set_value",
        "sort" => "forge_list_sort",
        "sort_strings" => "forge_list_sort_strings",
        "slice" => "forge_list_slice",
        // Map operations
        "insert" => "forge_map_insert_cstr",
        "map_insert_ikey" => "forge_map_insert_ikey",
        "map_get" => "forge_map_get_cstr",
        "map_get_ikey" => "forge_map_get_ikey",
        "contains_key" | "map_contains_key" => "forge_map_contains_cstr",
        "map_contains_ikey" => "forge_map_contains_ikey",
        "keys" | "map_keys" => "forge_map_keys_cstr",
        "map_values" => "forge_map_values_handle",
        "map_remove" => "forge_map_remove_cstr",
        "map_clear" => "forge_map_clear_handle",
        "map_is_empty" => "forge_map_is_empty_handle",
        "map_len" => "forge_map_len_handle",
        // Set operations
        "set_add" => "forge_set_add_cstr",
        "set_contains" => "forge_set_contains_cstr",
        "set_remove" => "forge_set_remove_cstr",
        "set_clear" => "forge_set_clear_handle",
        "set_is_empty" => "forge_set_is_empty_handle",
        "set_len" => "forge_set_len_handle",
        // Internal IR instructions
        "__list_get" | "__index" => "forge_list_get_value",
        "__list_new" => "forge_list_new_default",
        "__list_push" => "forge_list_push_value",
        "__map_new" => "forge_map_new_default",
        "__map_new_int" => "forge_map_new_int",
        "__set_new" => "forge_set_new_default",
        "__struct_alloc" => "forge_struct_alloc",
        "__closure_set_env" => "forge_closure_set_env",
        "__closure_get_env" => "forge_closure_get_env",
        "__str_eq" => "forge_cstring_eq",
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
        "to_float" => "forge_int_to_float",
        "to_int" => "forge_float_to_int",
        "random_int" => "forge_random_int",
        "random_seed" => "forge_random_seed",
        "random_string" => "forge_random_string",
        "random_float" => "forge_random_float",
        "int_to_hex" => "forge_int_to_hex",
        "int_to_oct" => "forge_int_to_oct",
        "int_to_bin" => "forge_int_to_bin",
        "format_int" => "forge_format_int",
        // Bitwise
        "bit_and" => "forge_bit_and",
        "bit_or" => "forge_bit_or",
        "bit_xor" => "forge_bit_xor",
        "bit_not" => "forge_bit_not",
        "bit_shl" => "forge_bit_shl",
        "bit_shr" => "forge_bit_shr",
        // IO / system
        "read_file" => "forge_read_file",
        "write_file" => "forge_write_file",
        "file_exists" => "forge_file_exists",
        "dir_exists" => "forge_dir_exists",
        "exec" => "forge_exec",
        "exit" => "forge_exit",
        "env" => "forge_env",
        "args" => "forge_args_to_list",
        "dns_resolve" => "forge_dns_resolve",
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
        "path_base" | "base" => "forge_path_base",
        "path_ext" | "ext" => "forge_path_ext",
        "path_stem" | "stem" => "forge_path_stem",
        // Logging
        "log_info" => "forge_log_info",
        "log_warn" => "forge_log_warn",
        "log_error" | "log_debug" => "forge_log_error",
        // JSON
        "parse" => "forge_json_parse",
        "type_of" => "forge_json_type_of",
        "get_string" => "forge_json_get_string",
        "get_int" => "forge_json_get_int",
        "get_float" => "forge_json_get_float",
        "get_bool" => "forge_json_get_bool",
        "object_get" => "forge_json_object_get",
        "object_has" => "forge_json_object_has",
        "object_keys" => "forge_json_object_keys",
        "array_len" => "forge_json_array_len",
        "array_get" => "forge_json_array_get",
        "make_object" => "forge_json_make_object",
        "make_array" => "forge_json_make_array",
        "make_int" => "forge_json_make_int",
        "make_string" => "forge_json_make_string",
        "array_push" => "forge_json_array_push",
        "object_set" => "forge_json_object_set",
        "encode" => "forge_smart_encode",
        // TOML
        "toml_parse" => "forge_toml_parse",
        "toml_type_of" => "forge_toml_type_of",
        "toml_get_string" => "forge_toml_get_string",
        "toml_get_int" => "forge_toml_get_int",
        "toml_get_float" => "forge_toml_get_float",
        "toml_get_bool" => "forge_toml_get_bool",
        "toml_has" => "forge_toml_has",
        "toml_get_array" => "forge_toml_get_array",
        "toml_array_len" => "forge_toml_array_len",
        "toml_array_get" => "forge_toml_array_get",
        "toml_get_table" => "forge_toml_get_table",
        "toml_keys" => "forge_toml_keys",
        // URL
        "scheme" => "forge_url_scheme",
        "host" => "forge_url_host",
        "port" => "forge_url_port",
        "path" => "forge_url_path",
        "query" => "forge_url_query",
        "fragment" => "forge_url_fragment",
        "decode" => "forge_url_decode",
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
    let reg: usize = s.parse().unwrap_or(0);
    regs.get(&reg)
        .or_else(|| regs.get(&usize::MAX)) // fallback to zero sentinel
        .copied()
        .unwrap_or_else(|| panic!("IR consumer: no registers available"))
}
