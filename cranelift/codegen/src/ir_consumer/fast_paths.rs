//! Inline Cranelift fast paths for lists, bytes, cstrings, closures, and
//! handle/magic validation.

use super::arith::{emit_int_fit, iconst_of};
use super::instrumentation::{cached_func_ref, runtime_func_ref};
use crate::{loop_hoist, CodeGen, CompileError};
use cranelift::codegen::ir::{FuncRef, Function};
use cranelift::prelude::*;
use cranelift_module::{DataId, FuncId, Module};
use pith_runtime::bytes::{
    BYTES_DATA_LEN_OFFSET, BYTES_DATA_PTR_OFFSET, BYTES_HANDLE_ALIGN, BYTES_MAGIC,
    BYTES_MAGIC_OFFSET,
};
use pith_runtime::collections::list::{
    LIST_HANDLE_ALIGN, LIST_IMPL_ELEM_SIZE_OFFSET, LIST_IMPL_TYPE_TAG_OFFSET,
    LIST_IMPL_VALUES8_LEN_OFFSET, LIST_IMPL_VALUES8_PTR_OFFSET, LIST_MAGIC,
    LIST_TYPE_TAG_PRIMITIVE,
};
use pith_runtime::runtime_core::{
    CLOSURE_ENV_OFFSET, CLOSURE_FUNC_PTR_OFFSET, CLOSURE_HANDLE_ALIGN, CLOSURE_MAGIC,
    CLOSURE_MAGIC_OFFSET, PITH_CLOSURE_ENV_SLOTS, STRUCT_FLAGS_FROM_DATA, STRUCT_SHARED_MASK,
};
use std::collections::{HashMap, HashSet};

/// Block arguments in the form Cranelift's branch builders take.
fn block_args<const N: usize>(
    values: [Value; N],
) -> [cranelift::codegen::ir::instructions::BlockArg; N] {
    values.map(cranelift::codegen::ir::instructions::BlockArg::Value)
}

pub(super) fn jump_with_i64_arg(builder: &mut FunctionBuilder<'_>, block: Block, value: Value) {
    builder.ins().jump(block, &block_args([value]));
}

fn jump_with_two_i64_args(builder: &mut FunctionBuilder<'_>, block: Block, a: Value, b: Value) {
    builder.ins().jump(block, &block_args([a, b]));
}

/// `brif` that carries `a` and `b` to `taken` and nothing to `fallthrough`.
/// The strict-index fast paths route every failed check to one slow block,
/// marked cold, that receives the handle and index as block parameters: the
/// runtime call's operands are then the slow block's own values, the moves
/// that feed them sit in the cold edge blocks, and the fast path is laid out
/// as one fall-through sequence.
fn brif_with_two_i64_args(
    builder: &mut FunctionBuilder<'_>,
    cond: Value,
    taken: Block,
    a: Value,
    b: Value,
    fallthrough: Block,
) {
    builder
        .ins()
        .brif(cond, taken, &block_args([a, b]), fallthrough, &[]);
}

/// Leave for `exit` with `args` when `cond` is set, and otherwise go on in
/// a fresh block, where the builder is left. The exit goes through an edge
/// block of its own, created before the fresh one.
fn exit_if<const N: usize>(
    builder: &mut FunctionBuilder<'_>,
    cond: Value,
    exit: Block,
    args: [Value; N],
) {
    let edge = builder.create_block();
    let next = builder.create_block();
    builder.ins().brif(cond, edge, &[], next, &[]);
    builder.switch_to_block(edge);
    builder.ins().jump(exit, &block_args(args));
    builder.switch_to_block(next);
}

/// `exit_if` for a condition that is set when the path may go on.
fn exit_unless<const N: usize>(
    builder: &mut FunctionBuilder<'_>,
    cond: Value,
    exit: Block,
    args: [Value; N],
) {
    let edge = builder.create_block();
    let next = builder.create_block();
    builder.ins().brif(cond, next, &[], edge, &[]);
    builder.switch_to_block(edge);
    builder.ins().jump(exit, &block_args(args));
    builder.switch_to_block(next);
}

/// The shared mark of the struct `obj` points into: nonzero once another
/// task can reach it. `obj` must be a live struct, never null.
fn struct_shared_mark(builder: &mut FunctionBuilder<'_>, obj: Value) -> Value {
    let word = builder.ins().load(types::I32, MemFlags::new(), obj, STRUCT_FLAGS_FROM_DATA);
    builder.ins().band_imm(word, STRUCT_SHARED_MASK as i64)
}

/// The slow side of a fast path: fill `slow` with a call to `fallback` on
/// `args` that joins `done` with its result, and leave the builder in
/// `done` on the merged value.
fn join_with_fallback_call(
    builder: &mut FunctionBuilder<'_>,
    slow: Block,
    done: Block,
    fallback: FuncRef,
    args: &[Value],
) -> Value {
    // the fallback is the rare case: laid out after the function's hot
    // blocks, and weighed as cold when registers are assigned.
    builder.set_cold_block(slow);
    builder.switch_to_block(slow);
    let call = builder.ins().call(fallback, args);
    let fallback_result = builder.func.dfg.first_result(call);
    jump_with_i64_arg(builder, done, fallback_result);

    builder.switch_to_block(done);
    builder.block_params(done)[0]
}

/// The blocks a strict fast path joins on: `done`, which takes the result,
/// and `slow`, which is cold and takes the handle and index every failed
/// check passes it (see `brif_with_two_i64_args`).
fn strict_path_blocks(builder: &mut FunctionBuilder<'_>) -> (Block, Block) {
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    let slow = builder.create_block();
    builder.append_block_param(slow, types::I64);
    builder.append_block_param(slow, types::I64);
    builder.set_cold_block(slow);
    (done, slow)
}

/// Runtime releases that return at once for a zero handle and return
/// nothing, so a call site may test for zero and skip the call.
pub(super) fn is_null_safe_release(name: &str) -> bool {
    matches!(
        name,
        "pith_struct_release"
            | "pith_list_release_handle"
            | "pith_map_release_handle"
            | "pith_set_release_handle"
            | "pith_bytes_release"
    )
}

fn inline_list_get_value(
    builder: &mut FunctionBuilder<'_>,
    list: Value,
    index: Value,
    checked: bool,
) -> Value {
    let zero = builder.ins().iconst(types::I64, 0);
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);

    let list_is_null = builder.ins().icmp_imm(IntCC::Equal, list, 0);
    exit_if(builder, list_is_null, done, [zero]);

    let index_is_negative = builder.ins().icmp_imm(IntCC::SignedLessThan, index, 0);
    exit_if(builder, index_is_negative, done, [zero]);

    let elem_size = builder.ins().load(
        types::I64,
        MemFlags::new(),
        list,
        LIST_IMPL_ELEM_SIZE_OFFSET,
    );
    let is_eight = builder.ins().icmp_imm(IntCC::Equal, elem_size, 8);
    exit_unless(builder, is_eight, done, [zero]);

    if checked {
        let len = builder.ins().load(
            types::I64,
            MemFlags::new(),
            list,
            LIST_IMPL_VALUES8_LEN_OFFSET,
        );
        let out_of_bounds = builder
            .ins()
            .icmp(IntCC::UnsignedGreaterThanOrEqual, index, len);
        exit_if(builder, out_of_bounds, done, [zero]);
    }

    let data_ptr = builder.ins().load(
        types::I64,
        MemFlags::new(),
        list,
        LIST_IMPL_VALUES8_PTR_OFFSET,
    );
    let data_is_null = builder.ins().icmp_imm(IntCC::Equal, data_ptr, 0);
    exit_if(builder, data_is_null, done, [zero]);

    let byte_offset = builder.ins().ishl_imm(index, 3);
    let elem_addr = builder.ins().iadd(data_ptr, byte_offset);
    let value = builder
        .ins()
        .load(types::I64, MemFlags::new(), elem_addr, 0);
    jump_with_i64_arg(builder, done, value);

    builder.switch_to_block(done);
    builder.block_params(done)[0]
}

