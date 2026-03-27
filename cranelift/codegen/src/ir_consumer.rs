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
    let mut string_data: Vec<(usize, String)> = Vec::new();

    // Pass 1: collect string data and declare functions
    for line in &lines {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        match parts[0] {
            "string" if parts.len() >= 3 => {
                let idx: usize = parts[1].parse().unwrap_or(0);
                // Extract quoted string content
                let rest = &line[line.find('"').unwrap_or(0)..];
                let content = rest.trim_matches('"').to_string();
                string_data.push((idx, content));
            }
            "func" if parts.len() >= 4 => {
                let name = parts[1];
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
            _ => {}
        }
    }

    // Declare string data functions
    let mut string_funcs: HashMap<usize, FuncId> = HashMap::new();
    for (idx, content) in &string_data {
        let name = format!("__irstr_{}", idx);
        let func_id =
            crate::declare_string_data(&mut codegen.module, &name, content)
                .map_err(|e| CompileError::ModuleError(format!("string data: {:?}", e)))?;
        string_funcs.insert(*idx, func_id);
    }

    // Pass 2: compile function bodies
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

        // Compile this function
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
    string_funcs: &HashMap<usize, FuncId>,
) -> Result<(), CompileError> {
    let mut ctx = codegen.module.make_context();

    // Build signature
    for _ in param_names {
        ctx.func.signature.params.push(AbiParam::new(types::I64));
    }
    ctx.func.signature.returns.push(AbiParam::new(types::I64));

    let mut builder_ctx = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);

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
                let val: i64 = parts[2].parse().unwrap_or(0);
                let v = builder.ins().iconst(types::I64, val);
                regs.insert(reg, v);
            }

            "strref" if parts.len() >= 3 => {
                let reg: usize = parts[1].parse().unwrap_or(0);
                let str_idx: usize = parts[2].parse().unwrap_or(0);
                if let Some(&sf_id) = string_funcs.get(&str_idx) {
                    let sf_ref = codegen.module.declare_func_in_func(sf_id, builder.func);
                    let call = builder.ins().call(sf_ref, &[]);
                    let v = builder.func.dfg.first_result(call);
                    regs.insert(reg, v);
                } else {
                    regs.insert(reg, builder.ins().iconst(types::I64, 0));
                }
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
                    let concat_ref =
                        codegen.module.declare_func_in_func(concat_id, builder.func);
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
                    let fref = codegen.module.declare_func_in_func(fid, builder.func);
                    let call = builder.ins().call(fref, &args);
                    if !builder.func.dfg.inst_results(call).is_empty() {
                        regs.insert(reg, builder.func.dfg.first_result(call));
                    } else {
                        regs.insert(reg, builder.ins().iconst(types::I64, 0));
                    }
                } else {
                    regs.insert(reg, builder.ins().iconst(types::I64, 0));
                }
            }

            "store" if parts.len() >= 3 => {
                let name = parts[1].to_string();
                let val = get_reg(&regs, parts[2]);
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

            "load" if parts.len() >= 3 => {
                let reg: usize = parts[1].parse().unwrap_or(0);
                let name = parts[2];
                if let Some(&var) = named_vars.get(name) {
                    let val = builder.use_var(var);
                    regs.insert(reg, val);
                } else {
                    regs.insert(reg, builder.ins().iconst(types::I64, 0));
                }
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
        .map_err(|e| CompileError::ModuleError(format!("IR consumer: {}", e)))?;

    Ok(())
}

/// Map Forge method/function names to runtime function names
fn resolve_func_name(name: &str) -> &str {
    match name {
        "print" => "forge_print_cstr",
        "print_err" => "forge_print_err",
        "to_string" => "forge_int_to_cstr",
        "to_int" => "forge_float_to_int",
        "to_float" => "forge_int_to_float",
        "len" => "forge_list_len",
        "__list_get" => "forge_list_get_value",
        "__list_new" => "forge_list_new_default",
        "__list_push" => "forge_list_push_value",
        "__index" => "forge_list_get_value",
        "push" => "forge_list_push_value",
        "pop" => "forge_list_pop",
        "contains" => "forge_cstring_contains",
        "substring" => "forge_cstring_substring",
        "trim" => "forge_cstring_trim",
        "split" => "forge_cstring_split",
        "join" => "forge_list_join",
        "starts_with" => "forge_cstring_starts_with",
        "ends_with" => "forge_cstring_ends_with",
        "replace" => "forge_cstring_replace",
        "to_upper" => "forge_cstring_to_upper",
        "to_lower" => "forge_cstring_to_lower",
        "reverse" => "forge_cstring_reverse",
        "index_of" => "forge_cstring_index_of",
        "chr" => "forge_chr",
        "ord" => "forge_ord",
        "read_file" => "forge_read_file",
        "write_file" => "forge_write_file",
        "file_exists" => "forge_file_exists",
        "dir_exists" => "forge_dir_exists",
        "exec" => "forge_exec",
        "exit" => "forge_exit",
        "args" => "forge_args_to_list",
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
