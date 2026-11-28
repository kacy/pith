//! Function-level IR lowering into Cranelift IR via `ModuleCtx` and `FuncLowerer`.

mod calls;
mod memory;

use super::arith::{
    emit_bits_op, emit_checked_int_arith, emit_checked_int_div_or_mod, iconst_of, plain_arith_op,
};
use super::fast_paths::string_lit_addr;
use super::instrumentation::{cached_func_ref, emit_preempt_safepoint, SanitizeHooks};
use super::ir::{
    get_label, get_reg, parse_i64_operand, parse_reg, push_return_types, result_abi_payload_type,
};
use super::module::emit_main_prologue;
use super::passes::scans::{scan_float_vars, scan_unpackable_opt_regs, RegKinds};
use crate::{CodeGen, CompileError};
use cranelift::codegen::ir::{FuncRef, Function};
use cranelift::prelude::*;
use cranelift_module::{DataId, FuncId, Module};
use std::collections::{HashMap, HashSet};

/// Module-wide read-only context shared across all functions lowered in a
/// single `compile_from_ir` pass. Replaces the 16-parameter `compile_ir_function`
/// signature and caches build-mode flags (`quarantine`, `fast_paths`) once per module.
pub(super) struct ModuleCtx<'a> {
    pub(super) runtime_funcs: &'a HashMap<String, FuncId>,
    pub(super) declared_funcs: &'a HashMap<String, FuncId>,
    pub(super) string_lits: &'a HashMap<String, DataId>,
    pub(super) struct_names: &'a HashSet<String>,
    pub(super) global_data: &'a HashMap<String, DataId>,
    pub(super) str_globals: &'a [(String, String)],
    pub(super) string_global_names: &'a HashSet<String>,
    pub(super) preempt_flag: Option<DataId>,
    pub(super) sanitize: Option<SanitizeHooks>,
    pub(super) quarantine: bool,
    pub(super) fast_paths: bool,
    pub(super) bits_aliases: &'a HashMap<String, &'static str>,
}

/// Per-function lowering state. Owns `tested_closures` directly (eliminating
/// the previous `TESTED_CLOSURES` `thread_local!`).
pub(super) struct FuncLowerer<'a, 'b, 'm, 'ir> {
    pub(super) codegen: &'a mut CodeGen,
    pub(super) mctx: &'m ModuleCtx<'m>,
    pub(super) builder: FunctionBuilder<'b>,
    pub(super) func_name: &'ir str,
    pub(super) func_retkind: &'ir str,
    pub(super) body_lines: &'ir [&'ir str],
    pub(super) func_ref_cache: HashMap<FuncId, FuncRef>,
    pub(super) regs: HashMap<usize, Value>,
    pub(super) kinds: RegKinds,
    pub(super) string_vars: HashSet<String>,
    pub(super) float_vars: HashSet<String>,
    pub(super) literal_values: HashSet<Value>,
    pub(super) named_vars: HashMap<String, Variable>,
    pub(super) defined_labels: HashSet<String>,
    pub(super) labels: HashMap<String, Block>,
    pub(super) loop_exits: HashSet<&'ir str>,
    pub(super) inline_opt_regs: HashSet<usize>,
    pub(super) virtual_opts: HashMap<usize, (Value, Value)>,
    /// Closures whose handle test has passed since the last IR label or store.
    pub(super) tested_closures: Vec<(String, Value)>,
    pub(super) terminated: bool,
}

/// Normalize an i64 result (`iadd(val, 0)`), working around a Cranelift
/// register-state issue on struct-from-list and pair returns.
pub(super) fn normalize_i64_result(builder: &mut FunctionBuilder<'_>, val: Value) -> Value {
    let zero = builder.ins().iconst(types::I64, 0);
    builder.ins().iadd(val, zero)
}