// `xs[i]` lowers to a call to pith_list_get_opt, which heap-allocates a
// two-slot Optional tuple that the very next instructions unpack (flag at
// offset 0, value at offset 8) and release. When scan_unpackable_opt_regs
// proves a call's result is used only by that unpack pattern, the whole round
// trip collapses to these loads and compares, with no call, no allocation
// and no release. The checks mirror the runtime path exactly: a null or stale handle
// (magic scrubbed on free), a non-8-byte-element list, or an out-of-range
// index all yield is_some == 0, which the emitted IR turns into the same loud
// "index out of bounds" failure the runtime call produced.
pub(super) fn inline_list_get_opt(
    builder: &mut FunctionBuilder<'_>,
    list: Value,
    index: Value,
) -> (Value, Value) {
    let zero = builder.ins().iconst(types::I64, 0);
    let done = builder.create_block();
    builder.append_block_param(done, types::I64); // is_some
    builder.append_block_param(done, types::I64); // value

    let list_is_null = builder.ins().icmp_imm(IntCC::Equal, list, 0);
    exit_if(builder, list_is_null, done, [zero, zero]);

    // the magic word is scrubbed when a list is freed, so this rejects stale
    // handles the same way list_ref does in the runtime.
    let magic = builder.ins().load(types::I32, MemFlags::new(), list, 0);
    let magic_bad = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, magic, LIST_MAGIC as i64);
    exit_if(builder, magic_bad, done, [zero, zero]);

    let elem_size = builder.ins().load(
        types::I64,
        MemFlags::new(),
        list,
        LIST_IMPL_ELEM_SIZE_OFFSET,
    );
    let is_eight = builder.ins().icmp_imm(IntCC::Equal, elem_size, 8);
    exit_unless(builder, is_eight, done, [zero, zero]);

    // one unsigned compare covers both `index < 0` and `index >= len`.
    let len = builder.ins().load(
        types::I64,
        MemFlags::new(),
        list,
        LIST_IMPL_VALUES8_LEN_OFFSET,
    );
    let out_of_bounds = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, len);
    exit_if(builder, out_of_bounds, done, [zero, zero]);

    let data_ptr = builder.ins().load(
        types::I64,
        MemFlags::new(),
        list,
        LIST_IMPL_VALUES8_PTR_OFFSET,
    );
    let data_is_null = builder.ins().icmp_imm(IntCC::Equal, data_ptr, 0);
    exit_if(builder, data_is_null, done, [zero, zero]);

    let byte_offset = builder.ins().ishl_imm(index, 3);
    let elem_addr = builder.ins().iadd(data_ptr, byte_offset);
    let value = builder
        .ins()
        .load(types::I64, MemFlags::new(), elem_addr, 0);
    let one = builder.ins().iconst(types::I64, 1);
    jump_with_two_i64_args(builder, done, one, value);

    builder.switch_to_block(done);
    let params = builder.block_params(done);
    (params[0], params[1])
}

// `xs[i] = v` on a primitive-element list is an in-bounds store with no
// retain/release choreography, so the common case inlines to a handful of
// loads and one store. Anything the fast path cannot prove (a tagged list,
// with element counts to move; an out-of-bounds index, a silent no-op by the
// runtime's contract; a stale or null handle) falls back to the real runtime
// call, which keeps every semantic exactly as it was.
fn inline_list_set_value(
    builder: &mut FunctionBuilder<'_>,
    list: Value,
    index: Value,
    value: Value,
    fallback: FuncRef,
    anchor: FuncRef,
) {
    let done = builder.create_block();
    let slow = inline_list_set_fast_path(builder, list, index, value, anchor, done);
    builder.switch_to_block(slow);
    builder.ins().call(fallback, &[list, index, value]);
    builder.ins().jump(done, &[]);
    builder.switch_to_block(done);
}

/// A `_checked` store and the `brif` on its result, lowered as one branch.
/// The checked stores sit inside fallible functions, where the emitter
/// follows each with a branch to the store's continuation or to its miss
/// arm. Merging the flag into a block parameter would leave a constant 1 to
/// test on the fast path, so the fast path jumps straight to `stored` and only
/// the slow path tests the runtime's answer. The block is left terminated.
pub(super) fn inline_list_set_value_branch(
    builder: &mut FunctionBuilder<'_>,
    args: [Value; 3],
    fallback: FuncRef,
    anchor: FuncRef,
    stored: Block,
    missed: Block,
) {
    let [list, index, value] = args;
    let slow = inline_list_set_fast_path(builder, list, index, value, anchor, stored);
    builder.switch_to_block(slow);
    let call = builder.ins().call(fallback, &[list, index, value]);
    let did_store = builder.inst_results(call)[0];
    builder.ins().brif(did_store, stored, &[], missed, &[]);
}

/// The in-bounds store of a primitive list, which jumps to `done`. Returns
/// the cold block every other case branches to, for the caller to fill with
/// the runtime call.
fn inline_list_set_fast_path(
    builder: &mut FunctionBuilder<'_>,
    list: Value,
    index: Value,
    value: Value,
    anchor: FuncRef,
    done: Block,
) -> Block {
    let (live, base) = list_header(builder, list, anchor);
    let tag = builder
        .ins()
        .load(types::I32, loop_hoist::HEADER_FLAGS, base, LIST_IMPL_TYPE_TAG_OFFSET);
    let primitive = builder
        .ins()
        .icmp_imm(IntCC::Equal, tag, LIST_TYPE_TAG_PRIMITIVE as i64);
    let writable = builder.ins().band(live, primitive);
    let (in_bounds, data_ptr) = list_index_in_bounds(builder, writable, base, index);

    let store_block = builder.create_block();
    let slow = builder.create_block();
    builder.set_cold_block(slow);
    builder.ins().brif(in_bounds, store_block, &[], slow, &[]);

    builder.switch_to_block(store_block);
    let byte_offset = builder.ins().ishl_imm(index, 3);
    let elem_addr = builder.ins().iadd(data_ptr, byte_offset);
    builder
        .ins()
        .store(MemFlags::new(), value, elem_addr, 0);
    builder.ins().jump(done, &[]);
    slow
}

