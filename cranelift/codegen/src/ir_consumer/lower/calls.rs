//! Call, indirect-call, register-pair `rcall`, and string-join lowering.

use super::{normalize_i64_result, FuncLowerer};
use crate::ir_consumer::arith::emit_bits_op;
use crate::ir_consumer::fast_paths::{
    checked_store_branch, emit_closure_call, inline_list_get_opt, inline_list_get_value_strict,
    inline_list_set_value_branch, inline_runtime_call, is_noop_count_call, is_null_safe_release,
};
use crate::ir_consumer::instrumentation::{
    bitcast_float_args, cached_func_ref, emit_sanitize_check, runtime_func_ref,
};
use crate::ir_consumer::ir::{
    get_label, get_reg, normalize_runtime_result, parse_call_shape, parse_reg,
    retkind_expects_a_value,
};
use crate::ir_consumer::passes::scans::{bits_builtin_op, opt_getter_pair};
use crate::ir_consumer::passes::string_fusion::join_int_bits;
use crate::CompileError;
use cranelift::prelude::*;
use cranelift_module::Module;

impl FuncLowerer<'_, '_, '_, '_> {
    pub(super) fn lower_concatn(&mut self, line: &str, parts: &[&str]) -> Result<(), CompileError> {
        let reg = parse_reg(parts[1], line, self.func_name)?;
        let ints = join_int_bits(parts[2], parts.len() - 3, pith_runtime::CONCAT_MANY_MAX);
        let mut values = Vec::with_capacity(parts.len() - 3);
        for t in &parts[3..] {
            values.push(get_reg(&self.regs, t)?);
        }
        let many = runtime_func_ref(
            self.codegen,
            &mut self.builder,
            &mut self.func_ref_cache,
            self.mctx.runtime_funcs,
            "pith_concat_cstr_many",
        )?;
        let release = runtime_func_ref(
            self.codegen,
            &mut self.builder,
            &mut self.func_ref_cache,
            self.mctx.runtime_funcs,
            "pith_cstring_release",
        )?;
        let max = pith_runtime::CONCAT_MANY_MAX;
        let mut acc: Option<Value> = None;
        let mut rest = &values[..];
        let joined = loop {
            let room = if acc.is_some() { max - 1 } else { max };
            let take = rest.len().min(room);
            let mut group: Vec<Value> = acc.into_iter().collect();
            group.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            let count = self.builder.ins().iconst(types::I64, group.len() as i64 | ints);
            let mut args = vec![count];
            args.extend_from_slice(&group);
            while args.len() < max + 1 {
                args.push(self.builder.ins().iconst(types::I64, 0));
            }
            let call = self.builder.ins().call(many, &args);
            let joined = self.builder.func.dfg.first_result(call);
            if let Some(previous) = acc {
                self.builder.ins().call(release, &[previous]);
            }
            if rest.is_empty() {
                break joined;
            }
            acc = Some(joined);
        };
        self.regs.insert(reg, joined);
        self.kinds.set(reg, "string");
        Ok(())
    }

    pub(super) fn lower_appendn(&mut self, line: &str, parts: &[&str]) -> Result<(), CompileError> {
        let reg = parse_reg(parts[1], line, self.func_name)?;
        let ints = join_int_bits(parts[2], parts.len() - 4, pith_runtime::APPEND_MANY_MAX);
        let mut acc = get_reg(&self.regs, parts[3])?;
        let mut values = Vec::with_capacity(parts.len() - 4);
        for t in &parts[4..] {
            values.push(get_reg(&self.regs, t)?);
        }
        let append = runtime_func_ref(
            self.codegen,
            &mut self.builder,
            &mut self.func_ref_cache,
            self.mctx.runtime_funcs,
            "pith_cstring_append_many",
        )?;
        let max = pith_runtime::APPEND_MANY_MAX;
        for group in values.chunks(max) {
            let count = self.builder.ins().iconst(types::I64, group.len() as i64 | ints);
            let mut args = vec![count, acc];
            args.extend_from_slice(group);
            while args.len() < max + 2 {
                args.push(self.builder.ins().iconst(types::I64, 0));
            }
            let call = self.builder.ins().call(append, &args);
            acc = self.builder.func.dfg.first_result(call);
        }
        self.regs.insert(reg, acc);
        self.kinds.set(reg, "string");
        Ok(())
    }