/// A block for each label of a function body, created in stream order, and
/// the labels where a loop exits: the label right after a jmp back to an
/// earlier label. A `label NAME cold` block is marked cold.
pub(super) fn create_label_blocks<'a>(
    builder: &mut FunctionBuilder<'_>,
    body_lines: &[&'a str],
) -> (HashMap<String, Block>, HashSet<&'a str>) {
    let mut labels: HashMap<String, Block> = HashMap::new();
    let mut loop_exits: HashSet<&'a str> = HashSet::new();
    let mut after_back_edge = false;
    for line in body_lines {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if !parts.is_empty() && parts[0] == "label" && parts.len() >= 2 {
            let block = builder.create_block();
            labels.insert(parts[1].to_string(), block);
            if parts.get(2) == Some(&"cold") {
                builder.set_cold_block(block);
            }
            if after_back_edge {
                loop_exits.insert(parts[1]);
            }
        }
        after_back_edge = parts.first() == Some(&"jmp")
            && parts.get(1).is_some_and(|target| labels.contains_key(*target));
    }
    (labels, loop_exits)
}

pub(super) fn compile_ir_function(
    codegen: &mut CodeGen,
    mctx: &ModuleCtx<'_>,
    func_name: &str,
    func_retkind: &str,
    param_names: &[String],
    param_kinds: &[String],
    body_lines: &[&str],
) -> Result<Function, CompileError> {
    let mut ctx = codegen.module.make_context();

    for _ in param_names {
        ctx.func.signature.params.push(AbiParam::new(types::I64));
    }
    push_return_types(&mut ctx.func.signature.returns, func_retkind);

    let mut builder_ctx = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);

    let entry_block = builder.create_block();
    builder.append_block_params_for_function_params(entry_block);
    builder.switch_to_block(entry_block);

    let block_params: Vec<Value> = builder.block_params(entry_block).to_vec();
    let mut regs: HashMap<usize, Value> = HashMap::new();
    let mut named_vars: HashMap<String, Variable> = HashMap::new();

    if func_name == "main" {
        emit_main_prologue(codegen, &mut builder, mctx, body_lines);
    }

    for (i, name) in param_names.iter().enumerate() {
        if i < block_params.len() {
            let var = builder.declare_var(types::I64);
            builder.def_var(var, block_params[i]);
            named_vars.insert(name.clone(), var);
            regs.insert(i, block_params[i]);
        }
    }

    let float_vars = scan_float_vars(param_names, param_kinds, body_lines);
    let (labels, loop_exits) = create_label_blocks(&mut builder, body_lines);
    let inline_opt_regs = if mctx.fast_paths {
        scan_unpackable_opt_regs(body_lines)
    } else {
        HashSet::new()
    };

    let nonneg_names = crate::nonneg::nonneg_vars(
        body_lines,
        param_names,
        |name| mctx.global_data.contains_key(name),
        |name| mctx.declared_funcs.contains_key(name),
    );

    let mut lowerer = FuncLowerer {
        codegen,
        mctx,
        builder,
        func_name,
        func_retkind,
        body_lines,
        func_ref_cache: HashMap::new(),
        regs,
        kinds: RegKinds::default(),
        string_vars: HashSet::new(),
        float_vars,
        literal_values: HashSet::new(),
        named_vars,
        defined_labels: HashSet::new(),
        labels,
        loop_exits,
        inline_opt_regs,
        virtual_opts: HashMap::new(),
        tested_closures: Vec::new(),
        terminated: false,
    };

    lowerer.lower_body(&nonneg_names)?;
    lowerer.finish_function();

    Ok(ctx.func)
}