/// The (stored, missed) labels when `fname` is a `_checked` list store into
/// register `reg` and `next` is the `brif` on that register.
pub(super) fn checked_store_branch<'a>(fname: &str, reg: usize, next: Option<&&'a str>) -> Option<(&'a str, &'a str)> {
    if !matches!(
        fname,
        "pith_list_set_value_checked" | "pith_list_set_value_owned_checked" | "pith_list_set_scalar_checked"
    ) {
        return None;
    }
    let parts: Vec<&'a str> = next?.split_whitespace().collect();
    match parts.as_slice() {
        ["brif", cond, stored, missed] if cond.parse::<usize>() == Ok(reg) => Some((stored, missed)),
        _ => None,
    }
}

/// Read a list's header without a branch: whether `list` is a live list,
/// and the address its header fields are read from. A handle that is null
/// or misaligned reads `anchor`'s code instead, which is always mapped, and
/// the handle test masks out what it holds; a freed list fails the magic
/// compare, since the runtime scrubs the magic. The reads carry
/// `loop_hoist::HEADER_FLAGS`, so a loop that makes no calls reads each
/// header once, before it starts.
fn list_header(builder: &mut FunctionBuilder<'_>, list: Value, anchor: FuncRef) -> (Value, Value) {
    let addressable = handle_is_addressable(builder, list, LIST_HANDLE_ALIGN);
    let anchor_code = builder.ins().func_addr(types::I64, anchor);
    let base = builder.ins().select(addressable, list, anchor_code);
    let magic = builder
        .ins()
        .load(types::I32, loop_hoist::HEADER_FLAGS, base, 0);
    let magic_ok = builder
        .ins()
        .icmp_imm(IntCC::Equal, magic, LIST_MAGIC as i64);
    (builder.ins().band(addressable, magic_ok), base)
}

/// Whether `index` is inside a list that passed `usable`, folding a
/// negative index into the same unsigned compare, and the list's data
/// pointer. A list that failed reads as empty, so the compare fails too.
/// The length is the one the 8-byte storage keeps, which stays zero for a
/// list of any other element size, so the compare also turns those away;
/// and a non-empty list of 8-byte elements always has a data pointer.
fn list_index_in_bounds(
    builder: &mut FunctionBuilder<'_>,
    usable: Value,
    base: Value,
    index: Value,
) -> (Value, Value) {
    let flags = loop_hoist::HEADER_FLAGS;
    let len = builder
        .ins()
        .load(types::I64, flags, base, LIST_IMPL_VALUES8_LEN_OFFSET);
    let zero = builder.ins().iconst(types::I64, 0);
    let limit = builder.ins().select(usable, len, zero);
    let in_bounds = builder.ins().icmp(IntCC::UnsignedLessThan, index, limit);
    let data_ptr = builder
        .ins()
        .load(types::I64, flags, base, LIST_IMPL_VALUES8_PTR_OFFSET);
    (in_bounds, data_ptr)
}


/// The address of a string literal's characters, just past the header that
/// `declare_string_data` writes in front of them.
pub(super) fn string_lit_addr(codegen: &mut CodeGen, builder: &mut FunctionBuilder<'_>, lit: DataId) -> Value {
    let gv = codegen.module.declare_data_in_func(lit, builder.func);
    let base = builder.ins().global_value(types::I64, gv);
    builder.ins().iadd_imm(base, 16)
}

/// Whether a one-argument call is a retain or release that the runtime would
/// return from without touching anything: any of them on a constant zero
/// (null), and a string retain or release on a literal, which has no count.
pub(super) fn is_noop_count_call(
    fname: &str,
    arg: Value,
    literal_values: &HashSet<Value>,
    func: &Function,
) -> bool {
    let string_count = matches!(fname, "pith_cstring_retain" | "pith_cstring_release");
    let handle_release = matches!(
        fname,
        "pith_list_release_handle"
            | "pith_map_release_handle"
            | "pith_set_release_handle"
            | "pith_struct_release"
            | "pith_closure_release"
            | "pith_bytes_release"
    );
    if !string_count && !handle_release {
        return false;
    }
    if string_count && literal_values.contains(&arg) {
        return true;
    }
    let cranelift::codegen::ir::ValueDef::Result(inst, _) = func.dfg.value_def(arg) else {
        return false;
    };
    matches!(
        func.dfg.insts[inst],
        cranelift::codegen::ir::InstructionData::UnaryImm {
            opcode: cranelift::codegen::ir::Opcode::Iconst,
            imm,
        } if imm.bits() == 0
    )
}


fn inline_bytes_get(builder: &mut FunctionBuilder<'_>, bytes: Value, index: Value) -> Value {
    let zero = builder.ins().iconst(types::I64, 0);
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);

    let bytes_is_null = builder.ins().icmp_imm(IntCC::Equal, bytes, 0);
    exit_if(builder, bytes_is_null, done, [zero]);

    let index_is_negative = builder.ins().icmp_imm(IntCC::SignedLessThan, index, 0);
    exit_if(builder, index_is_negative, done, [zero]);

    let data_ptr = builder.ins().load(types::I64, MemFlags::new(), bytes, BYTES_DATA_PTR_OFFSET);
    let len = builder.ins().load(types::I64, MemFlags::new(), bytes, BYTES_DATA_LEN_OFFSET);
    let out_of_bounds = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, len);
    exit_if(builder, out_of_bounds, done, [zero]);

    let elem_addr = builder.ins().iadd(data_ptr, index);
    let byte = builder.ins().load(types::I8, MemFlags::new(), elem_addr, 0);
    let value = builder.ins().uextend(types::I64, byte);
    jump_with_i64_arg(builder, done, value);

    builder.switch_to_block(done);
    builder.block_params(done)[0]
}

/// Whether `handle` is non-null and `align`-aligned, in one compare:
/// rotating the low s bits to the top (s = log2 align) turns an aligned
/// non-null handle into a value in 1..2^(64-s), null into 0, and a
/// misaligned handle into a value with a top bit set. Subtracting one then
/// puts exactly the good handles below 2^(64-s) - 1 as unsigned values.
fn handle_is_addressable(builder: &mut FunctionBuilder<'_>, handle: Value, align: i64) -> Value {
    debug_assert!((align as u64).is_power_of_two());
    let shift = align.trailing_zeros() as i64;
    let rotated = builder.ins().rotr_imm(handle, shift);
    let rotated_m1 = builder.ins().iadd_imm(rotated, -1);
    let good_limit = ((1u64 << (64 - shift)) - 1) as i64;
    builder
        .ins()
        .icmp_imm(IntCC::UnsignedLessThan, rotated_m1, good_limit)
}

