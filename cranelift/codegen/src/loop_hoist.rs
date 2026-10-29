//! Moves list header reads out of loops that cannot change a list.
//!
//! `xs[i]`, `xs[i] = v` and `xs.len()` read the list's header (magic word,
//! element size, length, data pointer) on every use. Cranelift hoists only
//! loads it may treat as constant for the whole function, and a list header
//! is not: a push changes its length and data pointer. Only the runtime
//! changes a header, though, and always inside a call, so in a loop that
//! makes no calls every header read gives the same answer on each pass. This
//! pass moves those reads, and the address arithmetic they need, to the block
//! that enters the loop. Cranelift's own loop-invariant motion then lifts the
//! checks built on them.
//!
//! The header reads are marked with `HEADER_FLAGS`: they cannot trap, because
//! the codegen points them at mapped memory whatever the handle holds, so
//! running one before its guard is safe. Some runtime calls are known to
//! leave every list header alone (see `keep_calls`), and a loop may make
//! those.

use cranelift::codegen::cursor::{Cursor, FuncCursor};
use cranelift::codegen::dominator_tree::DominatorTree;
use cranelift::codegen::flowgraph::ControlFlowGraph;
use cranelift::codegen::ir::{
    AliasRegion, Block, ExternalName, Function, Inst, InstructionData, MemFlags, Opcode,
    UserExternalName, Value, ValueDef,
};
use cranelift::codegen::loop_analysis::{Loop, LoopAnalysis};
use std::collections::{HashMap, HashSet};

/// The flags the codegen puts on a list header read, and the mark this pass
/// looks for. The table region keeps Cranelift's redundant-load pass from
/// treating element stores as writes to a header.
pub(crate) const HEADER_FLAGS: MemFlags = MemFlags::new()
    .with_notrap()
    .with_can_move()
    .with_alias_region(Some(AliasRegion::Table));

/// Hoist the list header reads in `func`'s loops that call nothing outside
/// `keep_calls`, the module indexes of runtime functions that never change a
/// list header.
pub(crate) fn hoist_list_headers(func: &mut Function, keep_calls: &[u32]) {
    func.dfg.resolve_all_aliases();
    let cfg = ControlFlowGraph::with_function(func);
    let domtree = DominatorTree::with_function(func, &cfg);
    let mut loops = LoopAnalysis::new();
    loops.compute(func, &cfg, &domtree);
    let rpo: Vec<Block> = domtree.cfg_rpo().copied().collect();
    let body = |lp: Loop| -> Vec<Block> {
        rpo.iter()
            .copied()
            .filter(|&b| loops.is_in_loop(b, lp))
            .collect()
    };

    // inner loops first, so a read lifted out of an inner loop can then be
    // lifted out of the loop around it
    let mut order: Vec<Loop> = loops.loops().collect();
    order.sort_by_key(|&lp| std::cmp::Reverse(loops.loop_level(loops.loop_header(lp)).level()));
    for lp in order {
        let blocks = body(lp);
        let Some(entry) = single_entry(func, &cfg, &loops, lp) else {
            continue;
        };
        if blocks.iter().any(|&b| calls_out(func, b, keep_calls)) {
            continue;
        }
        let Some(at) = func.layout.last_inst(entry) else {
            continue;
        };
        let copies = loop_copies(func, &blocks);
        let mut hoist = Hoist {
            func: &mut *func,
            loops: &loops,
            lp,
            at,
            copies,
        };
        for &block in &blocks {
            let insts: Vec<Inst> = hoist.func.layout.block_insts(block).collect();
            for inst in insts {
                if is_header_read(hoist.func, inst) {
                    hoist.try_inst(inst);
                }
            }
        }
    }
}

