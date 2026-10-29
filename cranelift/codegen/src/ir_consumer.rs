//! IR Consumer — translates Pith text IR to Cranelift native code
//!
//! This module parses the simple text IR emitted by self-host/ir_emitter_core.pith
//! and translates it to Cranelift API calls. This is the Rust-side half of
//! Stage 2: moving compilation logic from Rust to Pith.
//!
//! The IR grammar, the type/retkind vocabulary, and the ABI conventions are
//! specified in docs/ir-contract.md, which is authoritative. Update that file
//! whenever the instruction set or the conventions in this consumer change.

use crate::{CodeGen, CompileError};
use cranelift::prelude::*;
use cranelift_module::{FuncId, Linkage, Module};
use pith_runtime::bytes::{
    BYTES_DATA_LEN_OFFSET, BYTES_DATA_PTR_OFFSET, BYTES_HANDLE_ALIGN, BYTES_MAGIC,
    BYTES_MAGIC_OFFSET,
};
use pith_runtime::runtime_core::{
    CLOSURE_CYCLE_FLAGS_OFFSET, CLOSURE_ENV_OFFSET, CLOSURE_ENV_TAGS_OFFSET, CLOSURE_FUNC_PTR_OFFSET,
    CLOSURE_HANDLE_ALIGN, CLOSURE_MAGIC, CLOSURE_MAGIC_OFFSET, CLOSURE_OVERFLOW_OFFSET,
    CLOSURE_REF_COUNT_OFFSET, CLOSURE_SIZE,
    PITH_CLOSURE_ENV_SLOTS, STRUCT_DEAD_OFFSET, STRUCT_DTOR_OFFSET, STRUCT_FLAGS_FROM_DATA,
    STRUCT_HEADER, STRUCT_MAGIC, STRUCT_SHARED_MASK, STRUCT_SIZE_OFFSET, STRUCT_STRONG_OFFSET,
    STRUCT_WEAK_OFFSET,
};
use pith_runtime::collections::list::{
    LIST_HANDLE_ALIGN, LIST_IMPL_ELEM_SIZE_OFFSET, LIST_IMPL_TYPE_TAG_OFFSET,
    LIST_IMPL_VALUES8_LEN_OFFSET, LIST_IMPL_VALUES8_PTR_OFFSET, LIST_MAGIC,
    LIST_TYPE_TAG_PRIMITIVE,
};
use std::collections::{HashMap, HashSet};

#[cfg(pith_cranelift_new_api)]
fn declare_i64_var(builder: &mut FunctionBuilder<'_>) -> Variable {
    builder.declare_var(types::I64)
}

#[cfg(pith_cranelift_new_api)]
fn jump_with_i64_arg(builder: &mut FunctionBuilder<'_>, block: Block, value: Value) {
    builder.ins().jump(
        block,
        &[cranelift::codegen::ir::instructions::BlockArg::Value(value)],
    );
}

#[cfg(not(pith_cranelift_new_api))]
fn declare_i64_var(builder: &mut FunctionBuilder<'_>, next_var_id: &mut u32) -> Variable {
    let var = Variable::new((*next_var_id) as usize);
    *next_var_id += 1;
    builder.declare_var(var, types::I64);
    var
}

#[cfg(not(pith_cranelift_new_api))]
fn jump_with_i64_arg(builder: &mut FunctionBuilder<'_>, block: Block, value: Value) {
    builder.ins().jump(block, &[value]);
}

/// Runtime releases that return at once for a zero handle and return
/// nothing, so a call site may test for zero and skip the call.
fn is_null_safe_release(name: &str) -> bool {
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
    let null_block = builder.create_block();
    let after_null = builder.create_block();
    builder
        .ins()
        .brif(list_is_null, null_block, &[], after_null, &[]);
    builder.switch_to_block(null_block);
    jump_with_i64_arg(builder, done, zero);
    builder.switch_to_block(after_null);

    let index_is_negative = builder.ins().icmp_imm(IntCC::SignedLessThan, index, 0);
    let neg_block = builder.create_block();
    let after_neg = builder.create_block();
    builder
        .ins()
        .brif(index_is_negative, neg_block, &[], after_neg, &[]);
    builder.switch_to_block(neg_block);
    jump_with_i64_arg(builder, done, zero);
    builder.switch_to_block(after_neg);

    let elem_size = builder.ins().load(
        types::I64,
        MemFlags::new(),
        list,
        LIST_IMPL_ELEM_SIZE_OFFSET,
    );
    let is_eight = builder.ins().icmp_imm(IntCC::Equal, elem_size, 8);
    let size_fail = builder.create_block();
    let after_size = builder.create_block();
    builder
        .ins()
        .brif(is_eight, after_size, &[], size_fail, &[]);
    builder.switch_to_block(size_fail);
    jump_with_i64_arg(builder, done, zero);
    builder.switch_to_block(after_size);

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
        let bounds_fail = builder.create_block();
        let after_bounds = builder.create_block();
        builder
            .ins()
            .brif(out_of_bounds, bounds_fail, &[], after_bounds, &[]);
        builder.switch_to_block(bounds_fail);
        jump_with_i64_arg(builder, done, zero);
        builder.switch_to_block(after_bounds);
    }

    let data_ptr = builder.ins().load(
        types::I64,
        MemFlags::new(),
        list,
        LIST_IMPL_VALUES8_PTR_OFFSET,
    );
    let data_is_null = builder.ins().icmp_imm(IntCC::Equal, data_ptr, 0);
    let ptr_fail = builder.create_block();
    let load_block = builder.create_block();
    builder
        .ins()
        .brif(data_is_null, ptr_fail, &[], load_block, &[]);
    builder.switch_to_block(ptr_fail);
    jump_with_i64_arg(builder, done, zero);
    builder.switch_to_block(load_block);

    let byte_offset = builder.ins().ishl_imm(index, 3);
    let elem_addr = builder.ins().iadd(data_ptr, byte_offset);
    let value = builder
        .ins()
        .load(types::I64, MemFlags::new(), elem_addr, 0);
    jump_with_i64_arg(builder, done, value);

    builder.switch_to_block(done);
    builder.block_params(done)[0]
}

#[cfg(pith_cranelift_new_api)]
fn jump_with_two_i64_args(builder: &mut FunctionBuilder<'_>, block: Block, a: Value, b: Value) {
    builder.ins().jump(
        block,
        &[
            cranelift::codegen::ir::instructions::BlockArg::Value(a),
            cranelift::codegen::ir::instructions::BlockArg::Value(b),
        ],
    );
}

#[cfg(not(pith_cranelift_new_api))]
fn jump_with_two_i64_args(builder: &mut FunctionBuilder<'_>, block: Block, a: Value, b: Value) {
    builder.ins().jump(block, &[a, b]);
}

// `xs[i]` lowers to a call to pith_list_get_opt, which heap-allocates a
// two-slot Optional tuple that the very next instructions unpack (flag at
// offset 0, value at offset 8) and release. When scan_unpackable_opt_regs
// proves a call's result is used only by that unpack pattern, the whole round
// trip collapses to these loads and compares — no call, no allocation, no
// release. The checks mirror the runtime path exactly: a null or stale handle
// (magic scrubbed on free), a non-8-byte-element list, or an out-of-range
// index all yield is_some == 0, which the emitted IR turns into the same loud
// "index out of bounds" failure the runtime call produced.
fn inline_list_get_opt(
    builder: &mut FunctionBuilder<'_>,
    list: Value,
    index: Value,
) -> (Value, Value) {
    let zero = builder.ins().iconst(types::I64, 0);
    let done = builder.create_block();
    builder.append_block_param(done, types::I64); // is_some
    builder.append_block_param(done, types::I64); // value

    let list_is_null = builder.ins().icmp_imm(IntCC::Equal, list, 0);
    let null_block = builder.create_block();
    let after_null = builder.create_block();
    builder
        .ins()
        .brif(list_is_null, null_block, &[], after_null, &[]);
    builder.switch_to_block(null_block);
    jump_with_two_i64_args(builder, done, zero, zero);
    builder.switch_to_block(after_null);

    // the magic word is scrubbed when a list is freed, so this rejects stale
    // handles the same way list_ref does in the runtime.
    let magic = builder.ins().load(types::I32, MemFlags::new(), list, 0);
    let magic_bad = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, magic, LIST_MAGIC as i64);
    let magic_fail = builder.create_block();
    let after_magic = builder.create_block();
    builder
        .ins()
        .brif(magic_bad, magic_fail, &[], after_magic, &[]);
    builder.switch_to_block(magic_fail);
    jump_with_two_i64_args(builder, done, zero, zero);
    builder.switch_to_block(after_magic);

    let elem_size = builder.ins().load(
        types::I64,
        MemFlags::new(),
        list,
        LIST_IMPL_ELEM_SIZE_OFFSET,
    );
    let is_eight = builder.ins().icmp_imm(IntCC::Equal, elem_size, 8);
    let size_fail = builder.create_block();
    let after_size = builder.create_block();
    builder
        .ins()
        .brif(is_eight, after_size, &[], size_fail, &[]);
    builder.switch_to_block(size_fail);
    jump_with_two_i64_args(builder, done, zero, zero);
    builder.switch_to_block(after_size);

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
    let bounds_fail = builder.create_block();
    let after_bounds = builder.create_block();
    builder
        .ins()
        .brif(out_of_bounds, bounds_fail, &[], after_bounds, &[]);
    builder.switch_to_block(bounds_fail);
    jump_with_two_i64_args(builder, done, zero, zero);
    builder.switch_to_block(after_bounds);

    let data_ptr = builder.ins().load(
        types::I64,
        MemFlags::new(),
        list,
        LIST_IMPL_VALUES8_PTR_OFFSET,
    );
    let data_is_null = builder.ins().icmp_imm(IntCC::Equal, data_ptr, 0);
    let ptr_fail = builder.create_block();
    let load_block = builder.create_block();
    builder
        .ins()
        .brif(data_is_null, ptr_fail, &[], load_block, &[]);
    builder.switch_to_block(ptr_fail);
    jump_with_two_i64_args(builder, done, zero, zero);
    builder.switch_to_block(load_block);

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
// loads and one store. Anything the fast path cannot prove — a tagged list
// (element counts to move), an out-of-bounds index (a silent no-op by the
// runtime's contract), a stale or null handle — falls back to the real
// runtime call, which keeps every semantic exactly as it was.
fn inline_list_set_value(
    builder: &mut FunctionBuilder<'_>,
    list: Value,
    index: Value,
    value: Value,
    fallback: cranelift::codegen::ir::FuncRef,
) {
    let done = builder.create_block();
    let slow = builder.create_block();

    let list_is_null = builder.ins().icmp_imm(IntCC::Equal, list, 0);
    let check_magic = builder.create_block();
    builder.ins().brif(list_is_null, slow, &[], check_magic, &[]);
    builder.switch_to_block(check_magic);

    let magic = builder.ins().load(types::I32, MemFlags::new(), list, 0);
    let magic_bad = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, magic, LIST_MAGIC as i64);
    let check_size = builder.create_block();
    builder.ins().brif(magic_bad, slow, &[], check_size, &[]);
    builder.switch_to_block(check_size);

    let elem_size = builder.ins().load(
        types::I64,
        MemFlags::new(),
        list,
        LIST_IMPL_ELEM_SIZE_OFFSET,
    );
    let not_eight = builder.ins().icmp_imm(IntCC::NotEqual, elem_size, 8);
    let check_tag = builder.create_block();
    builder.ins().brif(not_eight, slow, &[], check_tag, &[]);
    builder.switch_to_block(check_tag);

    let tag = builder.ins().load(
        types::I32,
        MemFlags::new(),
        list,
        LIST_IMPL_TYPE_TAG_OFFSET,
    );
    let not_primitive = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, tag, LIST_TYPE_TAG_PRIMITIVE as i64);
    let check_bounds = builder.create_block();
    builder.ins().brif(not_primitive, slow, &[], check_bounds, &[]);
    builder.switch_to_block(check_bounds);

    let len = builder.ins().load(
        types::I64,
        MemFlags::new(),
        list,
        LIST_IMPL_VALUES8_LEN_OFFSET,
    );
    let out_of_bounds = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, len);
    let check_ptr = builder.create_block();
    builder.ins().brif(out_of_bounds, slow, &[], check_ptr, &[]);
    builder.switch_to_block(check_ptr);

    let data_ptr = builder.ins().load(
        types::I64,
        MemFlags::new(),
        list,
        LIST_IMPL_VALUES8_PTR_OFFSET,
    );
    let data_is_null = builder.ins().icmp_imm(IntCC::Equal, data_ptr, 0);
    let store_block = builder.create_block();
    builder.ins().brif(data_is_null, slow, &[], store_block, &[]);
    builder.switch_to_block(store_block);

    let byte_offset = builder.ins().ishl_imm(index, 3);
    let elem_addr = builder.ins().iadd(data_ptr, byte_offset);
    builder
        .ins()
        .store(MemFlags::new(), value, elem_addr, 0);
    builder.ins().jump(done, &[]);

    builder.switch_to_block(slow);
    builder.ins().call(fallback, &[list, index, value]);
    builder.ins().jump(done, &[]);

    builder.switch_to_block(done);
}

// the native instruction behind a bit builtin, or None for anything else.
// these mirror the runtime externs exactly: pith_bit_shr is a logical shift
// (ushr) and shift amounts mask mod 64 on both paths.
fn bits_builtin_op(builtin: &str) -> Option<&'static str> {
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
// every `bits.band` in a hot loop a pith call into an FFI call — two call
// layers for one machine instruction. When a two-parameter function's body is
// verifiably just `return <bit builtin>(p0, p1)`, calls to it lower to that
// single instruction instead. The body is matched structurally against the
// exact shape the emitter produces (two param loads, the builtin call on
// them, the return, and the emitter's dead fallthrough tail), so an edited
// wrapper simply stops matching and keeps its call semantics.
fn detect_bits_wrapper(param_names: &[String], body_lines: &[&str]) -> Option<&'static str> {
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

/// The address of a string literal's characters, just past the header that
/// `declare_string_data` writes in front of them.
fn string_lit_addr(
    codegen: &mut CodeGen,
    builder: &mut FunctionBuilder<'_>,
    lit: cranelift_module::DataId,
) -> Value {
    let gv = codegen.module.declare_data_in_func(lit, builder.func);
    let base = builder.ins().global_value(types::I64, gv);
    builder.ins().iadd_imm(base, 16)
}