/// Branch to `slow(handle, index)` unless `handle` is a pointer the
/// runtime's magic-checked accessor would accept: non-null, aligned as the
/// runtime struct requires, and carrying `magic` at `magic_offset`. This is
/// `plausibly_aligned` plus the magic compare, in that order, so a null or
/// misaligned handle is rejected before anything is read through it,
/// exactly as the runtime does. On return the builder sits in a fresh block
/// where the handle is known good.
fn branch_if_not_magic_handle(
    builder: &mut FunctionBuilder<'_>,
    handle: Value,
    index: Value,
    align: i64,
    magic_offset: i32,
    magic: u32,
    slow: Block,
) {
    let addressable = handle_is_addressable(builder, handle, align);
    let handle_bad = builder.ins().icmp_imm(IntCC::Equal, addressable, 0);
    let check_magic = builder.create_block();
    brif_with_two_i64_args(builder, handle_bad, slow, handle, index, check_magic);
    builder.switch_to_block(check_magic);

    let word = builder
        .ins()
        .load(types::I32, MemFlags::new(), handle, magic_offset);
    let magic_bad = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, word, magic as i64);
    let good = builder.create_block();
    brif_with_two_i64_args(builder, magic_bad, slow, handle, index, good);
    builder.switch_to_block(good);
}

/// The shared tail of the strict fast paths: `slow` calls `fallback` on the
/// handle and index it was handed and joins `done` with the result, and the
/// builder is left in `done` with the merged value.
fn finish_strict_fast_path(
    builder: &mut FunctionBuilder<'_>,
    slow: Block,
    done: Block,
    fallback: FuncRef,
) -> Value {
    let params = builder.block_params(slow);
    let (handle, index) = (params[0], params[1]);
    join_with_fallback_call(builder, slow, done, fallback, &[handle, index])
}

/// Inline `bytes[i]`, the strict form that aborts instead of answering
/// zero, as a magic check, one bounds compare and one byte load. The fast
/// path accepts exactly the inputs `pith_bytes_get_strict` accepts: a
/// handle `pith_bytes_ref` would take (non-null, aligned, magic intact) and
/// an index in `0..len`, the sign and bound folded into one unsigned
/// compare. Everything else jumps to `fallback`, the runtime function
/// itself, which re-runs its own checks in its own order and prints the
/// same diagnostic and exit code it always has; so the failure modes are
/// byte-identical and the fast path only changes the cost of a read that
/// was going to succeed.
fn inline_bytes_get_strict(
    builder: &mut FunctionBuilder<'_>,
    bytes: Value,
    index: Value,
    fallback: FuncRef,
) -> Value {
    let (done, slow) = strict_path_blocks(builder);

    branch_if_not_magic_handle(
        builder,
        bytes,
        index,
        BYTES_HANDLE_ALIGN,
        BYTES_MAGIC_OFFSET,
        BYTES_MAGIC,
        slow,
    );

    let len = builder.ins().load(
        types::I64,
        MemFlags::new(),
        bytes,
        BYTES_DATA_LEN_OFFSET,
    );
    let out_of_bounds = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, len);
    let read = builder.create_block();
    brif_with_two_i64_args(builder, out_of_bounds, slow, bytes, index, read);
    builder.switch_to_block(read);

    let data_ptr = builder.ins().load(
        types::I64,
        MemFlags::new(),
        bytes,
        BYTES_DATA_PTR_OFFSET,
    );
    let elem_addr = builder.ins().iadd(data_ptr, index);
    let byte = builder.ins().load(types::I8, MemFlags::new(), elem_addr, 0);
    let value = builder.ins().uextend(types::I64, byte);
    jump_with_i64_arg(builder, done, value);

    finish_strict_fast_path(builder, slow, done, fallback)
}

/// Inline `xs[i]` (`pith_list_get_value_strict`, the form that aborts on a
/// bad index) the same way as `inline_bytes_get_strict`: the fast path
/// accepts exactly what the runtime accepts and everything else calls the
/// runtime function, which produces the diagnostic. The checks are the
/// handle test `list_ref` performs (non-null, aligned, magic intact; the
/// magic is scrubbed when a list is freed, so a stale handle fails here),
/// read without a branch by `list_header`, and one unsigned compare against
/// the 8-byte storage length for both `i < 0` and `i >= len`. A list whose
/// elements are not 8 bytes wide keeps that length at zero, so the same
/// compare turns it away without a separate element-size check. The data
/// pointer needs no null test: a non-empty 8-byte list always has one.
pub(super) fn inline_list_get_value_strict(
    builder: &mut FunctionBuilder<'_>,
    list: Value,
    index: Value,
    fallback: FuncRef,
    anchor: FuncRef,
) -> Value {
    let (live, base) = list_header(builder, list, anchor);
    let (in_bounds, data_ptr) = list_index_in_bounds(builder, live, base, index);

    let (done, slow) = strict_path_blocks(builder);
    let out_of_bounds = builder.ins().icmp_imm(IntCC::Equal, in_bounds, 0);
    let read = builder.create_block();
    brif_with_two_i64_args(builder, out_of_bounds, slow, list, index, read);
    builder.switch_to_block(read);

    let byte_offset = builder.ins().ishl_imm(index, 3);
    let elem_addr = builder.ins().iadd(data_ptr, byte_offset);
    let value = builder
        .ins()
        .load(types::I64, MemFlags::new(), elem_addr, 0);
    jump_with_i64_arg(builder, done, value);

    finish_strict_fast_path(builder, slow, done, fallback)
}

/// Inline `xs.len()` (`pith_list_len`) as the header read the getters use.
/// A list only keeps its length in the 8-byte storage when its elements are
/// 8 bytes wide; any other list, and a bad handle, goes to `fallback`,
/// which answers 0 for a bad handle as it always has.
fn inline_list_len(builder: &mut FunctionBuilder<'_>, list: Value, fallback: FuncRef) -> Value {
    let (live, base) = list_header(builder, list, fallback);
    let elem_size = builder.ins().load(
        types::I64,
        loop_hoist::HEADER_FLAGS,
        base,
        LIST_IMPL_ELEM_SIZE_OFFSET,
    );
    let eight = builder.ins().icmp_imm(IntCC::Equal, elem_size, 8);
    let values8 = builder.ins().band(live, eight);
    let len = builder.ins().load(
        types::I64,
        loop_hoist::HEADER_FLAGS,
        base,
        LIST_IMPL_VALUES8_LEN_OFFSET,
    );
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    let fast = builder.create_block();
    let slow = builder.create_block();
    builder.set_cold_block(slow);
    builder.ins().brif(values8, fast, &[], slow, &[]);

    builder.switch_to_block(fast);
    jump_with_i64_arg(builder, done, len);

    join_with_fallback_call(builder, slow, done, fallback, &[list])
}

/// Inline a one-word field read off a bytes handle (`b.len()`, the data
/// pointer): the handle test the runtime makes,
/// then one load at `field_offset`. Everything the fast path does not accept,
/// null included, goes to `fallback`, which answers for a bad handle as it
/// always has. A loop that tests `i < b.len()` makes this read once per pass,
/// so the FFI call cost more than the loop body did.
#[allow(clippy::too_many_arguments)]
fn inline_handle_field(
    builder: &mut FunctionBuilder<'_>,
    handle: Value,
    align: i64,
    magic_offset: i32,
    magic: u32,
    field_offset: i32,
    fallback: FuncRef,
) -> Value {
    let (done, slow) = strict_path_blocks(builder);

    // the handle test passes two values to `slow`; a field read has no
    // index, so the handle fills both slots
    branch_if_not_magic_handle(builder, handle, handle, align, magic_offset, magic, slow);
    let field = builder
        .ins()
        .load(types::I64, MemFlags::new(), handle, field_offset);
    jump_with_i64_arg(builder, done, field);

    let handle = builder.block_params(slow)[0];
    join_with_fallback_call(builder, slow, done, fallback, &[handle])
}