/// Inside a loop, a variable the loop never assigns is still a parameter of
/// the loop's blocks, passed around unchanged. Map each such parameter to
/// the one value it always holds, so the pass sees a list the loop only
/// reads as defined outside it. Cranelift's constant-phi removal does this
/// only for values that never change anywhere in the function, and misses a
/// list an outer loop reassigns. Only the instructions the pass moves are
/// rewritten: rewriting every use stretches live ranges in loops it cannot
/// help, and costs registers there.
fn loop_copies(func: &Function, blocks: &[Block]) -> HashMap<Value, Value> {
    #[derive(Clone, Copy, PartialEq)]
    enum Seen {
        One(Value),
        Many,
    }
    let in_loop: HashSet<Block> = blocks.iter().copied().collect();
    let params: HashSet<Value> = blocks
        .iter()
        .flat_map(|&b| func.dfg.block_params(b).iter().copied())
        .collect();

    // every edge into a block of the loop, as (parameter, argument) pairs
    let mut edges: Vec<(Value, Option<Value>)> = Vec::new();
    for block in func.layout.blocks() {
        let Some(inst) = func.layout.last_inst(block) else {
            continue;
        };
        let dests = func.dfg.insts[inst].branch_destination(&func.dfg.jump_tables, &func.dfg.exception_tables);
        for dest in dests {
            let target = dest.block(&func.dfg.value_lists);
            if !in_loop.contains(&target) {
                continue;
            }
            let formals = func.dfg.block_params(target);
            for (formal, arg) in formals.iter().zip(dest.args(&func.dfg.value_lists)) {
                let arg = arg.as_value().map(|v| func.dfg.resolve_aliases(v));
                edges.push((*formal, arg));
            }
        }
    }

    let mut seen: HashMap<Value, Seen> = HashMap::new();
    loop {
        let mut changed = false;
        for &(formal, arg) in &edges {
            let incoming = match arg {
                Some(arg) if arg == formal => continue,
                Some(arg) if params.contains(&arg) => match seen.get(&arg) {
                    Some(&s) => s,
                    None => continue,
                },
                Some(arg) => Seen::One(arg),
                None => Seen::Many,
            };
            let joined = match seen.get(&formal) {
                None => incoming,
                Some(&old) if old == incoming => old,
                Some(_) => Seen::Many,
            };
            if seen.insert(formal, joined) != Some(joined) {
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    seen.into_iter()
        .filter_map(|(formal, s)| match s {
            Seen::One(value) => Some((formal, value)),
            Seen::Many => None,
        })
        .collect()
}

/// The block outside `lp` that is the only way into it, when that block ends
/// in a plain jump to the header, so code put before the jump runs exactly
/// when the loop is entered.
fn single_entry(
    func: &Function,
    cfg: &ControlFlowGraph,
    loops: &LoopAnalysis,
    lp: Loop,
) -> Option<Block> {
    let header = loops.loop_header(lp);
    let mut outside = cfg
        .pred_iter(header)
        .filter(|pred| !loops.is_in_loop(pred.block, lp));
    let pred = outside.next()?;
    if outside.next().is_some() || func.dfg.insts[pred.inst].opcode() != Opcode::Jump {
        return None;
    }
    Some(pred.block)
}

/// Whether `block` calls anything outside `keep_calls`.
fn calls_out(func: &Function, block: Block, keep_calls: &[u32]) -> bool {
    func.layout.block_insts(block).any(|inst| {
        let data = &func.dfg.insts[inst];
        if !data.opcode().is_call() {
            return false;
        }
        let InstructionData::Call { func_ref, .. } = *data else {
            return true;
        };
        let ExternalName::User(name) = func.dfg.ext_funcs[func_ref].name else {
            return true;
        };
        let callee = &func.params.user_named_funcs()[name];
        !keep_calls
            .iter()
            .any(|&index| *callee == UserExternalName::new(0, index))
    })
}

fn is_header_read(func: &Function, inst: Inst) -> bool {
    matches!(
        func.dfg.insts[inst],
        InstructionData::Load { opcode: Opcode::Load, flags, .. } if flags == HEADER_FLAGS
    )
}

/// Whether `inst` computes its result from its arguments alone, so it can
/// run earlier than written.
fn is_pure(func: &Function, inst: Inst) -> bool {
    let op = func.dfg.insts[inst].opcode();
    !(op.can_load()
        || op.can_store()
        || op.is_call()
        || op.is_branch()
        || op.is_terminator()
        || op.can_trap()
        || op.other_side_effects())
        && func.dfg.inst_results(inst).len() == 1
}

struct Hoist<'a> {
    func: &'a mut Function,
    loops: &'a LoopAnalysis,
    lp: Loop,
    /// The entry block's jump; hoisted code goes in front of it.
    at: Inst,
    /// The loop's parameters that only ever hold one value, and that value.
    copies: HashMap<Value, Value>,
}

impl Hoist<'_> {
    /// Move `inst` in front of the entry jump if its arguments are, or can
    /// be made, available there.
    fn try_inst(&mut self, inst: Inst) -> bool {
        let args: Vec<Value> = self.func.dfg.inst_values(inst).map(|v| self.copy_of(v)).collect();
        if !args.into_iter().all(|arg| self.try_value(arg)) {
            return false;
        }
        let copies = &self.copies;
        self.func
            .dfg
            .map_inst_values(inst, |v| copies.get(&v).copied().unwrap_or(v));
        self.func.layout.remove_inst(inst);
        let mut cursor = FuncCursor::new(self.func).at_inst(self.at);
        cursor.insert_inst(inst);
        true
    }

    /// Whether `value` is available in front of the entry jump, moving the
    /// pure instruction that defines it there if that is what it takes.
    fn copy_of(&self, value: Value) -> Value {
        self.copies.get(&value).copied().unwrap_or(value)
    }

    fn try_value(&mut self, value: Value) -> bool {
        let value = self.func.dfg.resolve_aliases(value);
        let (block, inst) = match self.func.dfg.value_def(value) {
            ValueDef::Result(inst, _) => (self.func.layout.inst_block(inst), Some(inst)),
            ValueDef::Param(block, _) => (Some(block), None),
            _ => return false,
        };
        let Some(block) = block else {
            return false;
        };
        if !self.loops.is_in_loop(block, self.lp) {
            return true;
        }
        match inst {
            Some(inst) if is_pure(self.func, inst) => self.try_inst(inst),
            _ => false,
        }
    }
}