/// Whether a one-argument call is a retain or release that the runtime would
/// return from without touching anything: any of them on a constant zero
/// (null), and a string retain or release on a literal, which has no count.
fn is_noop_count_call(
    fname: &str,
    arg: Value,
    literal_values: &HashSet<Value>,
    func: &cranelift::codegen::ir::Function,
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

fn emit_bits_op(builder: &mut FunctionBuilder<'_>, op: &str, a: Value, b: Value) -> Value {
    match op {
        "band" => builder.ins().band(a, b),
        "bor" => builder.ins().bor(a, b),
        "bxor" => builder.ins().bxor(a, b),
        "shl" => builder.ins().ishl(a, b),
        // a 32-bit rotate turns the low half of the value and zero-extends
        // the result, as pith_bit_rotr32 does
        "rotl32" | "rotr32" => {
            let low = builder.ins().ireduce(types::I32, a);
            let rotated = if op == "rotl32" {
                builder.ins().rotl(low, b)
            } else {
                builder.ins().rotr(low, b)
            };
            builder.ins().uextend(types::I64, rotated)
        }
        "rotl64" => builder.ins().rotl(a, b),
        "rotr64" => builder.ins().rotr(a, b),
        _ => builder.ins().ushr(a, b),
    }
}

// the runtime getters that also come as a register pair returning
// (is_some, value), for an optional the caller unpacks on the spot.
fn opt_getter_pair(name: &str) -> Option<&'static str> {
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
fn is_unpackable_opt_def(parts: &[&str]) -> bool {
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
// or the release of the
// throwaway tuple shell. anything else — a store, a return, an argument to
// some other call — means the tuple escapes and must really exist.
fn is_sanctioned_opt_use(parts: &[&str], i: usize) -> bool {
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
fn scan_unpackable_opt_regs(body_lines: &[&str]) -> HashSet<usize> {
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

fn inline_bytes_get(builder: &mut FunctionBuilder<'_>, bytes: Value, index: Value) -> Value {
    let zero = builder.ins().iconst(types::I64, 0);
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);

    let bytes_is_null = builder.ins().icmp_imm(IntCC::Equal, bytes, 0);
    let null_block = builder.create_block();
    let after_null = builder.create_block();
    builder
        .ins()
        .brif(bytes_is_null, null_block, &[], after_null, &[]);
    builder.switch_to_block(null_block);
    jump_with_i64_arg(builder, done, zero);
    builder.switch_to_block(after_null);

    let index_is_negative = builder.ins().icmp_imm(IntCC::SignedLessThan, index, 0);
    let neg_block = builder.create_block();
    let after_neg = builder.create_block();
    builder
        .ins()
        .brif(index_is_negative, neg_block, &[], after_neg, &[]);
    builder.switch_to_block(neg_block);
    jump_with_i64_arg(builder, done, zero);
    builder.switch_to_block(after_neg);

    let data_ptr = builder.ins().load(types::I64, MemFlags::new(), bytes, 0);
    let len = builder.ins().load(types::I64, MemFlags::new(), bytes, 8);
    let out_of_bounds = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, len);
    let bounds_fail = builder.create_block();
    let after_bounds = builder.create_block();
    builder
        .ins()
        .brif(out_of_bounds, bounds_fail, &[], after_bounds, &[]);
    builder.switch_to_block(bounds_fail);
    jump_with_i64_arg(builder, done, zero);
    builder.switch_to_block(after_bounds);

    let elem_addr = builder.ins().iadd(data_ptr, index);
    let byte = builder.ins().load(types::I8, MemFlags::new(), elem_addr, 0);
    let value = builder.ins().uextend(types::I64, byte);
    jump_with_i64_arg(builder, done, value);

    builder.switch_to_block(done);
    builder.block_params(done)[0]
}

/// `brif` that carries `a` and `b` to `taken` and nothing to `fallthrough`.
/// The strict-index fast paths route every failed check to one slow block,
/// marked cold, that receives the handle and index as block parameters: the
/// runtime call's operands are then the slow block's own values, the moves
/// that feed them sit in the cold edge blocks, and the fast path is laid out
/// as one fall-through sequence.
#[cfg(pith_cranelift_new_api)]
fn brif_with_two_i64_args(
    builder: &mut FunctionBuilder<'_>,
    cond: Value,
    taken: Block,
    a: Value,
    b: Value,
    fallthrough: Block,
) {
    builder.ins().brif(
        cond,
        taken,
        &[
            cranelift::codegen::ir::instructions::BlockArg::Value(a),
            cranelift::codegen::ir::instructions::BlockArg::Value(b),
        ],
        fallthrough,
        &[],
    );
}

#[cfg(not(pith_cranelift_new_api))]
fn brif_with_two_i64_args(
    builder: &mut FunctionBuilder<'_>,
    cond: Value,
    taken: Block,
    a: Value,
    b: Value,
    fallthrough: Block,
) {
    builder.ins().brif(cond, taken, &[a, b], fallthrough, &[]);
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
    // null and misalignment in one compare: rotating the low s bits to the
    // top (s = log2 align) turns an aligned non-null handle into a value in
    // 1..2^(64-s), null into 0, and a misaligned handle into a value with a
    // top bit set. Subtracting one then puts exactly the good handles below
    // 2^(64-s) - 1 as unsigned values.
    debug_assert!((align as u64).is_power_of_two());
    let shift = align.trailing_zeros() as i64;
    let rotated = builder.ins().rotr_imm(handle, shift);
    let rotated_m1 = builder.ins().iadd_imm(rotated, -1);
    let good_limit = ((1u64 << (64 - shift)) - 1) as i64;
    let handle_bad = builder.ins().icmp_imm(
        IntCC::UnsignedGreaterThanOrEqual,
        rotated_m1,
        good_limit,
    );
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
    fallback: cranelift::codegen::ir::FuncRef,
) -> Value {
    builder.switch_to_block(slow);
    let params = builder.block_params(slow);
    let (handle, index) = (params[0], params[1]);
    let call = builder.ins().call(fallback, &[handle, index]);
    let fallback_result = builder.func.dfg.inst_results(call)[0];
    jump_with_i64_arg(builder, done, fallback_result);

    builder.switch_to_block(done);
    builder.block_params(done)[0]
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
    fallback: cranelift::codegen::ir::FuncRef,
) -> Value {
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    let slow = builder.create_block();
    builder.append_block_param(slow, types::I64);
    builder.append_block_param(slow, types::I64);
    builder.set_cold_block(slow);

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
/// the element size the runtime insists on, and one unsigned compare
/// against the 8-byte storage length for both `i < 0` and `i >= len`. A
/// list whose elements are not 8 bytes wide keeps that length at zero, so
/// the size check is also what keeps the compare honest for it. The data
/// pointer needs no null test: a non-empty 8-byte list always has one.
fn inline_list_get_value_strict(
    builder: &mut FunctionBuilder<'_>,
    list: Value,
    index: Value,
    fallback: cranelift::codegen::ir::FuncRef,
) -> Value {
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    let slow = builder.create_block();
    builder.append_block_param(slow, types::I64);
    builder.append_block_param(slow, types::I64);
    builder.set_cold_block(slow);

    branch_if_not_magic_handle(builder, list, index, LIST_HANDLE_ALIGN, 0, LIST_MAGIC, slow);

    let elem_size = builder.ins().load(
        types::I64,
        MemFlags::new(),
        list,
        LIST_IMPL_ELEM_SIZE_OFFSET,
    );
    let not_eight = builder.ins().icmp_imm(IntCC::NotEqual, elem_size, 8);
    let check_bounds = builder.create_block();
    brif_with_two_i64_args(builder, not_eight, slow, list, index, check_bounds);
    builder.switch_to_block(check_bounds);

    let len = builder.ins().load(
        types::I64,
        MemFlags::new(),
        list,
        LIST_IMPL_VALUES8_LEN_OFFSET,
    );
    let out_of_bounds = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, len);
    let read = builder.create_block();
    brif_with_two_i64_args(builder, out_of_bounds, slow, list, index, read);
    builder.switch_to_block(read);

    let data_ptr = builder.ins().load(
        types::I64,
        MemFlags::new(),
        list,
        LIST_IMPL_VALUES8_PTR_OFFSET,
    );
    let byte_offset = builder.ins().ishl_imm(index, 3);
    let elem_addr = builder.ins().iadd(data_ptr, byte_offset);
    let value = builder
        .ins()
        .load(types::I64, MemFlags::new(), elem_addr, 0);
    jump_with_i64_arg(builder, done, value);

    finish_strict_fast_path(builder, slow, done, fallback)
}

/// Inline `b.len()` (`pith_bytes_len`) and `xs.len()` (`pith_list_len`) as
/// the same handle test the getters above use and one load of the length.
/// A loop that tests `i < b.len()` makes this call once per pass, so the
/// FFI call cost more than the loop body did. A list only keeps its length
/// in the 8-byte storage when its elements are 8 bytes wide, so for a list
/// `elem_size_offset` names the field to check first. Everything the fast
/// path does not accept, null included, goes to `fallback`, which answers
/// 0 for a bad handle as it always has.
fn inline_handle_len(
    builder: &mut FunctionBuilder<'_>,
    handle: Value,
    align: i64,
    magic_offset: i32,
    magic: u32,
    elem_size_offset: Option<i32>,
    len_offset: i32,
    fallback: cranelift::codegen::ir::FuncRef,
) -> Value {
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    let slow = builder.create_block();
    builder.append_block_param(slow, types::I64);
    builder.append_block_param(slow, types::I64);
    builder.set_cold_block(slow);

    // the handle test passes two values to `slow`; a length has no index,
    // so the handle fills both slots
    branch_if_not_magic_handle(builder, handle, handle, align, magic_offset, magic, slow);
    if let Some(offset) = elem_size_offset {
        let elem_size = builder
            .ins()
            .load(types::I64, MemFlags::new(), handle, offset);
        let not_eight = builder.ins().icmp_imm(IntCC::NotEqual, elem_size, 8);
        let read = builder.create_block();
        brif_with_two_i64_args(builder, not_eight, slow, handle, handle, read);
        builder.switch_to_block(read);
    }
    let len = builder
        .ins()
        .load(types::I64, MemFlags::new(), handle, len_offset);
    jump_with_i64_arg(builder, done, len);

    builder.switch_to_block(slow);
    let arg = builder.block_params(slow)[0];
    let call = builder.ins().call(fallback, &[arg]);
    let fallback_result = builder.func.dfg.inst_results(call)[0];
    jump_with_i64_arg(builder, done, fallback_result);

    builder.switch_to_block(done);
    builder.block_params(done)[0]
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
    fallback: cranelift::codegen::ir::FuncRef,
) -> Value {
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
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
fn closure_fn_ptr(
    codegen: &mut CodeGen,
    builder: &mut FunctionBuilder<'_>,
    func_ref_cache: &mut HashMap<FuncId, cranelift::codegen::ir::FuncRef>,
    runtime_funcs: &HashMap<String, FuncId>,
    closure: Value,
) -> Value {
    let Some(&fid) = runtime_funcs.get("pith_closure_get_fn") else {
        return closure;
    };
    let fallback = *func_ref_cache
        .entry(fid)
        .or_insert_with(|| codegen.module.declare_func_in_func(fid, builder.func));
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    let slow = builder.create_block();
    builder.append_block_param(slow, types::I64);
    builder.append_block_param(slow, types::I64);
    builder.set_cold_block(slow);

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

    builder.switch_to_block(slow);
    let handle = builder.block_params(slow)[0];
    let call = builder.ins().call(fallback, &[handle]);
    let answer = builder.func.dfg.first_result(call);
    jump_with_i64_arg(builder, done, answer);

    builder.switch_to_block(done);
    builder.block_params(done)[0]
}

/// Inline the store of one capture (`pith_closure_set_env`) the way
/// `inline_closure_get_env` inlines the read: the handle test, then one store
/// into the inline env. A closure is built with one of these per capture, so
/// the FFI call was a large share of building a small one. A spilled slot and
/// a bad handle go to `fallback`.
fn inline_closure_set_env(
    builder: &mut FunctionBuilder<'_>,
    closure: Value,
    slot: Value,
    value: Value,
    fallback: cranelift::codegen::ir::FuncRef,
) {
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

/// Inline bytes.read_word for the common case — a count of 1..=8 with a
/// full 8-byte window inside the data — as one load and a mask, skipping
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
    fallback: cranelift::codegen::ir::FuncRef,
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
    let len = builder.ins().load(types::I64, MemFlags::new(), bytes, 8);
    let off_p8 = builder.ins().iadd_imm(off, 8);
    let no_room = builder.ins().icmp(IntCC::UnsignedGreaterThan, off_p8, len);
    let read = builder.create_block();
    builder.ins().brif(no_room, slow, &[], read, &[]);
    builder.switch_to_block(read);

    let data_ptr = builder.ins().load(types::I64, MemFlags::new(), bytes, 0);
    let addr = builder.ins().iadd(data_ptr, off);
    let word = builder.ins().load(types::I64, MemFlags::new(), addr, 0);
    // keep the low `count` bytes: all-ones shifted down by (8 - count) * 8,
    // which is 0 for a full word — no shift-by-64 edge case.
    let eight = builder.ins().iconst(types::I64, 8);
    let spare = builder.ins().isub(eight, count);
    let shift_bits = builder.ins().ishl_imm(spare, 3);
    let all_ones = builder.ins().iconst(types::I64, -1);
    let mask = builder.ins().ushr(all_ones, shift_bits);
    let value = builder.ins().band(word, mask);
    jump_with_i64_arg(builder, done, value);

    builder.switch_to_block(slow);
    let call = builder.ins().call(fallback, &[bytes, off, count]);
    let fallback_result = builder.func.dfg.inst_results(call)[0];
    jump_with_i64_arg(builder, done, fallback_result);

    builder.switch_to_block(done);
    builder.block_params(done)[0]
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
fn cstring_header(
    builder: &mut FunctionBuilder<'_>,
    s: Value,
    anchor: cranelift::codegen::ir::FuncRef,
) -> (Value, Value) {
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
    fallback: cranelift::codegen::ir::FuncRef,
    anchor: cranelift::codegen::ir::FuncRef,
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

    builder.switch_to_block(slow);
    let call = builder.ins().call(fallback, &[s, index]);
    let fallback_result = builder.func.dfg.inst_results(call)[0];
    jump_with_i64_arg(builder, done, fallback_result);

    builder.switch_to_block(done);
    builder.block_params(done)[0]
}

/// Inline `s.len()` (string_len) for strings with a length header: one header
/// read instead of an FFI call. Anything else falls back to `fallback`
/// (pith_cstring_len).
fn inline_cstring_len(
    builder: &mut FunctionBuilder<'_>,
    s: Value,
    fallback: cranelift::codegen::ir::FuncRef,
) -> Value {
    let (has_header, len) = cstring_header(builder, s, fallback);
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    let fast = builder.create_block();
    let slow = builder.create_block();
    builder.ins().brif(has_header, fast, &[], slow, &[]);

    builder.switch_to_block(fast);
    jump_with_i64_arg(builder, done, len);

    builder.switch_to_block(slow);
    let call = builder.ins().call(fallback, &[s]);
    let fallback_result = builder.func.dfg.inst_results(call)[0];
    jump_with_i64_arg(builder, done, fallback_result);

    builder.switch_to_block(done);
    builder.block_params(done)[0]
}

fn runtime_func_ref(
    codegen: &mut CodeGen,
    builder: &mut FunctionBuilder<'_>,
    func_ref_cache: &mut HashMap<FuncId, cranelift::codegen::ir::FuncRef>,
    runtime_funcs: &HashMap<String, FuncId>,
    name: &str,
) -> Result<cranelift::codegen::ir::FuncRef, CompileError> {
    let fid = runtime_funcs.get(name).copied().ok_or_else(|| {
        CompileError::ModuleError(format!("IR consumer: missing runtime function '{name}'"))
    })?;
    Ok(*func_ref_cache
        .entry(fid)
        .or_insert_with(|| codegen.module.declare_func_in_func(fid, builder.func)))
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
fn preempt_from_env(value: Option<&str>) -> bool {
    !matches!(
        value.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("0") | Some("off") | Some("false") | Some("no") | Some("n")
    )
}

/// Read `PITH_GREEN_PREEMPT` and answer whether to emit the safe-points.
fn green_preempt_enabled() -> bool {
    preempt_from_env(std::env::var("PITH_GREEN_PREEMPT").ok().as_deref())
}

/// Emit a cooperative-preemption safe-point inline, just before a loop back-edge
/// branch. Loads the process-global `PITH_PREEMPT_REQUESTED` flag (declared in
/// the runtime, see `green.rs`); on the hot path — the flag clear — this is a
/// single i8 load and a predicted-not-taken branch, no call. Only when the green
/// monitor has set the flag do we call `pith_green_maybe_yield`, which itself
/// decides whether the running task must actually yield (and is a no-op off the
/// green backend, so this is inert under os threads even when reached).
///
/// Leaves the builder positioned on a fresh continuation block; the caller then
/// emits the original back-edge branch there.
fn emit_preempt_safepoint(
    codegen: &mut CodeGen,
    builder: &mut FunctionBuilder<'_>,
    func_ref_cache: &mut HashMap<FuncId, cranelift::codegen::ir::FuncRef>,
    runtime_funcs: &HashMap<String, FuncId>,
    preempt_flag: cranelift_module::DataId,
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

fn emit_runtime_error_value(
    codegen: &mut CodeGen,
    builder: &mut FunctionBuilder<'_>,
    func_ref_cache: &mut HashMap<FuncId, cranelift::codegen::ir::FuncRef>,
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

fn emit_checked_int_div_or_mod(
    codegen: &mut CodeGen,
    builder: &mut FunctionBuilder<'_>,
    func_ref_cache: &mut HashMap<FuncId, cranelift::codegen::ir::FuncRef>,
    runtime_funcs: &HashMap<String, FuncId>,
    op: &str,
    a: Value,
    b: Value,
) -> Result<Value, CompileError> {
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);

    let zero_divisor = builder.ins().icmp_imm(IntCC::Equal, b, 0);
    let zero_error = builder.create_block();
    let after_zero_check = builder.create_block();
    builder
        .ins()
        .brif(zero_divisor, zero_error, &[], after_zero_check, &[]);

    builder.switch_to_block(zero_error);
    let zero_result = emit_runtime_error_value(codegen, builder, func_ref_cache, runtime_funcs, 1)?;
    jump_with_i64_arg(builder, done, zero_result);

    builder.switch_to_block(after_zero_check);
    let min_int = builder.ins().iconst(types::I64, i64::MIN);
    let is_min_int = builder.ins().icmp(IntCC::Equal, a, min_int);
    let is_minus_one = builder.ins().icmp_imm(IntCC::Equal, b, -1);
    let overflows = builder.ins().band(is_min_int, is_minus_one);
    let overflow_error = builder.create_block();
    let calculate = builder.create_block();
    builder
        .ins()
        .brif(overflows, overflow_error, &[], calculate, &[]);

    builder.switch_to_block(overflow_error);
    let overflow_result =
        emit_runtime_error_value(codegen, builder, func_ref_cache, runtime_funcs, 2)?;
    jump_with_i64_arg(builder, done, overflow_result);

    builder.switch_to_block(calculate);
    let value = match op {
        "div" => builder.ins().sdiv(a, b),
        "mod" => builder.ins().srem(a, b),
        _ => {
            return Err(CompileError::ModuleError(format!(
                "IR consumer: unsupported checked integer op '{op}'"
            )))
        }
    };
    jump_with_i64_arg(builder, done, value);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
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

    // green preemption safe-points are emitted by default. green is the default
    // backend, and a task that loops waiting on something another task must
    // publish holds its worker until it is preempted — without a safe-point it
    // never is, so it starves the task it is waiting for. the check costs
    // +0.07% to +1.85% instructions across the benchmarks here and +0.52% on the
    // compiler compiling itself; a flat arithmetic loop is the outlier at +86%
    // instructions and about 2x wall, because six instructions of check land on a
    // seven-instruction body and the call in them costs the loop its registers.
    // `PITH_GREEN_PREEMPT=0` is the opt-out for a build that will never run
    // green. `None` here means "emit nothing"; `Some(flag)` carries the imported
    // flag symbol.
    let preempt_flag = if green_preempt_enabled() {
        Some(
            codegen
                .module
                .declare_data("PITH_PREEMPT_REQUESTED", Linkage::Import, false, false)
                .map_err(|e| CompileError::ModuleError(e.to_string()))?,
        )
    } else {
        None
    };

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
                // accumulated as bytes, not chars: `byte as char` maps a byte
                // to the code point of the same value, so the two bytes of a
                // utf8 character each got re-encoded and "café" came out as
                // "cafÃ©". the escapes below are all ascii, and `raw` is valid
                // utf8, so the assembled bytes are too.
                let mut content: Vec<u8> = Vec::new();
                let bytes = raw.as_bytes();
                let mut j = 0;
                while j < bytes.len() {
                    if bytes[j] == b'{' && j + 1 < bytes.len() && bytes[j + 1] == b'{' {
                        // `{{` is an escaped literal brace
                        content.push(b'{');
                        j += 2;
                    } else if bytes[j] == b'}' && j + 1 < bytes.len() && bytes[j + 1] == b'}' {
                        // `}}` is an escaped literal brace
                        content.push(b'}');
                        j += 2;
                    } else if bytes[j] == b'\\' && j + 1 < bytes.len() {
                        match bytes[j + 1] {
                            b'n' => {
                                content.push(b'\n');
                                j += 2;
                            }
                            b't' => {
                                content.push(b'\t');
                                j += 2;
                            }
                            b'\\' => {
                                content.push(b'\\');
                                j += 2;
                            }
                            b'"' => {
                                content.push(b'"');
                                j += 2;
                            }
                            b'r' => {
                                content.push(b'\r');
                                j += 2;
                            }
                            b'0' => {
                                content.push(b'\0');
                                j += 2;
                            }
                            _ => {
                                content.push(bytes[j]);
                                j += 1;
                            }
                        }
                    } else {
                        content.push(bytes[j]);
                        j += 1;
                    }
                }
                let content = String::from_utf8(content)
                    .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
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
                    let init_val: i64 = if init_kind == "list"
                        || init_kind == "map"
                        || init_kind == "set"
                        || init_kind == "set_int"
                    {
                        0
                    } else if init_kind.starts_with("str:") {
                        0 // will be patched in __init_globals
                    } else {
                        // the same literal forms iconst accepts, and the same
                        // refusal to coerce: this used to be a decimal-only
                        // parse with unwrap_or(0), so a global initialized to
                        // a hex or binary literal silently became zero while
                        // the identical literal inline compiled correctly.
                        parse_i64_operand(init_kind, "global initializer", line, &gname)?
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
                    let nparam: usize = parts[2].parse().map_err(|_| {
                        CompileError::ModuleError(format!(
                            "IR consumer: invalid parameter count '{}' in function '{}'",
                            parts[2], name
                        ))
                    })?;
                    let mut sig = codegen.module.make_signature();
                    for _ in 0..nparam {
                        sig.params.push(AbiParam::new(types::I64));
                    }
                    push_return_types(&mut sig.returns, parts.get(3).copied().unwrap_or(""));
                    let linkage = if name == "main" {
                        Linkage::Export
                    } else {
                        Linkage::Local
                    };
                    if let Ok(func_id) = codegen.module.declare_function(name, linkage, &sig) {
                        declared_funcs.insert(name.to_string(), func_id);
                    }
                    // silently skip if name conflicts with runtime declaration
                }
            }
            _ => {}
        }
    }

    // Declare string literal data
    let mut string_lits: HashMap<String, cranelift_module::DataId> = HashMap::new();
    for (idx, content) in &string_data {
        if !string_lits.contains_key(idx) {
            let name = format!("__irstr_{}", idx);
            let data_id = crate::declare_string_data(&mut codegen.module, &name, content)
                .map_err(|e| CompileError::ModuleError(format!("string data: {:?}", e)))?;
            string_lits.insert(idx.clone(), data_id);
        }
    }

    // Pass 1.5: find bit-wrapper functions (std.bits and anything shaped like
    // it) whose calls can lower to a single native instruction. This must see
    // every function before any body compiles, since callers usually precede
    // the wrappers in the stream.
    let mut bits_aliases: HashMap<String, &'static str> = HashMap::new();
    {
        let mut j = 0;
        while j < lines.len() {
            let parts: Vec<&str> = lines[j].split_whitespace().collect();
            if parts.is_empty() || parts[0] != "func" {
                j += 1;
                continue;
            }
            let fname = parts[1].to_string();
            j += 1;
            let mut body: Vec<&str> = Vec::new();
            let mut params: Vec<String> = Vec::new();
            while j < lines.len() {
                let bparts: Vec<&str> = lines[j].split_whitespace().collect();
                if !bparts.is_empty() && bparts[0] == "endfunc" {
                    j += 1;
                    break;
                }
                if !bparts.is_empty() && bparts[0] == "param" && bparts.len() >= 2 {
                    params.push(bparts[1].to_string());
                } else {
                    body.push(lines[j]);
                }
                j += 1;
            }
            if let Some(op) = detect_bits_wrapper(&params, &body) {
                bits_aliases.entry(fname).or_insert(op);
            }
        }
    }

    // Pass 2: compile function bodies (first definition wins for duplicates)
    let mut compiled_funcs: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut built: Vec<(FuncId, String, cranelift::codegen::ir::Function)> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let parts: Vec<&str> = lines[i].split_whitespace().collect();
        if parts.is_empty() || parts[0] != "func" {
            i += 1;
            continue;
        }

        let func_name = parts[1].to_string();
        let _nparam: usize = parts[2].parse().unwrap_or(0);
        let func_retkind = parts.get(3).copied().unwrap_or("").to_string();
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
        let fused = fuse_concat_chains(&body_lines);
        let body_lines: Vec<&str> = match &fused {
            Some(lines) => lines.iter().map(String::as_str).collect(),
            None => body_lines,
        };
        let appended = fuse_self_appends(&body_lines);
        let body_lines: Vec<&str> = match &appended {
            Some(lines) => lines.iter().map(String::as_str).collect(),
            None => body_lines,
        };
        if let Some(&func_id) = declared_funcs.get(&func_name) {
            let func = compile_ir_function(
                codegen,
                func_id,
                &func_name,
                &func_retkind,
                &param_names,
                &body_lines,
                runtime_funcs,
                &declared_funcs,
                &string_lits,
                &struct_layouts,
                &global_data,
                &str_globals,
                &string_global_names,
                preempt_flag,
                &bits_aliases,
            )?;
            built.push((func_id, func_name.clone(), func));
        }
    }

    define_with_inlining(codegen, built)?;
    Ok(declared_funcs)
}

/// Callees with more instructions than this keep their call. A Pith function
/// this small is a helper (a predicate, an accessor, a one-line wrapper), for
/// which the call costs about as much as the body.
const INLINE_MAX_INSTS: usize = 24;

/// Define every built function, first inlining the small functions it calls.
/// The functions are all built before any is defined so that a caller can
/// inline a callee that appears later in the IR. `PITH_INLINE=0` turns the
/// inlining off, which is how to A/B it.
fn define_with_inlining(
    codegen: &mut CodeGen,
    built: Vec<(FuncId, String, cranelift::codegen::ir::Function)>,
) -> Result<(), CompileError> {
    let enabled = std::env::var("PITH_INLINE").map_or(true, |v| v != "0");
    // the inliner takes callee bodies in their legalized form, so each
    // candidate is legalized once, on a copy, before any caller asks for it
    let mut bodies: HashMap<FuncId, cranelift::codegen::ir::Function> = HashMap::new();
    if enabled {
        for (id, name, func) in &built {
            if is_inline_candidate(name, func) {
                let mut ctx = codegen.module.make_context();
                ctx.func = func.clone();
                if ctx.legalize(codegen.module.isa()).is_ok() && !names_data(&ctx.func) {
                    bodies.insert(*id, ctx.func);
                }
            }
        }
    }
    for (func_id, func_name, func) in &built {
        let mut ctx = codegen.module.make_context();
        ctx.func = func.clone();
        if enabled {
            let inliner = SmallFnInliner {
                caller: *func_id,
                bodies: &bodies,
            };
            ctx.inline(inliner).map_err(|e| {
                CompileError::ModuleError(format!("IR consumer: inlining into {func_name}: {e}"))
            })?;
        }
        define_built_function(codegen, *func_id, func_name, &mut ctx)?;
    }
    Ok(())
}

/// Whether the function refers to a global by symbol: a module global, a
/// string constant, the preemption flag. Cranelift's inliner copies such a
/// global value into the caller with its name reference unchanged, but that
/// reference indexes the callee's own name table, so in the caller it names
/// some other symbol. Function references are translated, so calls are fine;
/// a callee that names data is not inlined.
fn names_data(func: &cranelift::codegen::ir::Function) -> bool {
    func.global_values.values().any(|gv| {
        matches!(
            gv,
            cranelift::codegen::ir::GlobalValueData::Symbol {
                name: cranelift::codegen::ir::ExternalName::User(_),
                ..
            }
        )
    })
}

/// Whether calls to this function may be inlined: small, and not one of the
/// entry points the runtime calls by name.
fn is_inline_candidate(name: &str, func: &cranelift::codegen::ir::Function) -> bool {
    if name == "main" || name.starts_with("__init_globals") {
        return false;
    }
    let mut insts = 0usize;
    for block in func.layout.blocks() {
        insts += func.layout.block_insts(block).count();
        if insts > INLINE_MAX_INSTS {
            return false;
        }
    }
    true
}

/// Directs Cranelift's inliner: a direct call to a small Pith function is
/// inlined, everything else keeps its call. Calls inside an inlined body are
/// not inlined again, which bounds the growth and rules out recursion through
/// a cycle; a function never inlines itself.
struct SmallFnInliner<'a> {
    caller: FuncId,
    bodies: &'a HashMap<FuncId, cranelift::codegen::ir::Function>,
}

impl cranelift::codegen::inline::Inline for SmallFnInliner<'_> {
    fn inline(
        &mut self,
        caller: &cranelift::codegen::ir::Function,
        _call_inst: cranelift::codegen::ir::Inst,
        _call_opcode: cranelift::codegen::ir::Opcode,
        callee: cranelift::codegen::ir::FuncRef,
        _call_args: &[Value],
    ) -> cranelift::codegen::inline::InlineCommand<'_> {
        use cranelift::codegen::inline::InlineCommand;
        use cranelift::codegen::ir::ExternalName;
        // cranelift_module names a function as user name (namespace 0,
        // index = its FuncId); a runtime import is a function too, but the
        // runtime is not in `bodies`, so it keeps its call.
        let ExternalName::User(name_ref) = caller.dfg.ext_funcs[callee].name else {
            return InlineCommand::KeepCall;
        };
        let name = &caller.params.user_named_funcs()[name_ref];
        if name.namespace != 0 {
            return InlineCommand::KeepCall;
        }
        let id = FuncId::from_u32(name.index);
        if id == self.caller {
            return InlineCommand::KeepCall;
        }
        match self.bodies.get(&id) {
            Some(body) => InlineCommand::Inline {
                callee: std::borrow::Cow::Borrowed(body),
                visit_callee: false,
            },
            None => InlineCommand::KeepCall,
        }
    }
}