/// Inline a closure body's read of one of its captures
/// (`pith_closure_get_env`) as the handle test `closure_base` makes and one
/// load from the inline env. Every call into a closure reads its captures
/// this way, so the FFI call was most of the cost of a small closure. A slot
/// past the inline block lives in the spill vec, and it and a bad handle go
/// to `fallback`, which answers as it always has.
fn inline_closure_get_env(
    builder: &mut FunctionBuilder<'_>,
    closure: Value,
    slot: Value,
    fallback: FuncRef,
) -> Value {
    // a closure body reading its own capture with a constant slot: the
    // closure is the body's first parameter, and every call through a closure
    // value tests the handle before it jumps here (`closure_fn_ptr`), so the
    // test would only repeat that one; a constant slot inside the inline
    // block cannot be spilled. what is left is the load. small closures are
    // called in loops, where the repeated test was a third of the body.
    if let Some(slot_index) = iconst_of(builder.func, slot) {
        let own_env = builder.func.layout.entry_block().is_some_and(|entry| {
            matches!(builder.func.dfg.value_def(closure), cranelift::codegen::ir::ValueDef::Param(block, 0) if block == entry)
        });
        if own_env && (0..PITH_CLOSURE_ENV_SLOTS as i64).contains(&slot_index) {
            return builder.ins().load(
                types::I64,
                MemFlags::new(),
                closure,
                CLOSURE_ENV_OFFSET + (slot_index as i32) * 8,
            );
        }
    }
    let (done, slow) = strict_path_blocks(builder);

    branch_if_not_magic_handle(
        builder,
        closure,
        slot,
        CLOSURE_HANDLE_ALIGN,
        CLOSURE_MAGIC_OFFSET,
        CLOSURE_MAGIC,
        slow,
    );
    // one unsigned compare rejects a negative slot and a spilled one
    let spilled = builder.ins().icmp_imm(
        IntCC::UnsignedGreaterThanOrEqual,
        slot,
        PITH_CLOSURE_ENV_SLOTS as i64,
    );
    let read = builder.create_block();
    brif_with_two_i64_args(builder, spilled, slow, closure, slot, read);
    builder.switch_to_block(read);

    let offset = builder.ins().ishl_imm(slot, 3);
    let addr = builder.ins().iadd(closure, offset);
    let value = builder
        .ins()
        .load(types::I64, MemFlags::new(), addr, CLOSURE_ENV_OFFSET);
    jump_with_i64_arg(builder, done, value);

    finish_strict_fast_path(builder, slow, done, fallback)
}

/// The function a call through a closure value jumps to: the handle test
/// `closure_base` makes and one load of `func_ptr`, with a bad handle sent to
/// `pith_closure_get_fn`, which answers as it always has. Every call through a
/// closure value made that FFI call before the indirect call itself.
/// Look up or validate the function pointer of a closure handle, caching
/// validated closures in `tested_closures` until the next IR label or store to
/// that local.
pub(super) fn closure_fn_ptr(
    codegen: &mut CodeGen,
    builder: &mut FunctionBuilder<'_>,
    func_ref_cache: &mut HashMap<FuncId, FuncRef>,
    runtime_funcs: &HashMap<String, FuncId>,
    tested_closures: &mut Vec<(String, Value)>,
    closure: Value,
    local: Option<&str>,
    fast_paths: bool,
) -> Value {
    if !fast_paths {
        let Some(&fid) = runtime_funcs.get("pith_closure_get_fn") else {
            return closure;
        };
        let fref = cached_func_ref(codegen, builder, func_ref_cache, fid);
        let call = builder.ins().call(fref, &[closure]);
        return builder.func.dfg.first_result(call);
    }
    let key = match local {
        Some(name) => format!("n:{name}"),
        None => format!("v:{}", closure.as_u32()),
    };
    if let Some(fn_ptr) = tested_closures.iter().find(|(k, _)| *k == key).map(|(_, f)| *f) {
        return fn_ptr;
    }
    let fn_ptr = closure_fn_ptr_tested(codegen, builder, func_ref_cache, runtime_funcs, closure);
    tested_closures.push((key, fn_ptr));
    fn_ptr
}

fn closure_fn_ptr_tested(
    codegen: &mut CodeGen,
    builder: &mut FunctionBuilder<'_>,
    func_ref_cache: &mut HashMap<FuncId, FuncRef>,
    runtime_funcs: &HashMap<String, FuncId>,
    closure: Value,
) -> Value {
    let Some(&fid) = runtime_funcs.get("pith_closure_get_fn") else {
        return closure;
    };
    let fallback = cached_func_ref(codegen, builder, func_ref_cache, fid);
    let (done, slow) = strict_path_blocks(builder);

    // a zero fills the index slot of `slow`, which keeps the handle out of a
    // second register on the cold edge
    let zero = builder.ins().iconst(types::I64, 0);
    branch_if_not_magic_handle(
        builder,
        closure,
        zero,
        CLOSURE_HANDLE_ALIGN,
        CLOSURE_MAGIC_OFFSET,
        CLOSURE_MAGIC,
        slow,
    );
    let fn_ptr = builder
        .ins()
        .load(types::I64, MemFlags::new(), closure, CLOSURE_FUNC_PTR_OFFSET);
    jump_with_i64_arg(builder, done, fn_ptr);

    let handle = builder.block_params(slow)[0];
    join_with_fallback_call(builder, slow, done, fallback, &[handle])
}

/// Call the closure `closure` with `args`: its function takes the closure
/// itself first, for its captures, then the arguments, all as i64, and
/// returns one i64.
pub(super) fn emit_closure_call(
    codegen: &mut CodeGen,
    builder: &mut FunctionBuilder<'_>,
    func_ref_cache: &mut HashMap<FuncId, FuncRef>,
    runtime_funcs: &HashMap<String, FuncId>,
    tested_closures: &mut Vec<(String, Value)>,
    closure: Value,
    local: Option<&str>,
    args: &[Value],
    fast_paths: bool,
) -> Value {
    let fn_ptr = closure_fn_ptr(
        codegen,
        builder,
        func_ref_cache,
        runtime_funcs,
        tested_closures,
        closure,
        local,
        fast_paths,
    );
    let mut sig = codegen.module.make_signature();
    sig.params.push(AbiParam::new(types::I64));
    for _ in args {
        sig.params.push(AbiParam::new(types::I64));
    }
    sig.returns.push(AbiParam::new(types::I64));
    let sig_ref = builder.import_signature(sig);
    let mut indirect_args = vec![closure];
    indirect_args.extend_from_slice(args);
    let call = builder.ins().call_indirect(sig_ref, fn_ptr, &indirect_args);
    builder.func.dfg.first_result(call)
}

