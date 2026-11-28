//! Checked and unchecked integer arithmetic, range-fitting casts,
//! division/modulo traps, and bitwise operations.

use super::fast_paths::jump_with_i64_arg;
use super::instrumentation::emit_runtime_error_value;
use crate::{CodeGen, CompileError};
use cranelift::codegen::ir::FuncRef;
use cranelift::prelude::*;
use cranelift_module::FuncId;
use std::collections::HashMap;

pub(super) fn emit_bits_op(builder: &mut FunctionBuilder<'_>, op: &str, a: Value, b: Value) -> Value {
    match op {
        "band" => builder.ins().band(a, b),
        "bor" => builder.ins().bor(a, b),
        "bxor" => builder.ins().bxor(a, b),
        "shl" => {
            emit_int_fit(builder, b, 0, 63);
            builder.ins().ishl(a, b)
        }
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
        _ => {
            emit_int_fit(builder, b, 0, 63);
            builder.ins().ushr(a, b)
        }
    }
}

/// `x` when `lo <= x <= hi`, and the integer-overflow trap otherwise: one
/// unsigned compare of the distance from `lo`, or no check at all for a
/// constant in range. the conversions to the narrow and unsigned types and
/// the shift counts go through it.
pub(super) fn emit_int_fit(builder: &mut FunctionBuilder<'_>, x: Value, lo: i64, hi: i64) -> Value {
    if let Some(c) = iconst_of(builder.func, x) {
        if (lo..=hi).contains(&c) {
            return x;
        }
    }
    let out = if lo == 0 {
        builder.ins().icmp_imm(IntCC::UnsignedGreaterThan, x, hi)
    } else {
        let shifted = builder.ins().iadd_imm(x, lo.wrapping_neg());
        builder.ins().icmp_imm(IntCC::UnsignedGreaterThan, shifted, hi.wrapping_sub(lo))
    };
    builder.ins().trapnz(out, cranelift::codegen::ir::TrapCode::unwrap_user(OVERFLOW_TRAP_CODE));
    x
}


/// The trap code an integer overflow raises; any nonzero user code works,
/// since the runtime reads the faulting instruction rather than the code.
pub(super) const OVERFLOW_TRAP_CODE: u8 = 7;

/// The plain opcode behind a checked or exact one (`addc` -> `add`, `uaddc`
/// -> `add`, `addnsw` -> `add`), or the opcode itself. A checked op on a
/// string or float operand lowers as its plain form, which concatenates or
/// promotes the way `add` always has.
pub(super) fn plain_arith_op(op: &str) -> &str {
    match op {
        "addc" | "uaddc" | "addnsw" => "add",
        "subc" | "usubc" => "sub",
        "mulc" | "umulc" => "mul",
        _ => op,
    }
}

/// The value of an `iconst`, when `v` is one.
pub(super) fn iconst_of(func: &cranelift::codegen::ir::Function, v: Value) -> Option<i64> {
    let cranelift::codegen::ir::ValueDef::Result(inst, _) = func.dfg.value_def(v) else {
        return None;
    };
    match func.dfg.insts[inst] {
        cranelift::codegen::ir::InstructionData::UnaryImm {
            opcode: cranelift::codegen::ir::Opcode::Iconst,
            imm,
        } => Some(imm.bits()),
        _ => None,
    }
}