impl FuncLowerer<'_, '_, '_, '_> {
    fn lower_body(&mut self, nonneg_names: &HashSet<String>) -> Result<(), CompileError> {
        let mut nonneg_values: HashSet<Value> = HashSet::new();
        let mut pending_nonneg: Option<(usize, Option<Value>)> = None;

        for (line_idx, &line) in self.body_lines.iter().enumerate() {
            if let Some((reg, before)) = pending_nonneg.take() {
                if let Some(&v) = self.regs.get(&reg) {
                    if Some(v) != before
                        && !self.kinds.floats.contains(&reg)
                        && !self.kinds.strings.contains(&reg)
                    {
                        nonneg_values.insert(v);
                    }
                }
            }
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.is_empty() || parts[0].starts_with(';') {
                continue;
            }
            let value_nonneg = |token: &str| {
                token
                    .parse::<usize>()
                    .ok()
                    .and_then(|r| self.regs.get(&r))
                    .is_some_and(|&v| {
                        nonneg_values.contains(&v)
                            || iconst_of(self.builder.func, v).is_some_and(|c| c >= 0)
                    })
            };
            let result_nonneg = match parts[0] {
                "iconst" => parts
                    .get(2)
                    .and_then(|v| v.parse::<i64>().ok())
                    .is_some_and(|v| v >= 0),
                "load" => parts.get(2).is_some_and(|name| {
                    nonneg_names.contains(*name)
                        && !self.float_vars.contains(*name)
                        && !self.string_vars.contains(*name)
                }),
                "call" => parts.get(2).is_some_and(|f| {
                    crate::nonneg::LENGTH_CALLS.contains(f)
                        && !self.mctx.declared_funcs.contains_key(*f)
                }),
                op if crate::nonneg::keeps_nonneg(op) && parts.len() >= 4 => {
                    value_nonneg(parts[2]) && value_nonneg(parts[3])
                }
                _ => false,
            };
            let operands_nonneg = (
                parts.get(2).is_some_and(|t| value_nonneg(t)),
                parts.get(3).is_some_and(|t| value_nonneg(t)),
            );
            if result_nonneg {
                pending_nonneg = parts[1]
                    .parse::<usize>()
                    .ok()
                    .map(|r| (r, self.regs.get(&r).copied()));
            }

            if parts[0] == "label" && parts.len() >= 2 {
                if parts.len() > 3 || parts.get(2).is_some_and(|flag| *flag != "cold") {
                    return Err(CompileError::ModuleError(format!(
                        "ir consumer: a label takes only its name and `cold` in {}: {}",
                        self.func_name, line
                    )));
                }
                self.defined_labels.insert(parts[1].to_string());
                self.tested_closures.clear();
            }

            if self.terminated {
                if parts[0] == "label" && parts.len() >= 2 {
                    let block = self.labels[parts[1]];
                    self.builder.switch_to_block(block);
                    self.terminated = false;
                }
                continue;
            }

            self.lower_inst(line_idx, line, &parts, operands_nonneg)?;
        }
        Ok(())
    }