/// Inline the store of one capture (`pith_closure_set_env`) the way
/// `inline_closure_get_env` inlines the read: the handle test, then one store
/// into the inline env. A closure is built with one of these per capture, so
/// the FFI call was a large share of building a small one. A spilled slot and
/// a bad handle go to `fallback`.
/// Whether `v` is the address of a closure this function built on its own
/// stack: such a handle is valid for the whole function, so its handle test
/// can only pass.
fn is_stack_built(func: &cranelift::codegen::ir::Function, v: Value) -> bool {
    let cranelift::codegen::ir::ValueDef::Result(inst, _) = func.dfg.value_def(v) else {
        return false;
    };
    func.dfg.insts[inst].opcode() == cranelift::codegen::ir::Opcode::StackAddr
}

fn inline_closure_set_env(
    builder: &mut FunctionBuilder<'_>,
    closure: Value,
    slot: Value,
    value: Value,
    fallback: FuncRef,
) {
    // filling in a closure built on this stack, at a constant slot inside the
    // inline block: the handle test and the spill check cannot fail
    if is_stack_built(builder.func, closure) {
        if let Some(slot_index) = iconst_of(builder.func, slot) {
            if (0..PITH_CLOSURE_ENV_SLOTS as i64).contains(&slot_index) {
                builder.ins().store(
                    MemFlags::new(),
                    value,
                    closure,
                    CLOSURE_ENV_OFFSET + (slot_index as i32) * 8,
                );
                return;
            }
        }
    }
    let done = builder.create_block();
    let slow = builder.create_block();
    builder.append_block_param(slow, types::I64);
    builder.append_block_param(slow, types::I64);
    builder.set_cold_block(slow);

    branch_if_not_magic_handle(
        builder,
        closure,
        slot,
        CLOSURE_HANDLE_ALIGN,
        CLOSURE_MAGIC_OFFSET,
        CLOSURE_MAGIC,
        slow,
    );
    let spilled = builder.ins().icmp_imm(
        IntCC::UnsignedGreaterThanOrEqual,
        slot,
        PITH_CLOSURE_ENV_SLOTS as i64,
    );
    let write = builder.create_block();
    brif_with_two_i64_args(builder, spilled, slow, closure, slot, write);
    builder.switch_to_block(write);
    let offset = builder.ins().ishl_imm(slot, 3);
    let addr = builder.ins().iadd(closure, offset);
    builder
        .ins()
        .store(MemFlags::new(), value, addr, CLOSURE_ENV_OFFSET);
    builder.ins().jump(done, &[]);

    builder.switch_to_block(slow);
    let params = builder.block_params(slow);
    let (handle, index) = (params[0], params[1]);
    builder.ins().call(fallback, &[handle, index, value]);
    builder.ins().jump(done, &[]);

    builder.switch_to_block(done);
}

/// Inline bytes.read_word for the common case, a count of 1..=8 with a
/// full 8-byte window inside the data, as one load and a mask, skipping
/// the FFI call that dominates word-at-a-time scanners (the xxhash kernel,
/// the bitstream refill). Everything else (null handle, reads within the
/// last seven bytes, bad counts) falls back to pith_bytes_read_word for
/// identical semantics. The checks mirror inline_bytes_get: the 8-byte
/// load never reads past the allocation because the fast path requires
/// off + 8 <= len.
fn inline_bytes_read_word(
    builder: &mut FunctionBuilder<'_>,
    bytes: Value,
    off: Value,
    count: Value,
    fallback: FuncRef,
) -> Value {
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    let slow = builder.create_block();

    let is_null = builder.ins().icmp_imm(IntCC::Equal, bytes, 0);
    let after_null = builder.create_block();
    builder.ins().brif(is_null, slow, &[], after_null, &[]);
    builder.switch_to_block(after_null);

    // count outside 1..=8: one unsigned compare on count - 1
    let count_m1 = builder.ins().iadd_imm(count, -1);
    let count_bad = builder
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, count_m1, 8);
    let after_count = builder.create_block();
    builder.ins().brif(count_bad, slow, &[], after_count, &[]);
    builder.switch_to_block(after_count);

    // a negative offset must not reach the address arithmetic
    let off_neg = builder.ins().icmp_imm(IntCC::SignedLessThan, off, 0);
    let after_neg = builder.create_block();
    builder.ins().brif(off_neg, slow, &[], after_neg, &[]);
    builder.switch_to_block(after_neg);

    // require the full window: off + 8 <= len. off is non-negative here, so
    // the add can only wrap for absurd offsets, which the unsigned compare
    // then routes to the slow path anyway.
    let len = builder.ins().load(types::I64, MemFlags::new(), bytes, BYTES_DATA_LEN_OFFSET);
    let off_p8 = builder.ins().iadd_imm(off, 8);
    let no_room = builder.ins().icmp(IntCC::UnsignedGreaterThan, off_p8, len);
    let read = builder.create_block();
    builder.ins().brif(no_room, slow, &[], read, &[]);
    builder.switch_to_block(read);

    let data_ptr = builder.ins().load(types::I64, MemFlags::new(), bytes, BYTES_DATA_PTR_OFFSET);
    let addr = builder.ins().iadd(data_ptr, off);
    let word = builder.ins().load(types::I64, MemFlags::new(), addr, 0);
    // keep the low `count` bytes: all-ones shifted down by (8 - count) * 8,
    // which is 0 for a full word, so there is no shift-by-64 edge case.
    let eight = builder.ins().iconst(types::I64, 8);
    let spare = builder.ins().isub(eight, count);
    let shift_bits = builder.ins().ishl_imm(spare, 3);
    let all_ones = builder.ins().iconst(types::I64, -1);
    let mask = builder.ins().ushr(all_ones, shift_bits);
    let value = builder.ins().band(word, mask);
    jump_with_i64_arg(builder, done, value);

    join_with_fallback_call(builder, slow, done, fallback, &[bytes, off, count])
}

// The magic word every heap cstring carries at `ptr - 16`, and the one a
// string literal carries, which differs from it only in the low bit.
const CSTRING_MAGIC: i64 = pith_runtime::CSTRING_MAGIC as i64;
const _: () = assert!(pith_runtime::CSTRING_STATIC_MAGIC == pith_runtime::CSTRING_MAGIC | 1);

// The 16-byte header in front of heap cstring data: [magic: u32][rc: u32,
// atomic][data_len: u64], so the length lives 8 bytes before the pointer.
const CSTRING_LEN_OFFSET: i32 = -8;
const CSTRING_MAGIC_OFFSET: i32 = -16;

