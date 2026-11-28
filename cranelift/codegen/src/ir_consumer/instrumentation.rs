//! Build-mode instrumentation hooks (green preemption safe-points, quarantine
//! checks, ASan/TSan hooks) and per-function FuncRef caching.

use crate::{CodeGen, CompileError};
use cranelift::codegen::ir::FuncRef;
use cranelift::prelude::*;
use cranelift_module::{DataId, FuncId, Linkage, Module};
use pith_runtime::runtime_core::{
    STRUCT_DEAD_MASK, STRUCT_FLAGS_FROM_DATA, STRUCT_HEADER, STRUCT_MAGIC,
};
use std::collections::HashMap;

/// The reference the function being built calls `fid` through, declared in
/// it on first use. A `FuncRef` belongs to one function, so the cache lives
/// as long as one body does.
pub(super) fn cached_func_ref(
    codegen: &mut CodeGen,
    builder: &mut FunctionBuilder<'_>,
    func_ref_cache: &mut HashMap<FuncId, FuncRef>,
    fid: FuncId,
) -> FuncRef {
    *func_ref_cache
        .entry(fid)
        .or_insert_with(|| codegen.module.declare_func_in_func(fid, builder.func))
}

/// `cached_func_ref` for a runtime function named by its import key, which
/// must exist.
pub(super) fn runtime_func_ref(
    codegen: &mut CodeGen,
    builder: &mut FunctionBuilder<'_>,
    func_ref_cache: &mut HashMap<FuncId, FuncRef>,
    runtime_funcs: &HashMap<String, FuncId>,
    name: &str,
) -> Result<FuncRef, CompileError> {
    let fid = runtime_funcs.get(name).copied().ok_or_else(|| {
        CompileError::ModuleError(format!("IR consumer: missing runtime function '{name}'"))
    })?;
    Ok(cached_func_ref(codegen, builder, func_ref_cache, fid))
}


/// Whether this build should emit green-preemption safe-points. On by default:
/// green is the default backend, and without safe-points a task that loops on a
/// value another task has to publish never gives up its worker, so it starves the
/// tasks it is waiting for and the program hangs.
///
/// `PITH_GREEN_PREEMPT=0` is the escape hatch for a build that will never run
/// green and does not want to pay for a check that cannot fire. The off spellings
/// are as generous as `PITH_GREEN`'s, and for the same reason: someone reaching
/// for the opt-out who writes `no` must not silently keep the safe-points.
pub(super) fn preempt_from_env(value: Option<&str>) -> bool {
    !matches!(
        value.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("0") | Some("off") | Some("false") | Some("no") | Some("n")
    )
}

/// Read `PITH_GREEN_PREEMPT` and answer whether to emit the safe-points.
pub(super) fn green_preempt_enabled() -> bool {
    preempt_from_env(std::env::var("PITH_GREEN_PREEMPT").ok().as_deref())
}

/// Whether this build checks generated code for a use after free, read from
/// `PITH_QUARANTINE` when the program is compiled (the runtime reads the same
/// variable when it runs; see its quarantine.rs). Such a build leaves out the
/// inline fast paths, so every list, map, bytes and string read is a runtime
/// call that validates its handle, and it tests the header of a struct before
/// each field read or write. A default build emits none of this.
pub(super) fn quarantine_build() -> bool {
    pith_runtime::quarantine::enabled_by(std::env::var("PITH_QUARANTINE").ok().as_deref())
}

/// Floats travel as i64 bits in the IR. Bitcast each argument the callee's
/// signature takes as an f64.
pub(super) fn bitcast_float_args(builder: &mut FunctionBuilder<'_>, fref: FuncRef, args: &mut [Value]) {
    let sig_ref = builder.func.dfg.ext_funcs[fref].signature;
    let floats: Vec<bool> = builder.func.dfg.signatures[sig_ref]
        .params
        .iter()
        .map(|p| p.value_type == types::F64)
        .collect();
    for (arg, is_float) in args.iter_mut().zip(floats) {
        if is_float {
            *arg = builder.ins().bitcast(types::F64, MemFlags::new(), *arg);
        }
    }
}