    pub(super) fn lower_concat(&mut self, line: &str, parts: &[&str]) -> Result<(), CompileError> {
        let reg = parse_reg(parts[1], line, self.func_name)?;
        let a = get_reg(&self.regs, parts[2])?;
        let b = get_reg(&self.regs, parts[3])?;
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
            self.regs.insert(reg, a);
        }
        self.kinds.set(reg, "string");
        Ok(())
    }

    /// Register result ABI call: `rcall flag_reg val_reg fname payload_kind nargs args...`
    pub(super) fn lower_rcall(&mut self, line: &str, parts: &[&str]) -> Result<(), CompileError> {
        let flag_reg = parse_reg(parts[1], line, self.func_name)?;
        let val_reg = parse_reg(parts[2], line, self.func_name)?;
        let fname = parts[3];
        let payload_kind = parts[4];
        let nargs: usize = parts[5].parse().map_err(|_| {
            CompileError::ModuleError(format!(
                "ir consumer: malformed rcall instruction in {}: {}",
                self.func_name, line
            ))
        })?;
        if parts.len() < 6 + nargs {
            return Err(CompileError::ModuleError(format!(
                "ir consumer: rcall is missing arguments in {}: {}",
                self.func_name, line
            )));
        }
        let mut args: Vec<Value> = Vec::with_capacity(nargs);
        for j in 0..nargs {
            args.push(get_reg(&self.regs, parts[6 + j])?);
        }
        let fid = self
            .mctx
            .declared_funcs
            .get(fname)
            .or_else(|| self.mctx.runtime_funcs.get(fname))
            .copied()
            .ok_or_else(|| {
                CompileError::ModuleError(format!(
                    "ir consumer: rcall to unknown function {} in {}",
                    fname, self.func_name
                ))
            })?;
        let fref = cached_func_ref(
            self.codegen,
            &mut self.builder,
            &mut self.func_ref_cache,
            fid,
        );
        bitcast_float_args(&mut self.builder, fref, &mut args);
        let call = self.builder.ins().call(fref, &args);
        let results = self.builder.func.dfg.inst_results(call);
        let &[flag_v, payload_v] = results else {
            return Err(CompileError::ModuleError(format!(
                "ir consumer: rcall to {} in {}, which does not return a register pair",
                fname, self.func_name
            )));
        };
        let flag_norm = normalize_i64_result(&mut self.builder, flag_v);
        self.regs.insert(flag_reg, flag_norm);
        // Inspect the actual Cranelift payload type so `result_reg_f` bitcasts
        // F64 -> I64 even when the IR payload_kind is not literally `"float"`.
        let payload_is_f64 = self.builder.func.dfg.value_type(payload_v) == types::F64;
        if payload_is_f64 {
            let cast = self
                .builder
                .ins()
                .bitcast(types::I64, MemFlags::new(), payload_v);
            self.regs.insert(val_reg, cast);
        } else if payload_kind == "float" {
            self.regs.insert(val_reg, payload_v);
        } else {
            let payload_norm = normalize_i64_result(&mut self.builder, payload_v);
            self.regs.insert(val_reg, payload_norm);
        }
        self.kinds.clear(flag_reg);
        self.kinds.set(val_reg, payload_kind);
        Ok(())
    }

    pub(super) fn lower_call(
        &mut self,
        line_idx: usize,
        line: &str,
        parts: &[&str],
    ) -> Result<(), CompileError> {
        let reg = parse_reg(parts[1], line, self.func_name)?;
        let (mut fname, retkind, nargs, arg_start) = parse_call_shape(parts).ok_or_else(|| {
            CompileError::ModuleError(format!(
                "ir consumer: malformed call instruction in {}: {}",
                self.func_name, line
            ))
        })?;

        // Legacy struct constructor: `call REG StructName N args...`
        if self.mctx.struct_names.contains(fname) {
            let mut args: Vec<Value> = Vec::new();
            for j in 0..nargs {
                if j + arg_start < parts.len() {
                    args.push(get_reg(&self.regs, parts[j + arg_start])?);
                }
            }
            if let Some(&alloc_id) = self.mctx.runtime_funcs.get("pith_struct_alloc") {
                let alloc_ref = cached_func_ref(
                    self.codegen,
                    &mut self.builder,
                    &mut self.func_ref_cache,
                    alloc_id,
                );
                let nfields = self.builder.ins().iconst(types::I64, nargs as i64);
                let alloc_call = self.builder.ins().call(alloc_ref, &[nfields]);
                let ptr = self.builder.func.dfg.first_result(alloc_call);
                for (i, arg) in args.iter().enumerate() {
                    let offset = (i * 8) as i32;
                    self.builder.ins().store(MemFlags::new(), *arg, ptr, offset);
                }
                self.regs.insert(reg, ptr);
                self.kinds.clear(reg);
            } else {
                let zero = self.builder.ins().iconst(types::I64, 0);
                self.regs.insert(reg, zero);
                self.kinds.clear(reg);
            }
            return Ok(());
        }

        // Virtualized non-escaping `pith_list_get_opt`
        if fname == "pith_list_get_opt"
            && nargs == 2
            && parts.len() > arg_start + 1
            && self.inline_opt_regs.contains(&reg)
        {
            let list = get_reg(&self.regs, parts[arg_start])?;
            let index = get_reg(&self.regs, parts[arg_start + 1])?;
            let pair = inline_list_get_opt(&mut self.builder, list, index);
            self.virtual_opts.insert(reg, pair);
            self.regs.remove(&reg);
            self.kinds.clear(reg);
            return Ok(());
        }

        // Virtualized non-escaping pair-form optional getters
        if let Some(pair_name) = opt_getter_pair(fname)
            .filter(|_| self.inline_opt_regs.contains(&reg))
            .filter(|_| !self.mctx.declared_funcs.contains_key(fname))
        {
            if let Some(&fid) = self.mctx.runtime_funcs.get(pair_name) {
                let mut args = Vec::with_capacity(nargs);
                for j in 0..nargs {
                    args.push(get_reg(&self.regs, parts[arg_start + j])?);
                }
                let fref = cached_func_ref(
                    self.codegen,
                    &mut self.builder,
                    &mut self.func_ref_cache,
                    fid,
                );
                let call = self.builder.ins().call(fref, &args);
                let &[flag, value] = self.builder.func.dfg.inst_results(call) else {
                    return Err(CompileError::ModuleError(format!(
                        "ir consumer: {} does not return a register pair",
                        pair_name
                    )));
                };
                self.virtual_opts.insert(reg, (flag, value));
                self.regs.remove(&reg);
                self.kinds.clear(reg);
                return Ok(());
            }
        }

        // Release of a virtualized tuple is a no-op
        if fname == "pith_struct_release" && nargs == 1 && parts.len() > arg_start {
            if let Ok(arg_reg) = parts[arg_start].parse::<usize>() {
                if self.virtual_opts.contains_key(&arg_reg) {
                    return Ok(());
                }
            }
        }

        // Legacy `__list_get` / `__index` on a string -> `char_at`
        if (fname == "__list_get" || fname == "__index") && nargs >= 1 && parts.len() > arg_start {
            if let Ok(arg_reg) = parts[arg_start].parse::<usize>() {
                if self.kinds.strings.contains(&arg_reg) {
                    fname = "char_at";
                }
            }
        }

        let mut args: Vec<Value> = Vec::new();
        for j in 0..nargs {
            if j + arg_start < parts.len() {
                args.push(get_reg(&self.regs, parts[j + arg_start])?);
            }
        }

        let name_is_shadowed = self.mctx.declared_funcs.contains_key(fname)
            || self.named_vars.contains_key(fname)
            || self.mctx.global_data.contains_key(fname);

        if !name_is_shadowed
            && args.len() == 1
            && is_noop_count_call(fname, args[0], &self.literal_values, self.builder.func)
        {
            let zero = self.builder.ins().iconst(types::I64, 0);
            self.regs.insert(reg, zero);
            self.kinds.clear(reg);
            return Ok(());
        }

        if args.len() == 2 && !self.named_vars.contains_key(fname) {
            let alias_op = self.mctx.bits_aliases.get(fname).copied().or_else(|| {
                if self.mctx.declared_funcs.contains_key(fname)
                    || self.mctx.global_data.contains_key(fname)
                {
                    None
                } else {
                    bits_builtin_op(fname)
                }
            });
            if let Some(op) = alias_op {
                let v = emit_bits_op(&mut self.builder, op, args[0], args[1]);
                self.regs.insert(reg, v);
                self.kinds.clear(reg);
                return Ok(());
            }
        }

        if self.mctx.fast_paths && !name_is_shadowed && args.len() == 3 {
            if let Some((stored, missed)) =
                checked_store_branch(fname, reg, self.body_lines.get(line_idx + 1))
            {
                let fallback = runtime_func_ref(
                    self.codegen,
                    &mut self.builder,
                    &mut self.func_ref_cache,
                    self.mctx.runtime_funcs,
                    fname,
                )?;
                let anchor = runtime_func_ref(
                    self.codegen,
                    &mut self.builder,
                    &mut self.func_ref_cache,
                    self.mctx.runtime_funcs,
                    "pith_list_len",
                )?;
                inline_list_set_value_branch(
                    &mut self.builder,
                    [args[0], args[1], args[2]],
                    fallback,
                    anchor,
                    get_label(&self.labels, stored, self.func_name)?,
                    get_label(&self.labels, missed, self.func_name)?,
                );
                self.terminated = true;
                return Ok(());
            }
        }

        if let Some(value) = inline_runtime_call(
            self.codegen,
            &mut self.builder,
            &mut self.func_ref_cache,
            self.mctx.runtime_funcs,
            fname,
            &args,
            name_is_shadowed || !self.mctx.fast_paths,
        )? {
            self.regs.insert(reg, value);
            self.kinds.set(reg, retkind);
            return Ok(());
        }

        if fname == "pith_list_get_value_strict"
            && args.len() == 2
            && self.mctx.fast_paths
            && !name_is_shadowed
            && retkind != "result_int"
            && retkind != "result_bool"
        {
            let fallback = runtime_func_ref(
                self.codegen,
                &mut self.builder,
                &mut self.func_ref_cache,
                self.mctx.runtime_funcs,
                "pith_list_get_value_strict",
            )?;
            let anchor = runtime_func_ref(
                self.codegen,
                &mut self.builder,
                &mut self.func_ref_cache,
                self.mctx.runtime_funcs,
                "pith_list_len",
            )?;
            let inlined = inline_list_get_value_strict(
                &mut self.builder,
                args[0],
                args[1],
                fallback,
                anchor,
            );
            self.regs.insert(reg, inlined);
            self.kinds.set(reg, retkind);
            return Ok(());
        }

        let mut runtime_call = false;
        let fid = if self.named_vars.contains_key(fname) {
            None
        } else if let Some(&fid) = self.mctx.declared_funcs.get(fname) {
            Some(fid)
        } else if let Some(&fid) = self.mctx.runtime_funcs.get(fname) {
            runtime_call = true;
            Some(fid)
        } else {
            None
        };

        if let Some(fid) = fid {
            let fref = cached_func_ref(
                self.codegen,
                &mut self.builder,
                &mut self.func_ref_cache,
                fid,
            );
            let sig_ref = self.builder.func.dfg.ext_funcs[fref].signature;
            let sig = &self.builder.func.dfg.signatures[sig_ref];
            if sig.returns.len() > 1 {
                return Err(CompileError::ModuleError(format!(
                    "ir consumer: call to '{}' in {} must be an rcall: it returns a register pair",
                    fname, self.func_name
                )));
            }
            let mut typed_args = args;
            bitcast_float_args(&mut self.builder, fref, &mut typed_args);
            let guard_null = runtime_call && typed_args.len() == 1 && is_null_safe_release(fname);
            let done_block = if guard_null {
                let call_block = self.builder.create_block();
                let done_block = self.builder.create_block();
                self.builder
                    .ins()
                    .brif(typed_args[0], call_block, &[], done_block, &[]);
                self.builder.switch_to_block(call_block);
                self.builder.seal_block(call_block);
                Some(done_block)
            } else {
                None
            };
            let call = self.builder.ins().call(fref, &typed_args);
            if let Some(done_block) = done_block {
                self.builder.ins().jump(done_block, &[]);
                self.builder.switch_to_block(done_block);
                self.builder.seal_block(done_block);
            }
            let mut returns_float = false;
            if !self.builder.func.dfg.inst_results(call).is_empty() {
                let result = self.builder.func.dfg.first_result(call);
                let result_ty = self.builder.func.dfg.value_type(result);
                if result_ty == types::F64 {
                    let cast = self
                        .builder
                        .ins()
                        .bitcast(types::I64, MemFlags::new(), result);
                    self.regs.insert(reg, cast);
                    returns_float = true;
                } else {
                    let mut normalized = normalize_i64_result(&mut self.builder, result);
                    if runtime_call {
                        normalized =
                            normalize_runtime_result(&mut self.builder, normalized, retkind);
                    }
                    self.regs.insert(reg, normalized);
                }
            } else {
                if retkind_expects_a_value(retkind) {
                    return Err(CompileError::ModuleError(format!(
                        "ir consumer: call to '{}' in {} asks for a {} result, but the function returns nothing",
                        fname, self.func_name, retkind
                    )));
                }
                let zero = self.builder.ins().iconst(types::I64, 0);
                self.regs.insert(reg, zero);
            }
            self.kinds.set(reg, retkind);
            if returns_float {
                self.kinds.floats.insert(reg);
            }
        } else if let Some(&var) = self.named_vars.get(fname) {
            let closure_handle = self.builder.use_var(var);
            let result = emit_closure_call(
                self.codegen,
                &mut self.builder,
                &mut self.func_ref_cache,
                self.mctx.runtime_funcs,
                &mut self.tested_closures,
                closure_handle,
                Some(fname),
                &args,
                self.mctx.fast_paths,
            );
            self.regs.insert(reg, result);
            self.kinds.set(reg, retkind);
        } else if let Some(&data_id) = self.mctx.global_data.get(fname) {
            let gv = self
                .codegen
                .module
                .declare_data_in_func(data_id, self.builder.func);
            let addr = self.builder.ins().global_value(types::I64, gv);
            emit_sanitize_check(
                self.codegen,
                &mut self.builder,
                &mut self.func_ref_cache,
                self.mctx.sanitize,
                false,
                addr,
                0,
            );
            let closure_handle = self.builder.ins().load(types::I64, MemFlags::new(), addr, 0);
            let result = emit_closure_call(
                self.codegen,
                &mut self.builder,
                &mut self.func_ref_cache,
                self.mctx.runtime_funcs,
                &mut self.tested_closures,
                closure_handle,
                None,
                &args,
                self.mctx.fast_paths,
            );
            self.regs.insert(reg, result);
            self.kinds.set(reg, retkind);
        } else {
            return Err(CompileError::ModuleError(format!(
                "ir consumer: call to unknown function '{}' in {}",
                fname, self.func_name
            )));
        }
        Ok(())
    }

    pub(super) fn lower_calli(&mut self, line: &str, parts: &[&str]) -> Result<(), CompileError> {
        let reg = parse_reg(parts[1], line, self.func_name)?;
        let fn_ptr = get_reg(&self.regs, parts[2])?;
        let retkind = parts[3];
        let nargs = parts[4].parse::<usize>().map_err(|_| {
            CompileError::ModuleError(format!(
                "ir consumer: malformed calli in {}: {}",
                self.func_name, line
            ))
        })?;
        if parts.len() < 5 + nargs {
            return Err(CompileError::ModuleError(format!(
                "ir consumer: calli is missing arguments in {}: {}",
                self.func_name, line
            )));
        }
        let mut args = Vec::with_capacity(nargs);
        for j in 0..nargs {
            args.push(get_reg(&self.regs, parts[5 + j])?);
        }
        let mut sig = self.codegen.module.make_signature();
        for _ in 0..nargs {
            sig.params.push(AbiParam::new(types::I64));
        }
        sig.returns.push(AbiParam::new(types::I64));
        let sig_ref = self.builder.import_signature(sig);
        let call = self.builder.ins().call_indirect(sig_ref, fn_ptr, &args);
        self.regs
            .insert(reg, self.builder.func.dfg.first_result(call));
        self.kinds.set(reg, retkind);
        Ok(())
    }
}