/// Read `s`'s length header without a branch, returning whether it has one
/// and the length it holds. This mirrors `runtime_core::cstring_header_len`:
/// the data is at an address >= 16 (which also rejects null), is 8-aligned,
/// and carries either magic word 16 bytes in front of it. The shared empty
/// string fails one of these checks. Only the length may be read, since a
/// literal's count word is not a count.
///
/// An address that could not carry a header reads the first bytes of
/// `anchor`'s code instead, which are always mapped, and the address test
/// masks out whatever they hold. That makes both loads safe to run anywhere.
/// Marked that way, and with a header that never changes while the string is
/// alive, they depend on `s` alone: Cranelift hoists the whole check out of a
/// loop that indexes the same string, and merges repeated checks of it. The
/// anchor is a function rather than a data symbol because the inliner only
/// translates function references.
fn cstring_header(builder: &mut FunctionBuilder<'_>, s: Value, anchor: FuncRef) -> (Value, Value) {
    let big_enough = builder
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, s, 16);
    let low_bits = builder.ins().band_imm(s, 7);
    let aligned = builder.ins().icmp_imm(IntCC::Equal, low_bits, 0);
    let addressable = builder.ins().band(big_enough, aligned);
    let anchor_code = builder.ins().func_addr(types::I64, anchor);
    let stand_in = builder.ins().iadd_imm(anchor_code, 16);
    let data = builder.ins().select(addressable, s, stand_in);

    // the literal magic differs from the heap one only in the low bit, so
    // setting that bit folds the two onto one compare.
    let flags = MemFlags::trusted().with_readonly().with_can_move();
    let magic = builder
        .ins()
        .load(types::I32, flags, data, CSTRING_MAGIC_OFFSET);
    let folded = builder.ins().bor_imm(magic, 1);
    let magic_ok = builder.ins().icmp_imm(IntCC::Equal, folded, CSTRING_MAGIC | 1);
    let has_header = builder.ins().band(addressable, magic_ok);
    let len = builder.ins().load(types::I64, flags, data, CSTRING_LEN_OFFSET);
    (has_header, len)
}

/// Inline `s[i]` (byte_at) for strings with a length header: bounds-check
/// against the header and index the byte directly, skipping the FFI dispatch
/// that dominates tight character-scanning loops. Anything else, including an
/// index out of range, falls back to `fallback` (pith_cstring_byte_at) for
/// identical semantics: out of range returns -1.
fn inline_cstring_byte_at(
    builder: &mut FunctionBuilder<'_>,
    s: Value,
    index: Value,
    fallback: FuncRef,
    anchor: FuncRef,
) -> Value {
    let (has_header, len) = cstring_header(builder, s, anchor);
    // a string without a header reads as empty here, so it always takes the
    // fallback; one unsigned compare also sends a negative index there.
    let zero = builder.ins().iconst(types::I64, 0);
    let indexable = builder.ins().select(has_header, len, zero);
    let in_bounds = builder.ins().icmp(IntCC::UnsignedLessThan, index, indexable);

    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    let read = builder.create_block();
    let slow = builder.create_block();
    builder.ins().brif(in_bounds, read, &[], slow, &[]);

    builder.switch_to_block(read);
    let elem_addr = builder.ins().iadd(s, index);
    let byte = builder.ins().load(types::I8, MemFlags::trusted(), elem_addr, 0);
    let value = builder.ins().uextend(types::I64, byte);
    jump_with_i64_arg(builder, done, value);

    join_with_fallback_call(builder, slow, done, fallback, &[s, index])
}

/// Inline `s.len()` (string_len) for strings with a length header: one header
/// read instead of an FFI call. Anything else falls back to `fallback`
/// (pith_cstring_len).
fn inline_cstring_len(builder: &mut FunctionBuilder<'_>, s: Value, fallback: FuncRef) -> Value {
    let (has_header, len) = cstring_header(builder, s, fallback);
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    let fast = builder.create_block();
    let slow = builder.create_block();
    builder.ins().brif(has_header, fast, &[], slow, &[]);

    builder.switch_to_block(fast);
    jump_with_i64_arg(builder, done, len);

    join_with_fallback_call(builder, slow, done, fallback, &[s])
}