/// Rewrite each chain of string concatenations in a function body into one
/// `concatn dst count part...` instruction, or return `None` when the body has
/// no chain. The emitter lowers `a + b + c + d` to a concat per `+`, each
/// result released right after the next concat reads it, so a chain of n
/// parts made n - 1 strings and copied the leading parts over and over;
/// `concatn` makes one.
///
/// A concat continues a chain when its left operand is the result of an
/// earlier concat that nothing else reads: the register appears once as a
/// left operand, once in a `pith_cstring_release`, and nowhere else (a number
/// that merely looks like it, an immediate, only makes the check refuse). No
/// label, branch or return may come between the two. The chain's
/// intermediate results and their releases go away, and a release of one of
/// the chain's parts that sat inside the chain moves to just after the
/// `concatn`, so every part is still alive when it is read. A string is
/// immutable, so releasing it later changes nothing but when it is freed.
fn fuse_concat_chains(body: &[&str]) -> Option<Vec<String>> {
    let parsed: Vec<Vec<&str>> = body.iter().map(|l| l.split_whitespace().collect()).collect();
    if !parsed.iter().any(|p| p.first() == Some(&"concat")) {
        return None;
    }
    let is_concat = |p: &[&str]| p.len() == 4 && p[0] == "concat";
    let release_of = |p: &[&str]| -> Option<usize> {
        if p.len() == 6 && p[0] == "call" && p[2] == "pith_cstring_release" && p[4] == "1" {
            p[5].parse().ok()
        } else {
            None
        }
    };
    let is_barrier = |p: &[&str]| matches!(p.first(), Some(&"label" | &"brif" | &"jmp" | &"ret"));

    // every token after the opcode, counted. a register's own definition and
    // its uses all show up here, and so does anything that looks like it.
    let mut mentions: HashMap<&str, usize> = HashMap::new();
    for p in &parsed {
        for t in p.iter().skip(1) {
            *mentions.entry(t).or_insert(0) += 1;
        }
    }
    let mut concat_line: HashMap<usize, usize> = HashMap::new();
    let mut release_lines: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, p) in parsed.iter().enumerate() {
        if is_concat(p) {
            if let Ok(dst) = p[1].parse::<usize>() {
                concat_line.insert(dst, i);
            }
        }
        if let Some(reg) = release_of(p) {
            release_lines.entry(reg).or_default().push(i);
        }
    }

    // prev[i] = the line of the concat whose result line i extends.
    let mut prev: HashMap<usize, usize> = HashMap::new();
    let mut barrier_before = vec![0usize; parsed.len() + 1];
    for (i, p) in parsed.iter().enumerate() {
        barrier_before[i + 1] = barrier_before[i] + usize::from(is_barrier(p));
    }
    for (i, p) in parsed.iter().enumerate() {
        if !is_concat(p) {
            continue;
        }
        let Ok(left) = p[2].parse::<usize>() else {
            continue;
        };
        let Some(&j) = concat_line.get(&left) else {
            continue;
        };
        let released_after = release_lines.get(&left).is_some_and(|r| r.len() == 1 && r[0] > i);
        if j < i
            && mentions.get(p[2]).copied() == Some(3)
            && released_after
            && barrier_before[i] == barrier_before[j + 1]
        {
            prev.insert(i, j);
        }
    }
    if prev.is_empty() {
        return None;
    }

    let extended: std::collections::HashSet<usize> = prev.values().copied().collect();
    let mut chains: Vec<(Vec<usize>, Vec<&str>)> = Vec::new();
    for &last in prev.keys() {
        if extended.contains(&last) {
            continue; // not the end of its chain
        }
        let mut members = vec![last];
        let mut at = last;
        while let Some(&j) = prev.get(&at) {
            members.push(j);
            at = j;
        }
        members.reverse();
        let first = members[0];
        let mut parts: Vec<&str> = vec![parsed[first][2], parsed[first][3]];
        for &m in &members[1..] {
            parts.push(parsed[m][3]);
        }
        chains.push((members, parts));
    }
    // a register that is a part of two chains would have its release moved
    // by one and needed by the other, so neither of those chains is fused.
    let mut chains_of_part: HashMap<&str, usize> = HashMap::new();
    for (_, parts) in &chains {
        let unique: std::collections::HashSet<&str> = parts.iter().copied().collect();
        for part in unique {
            *chains_of_part.entry(part).or_insert(0) += 1;
        }
    }
    chains.retain(|(_, parts)| parts.iter().all(|part| chains_of_part[part] == 1));
    if chains.is_empty() {
        return None;
    }

    let mut dropped = vec![false; parsed.len()];
    let mut replaced: HashMap<usize, String> = HashMap::new();
    let mut moved_after: HashMap<usize, Vec<usize>> = HashMap::new();
    for (members, parts) in &chains {
        let first = members[0];
        let last = members[members.len() - 1];
        for &m in &members[..members.len() - 1] {
            dropped[m] = true;
            if let Ok(dst) = parsed[m][1].parse::<usize>() {
                for &r in &release_lines[&dst] {
                    dropped[r] = true;
                }
            }
        }
        let part_regs: std::collections::HashSet<usize> =
            parts.iter().filter_map(|t| t.parse().ok()).collect();
        let mut moved = Vec::new();
        for k in first + 1..last {
            if dropped[k] {
                continue;
            }
            if release_of(&parsed[k]).is_some_and(|r| part_regs.contains(&r)) {
                dropped[k] = true;
                moved.push(k);
            }
        }
        moved_after.insert(last, moved);
        replaced.insert(
            last,
            format!("concatn {} {} {}", parsed[last][1], parts.len(), parts.join(" ")),
        );
    }

    let mut out = Vec::with_capacity(body.len());
    for (i, line) in body.iter().enumerate() {
        if let Some(fused) = replaced.get(&i) {
            out.push(fused.clone());
            for &k in &moved_after[&i] {
                out.push(body[k].to_string());
            }
        } else if !dropped[i] {
            out.push(line.to_string());
        }
    }
    Some(out)
}