    fn lower_inst(
        &mut self,
        line_idx: usize,
        line: &str,
        parts: &[&str],
        operands_nonneg: (bool, bool),
    ) -> Result<(), CompileError> {
        match parts[0] {
            "iconst" if parts.len() >= 3 => {
                let reg = parse_reg(parts[1], line, self.func_name)?;
                let val = parse_i64_operand(parts[2], "integer constant", line, self.func_name)?;
                let v = self.builder.ins().iconst(types::I64, val);
                self.regs.insert(reg, v);
                self.kinds.clear(reg);
            }

            "fconst" if parts.len() >= 3 => {
                let reg = parse_reg(parts[1], line, self.func_name)?;
                let fval: f64 = parts[2].parse().map_err(|_| {
                    CompileError::ModuleError(format!(
                        "IR consumer: invalid float constant '{}' in {}: {}",
                        parts[2], self.func_name, line
                    ))
                })?;
                let fv = self.builder.ins().f64const(fval);
                let v = self.builder.ins().bitcast(types::I64, MemFlags::new(), fv);
                self.regs.insert(reg, v);
                self.kinds.set(reg, "float");
            }

            "strref" if parts.len() >= 3 => {
                let reg = parse_reg(parts[1], line, self.func_name)?;
                let str_idx = parts[2];
                if let Some(&lit_id) = self.mctx.string_lits.get(str_idx) {
                    let v = string_lit_addr(self.codegen, &mut self.builder, lit_id);
                    self.literal_values.insert(v);
                    self.regs.insert(reg, v);
                } else {
                    self.regs.insert(reg, self.builder.ins().iconst(types::I64, 0));
                }
                self.kinds.set(reg, "string");
            }

            "band" | "bor" | "bxor" | "shl" | "shr" if parts.len() >= 4 => {
                let reg = parse_reg(parts[1], line, self.func_name)?;
                let a = get_reg(&self.regs, parts[2])?;
                let b = get_reg(&self.regs, parts[3])?;
                let v = match parts[0] {
                    "band" => self.builder.ins().band(a, b),
                    "bor" => self.builder.ins().bor(a, b),
                    "bxor" => self.builder.ins().bxor(a, b),
                    "shl" | "shr" => emit_bits_op(&mut self.builder, parts[0], a, b),
                    _ => {
                        return Err(CompileError::ModuleError(format!(
                            "IR consumer: unsupported bitwise instruction in {}: {}",
                            self.func_name, line
                        )))
                    }
                };
                self.regs.insert(reg, v);
                self.kinds.clear(reg);
            }

            "bnot" if parts.len() >= 3 => {
                let reg = parse_reg(parts[1], line, self.func_name)?;
                let a = get_reg(&self.regs, parts[2])?;
                let v = self.builder.ins().bnot(a);
                self.regs.insert(reg, v);
                self.kinds.clear(reg);
            }

            "and" | "or" if parts.len() >= 4 => {
                let reg = parse_reg(parts[1], line, self.func_name)?;
                let a = get_reg(&self.regs, parts[2])?;
                let b = get_reg(&self.regs, parts[3])?;
                let v = match parts[0] {
                    "and" => self.builder.ins().band(a, b),
                    "or" => self.builder.ins().bor(a, b),
                    _ => {
                        return Err(CompileError::ModuleError(format!(
                            "IR consumer: unsupported boolean instruction in {}: {}",
                            self.func_name, line
                        )))
                    }
                };
                self.regs.insert(reg, v);
                self.kinds.clear(reg);
            }

            "fadd" | "fsub" | "fmul" | "fdiv" if parts.len() >= 4 => {
                let reg = parse_reg(parts[1], line, self.func_name)?;
                let a = get_reg(&self.regs, parts[2])?;
                let b = get_reg(&self.regs, parts[3])?;
                let fa = self.builder.ins().bitcast(types::F64, MemFlags::new(), a);
                let fb = self.builder.ins().bitcast(types::F64, MemFlags::new(), b);
                let fv = match parts[0] {
                    "fadd" => self.builder.ins().fadd(fa, fb),
                    "fsub" => self.builder.ins().fsub(fa, fb),
                    "fmul" => self.builder.ins().fmul(fa, fb),
                    "fdiv" => self.builder.ins().fdiv(fa, fb),
                    _ => {
                        return Err(CompileError::ModuleError(format!(
                            "IR consumer: unsupported float instruction in {}: {}",
                            self.func_name, line
                        )))
                    }
                };
                let v = self.builder.ins().bitcast(types::I64, MemFlags::new(), fv);
                self.regs.insert(reg, v);
                self.kinds.set(reg, "float");
            }

            "add" | "sub" | "mul" | "div" | "mod" | "addc" | "subc" | "mulc" | "uaddc"
            | "usubc" | "umulc" | "addnsw"
                if parts.len() >= 4 =>
            {
                self.lower_arith(line, parts, operands_nonneg)?;
            }

            "udiv" | "urem" if parts.len() >= 4 => {
                let reg = parse_reg(parts[1], line, self.func_name)?;
                let a = get_reg(&self.regs, parts[2])?;
                let b = get_reg(&self.regs, parts[3])?;
                let v = emit_checked_int_div_or_mod(
                    self.codegen,
                    &mut self.builder,
                    &mut self.func_ref_cache,
                    self.mctx.runtime_funcs,
                    parts[0],
                    a,
                    b,
                )?;
                self.regs.insert(reg, v);
                self.kinds.clear(reg);
            }

            "ult" | "ugt" | "ulte" | "ugte" if parts.len() >= 4 => {
                let reg = parse_reg(parts[1], line, self.func_name)?;
                let a = get_reg(&self.regs, parts[2])?;
                let b = get_reg(&self.regs, parts[3])?;
                let cc = match parts[0] {
                    "ult" => IntCC::UnsignedLessThan,
                    "ugt" => IntCC::UnsignedGreaterThan,
                    "ulte" => IntCC::UnsignedLessThanOrEqual,
                    _ => IntCC::UnsignedGreaterThanOrEqual,
                };
                let cmp = self.builder.ins().icmp(cc, a, b);
                let ext = self.builder.ins().uextend(types::I64, cmp);
                self.regs.insert(reg, ext);
                self.kinds.clear(reg);
            }

            "eq" | "neq" | "lt" | "gt" | "lte" | "gte" if parts.len() >= 4 => {
                self.lower_cmp(line, parts)?;
            }

            "concatn" if parts.len() >= 4 => self.lower_concatn(line, parts)?,
            "appendn" if parts.len() >= 5 => self.lower_appendn(line, parts)?,
            "concat" if parts.len() >= 4 => self.lower_concat(line, parts)?,
            "rcall" if parts.len() >= 6 => self.lower_rcall(line, parts)?,
            "call" if parts.len() >= 4 => self.lower_call(line_idx, line, parts)?,
            "calli" if parts.len() >= 5 => self.lower_calli(line, parts)?,

            "store" if parts.len() >= 3 => self.lower_store(parts)?,
            "load" if parts.len() >= 3 => self.lower_load(line, parts)?,
            "optflag" if parts.len() == 3 => self.lower_optflag(line, parts)?,
            "optval" if parts.len() == 5 => self.lower_optval(line, parts)?,
            "field" if parts.len() >= 4 => self.lower_field(line, parts)?,
            "funcref" if parts.len() >= 3 => self.lower_funcref(line, parts)?,
            "closure_ref" if parts.len() >= 3 => self.lower_closure_ref(line, parts)?,
            "closure_stack" if parts.len() >= 3 => self.lower_closure_stack(line, parts)?,
            "tuple_stack" if parts.len() >= 3 => self.lower_tuple_stack(line, parts)?,
            "sstore" if parts.len() >= 4 => self.lower_sstore(line, parts)?,

            "ret" if parts.len() >= 2 => {
                if self.func_name == "main" {
                    if self.func_retkind == "int" {
                        let val = get_reg(&self.regs, parts[1])?;
                        self.builder.ins().return_(&[val]);
                    } else {
                        let zero = self.builder.ins().iconst(types::I64, 0);
                        self.builder.ins().return_(&[zero]);
                    }
                } else {
                    let val = get_reg(&self.regs, parts[1])?;
                    self.builder.ins().return_(&[val]);
                }
                self.terminated = true;
            }

            "ret2" if parts.len() >= 3 => {
                let flag = get_reg(&self.regs, parts[1])?;
                let mut payload = get_reg(&self.regs, parts[2])?;
                if self.func_retkind == "result_reg_f" {
                    payload = self
                        .builder
                        .ins()
                        .bitcast(types::F64, MemFlags::new(), payload);
                }
                self.builder.ins().return_(&[flag, payload]);
                self.terminated = true;
            }

            "brif" if parts.len() >= 4 => {
                let cond = get_reg(&self.regs, parts[1])?;
                let then_label = parts[2];
                let else_label = parts[3];
                if let Some(flag) = self.mctx.preempt_flag {
                    if self.defined_labels.contains(then_label)
                        || self.defined_labels.contains(else_label)
                    {
                        emit_preempt_safepoint(
                            self.codegen,
                            &mut self.builder,
                            &mut self.func_ref_cache,
                            self.mctx.runtime_funcs,
                            flag,
                        )?;
                    }
                }
                let then_block = get_label(&self.labels, then_label, self.func_name)?;
                let else_block = get_label(&self.labels, else_label, self.func_name)?;
                if self.loop_exits.contains(else_label) {
                    let cond_bool = self.builder.ins().icmp_imm(IntCC::Equal, cond, 0);
                    self.builder
                        .ins()
                        .brif(cond_bool, else_block, &[], then_block, &[]);
                } else {
                    let cond_bool = self.builder.ins().icmp_imm(IntCC::NotEqual, cond, 0);
                    self.builder
                        .ins()
                        .brif(cond_bool, then_block, &[], else_block, &[]);
                }
                self.terminated = true;
            }

            "jmp" if parts.len() >= 2 => {
                let target = parts[1];
                if let Some(flag) = self.mctx.preempt_flag {
                    if self.defined_labels.contains(target) {
                        emit_preempt_safepoint(
                            self.codegen,
                            &mut self.builder,
                            &mut self.func_ref_cache,
                            self.mctx.runtime_funcs,
                            flag,
                        )?;
                    }
                }
                let block = get_label(&self.labels, target, self.func_name)?;
                self.builder.ins().jump(block, &[]);
                self.terminated = true;
            }

            "label" if parts.len() >= 2 => {
                let block = self.labels[parts[1]];
                if !self.terminated {
                    self.builder.ins().jump(block, &[]);
                }
                self.builder.switch_to_block(block);
                self.terminated = false;
            }

            _ if parts[0].starts_with('#') || parts[0].starts_with("//") => {}

            _ => {
                return Err(CompileError::ModuleError(format!(
                    "ir consumer: unknown or malformed instruction in {}: {}",
                    self.func_name, line
                )));
            }
        }
        Ok(())
    }