/// The checks a struct field read or store makes before it touches memory:
/// the quarantine test of the object, when quarantine is on, and the
/// sanitizer hook for the field's address. A default build emits neither.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_struct_access_checks(
    codegen: &mut CodeGen,
    builder: &mut FunctionBuilder<'_>,
    func_ref_cache: &mut HashMap<FuncId, FuncRef>,
    runtime_funcs: &HashMap<String, FuncId>,
    quarantine: bool,
    sanitize: Option<SanitizeHooks>,
    write: bool,
    obj: Value,
    offset: i32,
) -> Result<(), CompileError> {
    if quarantine {
        let check = runtime_func_ref(
            codegen,
            builder,
            func_ref_cache,
            runtime_funcs,
            "pith_quarantine_check_struct",
        )?;
        emit_quarantine_struct_check(builder, check, obj);
    }
    emit_sanitize_check(codegen, builder, func_ref_cache, sanitize, write, obj, offset);
    Ok(())
}

/// The struct test a quarantine build makes before generated code touches a
/// field of `obj`: a header that is not a live struct's (another magic, or
/// the dead bit set) goes to the runtime, which reports a freed or dead
/// struct and lets any other pointer through.
pub(super) fn emit_quarantine_struct_check(builder: &mut FunctionBuilder<'_>, check: FuncRef, obj: Value) {
    let magic = builder
        .ins()
        .load(types::I32, MemFlags::new(), obj, -(STRUCT_HEADER as i32));
    let flags = builder
        .ins()
        .load(types::I32, MemFlags::new(), obj, STRUCT_FLAGS_FROM_DATA);
    let foreign = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, magic, STRUCT_MAGIC as i64);
    let dead_bit = builder.ins().band_imm(flags, STRUCT_DEAD_MASK as i64);
    let dead = builder.ins().icmp_imm(IntCC::NotEqual, dead_bit, 0);
    let bad = builder.ins().bor(foreign, dead);
    let slow = builder.create_block();
    let done = builder.create_block();
    builder.set_cold_block(slow);
    builder.ins().brif(bad, slow, &[], done, &[]);
    builder.switch_to_block(slow);
    builder.ins().call(check, &[obj]);
    builder.ins().jump(done, &[]);
    builder.switch_to_block(done);
}

/// The sanitizer's checks for one 8-byte read and one 8-byte write, for a
/// build linked against a sanitized runtime.
#[derive(Clone, Copy)]
pub(super) struct SanitizeHooks {
    pub(super) read: FuncId,
    pub(super) write: FuncId,
}

/// The hook symbols `PITH_SANITIZE` names: `thread` or `address`. Unset,
/// empty or `0` means none.
pub(super) fn sanitize_hook_names(value: Option<&str>) -> Result<Option<(&'static str, &'static str)>, String> {
    match value.map(str::trim) {
        None | Some("") | Some("0") => Ok(None),
        Some("thread") => Ok(Some(("__tsan_read8", "__tsan_write8"))),
        Some("address") => Ok(Some(("__asan_load8", "__asan_store8"))),
        Some(other) => Err(format!("PITH_SANITIZE={other}: expected thread or address")),
    }
}

/// `PITH_SANITIZE`, for a build linked against a sanitized runtime. The
/// sanitizer only sees memory the runtime touches, so with it set the
/// emitted code calls the runtime instead of taking an inline fast path, and
/// reports each struct field, optional payload and global it reads or writes
/// to the sanitizer's check. Every retain and release is a runtime call in
/// any build, so the counts need nothing here.
pub(super) fn sanitize_hooks(codegen: &mut CodeGen) -> Result<Option<SanitizeHooks>, CompileError> {
    let names = sanitize_hook_names(std::env::var("PITH_SANITIZE").ok().as_deref())
        .map_err(CompileError::ModuleError)?;
    let Some((read, write)) = names else {
        return Ok(None);
    };
    let mut sig = codegen.module.make_signature();
    sig.params.push(AbiParam::new(types::I64));
    let mut declare = |name: &str| {
        codegen
            .module
            .declare_function(name, Linkage::Import, &sig)
            .map_err(|e| CompileError::ModuleError(e.to_string()))
    };
    Ok(Some(SanitizeHooks { read: declare(read)?, write: declare(write)? }))
}