/// Lower a call to a runtime builtin that has an inline fast path, or
/// answer `None` and leave the call to the generic path. A builtin that
/// returns nothing answers a zero for its destination register, as the
/// generic path does. Every fast path steps aside when a user function or a
/// local shares the builtin's name (`name_is_shadowed`): the user's
/// definition always wins, and `fn byte_at(r: Reader, i: Int)` once read a
/// byte out of its struct's own memory because it did not.
pub(super) fn inline_runtime_call(
    codegen: &mut CodeGen,
    builder: &mut FunctionBuilder<'_>,
    func_ref_cache: &mut HashMap<FuncId, FuncRef>,
    runtime_funcs: &HashMap<String, FuncId>,
    fname: &str,
    args: &[Value],
    name_is_shadowed: bool,
) -> Result<Option<Value>, CompileError> {
    if name_is_shadowed {
        return Ok(None);
    }
    let mut runtime_ref = |builder: &mut FunctionBuilder<'_>, name: &str| {
        runtime_func_ref(codegen, builder, func_ref_cache, runtime_funcs, name)
    };
    let value = match (fname, args.len()) {
        (
            "pith_list_set_value"
            | "pith_list_set_value_owned"
            | "pith_list_set_scalar"
            | "pith_list_set_value_strict"
            | "pith_list_set_value_owned_strict"
            | "pith_list_set_scalar_strict",
            3,
        ) => {
            let fallback = runtime_ref(builder, fname)?;
            let anchor = runtime_ref(builder, "pith_list_len")?;
            inline_list_set_value(builder, args[0], args[1], args[2], fallback, anchor);
            builder.ins().iconst(types::I64, 0)
        }
        // a field store's write barrier: only a holder that is already
        // shared makes the call, so test its mark here. the holder is a
        // struct the emitter is storing into, never null.
        ("pith_share_into", 3) => {
            let fallback = runtime_ref(builder, "pith_share_into")?;
            let shared = struct_shared_mark(builder, args[0]);
            let call_block = builder.create_block();
            let done = builder.create_block();
            builder.set_cold_block(call_block);
            builder.ins().brif(shared, call_block, &[], done, &[]);
            builder.switch_to_block(call_block);
            builder.ins().call(fallback, &[args[0], args[1], args[2]]);
            builder.ins().jump(done, &[]);
            builder.switch_to_block(done);
            builder.ins().iconst(types::I64, 0)
        }
        // a weak field read or store takes the runtime's weak field lock
        // only when its holder is shared, since only a shared holder is
        // reachable from another task (see `weak_holder_is_local` in the
        // runtime). for a local holder, the read loads and upgrades the field
        // here, and the store swaps it and drops the old target's weak count.
        ("pith_struct_weak_load_field", 2) => {
            let locked = runtime_ref(builder, fname)?;
            let upgrade = runtime_ref(builder, "pith_struct_weak_upgrade")?;
            let shared = struct_shared_mark(builder, args[0]);
            let local = builder.create_block();
            let slow = builder.create_block();
            let done = builder.create_block();
            builder.append_block_param(done, types::I64);
            builder.ins().brif(shared, slow, &[], local, &[]);
            builder.switch_to_block(local);
            let slot = builder.ins().iadd(args[0], args[1]);
            let target = builder.ins().load(types::I64, MemFlags::new(), slot, 0);
            let call = builder.ins().call(upgrade, &[target]);
            let upgraded = builder.func.dfg.first_result(call);
            jump_with_i64_arg(builder, done, upgraded);
            join_with_fallback_call(builder, slow, done, locked, &[args[0], args[1]])
        }
        ("pith_struct_weak_store_field", 3) => {
            let locked = runtime_ref(builder, fname)?;
            let release = runtime_ref(builder, "pith_struct_weak_release")?;
            let shared = struct_shared_mark(builder, args[0]);
            let local = builder.create_block();
            let slow = builder.create_block();
            let done = builder.create_block();
            builder.set_cold_block(slow);
            builder.ins().brif(shared, slow, &[], local, &[]);
            builder.switch_to_block(local);
            let slot = builder.ins().iadd(args[0], args[1]);
            let old = builder.ins().load(types::I64, MemFlags::new(), slot, 0);
            builder.ins().store(MemFlags::new(), args[2], slot, 0);
            builder.ins().call(release, &[old]);
            builder.ins().jump(done, &[]);
            builder.switch_to_block(slow);
            builder.ins().call(locked, &[args[0], args[1], args[2]]);
            builder.ins().jump(done, &[]);
            builder.switch_to_block(done);
            builder.ins().iconst(types::I64, 0)
        }
        ("pith_closure_get_env", 2) => {
            let fallback = runtime_ref(builder, "pith_closure_get_env")?;
            inline_closure_get_env(builder, args[0], args[1], fallback)
        }
        ("pith_closure_set_env", 3) => {
            let fallback = runtime_ref(builder, "pith_closure_set_env")?;
            inline_closure_set_env(builder, args[0], args[1], args[2], fallback);
            builder.ins().iconst(types::I64, 0)
        }
        ("bytes_get", 2) => inline_bytes_get(builder, args[0], args[1]),
        // `bytes[i]`: the same fast path, with the runtime's strict getter as
        // the fallback so a failing read still aborts with its diagnostic.
        ("bytes_get_strict", 2) => {
            let fallback = runtime_ref(builder, "bytes_get_strict")?;
            inline_bytes_get_strict(builder, args[0], args[1], fallback)
        }
        // the typed json decoder's input: the handle tested once, then
        // unchecked byte loads at indexes the decoder compared against the
        // length itself.
        ("pith_bytes_data_ptr", 1) => {
            let fallback = runtime_ref(builder, fname)?;
            inline_handle_field(
                builder,
                args[0],
                BYTES_HANDLE_ALIGN,
                BYTES_MAGIC_OFFSET,
                BYTES_MAGIC,
                BYTES_DATA_PTR_OFFSET,
                fallback,
            )
        }
        ("pith_store_u64", 3) => {
            let addr = builder.ins().iadd(args[0], args[1]);
            builder.ins().store(MemFlags::new().with_notrap(), args[2], addr, 0);
            builder.ins().iconst(types::I64, 0)
        }
        ("pith_load_u8" | "pith_load_u16" | "pith_load_u32" | "pith_load_u64", 2) => {
            let ty = match fname {
                "pith_load_u8" => types::I8,
                "pith_load_u16" => types::I16,
                "pith_load_u32" => types::I32,
                _ => types::I64,
            };
            let addr = builder.ins().iadd(args[0], args[1]);
            let word = builder.ins().load(ty, MemFlags::new().with_notrap(), addr, 0);
            if ty == types::I64 {
                word
            } else {
                builder.ins().uextend(types::I64, word)
            }
        }
        ("bytes_read_word", 3) => {
            let fallback = runtime_ref(builder, "bytes_read_word")?;
            inline_bytes_read_word(builder, args[0], args[1], args[2], fallback)
        }
        // string indexing: a header read instead of an FFI call per
        // character, with the runtime helper as the fallback for literals.
        // these names are emitted only for strings (bytes use bytes_get).
        ("byte_at" | "pith_cstring_byte_at", 2) => {
            let fallback = runtime_ref(builder, "byte_at")?;
            // the same anchor `s.len()` uses, so the two share one header
            // check
            let anchor = runtime_ref(builder, "string_len")?;
            inline_cstring_byte_at(builder, args[0], args[1], fallback, anchor)
        }
        ("bytes_len", 1) => {
            let fallback = runtime_ref(builder, fname)?;
            inline_handle_field(
                builder,
                args[0],
                BYTES_HANDLE_ALIGN,
                BYTES_MAGIC_OFFSET,
                BYTES_MAGIC,
                BYTES_DATA_LEN_OFFSET,
                fallback,
            )
        }
        ("pith_list_len", 1) => {
            let fallback = runtime_ref(builder, fname)?;
            inline_list_len(builder, args[0], fallback)
        }
        // the conversions from Int: one compare and a trap when the value
        // does not fit the target type, the value itself otherwise. the
        // emitter also puts the narrow ones after every arithmetic op on a
        // narrow type, which is how `Int8(127) + 1` stops.
        ("Int8", 1) => emit_int_fit(builder, args[0], i8::MIN as i64, i8::MAX as i64),
        ("Int16", 1) => emit_int_fit(builder, args[0], i16::MIN as i64, i16::MAX as i64),
        ("Int32", 1) => emit_int_fit(builder, args[0], i32::MIN as i64, i32::MAX as i64),
        ("UInt8", 1) => emit_int_fit(builder, args[0], 0, u8::MAX as i64),
        ("UInt16", 1) => emit_int_fit(builder, args[0], 0, u16::MAX as i64),
        ("UInt32", 1) => emit_int_fit(builder, args[0], 0, u32::MAX as i64),
        ("UInt" | "UInt64", 1) => emit_int_fit(builder, args[0], 0, i64::MAX),
        ("Int64" | "pith_wrapping_uint64", 1) => args[0],
        // the wrapping conversions keep the low bits: a truncate and a sign
        // or zero extend.
        ("pith_wrapping_int8", 1) => {
            let low = builder.ins().ireduce(types::I8, args[0]);
            builder.ins().sextend(types::I64, low)
        }
        ("pith_wrapping_int16", 1) => {
            let low = builder.ins().ireduce(types::I16, args[0]);
            builder.ins().sextend(types::I64, low)
        }
        ("pith_wrapping_int32", 1) => {
            let low = builder.ins().ireduce(types::I32, args[0]);
            builder.ins().sextend(types::I64, low)
        }
        ("pith_wrapping_uint8", 1) => {
            let low = builder.ins().ireduce(types::I8, args[0]);
            builder.ins().uextend(types::I64, low)
        }
        ("pith_wrapping_uint16", 1) => {
            let low = builder.ins().ireduce(types::I16, args[0]);
            builder.ins().uextend(types::I64, low)
        }
        ("pith_wrapping_uint32", 1) => {
            let low = builder.ins().ireduce(types::I32, args[0]);
            builder.ins().uextend(types::I64, low)
        }
        ("string_len" | "pith_cstring_len", 1) => {
            let fallback = runtime_ref(builder, "string_len")?;
            inline_cstring_len(builder, args[0], fallback)
        }
        ("pith_list_get_value" | "pith_list_get_value_unchecked", 2) => {
            inline_list_get_value(builder, args[0], args[1], fname == "pith_list_get_value")
        }
        _ => return Ok(None),
    };
    Ok(Some(value))
}