    fn lower_arith(
        &mut self,
        line: &str,
        parts: &[&str],
        operands_nonneg: (bool, bool),
    ) -> Result<(), CompileError> {
        let reg = parse_reg(parts[1], line, self.func_name)?;
        let a = get_reg(&self.regs, parts[2])?;
        let b = get_reg(&self.regs, parts[3])?;
        let checked = parts[0];
        let op = plain_arith_op(checked);
        // Legacy untyped IR fallback: `add` on strings -> concat, `add..div` on floats -> float op.
        if op == "add" && self.kinds.is_string(parts[2]) && self.kinds.is_string(parts[3]) {
            if let Some(&concat_id) = self.mctx.runtime_funcs.get("pith_concat_cstr") {
                let concat_ref = cached_func_ref(
                    self.codegen,
                    &mut self.builder,
                    &mut self.func_ref_cache,
                    concat_id,
                );
                let call = self.builder.ins().call(concat_ref, &[a, b]);
                if !self.builder.func.dfg.inst_results(call).is_empty() {
                    self.regs
                        .insert(reg, self.builder.func.dfg.first_result(call));
                } else {
                    self.regs.insert(reg, a);
                }
            } else {
                self.regs.insert(reg, self.builder.ins().iadd(a, b));
            }
            self.kinds.set(reg, "string");
        } else if matches!(op, "add" | "sub" | "mul" | "div")
            && (self.kinds.is_float(parts[2]) || self.kinds.is_float(parts[3]))
        {
            let fa = self.builder.ins().bitcast(types::F64, MemFlags::new(), a);
            let fb = self.builder.ins().bitcast(types::F64, MemFlags::new(), b);
            let fv = match op {
                "add" => self.builder.ins().fadd(fa, fb),
                "sub" => self.builder.ins().fsub(fa, fb),
                "mul" => self.builder.ins().fmul(fa, fb),
                "div" => self.builder.ins().fdiv(fa, fb),
                _ => {
                    return Err(CompileError::ModuleError(format!(
                        "IR consumer: unsupported float-promoted instruction in {}: {}",
                        self.func_name, line
                    )))
                }
            };
            let v = self.builder.ins().bitcast(types::I64, MemFlags::new(), fv);
            self.regs.insert(reg, v);
            self.kinds.set(reg, "float");
        } else {
            let v = match parts[0] {
                "addc" | "subc" | "mulc" | "uaddc" | "usubc" | "umulc" => emit_checked_int_arith(
                    &mut self.builder,
                    checked,
                    a,
                    b,
                    operands_nonneg.0,
                    operands_nonneg.1,
                )?,
                "add" | "addnsw" => self.builder.ins().iadd(a, b),
                "sub" => self.builder.ins().isub(a, b),
                "mul" => self.builder.ins().imul(a, b),
                "div" | "mod" => emit_checked_int_div_or_mod(
                    self.codegen,
                    &mut self.builder,
                    &mut self.func_ref_cache,
                    self.mctx.runtime_funcs,
                    parts[0],
                    a,
                    b,
                )?,
                _ => {
                    return Err(CompileError::ModuleError(format!(
                        "IR consumer: unsupported arithmetic instruction in {}: {}",
                        self.func_name, line
                    )))
                }
            };
            self.regs.insert(reg, v);
            self.kinds.clear(reg);
        }
        Ok(())
    }