/// The range of `x` for which a checked op with one constant operand stays in
/// range, as `(lo, hi)` in the op's own order (signed, or unsigned bits for the
/// `u` forms), or `None` when every `x` overflows-free. `const_on_left` is set
/// for `c - x`. Worked out in i128 so the bounds themselves cannot overflow.
pub(super) fn checked_operand_range(op: &str, c: i64, const_on_left: bool) -> Option<(i128, i128)> {
    let (min, max, c) = if op.starts_with('u') {
        (0i128, u64::MAX as i128, c as u64 as i128)
    } else {
        (i64::MIN as i128, i64::MAX as i128, c as i128)
    };
    let (lo, hi) = match (op, const_on_left) {
        ("addc" | "uaddc", _) => (min - c, max - c),
        ("subc" | "usubc", false) => (min + c, max + c),
        ("subc" | "usubc", true) => (c - max, c - min),
        ("mulc" | "umulc", _) => {
            if c == 0 || c == 1 {
                return None;
            } else if c == -1 {
                (-max, max)
            } else if c > 1 {
                // division truncates toward zero, which rounds the negative
                // bound up and the positive one down: both inward, as needed
                (min / c, max / c)
            } else {
                (max / c, min / c)
            }
        }
        _ => return None,
    };
    let (lo, hi) = (lo.max(min), hi.min(max));
    if lo == min && hi == max {
        None
    } else {
        Some((lo, hi))
    }
}

/// Integer `+`, `-` or `*` from source code: the result, with a branch to the
/// integer-overflow runtime error when it does not fit. `addc`/`subc`/`mulc`
/// check the signed range (Int) and `uaddc`/`usubc`/`umulc` the unsigned one
/// (UInt, UInt64, whose values are the bits of an i64).
///
/// With a constant operand (`i + 1`, `n * 10`, `0 - x`) the check is a compare
/// of the other operand against the range that cannot overflow, taken before a
/// plain add or multiply. The overflow forms make x64 materialize the flag and
/// test it, and a constant multiply in that form takes the slow one-operand
/// `imul`; the compare fuses with its branch and leaves the arithmetic alone.
///
/// `a_nonneg` and `b_nonneg` say an operand is never negative (see
/// nonneg.rs). For the signed ops such an operand leaves one way out of the
/// range, or none, and the check covers only that:
///
/// - `x op c`: the range check drops the end below zero, so `i * 3` on a
///   counter compares against the top alone.
/// - `a + b` with `b >= 0` can only overflow upward, and then the wrapped sum
///   comes out below `a`, so the check is `sum < a`; with both operands at
///   least zero it is `sum < 0`. Either compare fuses with its branch.
/// - `a - b` with both operands at least zero lies within -MAX..MAX and
///   needs no check. With `b >= 0` alone it can only overflow downward, and
///   then the wrapped difference comes out above `a`.
///
/// Two unsigned words need no flag either: `a + b` is an add whose carry
/// traps directly, and `a - b` leaves the range exactly when `a < b`.
pub(super) fn emit_checked_int_arith(
    builder: &mut FunctionBuilder<'_>,
    op: &str,
    a: Value,
    b: Value,
    a_nonneg: bool,
    b_nonneg: bool,
) -> Result<Value, CompileError> {
    let unsigned = op.starts_with('u');
    let (a_nonneg, b_nonneg) = (a_nonneg && !unsigned, b_nonneg && !unsigned);
    let commutes = !matches!(op, "subc" | "usubc");
    let constant = match (iconst_of(builder.func, a), iconst_of(builder.func, b)) {
        (_, Some(c)) => Some((c, a, a_nonneg, false)),
        (Some(c), None) if commutes => Some((c, b, b_nonneg, false)),
        (Some(c), None) => Some((c, b, b_nonneg, true)),
        _ => None,
    };
    let plain = |builder: &mut FunctionBuilder<'_>| match plain_arith_op(op) {
        "add" => builder.ins().iadd(a, b),
        "sub" => builder.ins().isub(a, b),
        _ => builder.ins().imul(a, b),
    };
    // a trap, not a call to the runtime error: a call on the cold path makes
    // every value live across the check survive a call, which costs spills
    // in the hot loop around it (twice the instructions in a merge loop).
    // the runtime turns the trap into the integer-overflow error
    // (overflow_trap.rs).
    let code = cranelift::codegen::ir::TrapCode::unwrap_user(OVERFLOW_TRAP_CODE);
    if let Some((c, x, x_nonneg, const_on_left)) = constant {
        let Some((lo, hi)) = checked_operand_range(op, c, const_on_left) else {
            return Ok(plain(builder));
        };
        let (min, max) = if unsigned { (0i128, u64::MAX as i128) } else { (i64::MIN as i128, i64::MAX as i128) };
        let least = if x_nonneg { 0 } else { min };
        let (below, above) = if unsigned {
            (IntCC::UnsignedLessThan, IntCC::UnsignedGreaterThan)
        } else {
            (IntCC::SignedLessThan, IntCC::SignedGreaterThan)
        };
        let overflowed = match (lo > least, hi < max) {
            (false, false) => return Ok(plain(builder)),
            (false, true) => builder.ins().icmp_imm(above, x, hi as i64),
            (true, false) => builder.ins().icmp_imm(below, x, lo as i64),
            (true, true) => {
                // both ends: one unsigned compare of the distance from lo
                let shifted = builder.ins().iadd_imm(x, (lo as i64).wrapping_neg());
                builder.ins().icmp_imm(IntCC::UnsignedGreaterThan, shifted, (hi - lo) as i64)
            }
        };
        builder.ins().trapnz(overflowed, code);
        return Ok(plain(builder));
    }
    let (value, overflowed) = match op {
        "addc" if a_nonneg || b_nonneg => {
            let sum = builder.ins().iadd(a, b);
            let overflowed = if a_nonneg && b_nonneg {
                builder.ins().icmp_imm(IntCC::SignedLessThan, sum, 0)
            } else if b_nonneg {
                builder.ins().icmp(IntCC::SignedLessThan, sum, a)
            } else {
                builder.ins().icmp(IntCC::SignedLessThan, sum, b)
            };
            (sum, overflowed)
        }
        "subc" if a_nonneg && b_nonneg => return Ok(plain(builder)),
        "subc" if b_nonneg => {
            let diff = builder.ins().isub(a, b);
            (diff, builder.ins().icmp(IntCC::SignedGreaterThan, diff, a))
        }
        "uaddc" => return Ok(builder.ins().uadd_overflow_trap(a, b, code)),
        "usubc" => (builder.ins().isub(a, b), builder.ins().icmp(IntCC::UnsignedLessThan, a, b)),
        "addc" => builder.ins().sadd_overflow(a, b),
        "subc" => builder.ins().ssub_overflow(a, b),
        "mulc" => builder.ins().smul_overflow(a, b),
        "umulc" => builder.ins().umul_overflow(a, b),
        _ => {
            return Err(CompileError::ModuleError(format!(
                "IR consumer: unsupported checked integer op '{op}'"
            )))
        }
    };
    builder.ins().trapnz(overflowed, code);
    Ok(value)
}

