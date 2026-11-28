//! Load, store, field access, optional unpack, closure/tuple allocation,
//! and struct-store lowering.

use super::{normalize_i64_result, FuncLowerer};
use crate::ir_consumer::fast_paths::jump_with_i64_arg;
use crate::ir_consumer::instrumentation::{
    cached_func_ref, emit_sanitize_check, emit_struct_access_checks,
};
use crate::ir_consumer::ir::{get_reg, parse_reg};
use crate::CompileError;
use cranelift::prelude::*;
use cranelift_module::Module;
use pith_runtime::runtime_core::{
    CLOSURE_CYCLE_FLAGS_OFFSET, CLOSURE_ENV_TAGS_OFFSET, CLOSURE_FUNC_PTR_OFFSET, CLOSURE_MAGIC,
    CLOSURE_MAGIC_OFFSET, CLOSURE_OVERFLOW_OFFSET, CLOSURE_REF_COUNT_OFFSET, CLOSURE_SIZE,
    STRUCT_DEAD_OFFSET, STRUCT_DTOR_OFFSET, STRUCT_HEADER, STRUCT_MAGIC, STRUCT_SIZE_OFFSET,
    STRUCT_STRONG_OFFSET, STRUCT_WEAK_OFFSET,
};

impl FuncLowerer<'_, '_, '_, '_> {
    pub(super) fn lower_store(&mut self, parts: &[&str]) -> Result<(), CompileError> {
        let name = parts[1].to_string();
        let stored = format!("n:{name}");
        self.tested_closures.retain(|(k, _)| *k != stored);
        let val = get_reg(&self.regs, parts[2])?;
        if let Ok(src_reg) = parts[2].parse::<usize>() {
            if self.kinds.strings.contains(&src_reg) {
                self.string_vars.insert(name.clone());
            } else {
                self.string_vars.remove(&name);
            }
            if self.kinds.floats.contains(&src_reg) {
                self.float_vars.insert(name.clone());
            } else {
                self.float_vars.remove(&name);
            }
        }
        if let Some(&data_id) = self.mctx.global_data.get(&name) {
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
                true,
                addr,
                0,
            );
            self.builder.ins().store(MemFlags::new(), val, addr, 0);
        } else {
            let var = if let Some(&v) = self.named_vars.get(&name) {
                v
            } else {
                let v = self.builder.declare_var(types::I64);
                self.named_vars.insert(name, v);
                v
            };
            self.builder.def_var(var, val);
        }
        Ok(())
    }

    pub(super) fn lower_load(&mut self, line: &str, parts: &[&str]) -> Result<(), CompileError> {
        let reg = parse_reg(parts[1], line, self.func_name)?;
        let name = parts[2];
        if let Some(&data_id) = self.mctx.global_data.get(name) {
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
            let val = self.builder.ins().load(types::I64, MemFlags::new(), addr, 0);
            self.regs.insert(reg, val);
        } else if let Some(&var) = self.named_vars.get(name) {
            let val = self.builder.use_var(var);
            self.regs.insert(reg, val);
        } else if self.mctx.struct_names.contains(name) {
            let zero = self.builder.ins().iconst(types::I64, 0);
            self.regs.insert(reg, zero);
        } else {
            return Err(CompileError::ModuleError(format!(
                "ir consumer: unknown load source '{}' in {}",
                name, self.func_name
            )));
        }
        self.kinds.clear(reg);
        if self.string_vars.contains(name) || self.mctx.string_global_names.contains(name) {
            self.kinds.strings.insert(reg);
        }
        if self.float_vars.contains(name) {
            self.kinds.floats.insert(reg);
        }
        Ok(())
    }

    pub(super) fn lower_optflag(&mut self, line: &str, parts: &[&str]) -> Result<(), CompileError> {
        let reg = parse_reg(parts[1], line, self.func_name)?;
        let obj_reg = parse_reg(parts[2], line, self.func_name)?;
        let v = if let Some(&(flag, _)) = self.virtual_opts.get(&obj_reg) {
            flag
        } else {
            let obj = get_reg(&self.regs, parts[2])?;
            let is_some = self.builder.ins().icmp_imm(IntCC::NotEqual, obj, 0);
            self.builder.ins().uextend(types::I64, is_some)
        };
        self.regs.insert(reg, v);
        self.kinds.clear(reg);
        Ok(())
    }

    pub(super) fn lower_optval(&mut self, line: &str, parts: &[&str]) -> Result<(), CompileError> {
        let reg = parse_reg(parts[1], line, self.func_name)?;
        let obj_reg = parse_reg(parts[2], line, self.func_name)?;
        let v = if let Some(&(_, value)) = self.virtual_opts.get(&obj_reg) {
            value
        } else {
            let obj = get_reg(&self.regs, parts[2])?;
            let zero = self.builder.ins().iconst(types::I64, 0);
            let load_block = self.builder.create_block();
            let none_block = self.builder.create_block();
            let done = self.builder.create_block();
            self.builder.append_block_param(done, types::I64);
            self.builder
                .ins()
                .brif(obj, load_block, &[], none_block, &[]);
            self.builder.switch_to_block(none_block);
            jump_with_i64_arg(&mut self.builder, done, zero);
            self.builder.switch_to_block(load_block);
            emit_struct_access_checks(
                self.codegen,
                &mut self.builder,
                &mut self.func_ref_cache,
                self.mctx.runtime_funcs,
                self.mctx.quarantine,
                self.mctx.sanitize,
                false,
                obj,
                8,
            )?;
            let payload = self.builder.ins().load(types::I64, MemFlags::new(), obj, 8);
            jump_with_i64_arg(&mut self.builder, done, payload);
            self.builder.switch_to_block(done);
            self.builder.block_params(done)[0]
        };
        self.regs.insert(reg, v);
        self.kinds.set(reg, parts[3]);
        Ok(())
    }

    pub(super) fn lower_field(&mut self, line: &str, parts: &[&str]) -> Result<(), CompileError> {
        let reg = parse_reg(parts[1], line, self.func_name)?;
        if let Ok(obj_reg) = parts[2].parse::<usize>() {
            if let Some(&(flag, value)) = self.virtual_opts.get(&obj_reg) {
                let v = match parts[3] {
                    "0" => flag,
                    "8" => value,
                    _ => {
                        return Err(CompileError::ModuleError(format!(
                            "ir consumer: unexpected field offset on inlined list get in {}: {}",
                            self.func_name, line
                        )))
                    }
                };
                self.regs.insert(reg, v);
                let kind = if parts.len() >= 6 { parts[4] } else { "" };
                self.kinds.set(reg, kind);
                return Ok(());
            }
        }
        let obj = get_reg(&self.regs, parts[2])?;
        let (offset, field_kind) = if parts.len() >= 6 {
            let Ok(off) = parts[3].parse::<i32>() else {
                return Err(CompileError::ModuleError(format!(
                    "ir consumer: malformed field instruction in {}: {}",
                    self.func_name, line
                )));
            };
            if off < 0 {
                return Err(CompileError::ModuleError(format!(
                    "ir consumer: negative field offset in {}: {}",
                    self.func_name, line
                )));
            }
            (off, parts[4])
        } else if parts.len() == 4 {
            let field_name = parts[3];
            if let Ok(idx) = field_name.parse::<i32>() {
                let off = idx
                    .checked_mul(8)
                    .filter(|&o| o >= 0)
                    .ok_or_else(|| {
                        CompileError::ModuleError(format!(
                            "ir consumer: field index overflow in {}: {}",
                            self.func_name, line
                        ))
                    })?;
                (off, "")
            } else {
                return Err(CompileError::ModuleError(format!(
                    "ir consumer: field instruction requires an explicit offset in {}: {}",
                    self.func_name, line
                )));
            }
        } else {
            return Err(CompileError::ModuleError(format!(
                "ir consumer: malformed field instruction in {}: {}",
                self.func_name, line
            )));
        };
        emit_struct_access_checks(
            self.codegen,
            &mut self.builder,
            &mut self.func_ref_cache,
            self.mctx.runtime_funcs,
            self.mctx.quarantine,
            self.mctx.sanitize,
            false,
            obj,
            offset,
        )?;
        let raw = self
            .builder
            .ins()
            .load(types::I64, MemFlags::new(), obj, offset);
        let v = normalize_i64_result(&mut self.builder, raw);
        self.regs.insert(reg, v);
        self.kinds.set(reg, field_kind);
        Ok(())
    }

    pub(super) fn lower_funcref(&mut self, line: &str, parts: &[&str]) -> Result<(), CompileError> {
        let reg = parse_reg(parts[1], line, self.func_name)?;
        let fname = parts[2];
        if let Some(&fid) = self.mctx.declared_funcs.get(fname) {
            let fref = cached_func_ref(
                self.codegen,
                &mut self.builder,
                &mut self.func_ref_cache,
                fid,
            );
            let addr = self.builder.ins().func_addr(types::I64, fref);
            self.regs.insert(reg, addr);
        } else {
            return Err(CompileError::ModuleError(format!(
                "ir consumer: unknown function reference '{}' in {}",
                fname, self.func_name
            )));
        }
        self.kinds.clear(reg);
        Ok(())
    }

    pub(super) fn lower_closure_ref(
        &mut self,
        line: &str,
        parts: &[&str],
    ) -> Result<(), CompileError> {
        let reg = parse_reg(parts[1], line, self.func_name)?;
        let fname = parts[2];
        if let Some(&fid) = self.mctx.declared_funcs.get(fname) {
            let fref = cached_func_ref(
                self.codegen,
                &mut self.builder,
                &mut self.func_ref_cache,
                fid,
            );
            let addr = self.builder.ins().func_addr(types::I64, fref);
            if let Some(&closure_new_id) = self.mctx.runtime_funcs.get("pith_closure_new") {
                let closure_new_ref = cached_func_ref(
                    self.codegen,
                    &mut self.builder,
                    &mut self.func_ref_cache,
                    closure_new_id,
                );
                let call = self.builder.ins().call(closure_new_ref, &[addr]);
                self.regs
                    .insert(reg, self.builder.func.dfg.first_result(call));
            } else {
                self.regs.insert(reg, addr);
            }
        } else {
            return Err(CompileError::ModuleError(format!(
                "ir consumer: unknown closure reference '{}' in {}",
                fname, self.func_name
            )));
        }
        self.kinds.clear(reg);
        Ok(())
    }

    pub(super) fn lower_closure_stack(
        &mut self,
        line: &str,
        parts: &[&str],
    ) -> Result<(), CompileError> {
        let reg = parse_reg(parts[1], line, self.func_name)?;
        let fname = parts[2];
        let Some(&fid) = self.mctx.declared_funcs.get(fname) else {
            return Err(CompileError::ModuleError(format!(
                "ir consumer: unknown closure reference '{}' in {}",
                fname, self.func_name
            )));
        };
        let fref = cached_func_ref(
            self.codegen,
            &mut self.builder,
            &mut self.func_ref_cache,
            fid,
        );
        let addr = self.builder.ins().func_addr(types::I64, fref);
        let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            CLOSURE_SIZE,
            3,
        ));
        let at = self.builder.ins().stack_addr(types::I64, slot, 0);
        let flags = MemFlags::trusted();
        let magic = self.builder.ins().iconst(types::I32, CLOSURE_MAGIC as i64);
        self.builder
            .ins()
            .store(flags, magic, at, CLOSURE_MAGIC_OFFSET);
        self.builder
            .ins()
            .store(flags, addr, at, CLOSURE_FUNC_PTR_OFFSET);
        let one = self.builder.ins().iconst(types::I64, 1);
        self.builder
            .ins()
            .store(flags, one, at, CLOSURE_REF_COUNT_OFFSET);
        let zero = self.builder.ins().iconst(types::I64, 0);
        self.builder
            .ins()
            .store(flags, zero, at, CLOSURE_ENV_TAGS_OFFSET);
        self.builder
            .ins()
            .store(flags, zero, at, CLOSURE_ENV_TAGS_OFFSET + 8);
        let zero8 = self.builder.ins().iconst(types::I8, 0);
        self.builder
            .ins()
            .store(flags, zero8, at, CLOSURE_CYCLE_FLAGS_OFFSET);
        self.builder
            .ins()
            .store(flags, zero, at, CLOSURE_OVERFLOW_OFFSET);
        self.regs.insert(reg, at);
        self.kinds.clear(reg);
        Ok(())
    }

    pub(super) fn lower_tuple_stack(
        &mut self,
        line: &str,
        parts: &[&str],
    ) -> Result<(), CompileError> {
        let reg = parse_reg(parts[1], line, self.func_name)?;
        let fields: u32 = parts[2].parse().map_err(|_| {
            CompileError::ModuleError(format!(
                "ir consumer: bad field count in '{}' in {}",
                line, self.func_name
            ))
        })?;
        let header = STRUCT_HEADER as u32;
        let payload_bytes = fields.checked_mul(8).ok_or_else(|| {
            CompileError::ModuleError(format!(
                "ir consumer: tuple_stack field count overflow in {}: {}",
                self.func_name, line
            ))
        })?;
        let total_bytes = header.checked_add(payload_bytes).ok_or_else(|| {
            CompileError::ModuleError(format!(
                "ir consumer: tuple_stack size overflow in {}: {}",
                self.func_name, line
            ))
        })?;
        let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            total_bytes,
            3,
        ));
        let base = self.builder.ins().stack_addr(types::I64, slot, 0);
        let flags = MemFlags::trusted();
        let magic = self.builder.ins().iconst(types::I32, STRUCT_MAGIC as i64);
        self.builder.ins().store(flags, magic, base, 0);
        let one = self.builder.ins().iconst(types::I32, 1);
        self.builder
            .ins()
            .store(flags, one, base, STRUCT_STRONG_OFFSET);
        self.builder
            .ins()
            .store(flags, one, base, STRUCT_WEAK_OFFSET);
        let zero32 = self.builder.ins().iconst(types::I32, 0);
        self.builder
            .ins()
            .store(flags, zero32, base, STRUCT_DEAD_OFFSET);
        let size = self.builder.ins().iconst(types::I64, payload_bytes as i64);
        self.builder
            .ins()
            .store(flags, size, base, STRUCT_SIZE_OFFSET);
        let zero = self.builder.ins().iconst(types::I64, 0);
        self.builder
            .ins()
            .store(flags, zero, base, STRUCT_DTOR_OFFSET);
        let at = self.builder.ins().iadd_imm(base, header as i64);
        self.regs.insert(reg, at);
        self.kinds.clear(reg);
        Ok(())
    }

    pub(super) fn lower_sstore(&mut self, line: &str, parts: &[&str]) -> Result<(), CompileError> {
        let struct_val = get_reg(&self.regs, parts[1])?;
        let field_idx: i32 = parts[2].parse().map_err(|_| {
            CompileError::ModuleError(format!(
                "ir consumer: invalid struct field index in {}: {}",
                self.func_name, line
            ))
        })?;
        let offset = field_idx
            .checked_mul(8)
            .filter(|&o| o >= 0)
            .ok_or_else(|| {
                CompileError::ModuleError(format!(
                    "ir consumer: struct field offset overflow in {}: {}",
                    self.func_name, line
                ))
            })?;
        let val = get_reg(&self.regs, parts[3])?;
        emit_struct_access_checks(
            self.codegen,
            &mut self.builder,
            &mut self.func_ref_cache,
            self.mctx.runtime_funcs,
            self.mctx.quarantine,
            self.mctx.sanitize,
            true,
            struct_val,
            offset,
        )?;
        self.builder
            .ins()
            .store(MemFlags::new(), val, struct_val, offset);
        Ok(())
    }
}