/// Report an 8-byte access at `addr + offset` to the sanitizer, when the
/// build has one.
pub(super) fn emit_sanitize_check(
    codegen: &mut CodeGen,
    builder: &mut FunctionBuilder<'_>,
    func_ref_cache: &mut HashMap<FuncId, FuncRef>,
    hooks: Option<SanitizeHooks>,
    write: bool,
    addr: Value,
    offset: i32,
) {
    let Some(hooks) = hooks else {
        return;
    };
    let hook = if write { hooks.write } else { hooks.read };
    let fref = cached_func_ref(codegen, builder, func_ref_cache, hook);
    let at = builder.ins().iadd_imm(addr, offset as i64);
    builder.ins().call(fref, &[at]);
}

/// Emit a cooperative-preemption safe-point inline, just before a loop back-edge
/// branch. Loads the process-global `PITH_PREEMPT_REQUESTED` flag (declared in
/// the runtime, see `green.rs`); on the hot path, with the flag clear, this is a
/// single i8 load and a predicted-not-taken branch, no call. Only when the green
/// monitor has set the flag do we call `pith_green_maybe_yield`, which itself
/// decides whether the running task must actually yield (and is a no-op off the
/// green backend, so this is inert under os threads even when reached).
///
/// Leaves the builder positioned on a fresh continuation block; the caller then
/// emits the original back-edge branch there.
pub(super) fn emit_preempt_safepoint(
    codegen: &mut CodeGen,
    builder: &mut FunctionBuilder<'_>,
    func_ref_cache: &mut HashMap<FuncId, FuncRef>,
    runtime_funcs: &HashMap<String, FuncId>,
    preempt_flag: DataId,
) -> Result<(), CompileError> {
    // Load the flag byte from the imported global.
    let gv = codegen.module.declare_data_in_func(preempt_flag, builder.func);
    let addr = builder.ins().global_value(types::I64, gv);
    let flag = builder
        .ins()
        .load(types::I8, MemFlags::new(), addr, 0);
    let requested = builder.ins().icmp_imm(IntCC::NotEqual, flag, 0);

    let yield_block = builder.create_block();
    let cont_block = builder.create_block();
    // the flag is almost never set, so the call goes out of line: placed
    // after the loop and kept from shaping the loop's register assignment.
    builder.set_cold_block(yield_block);
    // flag set -> slow path; clear -> straight to the continuation.
    builder
        .ins()
        .brif(requested, yield_block, &[], cont_block, &[]);

    // Slow path: call into the runtime, then fall through to the continuation.
    builder.switch_to_block(yield_block);
    let yield_ref = runtime_func_ref(
        codegen,
        builder,
        func_ref_cache,
        runtime_funcs,
        "pith_green_maybe_yield",
    )?;
    builder.ins().call(yield_ref, &[]);
    builder.ins().jump(cont_block, &[]);

    // Continuation: the caller emits the original back-edge branch here. Blocks
    // are sealed en masse at the end of the function, so no seal is needed now.
    builder.switch_to_block(cont_block);
    Ok(())
}

pub(super) fn emit_runtime_error_value(
    codegen: &mut CodeGen,
    builder: &mut FunctionBuilder<'_>,
    func_ref_cache: &mut HashMap<FuncId, FuncRef>,
    runtime_funcs: &HashMap<String, FuncId>,
    code: i64,
) -> Result<Value, CompileError> {
    let error_ref = runtime_func_ref(
        codegen,
        builder,
        func_ref_cache,
        runtime_funcs,
        "pith_runtime_error",
    )?;
    let code_value = builder.ins().iconst(types::I64, code);
    let call = builder.ins().call(error_ref, &[code_value]);
    Ok(builder.func.dfg.first_result(call))
}

