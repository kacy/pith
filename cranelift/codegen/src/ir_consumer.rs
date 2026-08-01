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
use pith_runtime::collections::list::{
    LIST_IMPL_ELEM_SIZE_OFFSET, LIST_IMPL_TYPE_TAG_OFFSET, LIST_IMPL_VALUES8_LEN_OFFSET,
    LIST_IMPL_VALUES8_PTR_OFFSET, LIST_MAGIC, LIST_TYPE_TAG_PRIMITIVE,
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
// offset 0, value at offset 8) and release. When scan_inline_list_get_opt_regs
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

fn emit_bits_op(builder: &mut FunctionBuilder<'_>, op: &str, a: Value, b: Value) -> Value {
    match op {
        "band" => builder.ins().band(a, b),
        "bor" => builder.ins().bor(a, b),
        "bxor" => builder.ins().bxor(a, b),
        "shl" => builder.ins().ishl(a, b),
        _ => builder.ins().ushr(a, b),
    }
}

// the defining shape of an inlinable indexed read: call R pith_list_get_opt
// tuple 2 LIST INDEX.
fn is_list_get_opt_def(parts: &[&str]) -> bool {
    parts.len() == 7
        && parts[0] == "call"
        && parts[2] == "pith_list_get_opt"
        && parts[3] == "tuple"
        && parts[4] == "2"
}

// a token occurrence (parts[i]) the inline unpack understands: the defining
// call itself, a field read of tuple slot 0 or 8, or the release of the
// throwaway tuple shell. anything else — a store, a return, an argument to
// some other call — means the tuple escapes and must really exist.
fn is_sanctioned_opt_use(parts: &[&str], i: usize) -> bool {
    if is_list_get_opt_def(parts) {
        return i == 1;
    }
    if parts.len() == 6 && parts[0] == "field" && (parts[3] == "0" || parts[3] == "8") {
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

// registers holding a pith_list_get_opt result whose every textual use is the
// emitter's own unpack pattern. the scan is deliberately conservative: any
// numeric token equal to a candidate register that sits outside a sanctioned
// position disqualifies it, so an escaping optional (`.get(i)` stored in a
// variable, passed on, returned) always takes the real runtime call.
fn scan_inline_list_get_opt_regs(body_lines: &[&str]) -> HashSet<usize> {
    let mut candidates: HashMap<usize, u32> = HashMap::new();
    for line in body_lines {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if is_list_get_opt_def(&parts) {
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

// The magic word every heap cstring carries at `ptr - 16`. This mirrors
// `runtime_core::CSTRING_MAGIC` and must stay in sync with it. A drift is not a
// safety problem — the fast path below simply stops matching and every access
// falls back to the FFI helper, which is correct but slow — so keep the two
// definitions together.
const CSTRING_MAGIC: i64 = 0x5043_5352;

// The 16-byte header in front of heap cstring data: [magic: u32][rc: u32,
// atomic][data_len: u64], so the length lives 8 bytes before the pointer.
const CSTRING_LEN_OFFSET: i32 = -8;
const CSTRING_MAGIC_OFFSET: i32 = -16;

/// Narrow `s` to a confirmed heap cstring, branching to `slow` for anything
/// else. This mirrors `runtime_core::cstring_base`: heap cstring data is at an
/// address >= 16 (which also rejects null), is 8-aligned, and carries the magic
/// word 16 bytes in front of it. Literals and the shared empty string fail one
/// of these checks and take the slow path. On return the builder sits in a
/// fresh block where `s` is known to be a heap cstring.
fn branch_if_not_heap_cstring(builder: &mut FunctionBuilder<'_>, s: Value, slow: Block) {
    // addr >= 16, unsigned so null and any stray small pointer are rejected
    // before the header word is read.
    let too_small = builder.ins().icmp_imm(IntCC::UnsignedLessThan, s, 16);
    let check_align = builder.create_block();
    builder.ins().brif(too_small, slow, &[], check_align, &[]);
    builder.switch_to_block(check_align);

    // heap cstring data is always 8-aligned; this rejects most literals cheaply.
    let low_bits = builder.ins().band_imm(s, 7);
    let unaligned = builder.ins().icmp_imm(IntCC::NotEqual, low_bits, 0);
    let check_magic = builder.create_block();
    builder.ins().brif(unaligned, slow, &[], check_magic, &[]);
    builder.switch_to_block(check_magic);

    // read the 4-byte magic in front of the data and compare. an aligned
    // literal reads mapped binary data here and simply fails the compare, the
    // same contract the runtime relies on for retain/release.
    let magic = builder
        .ins()
        .load(types::I32, MemFlags::new(), s, CSTRING_MAGIC_OFFSET);
    let not_magic = builder.ins().icmp_imm(IntCC::NotEqual, magic, CSTRING_MAGIC);
    let confirmed = builder.create_block();
    builder.ins().brif(not_magic, slow, &[], confirmed, &[]);
    builder.switch_to_block(confirmed);
}

/// Inline `s[i]` (byte_at) for heap cstrings: read the length from the header,
/// bounds-check, and index the byte directly, skipping the FFI dispatch that
/// dominates tight character-scanning loops. Literals and wild pointers fall
/// back to `fallback` (pith_cstring_byte_at) for identical semantics — out of
/// range returns -1.
fn inline_cstring_byte_at(
    builder: &mut FunctionBuilder<'_>,
    s: Value,
    index: Value,
    fallback: cranelift::codegen::ir::FuncRef,
) -> Value {
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    let slow = builder.create_block();

    branch_if_not_heap_cstring(builder, s, slow);
    let len = builder
        .ins()
        .load(types::I64, MemFlags::new(), s, CSTRING_LEN_OFFSET);
    // one unsigned compare catches both a negative index and one past the end.
    let out_of_bounds = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, len);
    let oob = builder.create_block();
    let read = builder.create_block();
    builder.ins().brif(out_of_bounds, oob, &[], read, &[]);
    builder.switch_to_block(oob);
    let neg_one = builder.ins().iconst(types::I64, -1);
    jump_with_i64_arg(builder, done, neg_one);
    builder.switch_to_block(read);
    let elem_addr = builder.ins().iadd(s, index);
    let byte = builder.ins().load(types::I8, MemFlags::new(), elem_addr, 0);
    let value = builder.ins().uextend(types::I64, byte);
    jump_with_i64_arg(builder, done, value);

    builder.switch_to_block(slow);
    let call = builder.ins().call(fallback, &[s, index]);
    let fallback_result = builder.func.dfg.inst_results(call)[0];
    jump_with_i64_arg(builder, done, fallback_result);

    builder.switch_to_block(done);
    builder.block_params(done)[0]
}

/// Inline `s.len()` (string_len) for heap cstrings: one header read instead of
/// an FFI call. Literals fall back to `fallback` (pith_cstring_len), which
/// strlen-scans them.
fn inline_cstring_len(
    builder: &mut FunctionBuilder<'_>,
    s: Value,
    fallback: cranelift::codegen::ir::FuncRef,
) -> Value {
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    let slow = builder.create_block();

    branch_if_not_heap_cstring(builder, s, slow);
    let len = builder
        .ins()
        .load(types::I64, MemFlags::new(), s, CSTRING_LEN_OFFSET);
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

/// Whether this build should emit green-preemption safe-points. Off by default,
/// because a flat loop pays a large relative cost for the per-iteration check and
/// most programs never have a compute-only task that needs descheduling. Set
/// `PITH_GREEN_PREEMPT` at build time to make compute-bound green tasks
/// preemptible.
fn green_preempt_enabled() -> bool {
    matches!(
        std::env::var("PITH_GREEN_PREEMPT").as_deref(),
        Ok("1") | Ok("on") | Ok("true")
    )
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

    // green preemption safe-points are opt-in at compile time. a flat arithmetic
    // loop pays a large relative cost for the inline flag check (its body is a
    // couple of instructions, so the extra load+test+branch per iteration is a
    // big fraction), and most programs have no compute-only task that would ever
    // need descheduling. so the default codegen emits no check at all — exactly
    // zero cost — and only a build that asks for preemption
    // (`PITH_GREEN_PREEMPT=1`) inserts the safe-points. `None` here means
    // "emit nothing"; `Some(flag)` carries the imported flag symbol.
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
                    sig.returns.push(AbiParam::new(types::I64));
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
                preempt_flag,
                &bits_aliases,
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
    preempt_flag: Option<cranelift_module::DataId>,
    bits_aliases: &HashMap<String, &'static str>,
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

    // Pre-scan for labels and create blocks
    for line in body_lines {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if !parts.is_empty() && parts[0] == "label" && parts.len() >= 2 {
            let block = builder.create_block();
            labels.insert(parts[1].to_string(), block);
        }
    }

    // Pre-scan for indexed list reads whose Optional tuple never escapes; those
    // calls compile to inline loads instead (see inline_list_get_opt). The map
    // carries each virtualized register's (is_some, value) pair for the field
    // reads that follow.
    let inline_opt_regs = scan_inline_list_get_opt_regs(body_lines);
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
                if let Some(&sf_id) = string_funcs.get(&str_idx) {
                    let sf_ref = codegen.module.declare_func_in_func(sf_id, builder.func);
                    let call = builder.ins().call(sf_ref, &[]);
                    let v = builder.func.dfg.first_result(call);
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
                    if fname == "bytes_get" && args.len() == 2 {
                        let inlined = inline_bytes_get(&mut builder, args[0], args[1]);
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
                    if fname == "byte_at" && args.len() == 2 {
                        let fallback = runtime_func_ref(
                            codegen,
                            &mut builder,
                            &mut func_ref_cache,
                            runtime_funcs,
                            "byte_at",
                        )?;
                        let inlined =
                            inline_cstring_byte_at(&mut builder, args[0], args[1], fallback);
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
                        let fn_ptr = if let Some(&closure_get_id) =
                            runtime_funcs.get("pith_closure_get_fn")
                        {
                            let closure_get_ref =
                                *func_ref_cache.entry(closure_get_id).or_insert_with(|| {
                                    codegen
                                        .module
                                        .declare_func_in_func(closure_get_id, builder.func)
                                });
                            let call = builder.ins().call(closure_get_ref, &[closure_handle]);
                            builder.func.dfg.first_result(call)
                        } else {
                            closure_handle
                        };
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
                        let fn_ptr = if let Some(&closure_get_id) =
                            runtime_funcs.get("pith_closure_get_fn")
                        {
                            let closure_get_ref =
                                *func_ref_cache.entry(closure_get_id).or_insert_with(|| {
                                    codegen
                                        .module
                                        .declare_func_in_func(closure_get_id, builder.func)
                                });
                            let call = builder.ins().call(closure_get_ref, &[closure_handle]);
                            builder.func.dfg.first_result(call)
                        } else {
                            closure_handle
                        };
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
                        struct_regs.remove(&reg);
                        if parts.len() >= 6 {
                            let retkind = parts[4];
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
                let cond_bool = builder.ins().icmp_imm(IntCC::NotEqual, cond, 0);
                let then_block = get_label(&labels, then_label, func_name)?;
                let else_block = get_label(&labels, else_label, func_name)?;
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
        let regs = scan_inline_list_get_opt_regs(&body);
        assert!(regs.contains(&10));

        // an escaping optional (stored into a variable) must keep the call
        let escaping = vec![
            "call 10 pith_list_get_opt tuple 2 4 5",
            "store opt 10",
            "field 11 10 0 bool is_some",
        ];
        assert!(scan_inline_list_get_opt_regs(&escaping).is_empty());

        // passing the tuple to any other call disqualifies it too
        let passed = vec![
            "call 10 pith_list_get_opt tuple 2 4 5",
            "field 11 10 0 bool is_some",
            "call 12 some_fn int 1 10",
        ];
        assert!(scan_inline_list_get_opt_regs(&passed).is_empty());
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