/// Rewrite `s = s + ...` on a local into `appendn dst count base part...`,
/// so the string grows in place while the local holds its only count. The
/// emitter lowers that assignment as a load of the old value (O), a second
/// load as the concat's left operand (A), the concat or concatn into D, a
/// release of O, and `store V D`. The release is the evidence that V owns a
/// count; the rewrite hands that count to `pith_cstring_append_many` instead
/// and drops the release. It applies only when, from the first of the two
/// loads to the store, nothing else loads or stores V and no label, branch
/// or return intervenes; O, A and D have no other use; and V is not a global
/// (`__g_`), which another task may be reading.
fn fuse_self_appends(body: &[&str]) -> Option<Vec<String>> {
    let parsed: Vec<Vec<&str>> = body.iter().map(|l| l.split_whitespace().collect()).collect();
    let is_join = |p: &[&str]| (p.len() == 4 && p[0] == "concat") || (p.len() >= 5 && p[0] == "concatn");
    if !parsed.iter().any(|p| is_join(p)) {
        return None;
    }
    let mut mentions: HashMap<&str, usize> = HashMap::new();
    let mut load_of: HashMap<&str, (usize, &str)> = HashMap::new();
    let mut join_line: HashMap<&str, usize> = HashMap::new();
    let mut release_line: HashMap<&str, usize> = HashMap::new();
    for (i, p) in parsed.iter().enumerate() {
        for t in p.iter().skip(1) {
            *mentions.entry(t).or_insert(0) += 1;
        }
        if p.len() == 3 && p[0] == "load" {
            load_of.insert(p[1], (i, p[2]));
        } else if is_join(p) {
            join_line.insert(p[1], i);
        } else if p.len() == 6 && p[0] == "call" && p[2] == "pith_cstring_release" && p[4] == "1" {
            release_line.insert(p[5], i);
        }
    }
    let once = |r: &str| mentions.get(r).copied() == Some(2);

    let mut dropped = vec![false; parsed.len()];
    let mut replaced: HashMap<usize, String> = HashMap::new();
    for (k, p) in parsed.iter().enumerate() {
        if p.len() != 3 || p[0] != "store" || p[1].starts_with("__g_") {
            continue;
        }
        let (var, dst) = (p[1], p[2]);
        let Some(&c) = join_line.get(dst) else { continue };
        let join = &parsed[c];
        let (base, rest) = if join[0] == "concat" {
            (join[2], &join[3..])
        } else {
            (join[3], &join[4..])
        };
        let Some(&(a, a_var)) = load_of.get(base) else { continue };
        if a_var != var || a > c || !once(dst) || !once(base) {
            continue;
        }
        // the release of the old value, between the join and the store
        let old = (c + 1..k).find_map(|r| {
            let rp = &parsed[r];
            let is_release = rp.len() == 6 && rp[0] == "call" && rp[2] == "pith_cstring_release";
            let reg = if is_release { rp[5] } else { return None };
            match load_of.get(reg) {
                Some(&(o, v)) if v == var && once(reg) && release_line.get(reg) == Some(&r) => {
                    Some((o, r))
                }
                _ => None,
            }
        });
        let Some((o, r)) = old else { continue };
        let start = o.min(a);
        let clean = (start..=k).all(|i| {
            let q = &parsed[i];
            let barrier = matches!(q.first(), Some(&"label" | &"brif" | &"jmp" | &"ret"));
            let stores_var = i != k && q.len() == 3 && q[0] == "store" && q[1] == var;
            let loads_var = q.len() == 3 && q[0] == "load" && q[2] == var && i != o && i != a;
            !barrier && !stores_var && !loads_var
        });
        if !clean {
            continue;
        }
        dropped[r] = true;
        replaced.insert(c, format!("appendn {} {} {} {}", dst, rest.len(), base, rest.join(" ")));
    }
    if replaced.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(body.len());
    for (i, line) in body.iter().enumerate() {
        if let Some(rewritten) = replaced.get(&i) {
            out.push(rewritten.clone());
        } else if !dropped[i] {
            out.push(line.to_string());
        }
    }
    Some(out)
}

/// A `-> T!` function whose ok value fits one register returns two values
/// instead of a heap-allocated result box: `(is_ok: i64, payload)`. The payload
/// carries the ok value when is_ok=1, else the error-message string pointer.
/// `result_reg` uses an i64 payload (ints, handles, pointers); `result_reg_f`
/// uses an f64 payload for float ok values. Any other retkind is the normal
/// single-i64 return. Callers and the callee agree via this retkind string.
fn result_abi_payload_type(retkind: &str) -> Option<types::Type> {
    match retkind {
        "result_reg" => Some(types::I64),
        "result_reg_f" => Some(types::F64),
        _ => None,
    }
}