pub(super) fn emit_checked_int_div_or_mod(
    codegen: &mut CodeGen,
    builder: &mut FunctionBuilder<'_>,
    func_ref_cache: &mut HashMap<FuncId, FuncRef>,
    runtime_funcs: &HashMap<String, FuncId>,
    op: &str,
    a: Value,
    b: Value,
) -> Result<Value, CompileError> {
    // a constant divisor other than 0 and -1 can neither divide by zero nor
    // overflow. without this the checks folded to constants but their
    // branches stayed, two dead tests on every `i % 7`.
    if let Some(d) = iconst_of(builder.func, b) {
        if d != 0 && d != -1 {
            return Ok(match op {
                "div" => builder.ins().sdiv(a, b),
                "mod" => builder.ins().srem(a, b),
                "udiv" => builder.ins().udiv(a, b),
                "urem" => builder.ins().urem(a, b),
                _ => {
                    return Err(CompileError::ModuleError(format!(
                        "IR consumer: unsupported checked integer op '{op}'"
                    )))
                }
            });
        }
    }
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
    // an unsigned division has no overflowing case
    if op == "udiv" || op == "urem" {
        let value = if op == "udiv" { builder.ins().udiv(a, b) } else { builder.ins().urem(a, b) };
        jump_with_i64_arg(builder, done, value);
        builder.switch_to_block(done);
        return Ok(builder.block_params(done)[0]);
    }
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