    fn lower_cmp(&mut self, line: &str, parts: &[&str]) -> Result<(), CompileError> {
        let reg = parse_reg(parts[1], line, self.func_name)?;
        let a = get_reg(&self.regs, parts[2])?;
        let b = get_reg(&self.regs, parts[3])?;
        let is_str_cmp = matches!(parts[0], "lt" | "gt" | "lte" | "gte")
            && (self.kinds.is_string(parts[2]) || self.kinds.is_string(parts[3]));
        if is_str_cmp {
            let cmp_name = match parts[0] {
                "lt" => "pith_cstring_lt",
                "gt" => "pith_cstring_gt",
                "lte" => "pith_cstring_lte",
                "gte" => "pith_cstring_gte",
                _ => "pith_cstring_lt",
            };
            if let Some(&fid) = self.mctx.runtime_funcs.get(cmp_name) {
                let fref = cached_func_ref(
                    self.codegen,
                    &mut self.builder,
                    &mut self.func_ref_cache,
                    fid,
                );
                let call = self.builder.ins().call(fref, &[a, b]);
                self.regs
                    .insert(reg, self.builder.func.dfg.first_result(call));
            } else {
                let cmp = self.builder.ins().icmp(IntCC::SignedLessThan, a, b);
                let ext = self.builder.ins().uextend(types::I64, cmp);
                self.regs.insert(reg, ext);
            }
        } else if self.kinds.is_float(parts[2]) || self.kinds.is_float(parts[3]) {
            let fa = self.builder.ins().bitcast(types::F64, MemFlags::new(), a);
            let fb = self.builder.ins().bitcast(types::F64, MemFlags::new(), b);
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
            let cmp = self.builder.ins().fcmp(fcc, fa, fb);
            let v = self.builder.ins().uextend(types::I64, cmp);
            self.regs.insert(reg, v);
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
            let cmp = self.builder.ins().icmp(cc, a, b);
            let v = self.builder.ins().uextend(types::I64, cmp);
            self.regs.insert(reg, v);
        }
        self.kinds.clear(reg);
        Ok(())
    }

    fn finish_function(mut self) {
        if !self.terminated {
            let zero = self.builder.ins().iconst(types::I64, 0);
            match result_abi_payload_type(self.func_retkind) {
                Some(types::F64) => {
                    let fzero = self.builder.ins().f64const(0.0);
                    self.builder.ins().return_(&[zero, fzero]);
                }
                Some(_) => {
                    let zero2 = self.builder.ins().iconst(types::I64, 0);
                    self.builder.ins().return_(&[zero, zero2]);
                }
                None => {
                    self.builder.ins().return_(&[zero]);
                }
            }
        }
        self.builder.seal_all_blocks();
        self.builder.finalize();
    }
}