/// Push the return AbiParams for a function's retkind: two values for the
/// register result ABI, one i64 otherwise.
fn push_return_types(returns: &mut Vec<AbiParam>, retkind: &str) {
    match result_abi_payload_type(retkind) {
        Some(payload) => {
            returns.push(AbiParam::new(types::I64));
            returns.push(AbiParam::new(payload));
        }
        None => returns.push(AbiParam::new(types::I64)),
    }
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
    func_retkind: &str,
    param_names: &[String],
    body_lines: &[&str],
    runtime_funcs: &HashMap<String, FuncId>,
    declared_funcs: &HashMap<String, FuncId>,
    string_lits: &HashMap<String, cranelift_module::DataId>,
    struct_layouts: &HashMap<String, Vec<String>>,
    global_data: &HashMap<String, cranelift_module::DataId>,
    str_globals: &[(String, String)],
    string_global_names: &std::collections::HashSet<String>,
    preempt_flag: Option<cranelift_module::DataId>,
    bits_aliases: &HashMap<String, &'static str>,
) -> Result<cranelift::codegen::ir::Function, CompileError> {
    let mut ctx = codegen.module.make_context();

    // Build signature
    for _ in param_names {
        ctx.func.signature.params.push(AbiParam::new(types::I64));
    }
    push_return_types(&mut ctx.func.signature.returns, func_retkind);

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
    // the values that hold the address of a string literal
    let mut literal_values: HashSet<Value> = HashSet::new();
    let mut bytes_regs: HashSet<usize> = HashSet::new();
    let mut bytes_vars: HashSet<String> = HashSet::new();
    let mut float_regs: HashSet<usize> = HashSet::new();
    let mut float_vars: HashSet<String> = HashSet::new();
    let mut reg_source_vars: HashMap<usize, String> = HashMap::new();
    let mut struct_regs: HashMap<usize, String> = HashMap::new();
    let mut struct_vars: HashMap<String, String> = HashMap::new();
    let mut named_vars: HashMap<String, Variable> = HashMap::new();
    let mut labels: HashMap<String, Block> = HashMap::new();
    // labels already emitted, in stream order, as we walk the instructions. a
    // jmp/brif whose target is in here is jumping *backward* to an earlier label
    // — a loop back-edge — which is where we insert a preemption safe-point. a
    // target not yet seen is a forward jump (if/else/result merge, break) and is
    // correctly excluded.
    let mut defined_labels: HashSet<String> = HashSet::new();
    #[cfg(not(pith_cranelift_new_api))]
    let mut next_var_id: u32 = 0;

    // Call __init_globals (and module-specific __init_globals_N) at the start of main
    if func_name == "main" {
        // every module's tracers are registered before any global is built:
        // a global is marked shared as it is stored, and the marking walks a
        // struct's fields through its tracer, so one registered later would
        // leave what the global reaches unmarked. main's body registers them
        // again, which only repeats the insert.
        for (name, &fid) in declared_funcs.iter() {
            if name.contains("__cc_register_tracers") {
                let reg_ref = codegen.module.declare_func_in_func(fid, builder.func);
                builder.ins().call(reg_ref, &[]);
            }
        }
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
            if let (Some(&data_id), Some(&lit_id)) = (
                global_data.get(gname.as_str()),
                string_lits.get(str_id.as_str()),
            ) {
                let str_result = string_lit_addr(codegen, &mut builder, lit_id);
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
            #[cfg(pith_cranelift_new_api)]
            let var = declare_i64_var(&mut builder);
            #[cfg(not(pith_cranelift_new_api))]
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

    // Pre-scan for labels and create blocks. the label right after a jmp back
    // to an earlier label is where that loop exits.
    let mut loop_exits: HashSet<&str> = HashSet::new();
    let mut after_back_edge = false;
    for line in body_lines {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if !parts.is_empty() && parts[0] == "label" && parts.len() >= 2 {
            let block = builder.create_block();
            labels.insert(parts[1].to_string(), block);
            if after_back_edge {
                loop_exits.insert(parts[1]);
            }
        }
        after_back_edge = parts.first() == Some(&"jmp")
            && parts.get(1).is_some_and(|target| labels.contains_key(*target));
    }

    // Pre-scan for indexed list reads whose Optional tuple never escapes; those
    // calls compile to inline loads instead (see inline_list_get_opt). The map
    // carries each virtualized register's (is_some, value) pair for the field
    // reads that follow.
    let inline_opt_regs = scan_unpackable_opt_regs(body_lines);
    let mut virtual_opts: HashMap<usize, (Value, Value)> = HashMap::new();

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

        // Record a label as "defined" the moment we reach it in the stream, in
        // both the terminated and live paths below. A later jmp/brif to a label
        // already in this set is a back-edge (see `defined_labels`).
        if parts[0] == "label" && parts.len() >= 2 {
            defined_labels.insert(parts[1].to_string());
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
                let reg = parse_reg(parts[1], line, func_name)?;
                let val = parse_i64_operand(parts[2], "integer constant", line, func_name)?;
                let v = builder.ins().iconst(types::I64, val);
                regs.insert(reg, v);
                reg_source_vars.remove(&reg);
                struct_regs.remove(&reg);
                string_regs.remove(&reg);
                bytes_regs.remove(&reg);
                float_regs.remove(&reg);
            }

            "fconst" if parts.len() >= 3 => {
                let reg = parse_reg(parts[1], line, func_name)?;
                let fval: f64 = parts[2].parse().map_err(|_| {
                    CompileError::ModuleError(format!(
                        "IR consumer: invalid float constant '{}' in {}: {}",
                        parts[2], func_name, line
                    ))
                })?;
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
                let reg = parse_reg(parts[1], line, func_name)?;
                let str_idx = parts[2].to_string();
                if let Some(&lit_id) = string_lits.get(&str_idx) {
                    let v = string_lit_addr(codegen, &mut builder, lit_id);
                    literal_values.insert(v);
                    regs.insert(reg, v);
                } else {
                    // a strref may name a string defined in another compilation
                    // unit (docgen and other cross-module paths do this); it
                    // resolves at link time, so a miss here is not malformed ir.
                    regs.insert(reg, builder.ins().iconst(types::I64, 0));
                }
                reg_source_vars.remove(&reg);
                struct_regs.remove(&reg);
                string_regs.insert(reg);
                bytes_regs.remove(&reg);
                float_regs.remove(&reg);
            }

            "band" | "bor" | "bxor" | "shl" | "shr" if parts.len() >= 4 => {
                let reg = parse_reg(parts[1], line, func_name)?;
                let a = get_reg(&regs, parts[2])?;
                let b = get_reg(&regs, parts[3])?;
                let v = match parts[0] {
                    "band" => builder.ins().band(a, b),
                    "bor" => builder.ins().bor(a, b),
                    "bxor" => builder.ins().bxor(a, b),
                    "shl" => builder.ins().ishl(a, b),
                    "shr" => builder.ins().ushr(a, b),
                    _ => {
                        return Err(CompileError::ModuleError(format!(
                            "IR consumer: unsupported bitwise instruction in {}: {}",
                            func_name, line
                        )))
                    }
                };
                regs.insert(reg, v);
                reg_source_vars.remove(&reg);
                struct_regs.remove(&reg);
                string_regs.remove(&reg);
                bytes_regs.remove(&reg);
                float_regs.remove(&reg);
            }

            "bnot" if parts.len() >= 3 => {
                let reg = parse_reg(parts[1], line, func_name)?;
                let a = get_reg(&regs, parts[2])?;
                let v = builder.ins().bnot(a);
                regs.insert(reg, v);
                reg_source_vars.remove(&reg);
                struct_regs.remove(&reg);
                string_regs.remove(&reg);
                bytes_regs.remove(&reg);
                float_regs.remove(&reg);
            }

            "and" | "or" if parts.len() >= 4 => {
                let reg = parse_reg(parts[1], line, func_name)?;
                let a = get_reg(&regs, parts[2])?;
                let b = get_reg(&regs, parts[3])?;
                let v = match parts[0] {
                    "and" => builder.ins().band(a, b),
                    "or" => builder.ins().bor(a, b),
                    _ => {
                        return Err(CompileError::ModuleError(format!(
                            "IR consumer: unsupported boolean instruction in {}: {}",
                            func_name, line
                        )))
                    }
                };
                regs.insert(reg, v);
                reg_source_vars.remove(&reg);
                struct_regs.remove(&reg);
                string_regs.remove(&reg);
                bytes_regs.remove(&reg);
                float_regs.remove(&reg);
            }

            "fadd" | "fsub" | "fmul" | "fdiv" if parts.len() >= 4 => {
                let reg = parse_reg(parts[1], line, func_name)?;
                let a = get_reg(&regs, parts[2])?;
                let b = get_reg(&regs, parts[3])?;
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
                    _ => {
                        return Err(CompileError::ModuleError(format!(
                            "IR consumer: unsupported float instruction in {}: {}",
                            func_name, line
                        )))
                    }
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
                let reg = parse_reg(parts[1], line, func_name)?;
                let a_reg = parts[2].parse::<usize>().ok();
                let b_reg = parts[3].parse::<usize>().ok();
                let a = get_reg(&regs, parts[2])?;
                let b = get_reg(&regs, parts[3])?;
                reg_source_vars.remove(&reg);
                struct_regs.remove(&reg);
                // If `add` has a string operand, treat as concat (IR emitter
                // sometimes emits `add` instead of `concat` when variable types
                // aren't tracked across function boundaries)
                if parts[0] == "add"
                    && a_reg.is_some_and(|r| string_regs.contains(&r))
                    && b_reg.is_some_and(|r| string_regs.contains(&r))
                {
                    if let Some(&concat_id) = runtime_funcs.get("pith_concat_cstr") {
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
                        _ => {
                            return Err(CompileError::ModuleError(format!(
                                "IR consumer: unsupported float-promoted instruction in {}: {}",
                                func_name, line
                            )))
                        }
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
                        "div" | "mod" => emit_checked_int_div_or_mod(
                            codegen,
                            &mut builder,
                            &mut func_ref_cache,
                            runtime_funcs,
                            parts[0],
                            a,
                            b,
                        )?,
                        _ => {
                            return Err(CompileError::ModuleError(format!(
                                "IR consumer: unsupported arithmetic instruction in {}: {}",
                                func_name, line
                            )))
                        }
                    };
                    regs.insert(reg, v);
                    string_regs.remove(&reg);
                    bytes_regs.remove(&reg);
                    float_regs.remove(&reg);
                }
            }

            "eq" | "neq" | "lt" | "gt" | "lte" | "gte" if parts.len() >= 4 => {
                let reg = parse_reg(parts[1], line, func_name)?;
                let a_reg = parts[2].parse::<usize>().ok();
                let b_reg = parts[3].parse::<usize>().ok();
                let a = get_reg(&regs, parts[2])?;
                let b = get_reg(&regs, parts[3])?;
                reg_source_vars.remove(&reg);
                struct_regs.remove(&reg);
                // For lt/gt/lte/gte on strings, call runtime comparison
                let is_str_cmp = matches!(parts[0], "lt" | "gt" | "lte" | "gte")
                    && (a_reg.is_some_and(|r| string_regs.contains(&r))
                        || b_reg.is_some_and(|r| string_regs.contains(&r)));
                if is_str_cmp {
                    let cmp_name = match parts[0] {
                        "lt" => "pith_cstring_lt",
                        "gt" => "pith_cstring_gt",
                        "lte" => "pith_cstring_lte",
                        "gte" => "pith_cstring_gte",
                        _ => "pith_cstring_lt",
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

            // `concatn dst count part...`, made by fuse_concat_chains: one
            // pith_concat_cstr_many call for up to eight parts. a longer chain
            // joins eight at a time, each call's result leading the next and
            // released once that one has copied it.
            "concatn" if parts.len() >= 4 => {
                let reg = parse_reg(parts[1], line, func_name)?;
                let mut values = Vec::with_capacity(parts.len() - 3);
                for t in &parts[3..] {
                    values.push(get_reg(&regs, t)?);
                }
                let many = runtime_func_ref(
                    codegen,
                    &mut builder,
                    &mut func_ref_cache,
                    runtime_funcs,
                    "pith_concat_cstr_many",
                )?;
                let release = runtime_func_ref(
                    codegen,
                    &mut builder,
                    &mut func_ref_cache,
                    runtime_funcs,
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
                    let count = builder.ins().iconst(types::I64, group.len() as i64);
                    let mut args = vec![count];
                    args.extend_from_slice(&group);
                    while args.len() < max + 1 {
                        args.push(builder.ins().iconst(types::I64, 0));
                    }
                    let call = builder.ins().call(many, &args);
                    let joined = builder.func.dfg.first_result(call);
                    if let Some(previous) = acc {
                        builder.ins().call(release, &[previous]);
                    }
                    if rest.is_empty() {
                        break joined;
                    }
                    acc = Some(joined);
                };
                regs.insert(reg, joined);
                reg_source_vars.remove(&reg);
                struct_regs.remove(&reg);
                string_regs.insert(reg);
                bytes_regs.remove(&reg);
                float_regs.remove(&reg);
            }

            // `appendn dst count base part...`, made by fuse_self_appends: the
            // base's count passes to pith_cstring_append_many, which adds up
            // to seven parts per call; a longer run chains calls, each taking
            // the previous result's count, so nothing is released here.
            "appendn" if parts.len() >= 5 => {
                let reg = parse_reg(parts[1], line, func_name)?;
                let mut acc = get_reg(&regs, parts[3])?;
                let mut values = Vec::with_capacity(parts.len() - 4);
                for t in &parts[4..] {
                    values.push(get_reg(&regs, t)?);
                }
                let append = runtime_func_ref(
                    codegen,
                    &mut builder,
                    &mut func_ref_cache,
                    runtime_funcs,
                    "pith_cstring_append_many",
                )?;
                let max = pith_runtime::APPEND_MANY_MAX;
                for group in values.chunks(max) {
                    let count = builder.ins().iconst(types::I64, group.len() as i64);
                    let mut args = vec![count, acc];
                    args.extend_from_slice(group);
                    while args.len() < max + 2 {
                        args.push(builder.ins().iconst(types::I64, 0));
                    }
                    let call = builder.ins().call(append, &args);
                    acc = builder.func.dfg.first_result(call);
                }
                regs.insert(reg, acc);
                reg_source_vars.remove(&reg);
                note_loaded_kind(
                    reg,
                    Some("string"),
                    &mut string_regs,
                    &mut bytes_regs,
                    &mut float_regs,
                    &mut struct_regs,
                );
            }

            "concat" if parts.len() >= 4 => {
                let reg = parse_reg(parts[1], line, func_name)?;
                let a = get_reg(&regs, parts[2])?;
                let b = get_reg(&regs, parts[3])?;
                reg_source_vars.remove(&reg);
                struct_regs.remove(&reg);
                if let Some(&concat_id) = runtime_funcs.get("pith_concat_cstr") {
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

            // Register result ABI call: `rcall flag_reg val_reg fname payload_kind nargs args...`
            // The callee returns two values (is_ok, payload); bind both into the
            // named registers. The callee is a user function declared with the
            // result ABI, or a runtime builtin whose table row returns I64,I64
            // (its two-word result struct). Either way it is a direct call.
            "rcall" if parts.len() >= 6 => {
                let flag_reg = parse_reg(parts[1], line, func_name)?;
                let val_reg = parse_reg(parts[2], line, func_name)?;
                let fname = parts[3];
                let payload_kind = parts[4];
                let nargs: usize = parts[5].parse().map_err(|_| {
                    CompileError::ModuleError(format!(
                        "ir consumer: malformed rcall instruction in {}: {}",
                        func_name, line
                    ))
                })?;
                let mut args: Vec<Value> = Vec::new();
                for j in 0..nargs {
                    args.push(get_reg(&regs, parts[6 + j])?);
                }
                let fid = declared_funcs
                    .get(fname)
                    .or_else(|| runtime_funcs.get(fname))
                    .copied()
                    .ok_or_else(|| {
                        CompileError::ModuleError(format!(
                            "ir consumer: rcall to unknown function {} in {}",
                            fname, func_name
                        ))
                    })?;
                let fref = *func_ref_cache.entry(fid).or_insert_with(|| {
                    codegen.module.declare_func_in_func(fid, builder.func)
                });
                // Bitcast any f64 params to match the callee signature.
                let sig_ref = builder.func.dfg.ext_funcs[fref].signature;
                let param_types: Vec<types::Type> = builder.func.dfg.signatures[sig_ref]
                    .params
                    .iter()
                    .map(|p| p.value_type)
                    .collect();
                for (i, arg) in args.iter_mut().enumerate() {
                    if i < param_types.len() && param_types[i] == types::F64 {
                        *arg = builder.ins().bitcast(
                            types::F64,
                            cranelift::codegen::ir::MemFlags::new(),
                            *arg,
                        );
                    }
                }
                let call = builder.ins().call(fref, &args);
                let results = builder.func.dfg.inst_results(call);
                let &[flag_v, payload_v] = results else {
                    return Err(CompileError::ModuleError(format!(
                        "ir consumer: rcall to {} in {}, which does not return a register pair",
                        fname, func_name
                    )));
                };
                // Normalize the flag (iadd 0 works around a Cranelift register
                // state issue also handled on the normal call path).
                let zero = builder.ins().iconst(types::I64, 0);
                let flag_norm = builder.ins().iadd(flag_v, zero);
                regs.insert(flag_reg, flag_norm);
                // A float ok value is held as an i64 bit pattern like the rest
                // of the register file. A `result_reg_f` function returns it
                // as an f64 and is cast here; a runtime builtin already returns
                // the bits in its second integer word.
                let payload_is_f64 = builder.func.dfg.value_type(payload_v) == types::F64;
                if payload_kind == "float" && !payload_is_f64 {
                    regs.insert(val_reg, payload_v);
                    float_regs.insert(val_reg);
                } else if payload_kind == "float" {
                    let cast = builder.ins().bitcast(
                        types::I64,
                        cranelift::codegen::ir::MemFlags::new(),
                        payload_v,
                    );
                    regs.insert(val_reg, cast);
                    float_regs.insert(val_reg);
                } else {
                    let zero2 = builder.ins().iconst(types::I64, 0);
                    let payload_norm = builder.ins().iadd(payload_v, zero2);
                    regs.insert(val_reg, payload_norm);
                    float_regs.remove(&val_reg);
                }
                // Tag the payload register so downstream field/RC handling knows
                // whether the ok value is a string, bytes, or struct handle.
                struct_regs.remove(&flag_reg);
                string_regs.remove(&flag_reg);
                bytes_regs.remove(&flag_reg);
                float_regs.remove(&flag_reg);
                if payload_kind == "string" {
                    string_regs.insert(val_reg);
                } else {
                    string_regs.remove(&val_reg);
                }
                if payload_kind == "bytes" {
                    bytes_regs.insert(val_reg);
                } else {
                    bytes_regs.remove(&val_reg);
                }
                if let Some(struct_name) = explicit_struct_name_from_retkind(payload_kind) {
                    struct_regs.insert(val_reg, struct_name.to_string());
                } else {
                    struct_regs.remove(&val_reg);
                }
                reg_source_vars.remove(&flag_reg);
                reg_source_vars.remove(&val_reg);
            }

            "call" if parts.len() >= 4 => {
                let reg = parse_reg(parts[1], line, func_name)?;
                let (mut fname, retkind, nargs, arg_start) =
                    parse_call_shape(&parts).ok_or_else(|| {
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
                            args.push(get_reg(&regs, parts[j + arg_start])?);
                        }
                    }
                    // Allocate struct
                    if let Some(&alloc_id) = runtime_funcs.get("pith_struct_alloc") {
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
                    // an indexed read whose Optional tuple never escapes: no
                    // call, no tuple — compute (is_some, value) inline and let
                    // the field reads below pick them up.
                    if fname == "pith_list_get_opt"
                        && nargs == 2
                        && parts.len() > arg_start + 1
                        && inline_opt_regs.contains(&reg)
                    {
                        let list = get_reg(&regs, parts[arg_start])?;
                        let index = get_reg(&regs, parts[arg_start + 1])?;
                        let pair = inline_list_get_opt(&mut builder, list, index);
                        virtual_opts.insert(reg, pair);
                        // deliberately not in regs: a use the pre-scan missed
                        // must fail loudly, not read a stale value.
                        regs.remove(&reg);
                        string_regs.remove(&reg);
                        bytes_regs.remove(&reg);
                        float_regs.remove(&reg);
                        continue;
                    }
                    // a getter's optional that never escapes: call its pair
                    // form and keep (is_some, value) in registers, so no Some
                    // shell is built.
                    if let Some(pair_name) = opt_getter_pair(fname)
                        .filter(|_| inline_opt_regs.contains(&reg))
                        .filter(|_| !declared_funcs.contains_key(fname))
                    {
                        if let Some(&fid) = runtime_funcs.get(pair_name) {
                            let mut args = Vec::with_capacity(nargs);
                            for j in 0..nargs {
                                args.push(get_reg(&regs, parts[arg_start + j])?);
                            }
                            let fref = *func_ref_cache.entry(fid).or_insert_with(|| {
                                codegen.module.declare_func_in_func(fid, builder.func)
                            });
                            let call = builder.ins().call(fref, &args);
                            let &[flag, value] = builder.func.dfg.inst_results(call) else {
                                return Err(CompileError::ModuleError(format!(
                                    "ir consumer: {} does not return a register pair",
                                    pair_name
                                )));
                            };
                            virtual_opts.insert(reg, (flag, value));
                            regs.remove(&reg);
                            string_regs.remove(&reg);
                            bytes_regs.remove(&reg);
                            float_regs.remove(&reg);
                            continue;
                        }
                    }
                    // releasing a virtualized tuple: it was never allocated,
                    // so there is nothing to free.
                    if fname == "pith_struct_release" && nargs == 1 && parts.len() > arg_start {
                        if let Ok(arg_reg) = parts[arg_start].parse::<usize>() {
                            if virtual_opts.contains_key(&arg_reg) {
                                continue;
                            }
                        }
                    }
                    // the tcp_read/tcp_read2 arity overload is now resolved by the
                    // emitter (ir_call_helpers.pith), so the ir names tcp_read2
                    // directly and no rewrite is needed here.
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
                            args.push(get_reg(&regs, parts[j + arg_start])?);
                        }
                    }
                    // the inline fast paths below stand in for a builtin, and
                    // each one `continue`s before the name is resolved. so a
                    // user-defined function -- or a local holding a closure --
                    // that happens to share a builtin's short name would be
                    // bypassed entirely and its arguments handed to the builtin
                    // instead. that is silent: `fn byte_at(r: Reader, i: Int)`
                    // was reading a byte out of the struct's own memory.
                    //
                    // the bit-operation path below already guarded against this;
                    // the rest did not. the user's definition always wins, and
                    // the fast path only applies to the builtin it replaces.
                    let name_is_shadowed =
                        declared_funcs.contains_key(fname) || named_vars.contains_key(fname);

                    // a count call on a string literal or a null handle does
                    // nothing at run time, and the emitter makes many of them
                    // (every owned local starts out null and is released
                    // before its first store), so they are left out.
                    if !name_is_shadowed
                        && args.len() == 1
                        && is_noop_count_call(fname, args[0], &literal_values, builder.func)
                    {
                        let zero = builder.ins().iconst(types::I64, 0);
                        regs.insert(reg, zero);
                        string_regs.remove(&reg);
                        bytes_regs.remove(&reg);
                        float_regs.remove(&reg);
                        struct_regs.remove(&reg);
                        continue;
                    }

                    // a call to a verified bit wrapper (or to a bit builtin
                    // directly) is one native instruction. a local variable
                    // shadowing the name keeps its call, same as below.
                    if args.len() == 2 && !named_vars.contains_key(fname) {
                        let alias_op = bits_aliases.get(fname).copied().or_else(|| {
                            if declared_funcs.contains_key(fname) {
                                None
                            } else {
                                bits_builtin_op(fname)
                            }
                        });
                        if let Some(op) = alias_op {
                            let v = emit_bits_op(&mut builder, op, args[0], args[1]);
                            regs.insert(reg, v);
                            string_regs.remove(&reg);
                            bytes_regs.remove(&reg);
                            float_regs.remove(&reg);
                            struct_regs.remove(&reg);
                            continue;
                        }
                    }
                    if (fname == "pith_list_set_value" || fname == "pith_list_set_value_owned")
                        && args.len() == 3
                    {
                        let fallback = runtime_func_ref(
                            codegen,
                            &mut builder,
                            &mut func_ref_cache,
                            runtime_funcs,
                            fname,
                        )?;
                        inline_list_set_value(
                            &mut builder,
                            args[0],
                            args[1],
                            args[2],
                            fallback,
                        );
                        let zero = builder.ins().iconst(types::I64, 0);
                        regs.insert(reg, zero);
                        string_regs.remove(&reg);
                        bytes_regs.remove(&reg);
                        float_regs.remove(&reg);
                        struct_regs.remove(&reg);
                        continue;
                    }
                    // a field store's write barrier: only a holder that is
                    // already shared makes the call, so test its mark here.
                    // the holder is a struct the emitter is storing into,
                    // never null.
                    if fname == "pith_share_into" && args.len() == 3 && !name_is_shadowed {
                        let fallback = runtime_func_ref(
                            codegen,
                            &mut builder,
                            &mut func_ref_cache,
                            runtime_funcs,
                            "pith_share_into",
                        )?;
                        let word = builder.ins().load(types::I32, MemFlags::new(), args[0], STRUCT_FLAGS_FROM_DATA);
                        let shared = builder.ins().band_imm(word, STRUCT_SHARED_MASK as i64);
                        let call_block = builder.create_block();
                        let done = builder.create_block();
                        builder.set_cold_block(call_block);
                        builder.ins().brif(shared, call_block, &[], done, &[]);
                        builder.switch_to_block(call_block);
                        builder.ins().call(fallback, &[args[0], args[1], args[2]]);
                        builder.ins().jump(done, &[]);
                        builder.switch_to_block(done);
                        let zero = builder.ins().iconst(types::I64, 0);
                        regs.insert(reg, zero);
                        string_regs.remove(&reg);
                        bytes_regs.remove(&reg);
                        float_regs.remove(&reg);
                        struct_regs.remove(&reg);
                        continue;
                    }
                    if fname == "pith_closure_get_env" && args.len() == 2 && !name_is_shadowed {
                        let fallback = runtime_func_ref(
                            codegen,
                            &mut builder,
                            &mut func_ref_cache,
                            runtime_funcs,
                            "pith_closure_get_env",
                        )?;
                        let inlined =
                            inline_closure_get_env(&mut builder, args[0], args[1], fallback);
                        regs.insert(reg, inlined);
                        string_regs.remove(&reg);
                        bytes_regs.remove(&reg);
                        float_regs.remove(&reg);
                        struct_regs.remove(&reg);
                        continue;
                    }
                    if fname == "pith_closure_set_env" && args.len() == 3 && !name_is_shadowed {
                        let fallback = runtime_func_ref(
                            codegen,
                            &mut builder,
                            &mut func_ref_cache,
                            runtime_funcs,
                            "pith_closure_set_env",
                        )?;
                        inline_closure_set_env(&mut builder, args[0], args[1], args[2], fallback);
                        let zero = builder.ins().iconst(types::I64, 0);
                        regs.insert(reg, zero);
                        string_regs.remove(&reg);
                        bytes_regs.remove(&reg);
                        float_regs.remove(&reg);
                        struct_regs.remove(&reg);
                        continue;
                    }
                    if fname == "bytes_get" && args.len() == 2 && !name_is_shadowed {
                        let inlined = inline_bytes_get(&mut builder, args[0], args[1]);
                        regs.insert(reg, inlined);
                        string_regs.remove(&reg);
                        bytes_regs.remove(&reg);
                        float_regs.remove(&reg);
                        struct_regs.remove(&reg);
                        continue;
                    }
                    // `bytes[i]`: the same fast path, with the runtime's
                    // strict getter as the fallback so a failing read still
                    // aborts with its diagnostic.
                    if fname == "bytes_get_strict" && args.len() == 2 && !name_is_shadowed {
                        let fallback = runtime_func_ref(
                            codegen,
                            &mut builder,
                            &mut func_ref_cache,
                            runtime_funcs,
                            "bytes_get_strict",
                        )?;
                        let inlined =
                            inline_bytes_get_strict(&mut builder, args[0], args[1], fallback);
                        regs.insert(reg, inlined);
                        string_regs.remove(&reg);
                        bytes_regs.remove(&reg);
                        float_regs.remove(&reg);
                        struct_regs.remove(&reg);
                        continue;
                    }
                    if fname == "bytes_read_word" && args.len() == 3 && !name_is_shadowed {
                        let fallback = runtime_func_ref(
                            codegen,
                            &mut builder,
                            &mut func_ref_cache,
                            runtime_funcs,
                            "bytes_read_word",
                        )?;
                        let inlined = inline_bytes_read_word(
                            &mut builder,
                            args[0],
                            args[1],
                            args[2],
                            fallback,
                        );
                        regs.insert(reg, inlined);
                        string_regs.remove(&reg);
                        bytes_regs.remove(&reg);
                        float_regs.remove(&reg);
                        struct_regs.remove(&reg);
                        continue;
                    }
                    // string indexing and length: inline the heap-cstring fast
                    // path (header read instead of an FFI call per character),
                    // falling back to the runtime helper for literals. these
                    // names are emitted only for strings (bytes use bytes_get).
                    if (fname == "byte_at" || fname == "pith_cstring_byte_at")
                        && args.len() == 2
                        && !name_is_shadowed
                    {
                        let fallback = runtime_func_ref(
                            codegen,
                            &mut builder,
                            &mut func_ref_cache,
                            runtime_funcs,
                            "byte_at",
                        )?;
                        // the same anchor `s.len()` uses, so the two share
                        // one header check
                        let anchor = runtime_func_ref(
                            codegen,
                            &mut builder,
                            &mut func_ref_cache,
                            runtime_funcs,
                            "string_len",
                        )?;
                        let inlined =
                            inline_cstring_byte_at(&mut builder, args[0], args[1], fallback, anchor);
                        regs.insert(reg, inlined);
                        string_regs.remove(&reg);
                        bytes_regs.remove(&reg);
                        float_regs.remove(&reg);
                        struct_regs.remove(&reg);
                        continue;
                    }
                    if (fname == "bytes_len" || fname == "pith_list_len")
                        && args.len() == 1
                        && !name_is_shadowed
                    {
                        let fallback = runtime_func_ref(
                            codegen,
                            &mut builder,
                            &mut func_ref_cache,
                            runtime_funcs,
                            fname,
                        )?;
                        let inlined = if fname == "bytes_len" {
                            inline_handle_len(
                                &mut builder,
                                args[0],
                                BYTES_HANDLE_ALIGN,
                                BYTES_MAGIC_OFFSET,
                                BYTES_MAGIC,
                                None,
                                BYTES_DATA_LEN_OFFSET,
                                fallback,
                            )
                        } else {
                            inline_handle_len(
                                &mut builder,
                                args[0],
                                LIST_HANDLE_ALIGN,
                                0,
                                LIST_MAGIC,
                                Some(LIST_IMPL_ELEM_SIZE_OFFSET),
                                LIST_IMPL_VALUES8_LEN_OFFSET,
                                fallback,
                            )
                        };
                        regs.insert(reg, inlined);
                        string_regs.remove(&reg);
                        bytes_regs.remove(&reg);
                        float_regs.remove(&reg);
                        struct_regs.remove(&reg);
                        continue;
                    }
                    if fname == "string_len" && args.len() == 1 {
                        let fallback = runtime_func_ref(
                            codegen,
                            &mut builder,
                            &mut func_ref_cache,
                            runtime_funcs,
                            "string_len",
                        )?;
                        let inlined = inline_cstring_len(&mut builder, args[0], fallback);
                        regs.insert(reg, inlined);
                        string_regs.remove(&reg);
                        bytes_regs.remove(&reg);
                        float_regs.remove(&reg);
                        struct_regs.remove(&reg);
                        continue;
                    }
                    // `xs[i]`: inline the in-bounds read and leave every
                    // failure to the runtime's strict getter. the register
                    // bookkeeping below is the generic call path's, keyed on
                    // the retkind, since the element can be of any kind.
                    if fname == "pith_list_get_value_strict"
                        && args.len() == 2
                        && !name_is_shadowed
                        && retkind != "result_int"
                        && retkind != "result_bool"
                    {
                        let fallback = runtime_func_ref(
                            codegen,
                            &mut builder,
                            &mut func_ref_cache,
                            runtime_funcs,
                            "pith_list_get_value_strict",
                        )?;
                        let inlined =
                            inline_list_get_value_strict(&mut builder, args[0], args[1], fallback);
                        regs.insert(reg, inlined);
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
                        } else {
                            struct_regs.remove(&reg);
                        }
                        continue;
                    }
                    if (fname == "pith_list_get_value" || fname == "pith_list_get_value_unchecked")
                        && args.len() == 2
                    {
                        let inlined = inline_list_get_value(
                            &mut builder,
                            args[0],
                            args[1],
                            fname == "pith_list_get_value",
                        );
                        regs.insert(reg, inlined);
                        string_regs.remove(&reg);
                        bytes_regs.remove(&reg);
                        float_regs.remove(&reg);
                        if let Some(struct_name) = explicit_struct_name_from_retkind(retkind) {
                            struct_regs.insert(reg, struct_name.to_string());
                        } else {
                            struct_regs.remove(&reg);
                        }
                        continue;
                    }
    // Look up function: user-defined first, then a direct runtime
                    // import key. A local variable shadows both — the checker
                    // guarantees a called local is closure-typed, and a bare
                    // runtime name like `second` must not capture a call to a
                    // local that happens to share it.
                    let mut runtime_call = false;
                    let fid = if named_vars.contains_key(fname) {
                        None
                    } else if let Some(&fid) = declared_funcs.get(fname) {
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
                        // a callee that returns the register pair is reached
                        // only through rcall: a plain call would read the flag
                        // as the value and drop the payload, an owned error
                        // string included.
                        if sig.returns.len() > 1 {
                            return Err(CompileError::ModuleError(format!(
                                "ir consumer: call to '{}' in {} must be an rcall: it returns a register pair",
                                fname, func_name
                            )));
                        }
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
                        // a release of a handle that is still zero, such as
                        // the cleanup of a local an early return never
                        // assigned, is a no-op in the runtime. the inline
                        // test skips the call for it.
                        let guard_null = runtime_call
                            && typed_args.len() == 1
                            && is_null_safe_release(fname);
                        let done_block = if guard_null {
                            let call_block = builder.create_block();
                            let done_block = builder.create_block();
                            builder.ins().brif(typed_args[0], call_block, &[], done_block, &[]);
                            builder.switch_to_block(call_block);
                            builder.seal_block(call_block);
                            Some(done_block)
                        } else {
                            None
                        };
                        let call = builder.ins().call(fref, &typed_args);
                        if let Some(done_block) = done_block {
                            builder.ins().jump(done_block, &[]);
                            builder.switch_to_block(done_block);
                            builder.seal_block(done_block);
                        }
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
                            // the callee returns nothing. the emitter and the
                            // runtime table are separate notions of what a call
                            // yields, so a retkind that promises a value here
                            // means they disagree — `process.wait(p)` once went
                            // out as a bare `wait`, landed on the waitgroup
                            // intrinsic, and read back a zero nobody wrote.
                            if retkind_expects_a_value(retkind) {
                                return Err(CompileError::ModuleError(format!(
                                    "ir consumer: call to '{}' in {} asks for a {} result, but the function returns nothing",
                                    fname, func_name, retkind
                                )));
                            }
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
                        // Indirect call through closure handle variable
                        let closure_handle = builder.use_var(var);
                        let fn_ptr = closure_fn_ptr(
                            codegen,
                            &mut builder,
                            &mut func_ref_cache,
                            runtime_funcs,
                            closure_handle,
                        );
                        let mut sig = codegen.module.make_signature();
                        sig.params.push(AbiParam::new(types::I64));
                        for _ in &args {
                            sig.params.push(AbiParam::new(types::I64));
                        }
                        sig.returns.push(AbiParam::new(types::I64));
                        let sig_ref = builder.import_signature(sig);
                        let mut indirect_args = vec![closure_handle];
                        indirect_args.extend(args.iter().copied());
                        let call = builder.ins().call_indirect(sig_ref, fn_ptr, &indirect_args);
                        regs.insert(reg, builder.func.dfg.first_result(call));
                        struct_regs.remove(&reg);
                        string_regs.remove(&reg);
                        bytes_regs.remove(&reg);
                        float_regs.remove(&reg);
                    } else if let Some(&data_id) = global_data.get(fname) {
                        // a module-level global holding a closure value,
                        // called by name. the checker only allows calling a
                        // global that is closure-typed, so load the handle
                        // and dispatch indirectly — the same path a local
                        // closure variable takes, sourced from the global
                        // slot instead of a named variable.
                        let gv = codegen.module.declare_data_in_func(data_id, builder.func);
                        let addr = builder.ins().global_value(types::I64, gv);
                        let closure_handle = builder.ins().load(
                            types::I64,
                            cranelift::codegen::ir::MemFlags::new(),
                            addr,
                            0,
                        );
                        let fn_ptr = closure_fn_ptr(
                            codegen,
                            &mut builder,
                            &mut func_ref_cache,
                            runtime_funcs,
                            closure_handle,
                        );
                        let mut sig = codegen.module.make_signature();
                        sig.params.push(AbiParam::new(types::I64));
                        for _ in &args {
                            sig.params.push(AbiParam::new(types::I64));
                        }
                        sig.returns.push(AbiParam::new(types::I64));
                        let sig_ref = builder.import_signature(sig);
                        let mut indirect_args = vec![closure_handle];
                        indirect_args.extend(args.iter().copied());
                        let call = builder.ins().call_indirect(sig_ref, fn_ptr, &indirect_args);
                        regs.insert(reg, builder.func.dfg.first_result(call));
                        struct_regs.remove(&reg);
                        string_regs.remove(&reg);
                        bytes_regs.remove(&reg);
                        float_regs.remove(&reg);
                    } else if runtime_funcs.contains_key("pith_runtime_error") {
                        // a call that resolves to nothing is a compiler bug
                        // upstream (a phantom import, a missed rename, an
                        // unlowered interface dispatch). generic template
                        // bodies keep dead unresolved calls, so this cannot
                        // be a compile error yet — but a live path must fail
                        // loudly instead of silently returning zero.
                        let result = emit_runtime_error_value(
                            codegen,
                            &mut builder,
                            &mut func_ref_cache,
                            runtime_funcs,
                            5,
                        )?;
                        regs.insert(reg, result);
                        struct_regs.remove(&reg);
                        string_regs.remove(&reg);
                        bytes_regs.remove(&reg);
                        float_regs.remove(&reg);
                    } else {
                        // no runtime registered (unit-test harness): keep the
                        // strict answer.
                        return Err(CompileError::ModuleError(format!(
                            "ir consumer: call to unknown function '{}' in {}",
                            fname, func_name
                        )));
                    }
                } // end struct constructor else
            }

            "store" if parts.len() >= 3 => {
                let name = parts[1].to_string();
                let val = get_reg(&regs, parts[2])?;
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
                        #[cfg(pith_cranelift_new_api)]
                        let v = declare_i64_var(&mut builder);
                        #[cfg(not(pith_cranelift_new_api))]
                        let v = declare_i64_var(&mut builder, &mut next_var_id);
                        named_vars.insert(name, v);
                        v
                    };
                    builder.def_var(var, val);
                }
            }

            "load" if parts.len() >= 3 => {
                let reg = parse_reg(parts[1], line, func_name)?;
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

            // `optflag R OPT`: 1 when the optional OPT holds a value. none is
            // the null pointer, so this is a compare, not a load.
            "optflag" if parts.len() == 3 => {
                let reg = parse_reg(parts[1], line, func_name)?;
                let obj_reg = parse_reg(parts[2], line, func_name)?;
                let v = if let Some(&(flag, _)) = virtual_opts.get(&obj_reg) {
                    flag
                } else {
                    let obj = get_reg(&regs, parts[2])?;
                    let is_some = builder.ins().icmp_imm(IntCC::NotEqual, obj, 0);
                    builder.ins().uextend(types::I64, is_some)
                };
                regs.insert(reg, v);
                reg_source_vars.remove(&reg);
                note_loaded_kind(
                    reg,
                    None,
                    &mut string_regs,
                    &mut bytes_regs,
                    &mut float_regs,
                    &mut struct_regs,
                );
            }

            // `optval R OPT KIND NAME`: the payload of the optional OPT, or 0
            // when OPT is none (the null pointer), so an unchecked read of a
            // none still yields 0 instead of faulting.
            "optval" if parts.len() == 5 => {
                let reg = parse_reg(parts[1], line, func_name)?;
                let obj_reg = parse_reg(parts[2], line, func_name)?;
                let v = if let Some(&(_, value)) = virtual_opts.get(&obj_reg) {
                    value
                } else {
                    let obj = get_reg(&regs, parts[2])?;
                    let zero = builder.ins().iconst(types::I64, 0);
                    let load_block = builder.create_block();
                    let none_block = builder.create_block();
                    let done = builder.create_block();
                    builder.append_block_param(done, types::I64);
                    builder.ins().brif(obj, load_block, &[], none_block, &[]);
                    builder.switch_to_block(none_block);
                    jump_with_i64_arg(&mut builder, done, zero);
                    builder.switch_to_block(load_block);
                    let payload = builder.ins().load(types::I64, MemFlags::new(), obj, 8);
                    jump_with_i64_arg(&mut builder, done, payload);
                    builder.switch_to_block(done);
                    builder.block_params(done)[0]
                };
                regs.insert(reg, v);
                reg_source_vars.remove(&reg);
                note_loaded_kind(
                    reg,
                    Some(parts[3]),
                    &mut string_regs,
                    &mut bytes_regs,
                    &mut float_regs,
                    &mut struct_regs,
                );
            }

            "field" if parts.len() >= 4 => {
                let reg = parse_reg(parts[1], line, func_name)?;
                // unpacking a virtualized pith_list_get_opt result: slot 0 is
                // the is_some flag, slot 8 the element value, both already in
                // registers — no tuple to load from.
                if let Ok(obj_reg) = parts[2].parse::<usize>() {
                    if let Some(&(flag, value)) = virtual_opts.get(&obj_reg) {
                        let v = match parts[3] {
                            "0" => flag,
                            "8" => value,
                            _ => {
                                return Err(CompileError::ModuleError(format!(
                                    "ir consumer: unexpected field offset on inlined \
                                     list get in {}: {}",
                                    func_name, line
                                )))
                            }
                        };
                        regs.insert(reg, v);
                        reg_source_vars.remove(&reg);
                        note_loaded_kind(
                            reg,
                            parts.get(4).copied().filter(|_| parts.len() >= 6),
                            &mut string_regs,
                            &mut bytes_regs,
                            &mut float_regs,
                            &mut struct_regs,
                        );
                        continue;
                    }
                }
                let obj = get_reg(&regs, parts[2])?;
                let (offset, field_retkind) = if parts.len() >= 6 && parts[3].parse::<i32>().is_ok()
                {
                    (parts[3].parse::<i32>().unwrap_or(0), Some(parts[4]))
                } else if parts.len() == 4 {
                    let field_name = parts[3];
                    if let Ok(idx) = field_name.parse::<usize>() {
                        ((idx * 8) as i32, None)
                    } else {
                        return Err(CompileError::ModuleError(format!(
                            "ir consumer: field instruction requires an explicit offset in {}: {}",
                            func_name, line
                        )));
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
                note_loaded_kind(
                    reg,
                    field_retkind,
                    &mut string_regs,
                    &mut bytes_regs,
                    &mut float_regs,
                    &mut struct_regs,
                );
            }

            "funcref" if parts.len() >= 3 => {
                let reg = parse_reg(parts[1], line, func_name)?;
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

            "closure_ref" if parts.len() >= 3 => {
                let reg = parse_reg(parts[1], line, func_name)?;
                let fname = parts[2];
                if let Some(&fid) = declared_funcs.get(fname) {
                    let fref = *func_ref_cache
                        .entry(fid)
                        .or_insert_with(|| codegen.module.declare_func_in_func(fid, builder.func));
                    let addr = builder.ins().func_addr(types::I64, fref);
                    if let Some(&closure_new_id) = runtime_funcs.get("pith_closure_new") {
                        let closure_new_ref =
                            *func_ref_cache.entry(closure_new_id).or_insert_with(|| {
                                codegen
                                    .module
                                    .declare_func_in_func(closure_new_id, builder.func)
                            });
                        let call = builder.ins().call(closure_new_ref, &[addr]);
                        regs.insert(reg, builder.func.dfg.first_result(call));
                    } else {
                        regs.insert(reg, addr);
                    }
                } else {
                    return Err(CompileError::ModuleError(format!(
                        "ir consumer: unknown closure reference '{}' in {}",
                        fname, func_name
                    )));
                }
                reg_source_vars.remove(&reg);
                struct_regs.remove(&reg);
                string_regs.remove(&reg);
                bytes_regs.remove(&reg);
                float_regs.remove(&reg);
            }

            // a closure the emitter proved never outlives this frame: the same
            // object closure_ref builds, written into a stack slot of its own
            // instead of the heap. each op gets its own slot, so a loop that
            // rebuilds one writes over the previous, dead, copy. the header is
            // written here rather than by a runtime call; see CLOSURE_SIZE.
            "closure_stack" if parts.len() >= 3 => {
                let reg = parse_reg(parts[1], line, func_name)?;
                let fname = parts[2];
                let Some(&fid) = declared_funcs.get(fname) else {
                    return Err(CompileError::ModuleError(format!(
                        "ir consumer: unknown closure reference '{}' in {}",
                        fname, func_name
                    )));
                };
                let fref = *func_ref_cache
                    .entry(fid)
                    .or_insert_with(|| codegen.module.declare_func_in_func(fid, builder.func));
                let addr = builder.ins().func_addr(types::I64, fref);
                let slot = builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    CLOSURE_SIZE,
                    3,
                ));
                let at = builder.ins().stack_addr(types::I64, slot, 0);
                let flags = MemFlags::trusted();
                let magic = builder.ins().iconst(types::I32, CLOSURE_MAGIC as i64);
                builder.ins().store(flags, magic, at, CLOSURE_MAGIC_OFFSET);
                builder.ins().store(flags, addr, at, CLOSURE_FUNC_PTR_OFFSET);
                let one = builder.ins().iconst(types::I64, 1);
                builder.ins().store(flags, one, at, CLOSURE_REF_COUNT_OFFSET);
                let zero = builder.ins().iconst(types::I64, 0);
                builder.ins().store(flags, zero, at, CLOSURE_ENV_TAGS_OFFSET);
                builder.ins().store(flags, zero, at, CLOSURE_ENV_TAGS_OFFSET + 8);
                let zero8 = builder.ins().iconst(types::I8, 0);
                builder.ins().store(flags, zero8, at, CLOSURE_CYCLE_FLAGS_OFFSET);
                builder.ins().store(flags, zero, at, CLOSURE_OVERFLOW_OFFSET);
                regs.insert(reg, at);
                reg_source_vars.remove(&reg);
                struct_regs.remove(&reg);
                string_regs.remove(&reg);
                bytes_regs.remove(&reg);
                float_regs.remove(&reg);
            }

            // a result box the emitter proved only its own function reads
            // (ir_stack_tuple_binds), built in a stack slot of this frame with
            // the header pith_struct_alloc would write. nothing releases it;
            // the function's cleanup drops the payload it holds.
            "tuple_stack" if parts.len() >= 3 => {
                let reg = parse_reg(parts[1], line, func_name)?;
                let fields: u32 = parts[2].parse().map_err(|_| {
                    CompileError::ModuleError(format!(
                        "ir consumer: bad field count in '{}' in {}",
                        line, func_name
                    ))
                })?;
                let header = STRUCT_HEADER as u32;
                let slot = builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    header + fields * 8,
                    3,
                ));
                let base = builder.ins().stack_addr(types::I64, slot, 0);
                let flags = MemFlags::trusted();
                let magic = builder.ins().iconst(types::I32, STRUCT_MAGIC as i64);
                builder.ins().store(flags, magic, base, 0);
                let one = builder.ins().iconst(types::I32, 1);
                builder.ins().store(flags, one, base, STRUCT_STRONG_OFFSET);
                builder.ins().store(flags, one, base, STRUCT_WEAK_OFFSET);
                let zero32 = builder.ins().iconst(types::I32, 0);
                builder.ins().store(flags, zero32, base, STRUCT_DEAD_OFFSET);
                let size = builder.ins().iconst(types::I64, (fields * 8) as i64);
                builder.ins().store(flags, size, base, STRUCT_SIZE_OFFSET);
                let zero = builder.ins().iconst(types::I64, 0);
                builder.ins().store(flags, zero, base, STRUCT_DTOR_OFFSET);
                let at = builder.ins().iadd_imm(base, header as i64);
                regs.insert(reg, at);
                reg_source_vars.remove(&reg);
                struct_regs.remove(&reg);
                string_regs.remove(&reg);
                bytes_regs.remove(&reg);
                float_regs.remove(&reg);
            }

            "sstore" if parts.len() >= 4 => {
                // Store field in struct: sstore struct_reg field_idx value_reg
                let struct_val = get_reg(&regs, parts[1])?;
                let field_idx: i32 = parts[2].parse().map_err(|_| {
                    CompileError::ModuleError(format!(
                        "ir consumer: invalid struct field index in {}: {}",
                        func_name, line
                    ))
                })?;
                let val = get_reg(&regs, parts[3])?;
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
                    let val = get_reg(&regs, parts[1])?;
                    builder.ins().return_(&[val]);
                }
                terminated = true;
            }

            // Register result ABI return: `ret2 flag payload`. The function's
            // signature has two returns (is_ok, payload); the payload is the ok
            // value on success or the error-message pointer on failure.
            "ret2" if parts.len() >= 3 => {
                let flag = get_reg(&regs, parts[1])?;
                let mut payload = get_reg(&regs, parts[2])?;
                if func_retkind == "result_reg_f" {
                    // the payload reg is held as an i64 bit pattern; the second
                    // return is declared f64, so reinterpret before returning.
                    payload = builder.ins().bitcast(
                        types::F64,
                        cranelift::codegen::ir::MemFlags::new(),
                        payload,
                    );
                }
                builder.ins().return_(&[flag, payload]);
                terminated = true;
            }

            "brif" if parts.len() >= 4 => {
                let cond = get_reg(&regs, parts[1])?;
                let then_label = parts[2];
                let else_label = parts[3];
                // a brif with a back-edge arm closes a loop (e.g. a bottom-test
                // `while cond` that branches back to the header). insert the
                // preemption safe-point before the branch itself (only when this
                // build opted into preemption — otherwise `preempt_flag` is None).
                if let Some(flag) = preempt_flag {
                    if defined_labels.contains(then_label) || defined_labels.contains(else_label) {
                        emit_preempt_safepoint(
                            codegen,
                            &mut builder,
                            &mut func_ref_cache,
                            runtime_funcs,
                            flag,
                        )?;
                    }
                }
                let then_block = get_label(&labels, then_label, func_name)?;
                let else_block = get_label(&labels, else_label, func_name)?;
                // cranelift lays out a branch's last successor right after the
                // branch. a loop's condition branches to the body or to the loop
                // exit, so test the inverse and name the body last: it falls
                // through from the condition, and only the back-edge is taken.
                // other branches keep their order, which places the then arm
                // out of line.
                if loop_exits.contains(else_label) {
                    let cond_bool = builder.ins().icmp_imm(IntCC::Equal, cond, 0);
                    builder
                        .ins()
                        .brif(cond_bool, else_block, &[], then_block, &[]);
                } else {
                    let cond_bool = builder.ins().icmp_imm(IntCC::NotEqual, cond, 0);
                    builder
                        .ins()
                        .brif(cond_bool, then_block, &[], else_block, &[]);
                }
                terminated = true;
            }

            "jmp" if parts.len() >= 2 => {
                let target = parts[1];
                // Redirect break targets that incorrectly loop back
                let actual_target = break_redirects
                    .get(target)
                    .map(|s| s.as_str())
                    .unwrap_or(target);
                // a jump back to an already-defined label is a loop back-edge
                // (`while`/`for`/`loop` tail, or `continue`). insert the
                // preemption safe-point before the jump (only when this build
                // opted into preemption). forward jumps (if/else merges, `break`)
                // target not-yet-defined labels and are skipped either way.
                if let Some(flag) = preempt_flag {
                    if defined_labels.contains(actual_target) {
                        emit_preempt_safepoint(
                            codegen,
                            &mut builder,
                            &mut func_ref_cache,
                            runtime_funcs,
                            flag,
                        )?;
                    }
                }
                let block = get_label(&labels, actual_target, func_name)?;
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

            _ if parts[0].starts_with('#') || parts[0].starts_with("//") => {}

            _ => {
                return Err(CompileError::ModuleError(format!(
                    "ir consumer: unknown or malformed instruction in {}: {}",
                    func_name, line
                )));
            }
        }
    }

    // Default return if not terminated. Match the function's return arity so a
    // result-abi function that falls through still type-checks (is_ok=0 = error).
    if !terminated {
        let zero = builder.ins().iconst(types::I64, 0);
        match result_abi_payload_type(func_retkind) {
            Some(types::F64) => {
                let fzero = builder.ins().f64const(0.0);
                builder.ins().return_(&[zero, fzero]);
            }
            Some(_) => {
                let zero2 = builder.ins().iconst(types::I64, 0);
                builder.ins().return_(&[zero, zero2]);
            }
            None => {
                builder.ins().return_(&[zero]);
            }
        }
    }

    builder.seal_all_blocks();
    builder.finalize();

    let _ = func_id;
    Ok(ctx.func)
}

/// Define one built function, naming the failing instruction when the
/// verifier rejects it.
fn define_built_function(
    codegen: &mut CodeGen,
    func_id: FuncId,
    func_name: &str,
    ctx: &mut cranelift::codegen::Context,
) -> Result<(), CompileError> {
    codegen
        .module
        .define_function(func_id, ctx)
        .map_err(|e| {
            // the wrapped Display collapses verifier failures to the
            // string "Verifier errors"; pull the actual list out so the
            // failing instruction is named
            let detail = match &e {
                cranelift_module::ModuleError::Compilation(
                    cranelift::codegen::CodegenError::Verifier(errors),
                ) => errors
                    .0
                    .iter()
                    .map(|err| format!("  {}", err))
                    .collect::<Vec<_>>()
                    .join("\n"),
                other => format!("  {}", other),
            };
            eprintln!(
                "IR consumer verifier error in '{}':\n{}\nIR:\n{}",
                func_name,
                detail,
                ctx.func.display()
            );
            CompileError::ModuleError(format!("IR consumer: {}\n{}", e, detail))
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

fn parse_reg(s: &str, instruction: &str, func_name: &str) -> Result<usize, CompileError> {
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
fn parse_i64_operand(
    s: &str,
    what: &str,
    instruction: &str,
    func_name: &str,
) -> Result<i64, CompileError> {
    let parsed = if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        i64::from_str_radix(hex, 16)
    } else if let Some(bin) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
        i64::from_str_radix(bin, 2)
    } else if let Some(oct) = s.strip_prefix("0o").or_else(|| s.strip_prefix("0O")) {
        i64::from_str_radix(oct, 8)
    } else {
        s.parse::<i64>()
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
fn retkind_expects_a_value(retkind: &str) -> bool {
    retkind != "void" && retkind != "unknown"
}

fn explicit_struct_name_from_retkind(retkind: &str) -> Option<&str> {
    if let Some(name) = retkind.strip_prefix("struct:") {
        return Some(name);
    }
    None
}

// record what a loaded register holds, from the kind token a field-style
// instruction carries; None (no token) clears every kind.
fn note_loaded_kind(
    reg: usize,
    retkind: Option<&str>,
    string_regs: &mut HashSet<usize>,
    bytes_regs: &mut HashSet<usize>,
    float_regs: &mut HashSet<usize>,
    struct_regs: &mut HashMap<usize, String>,
) {
    let kind = retkind.unwrap_or("");
    for (set, wanted) in [
        (string_regs, "string"),
        (bytes_regs, "bytes"),
        (float_regs, "float"),
    ] {
        if kind == wanted {
            set.insert(reg);
        } else {
            set.remove(&reg);
        }
    }
    match explicit_struct_name_from_retkind(kind) {
        Some(name) => {
            struct_regs.insert(reg, name.to_string());
        }
        None => {
            struct_regs.remove(&reg);
        }
    }
}

fn get_reg(regs: &HashMap<usize, Value>, s: &str) -> Result<Value, CompileError> {
    let reg = s.parse::<usize>().map_err(|_| {
        CompileError::ModuleError(format!("IR consumer: invalid register reference '{}'", s))
    })?;
    regs.get(&reg)
        .copied()
        .ok_or_else(|| CompileError::ModuleError(format!("IR consumer: missing register {}", reg)))
}

fn get_label(
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fuse(body: &str) -> Option<Vec<String>> {
        let lines: Vec<&str> = body.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        fuse_concat_chains(&lines)
    }

    /// `chr(123) + q + "id" + q`, as the emitter lowers it.
    const CHAIN: &str = "
        iconst 25 123
        call 26 chr string 1 25
        load 27 q
        concat 28 26 27
        call 29 pith_cstring_release void 1 26
        strref 30 m0s1
        concat 31 28 30
        call 32 pith_cstring_release void 1 28
        call 33 pith_cstring_release void 1 30
        load 34 q
        concat 35 31 34
        call 36 pith_cstring_release void 1 31
        store body 35
    ";

    fn appends(body: &str) -> Option<Vec<String>> {
        let lines: Vec<&str> = body.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        fuse_self_appends(&lines)
    }

    /// `s = s + "ab"`, as the emitter lowers it.
    const SELF_APPEND: &str = "
        load 14 s
        load 15 s
        strref 16 m0s0
        concat 17 15 16
        call 18 pith_cstring_release void 1 16
        call 24 pith_cstring_release void 1 14
        store s 17
    ";

    #[test]
    fn a_self_append_takes_the_locals_count() {
        let rewritten = appends(SELF_APPEND).expect("the append is rewritten");
        assert!(rewritten.contains(&"appendn 17 1 15 16".to_string()));
        assert!(!rewritten.iter().any(|l| l == "call 24 pith_cstring_release void 1 14"));
        let fused = appends(&SELF_APPEND.replace("concat 17 15 16", "concatn 17 3 15 16 20"))
            .expect("a fused chain is rewritten too");
        assert!(fused.contains(&"appendn 17 2 15 16 20".to_string()));
    }

    #[test]
    fn a_self_append_that_is_not_plain_keeps_its_concat() {
        // a global may be read by another task at the same time
        assert!(appends(&SELF_APPEND.replace(" s", " __g_s")).is_none());
        // another read of the local, a branch, or another use of the result
        assert!(appends(&SELF_APPEND.replace("strref 16 m0s0", "load 16 s")).is_none());
        assert!(appends(&SELF_APPEND.replace("strref 16 m0s0", "strref 16 m0s0\nlabel L1")).is_none());
        assert!(appends(&SELF_APPEND.replace("store s 17", "store s 17\nstore t 17")).is_none());
        // no release of the old value: the local does not own it
        assert!(appends(&SELF_APPEND.replace("call 24 pith_cstring_release void 1 14\n", "")).is_none());
    }

    #[test]
    fn a_concat_chain_becomes_one_concatn() {
        let fused = fuse(CHAIN).expect("the chain is fused");
        assert_eq!(
            fused,
            vec![
                "iconst 25 123",
                "call 26 chr string 1 25",
                "load 27 q",
                "strref 30 m0s1",
                "load 34 q",
                "concatn 35 4 26 27 30 34",
                "call 29 pith_cstring_release void 1 26",
                "call 33 pith_cstring_release void 1 30",
                "store body 35",
            ]
        );
    }

    #[test]
    fn an_intermediate_read_twice_ends_the_chain() {
        // 28 is stored as well as extended, so it has to exist; the chain
        // starts at it instead.
        let body = CHAIN.replace("store body 35", "store body 35\nstore head 28");
        let fused = fuse(&body).expect("the rest of the chain is fused");
        assert!(fused.contains(&"concat 28 26 27".to_string()));
        assert!(fused.contains(&"concatn 35 3 28 30 34".to_string()));
        assert!(fused.contains(&"call 32 pith_cstring_release void 1 28".to_string()));
    }

    #[test]
    fn a_branch_inside_a_chain_ends_it() {
        let body = CHAIN.replace("load 34 q", "label L1\nload 34 q");
        let fused = fuse(&body).expect("the part before the label is fused");
        assert!(fused.contains(&"concatn 31 3 26 27 30".to_string()));
        assert!(fused.contains(&"concat 35 31 34".to_string()));
    }

    #[test]
    fn a_body_without_a_chain_is_left_alone() {
        assert!(fuse("load 1 a\nload 2 b\nconcat 3 1 2\nret 3").is_none());
        assert!(fuse("iconst 1 2\nret 1").is_none());
    }

    #[test]
    fn a_part_shared_by_two_chains_keeps_both_unfused() {
        // 2 is a part of both chains, and its release sits inside the second.
        let body = "
            load 1 a
            load 2 b
            load 3 c
            concat 4 1 2
            concat 5 4 3
            call 6 pith_cstring_release void 1 4
            concat 7 2 3
            call 8 pith_cstring_release void 1 2
            concat 9 7 1
            call 10 pith_cstring_release void 1 7
            ret 9
        ";
        assert!(fuse(body).is_none());
    }

    #[test]
    fn safe_points_are_on_unless_the_build_asks_them_off() {
        // unset, and anything that is not an off spelling, emits the check.
        assert!(preempt_from_env(None));
        assert!(preempt_from_env(Some("1")));
        assert!(preempt_from_env(Some("")));
        assert!(preempt_from_env(Some("please")));

        // the opt-out is as generous as PITH_GREEN's, for the same reason: a
        // build reaching for it must not silently keep the safe-points.
        for off in ["0", "off", "false", "no", "n", " OFF ", "False"] {
            assert!(!preempt_from_env(Some(off)), "{off} should turn them off");
        }
    }

    #[test]
    fn bits_wrapper_detection_matches_only_the_exact_shape() {
        let params = vec!["left".to_string(), "right".to_string()];
        let wrapper = vec![
            "load 2 left",
            "load 3 right",
            "call 4 bit_and int 2 2 3",
            "ret 4",
            "iconst 5 0",
            "ret 5",
        ];
        assert_eq!(detect_bits_wrapper(&params, &wrapper), Some("band"));

        // swapped operands are a different function; it must keep its call
        let swapped = vec![
            "load 2 left",
            "load 3 right",
            "call 4 bit_and int 2 3 2",
            "ret 4",
        ];
        assert_eq!(detect_bits_wrapper(&params, &swapped), None);

        // extra work after the return means the body is not just the builtin
        let with_extra = vec![
            "load 2 left",
            "load 3 right",
            "call 4 bit_and int 2 2 3",
            "ret 4",
            "call 6 print void 1 4",
        ];
        assert_eq!(detect_bits_wrapper(&params, &with_extra), None);

        // a non-bit builtin never aliases
        let other_call = vec![
            "load 2 left",
            "load 3 right",
            "call 4 map_get int 2 2 3",
            "ret 4",
        ];
        assert_eq!(detect_bits_wrapper(&params, &other_call), None);
    }

    #[test]
    fn list_get_opt_scan_accepts_the_unpack_pattern_and_nothing_else() {
        // the emitter's indexed-read shape: call, flag read, value read,
        // release on both arms
        let body = vec![
            "call 10 pith_list_get_opt tuple 2 4 5",
            "field 11 10 0 bool is_some",
            "brif 11 L1 L2",
            "label L1",
            "field 12 10 8 int value",
            "call 13 pith_struct_release void 1 10",
            "jmp L3",
            "label L2",
            "call 14 pith_struct_release void 1 10",
            "ret 0",
            "label L3",
        ];
        let regs = scan_unpackable_opt_regs(&body);
        assert!(regs.contains(&10));

        // an escaping optional (stored into a variable) must keep the call
        let escaping = vec![
            "call 10 pith_list_get_opt tuple 2 4 5",
            "store opt 10",
            "field 11 10 0 bool is_some",
        ];
        assert!(scan_unpackable_opt_regs(&escaping).is_empty());

        // passing the tuple to any other call disqualifies it too
        let passed = vec![
            "call 10 pith_list_get_opt tuple 2 4 5",
            "field 11 10 0 bool is_some",
            "call 12 some_fn int 1 10",
        ];
        assert!(scan_unpackable_opt_regs(&passed).is_empty());
    }

    #[test]
    fn a_pair_getter_unpacked_in_place_needs_no_shell() {
        let body = vec![
            "call 6 pith_channel_recv tuple 1 5",
            "optflag 8 6",
            "brif 8 L1 L2",
            "label L1",
            "optval 10 6 int value",
            "call 11 pith_struct_release void 1 6",
            "label L2",
        ];
        assert!(scan_unpackable_opt_regs(&body).contains(&6));

        // a destructor attached to the shell means it owns the payload
        let owned = vec![
            "call 6 map_get_opt tuple 2 4 5",
            "funcref 7 __opt_dtor_string",
            "call 8 pith_struct_set_dtor void 2 6 7",
            "optflag 9 6",
        ];
        assert!(scan_unpackable_opt_regs(&owned).is_empty());

        // a getter with no pair form keeps its call
        let other = vec!["call 6 env_opt tuple 1 5", "optflag 7 6"];
        assert!(scan_unpackable_opt_regs(&other).is_empty());
    }

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
        assert_eq!(
            explicit_struct_name_from_retkind("struct:Token"),
            Some("Token")
        );
        assert_eq!(explicit_struct_name_from_retkind("Token"), None);
        assert_eq!(explicit_struct_name_from_retkind("string"), None);
        assert_eq!(explicit_struct_name_from_retkind("unknown"), None);
    }

    fn compile_err_for_ir(ir: &str) -> String {
        let mut codegen = crate::create_codegen().expect("create codegen");
        let runtime_funcs = HashMap::new();
        let result = compile_from_ir(&mut codegen, ir, &runtime_funcs);
        assert!(result.is_err(), "expected malformed IR to fail");
        result.err().expect("compile error").to_string()
    }

    fn compile_ok_for_ir(ir: &str) {
        let mut codegen = crate::create_codegen().expect("create codegen");
        let runtime_funcs = HashMap::new();
        let result = compile_from_ir(&mut codegen, ir, &runtime_funcs);
        assert!(result.is_ok(), "expected IR to compile, got {:?}", result.err());
    }

    // the same, with the runtime table declared, for ir that calls builtins.
    fn compile_with_runtime_for_ir(ir: &str) -> Result<(), String> {
        let mut codegen = crate::create_codegen().expect("create codegen");
        let runtime_funcs = crate::runtime_imports::declare_runtime_functions(&mut codegen.module)
            .expect("declare runtime functions");
        compile_from_ir(&mut codegen, ir, &runtime_funcs)
            .map(|_| ())
            .map_err(|err| err.to_string())
    }

    #[test]
    fn push_return_types_uses_two_returns_for_result_abi() {
        let mut r = Vec::new();
        push_return_types(&mut r, "int");
        assert_eq!(r.len(), 1);

        let mut r = Vec::new();
        push_return_types(&mut r, "result_reg");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].value_type, types::I64);
        assert_eq!(r[1].value_type, types::I64);

        let mut r = Vec::new();
        push_return_types(&mut r, "result_reg_f");
        assert_eq!(r.len(), 2);
        assert_eq!(r[1].value_type, types::F64);
    }

    #[test]
    fn result_abi_ret2_and_rcall_compile() {
        // A result-abi function returns (is_ok, payload) via ret2; a caller
        // binds both with rcall. Arity mismatches would trip the verifier, so
        // a clean compile validates the two-return plumbing end to end.
        let ir = "func const_ok 0 result_reg\n\
                  iconst 1 1\n\
                  iconst 2 42\n\
                  ret2 1 2\n\
                  endfunc\n\
                  func main 0 int\n\
                  rcall 3 4 const_ok int 0\n\
                  ret 4\n\
                  endfunc\n";
        compile_ok_for_ir(ir);
    }

    #[test]
    fn result_abi_float_payload_compiles() {
        let ir = "func const_okf 0 result_reg_f\n\
                  iconst 1 1\n\
                  fconst 2 2.5\n\
                  ret2 1 2\n\
                  endfunc\n\
                  func main 0 int\n\
                  rcall 3 4 const_okf float 0\n\
                  ret 3\n\
                  endfunc\n";
        compile_ok_for_ir(ir);
    }

    #[test]
    fn count_calls_on_null_and_literals_are_noops() {
        use cranelift::codegen::ir::{Function, UserFuncName};
        let sig = Signature::new(cranelift::codegen::isa::CallConv::SystemV);
        let mut func = Function::with_name_signature(UserFuncName::default(), sig);
        let mut fb_ctx = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut func, &mut fb_ctx);
        let block = builder.create_block();
        builder.switch_to_block(block);
        let null = builder.ins().iconst(types::I64, 0);
        let five = builder.ins().iconst(types::I64, 5);
        let literal = builder.ins().iadd_imm(five, 16);
        let literals: HashSet<Value> = [literal].into_iter().collect();
        let f = &*builder.func;
        assert!(is_noop_count_call("pith_cstring_release", null, &literals, f));
        assert!(is_noop_count_call("pith_list_release_handle", null, &literals, f));
        assert!(is_noop_count_call("pith_cstring_retain", literal, &literals, f));
        // a literal is not a list, and a nonzero constant is not null
        assert!(!is_noop_count_call("pith_list_release_handle", literal, &literals, f));
        assert!(!is_noop_count_call("pith_cstring_release", five, &literals, f));
        assert!(!is_noop_count_call("pith_list_retain", null, &literals, f));
    }

    #[test]
    fn rcall_reaches_a_runtime_pair_builtin() {
        // parse_int and parse_float return their (is_ok, payload) pair in two
        // registers; rcall resolves them from the runtime table, the float
        // payload arriving as bits in an integer word.
        let ir = "string 0 \"12\"\n\
                  func main 0 int\n\
                  strref 1 0\n\
                  rcall 2 3 parse_int int 1 1\n\
                  rcall 4 5 parse_float float 1 1\n\
                  ret 3\n\
                  endfunc\n";
        let result = compile_with_runtime_for_ir(ir);
        assert!(result.is_ok(), "expected IR to compile, got {result:?}");
    }

    #[test]
    fn a_plain_call_to_a_runtime_pair_builtin_is_an_error() {
        let err = compile_with_runtime_for_ir(
            "string 0 \"12\"\nfunc main 0 int\nstrref 1 0\ncall 2 parse_int tuple 1 1\nret 2\nendfunc\n",
        )
        .expect_err("a plain call to a pair builtin must not compile");
        assert!(err.contains("must be an rcall"), "{err}");
    }

    #[test]
    fn rcall_to_a_single_return_function_is_an_error() {
        let err = compile_with_runtime_for_ir(
            "string 0 \"12\"\nfunc main 0 int\nstrref 1 0\nrcall 2 3 ord int 1 1\nret 2\nendfunc\n",
        )
        .expect_err("an rcall to a single-return builtin must not compile");
        assert!(err.contains("does not return a register pair"), "{err}");
    }

    #[test]
    fn invalid_register_reference_returns_compile_error() {
        let err = compile_err_for_ir("func main 0 int\niconst 1 1\nadd 2 nope 1\nendfunc\n");
        assert!(err.contains("invalid register reference 'nope'"));
    }

    #[test]
    fn missing_register_reference_returns_compile_error() {
        let err = compile_err_for_ir("func main 0 int\niconst 1 1\nadd 2 1 99\nendfunc\n");
        assert!(err.contains("missing register 99"));
    }

    #[test]
    fn invalid_destination_register_returns_compile_error() {
        let err = compile_err_for_ir("func main 0 int\niconst nope 1\nendfunc\n");
        assert!(err.contains("invalid destination register 'nope'"));
    }

    #[test]
    fn unknown_instruction_returns_compile_error() {
        let err = compile_err_for_ir("func main 0 int\nsurprise 1 2 3\nendfunc\n");
        assert!(err.contains("unknown or malformed instruction"));
    }

    #[test]
    fn malformed_store_returns_compile_error() {
        let err = compile_err_for_ir("func main 0 int\niconst 17 1\nstore  17\nendfunc\n");
        assert!(err.contains("unknown or malformed instruction"));
    }

    #[test]
    fn call_to_unknown_function_returns_compile_error() {
        let err = compile_err_for_ir("func main 0 int\ncall 1 vanished int 0\nendfunc\n");
        assert!(err.contains("call to unknown function 'vanished'"));
    }

    // every IR-declared function returns i64, so only the runtime table can
    // supply a callee that returns nothing — which is exactly where the
    // emitter's idea of a call's result and the runtime's can drift apart.
    #[test]
    fn call_asking_a_value_of_a_void_runtime_function_returns_compile_error() {
        let mut codegen = crate::create_codegen().expect("create codegen");
        let mut sig = codegen.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        let func_id = codegen
            .module
            .declare_function("pith_test_void_sink", Linkage::Import, &sig)
            .expect("declare void runtime function");
        let mut runtime_funcs = HashMap::new();
        runtime_funcs.insert("void_sink".to_string(), func_id);

        let result = compile_from_ir(
            &mut codegen,
            "func main 0 int\niconst 1 0\ncall 2 void_sink int 1 1\nret 2\nendfunc\n",
            &runtime_funcs,
        );
        let err = result
            .err()
            .expect("expected a value-returning call to a void runtime function to fail")
            .to_string();
        assert!(err.contains("asks for a int result, but the function returns nothing"));
    }

    #[test]
    fn unknown_branch_label_returns_compile_error() {
        let err =
            compile_err_for_ir("func main 0 int\niconst 1 1\nbrif 1 then_l else_l\nendfunc\n");
        assert!(err.contains("unknown label 'then_l'"));
    }

    #[test]
    fn unknown_jump_label_returns_compile_error() {
        let err = compile_err_for_ir("func main 0 int\njmp missing_l\nendfunc\n");
        assert!(err.contains("unknown label 'missing_l'"));
    }

    #[test]
    fn malformed_integer_constant_returns_compile_error() {
        let err = compile_err_for_ir("func main 0 int\niconst 1 notanumber\nendfunc\n");
        assert!(err.contains("invalid integer constant 'notanumber'"));
    }

    #[test]
    fn malformed_float_constant_returns_compile_error() {
        let err = compile_err_for_ir("func main 0 int\nfconst 1 notafloat\nendfunc\n");
        assert!(err.contains("invalid float constant 'notafloat'"));
    }

    #[test]
    fn invalid_parameter_count_returns_compile_error() {
        let err = compile_err_for_ir("func main x int\nendfunc\n");
        assert!(err.contains("invalid parameter count 'x'"));
    }
}
