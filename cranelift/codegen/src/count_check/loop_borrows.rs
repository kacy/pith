//! Borrowed loop-variable safety analysis.

use super::ir::{Func, ParsedIr};
use super::{is_emitted_release, is_emitted_unshare};
use crate::runtime_ownership::runtime_ownership;
use std::collections::{HashMap, HashSet};

// ---- the borrow rule, first part: loop variables ----
//
// a list loop's variable borrows its element (`mark borrow V FROM`) only when
// the emitter proved the body can't drop the element (ir_subtree_keeps_storage),
// which is the claim #1270 found wrong. this pass makes the same judgement from
// the IR alone: a function can drop a stored element when it writes a field or
// a global, changes a container, calls something the checker can't see, or
// calls a function that does. a borrowed variable read after such an
// instruction in its loop body may read an element that is gone.

/// runtime calls that can drop a value a container holds: they evict an
/// element, overwrite a slot, or store a raw word over one. a push or an
/// insert of a new key only adds, so a borrowed element stays alive
const EVICTING_NAME_PARTS: &[&str] = &[
    "clear",
    "remove",
    "pop",
    "set_value",
    "set_kind",
    "map_insert",
    "take",
    "truncate",
    "drain",
    "store_u64",
    "tls_set",
    "set_env",
];

pub(super) fn runtime_call_writes(callee: &str) -> bool {
    if callee.contains("release") || callee.contains("retain") {
        return false;
    }
    // a byte buffer holds raw bytes and no counted value, so nothing done to
    // one can drop a stored value
    if callee.starts_with("byte_buffer_") {
        return false;
    }
    EVICTING_NAME_PARTS.iter().any(|p| callee.contains(p))
}

/// the functions that can drop a stored value, by name, worked out to a fixed
/// point over the calls between them
fn storage_writers(funcs: &[Func<'_>], globals: &HashSet<&str>) -> HashSet<String> {
    let user: HashSet<&str> = funcs.iter().map(|f| f.name).collect();
    let mut writers: HashSet<String> = HashSet::new();
    loop {
        let mut changed = false;
        for f in funcs {
            let (name, body) = (f.name, &f.body);
            if writers.contains(name) {
                continue;
            }
            let fresh = fresh_allocations(body);
            if body.iter().any(|ins| instruction_writes(ins, globals, &user, &writers, &fresh)) {
                writers.insert(name.to_string());
                changed = true;
            }
        }
        if !changed {
            return writers;
        }
    }
}

/// the registers a function's own allocations land in: a field store into one
/// fills a fresh struct and can't drop anything
fn fresh_allocations<'a>(body: &[Vec<&'a str>]) -> HashSet<&'a str> {
    body.iter()
        .filter(|ins| {
            (ins[0] == "call" && ins.len() > 2 && ins[2] == "pith_struct_alloc") || (ins[0] == "tuple_stack" && ins.len() > 1)
        })
        .map(|ins| ins[1])
        .collect()
}

fn instruction_writes(
    ins: &[&str],
    globals: &HashSet<&str>,
    user: &HashSet<&str>,
    writers: &HashSet<String>,
    fresh: &HashSet<&str>,
) -> bool {
    match ins[0] {
        "sstore" => ins.len() < 2 || !fresh.contains(ins[1]),
        "store" if ins.len() >= 2 => globals.contains(ins[1]),
        // a call through a function pointer may run anything
        "calli" => true,
        "call" | "rcall" => {
            let callee = if ins[0] == "call" { ins.get(2) } else { ins.get(3) };
            let Some(callee) = callee else { return true };
            // an emitted release helper drops only its own argument
            if is_emitted_release(callee) || is_emitted_unshare(callee) || callee.contains("__tuple_stack_cascade_") {
                return false;
            }
            if user.contains(callee) {
                writers.contains(*callee)
            } else if runtime_ownership(callee).is_some() {
                runtime_call_writes(callee)
            } else {
                // a call the checker can't see into may do anything
                true
            }
        }
        _ => false,
    }
}

/// every borrowed loop variable read after an instruction in its loop body
/// that can drop the element it borrows
pub fn check_loop_borrows(lines: &[&str]) -> Vec<String> {
    let parsed = ParsedIr::parse(lines);
    check_loop_borrows_parsed(&parsed)
}

pub(super) fn check_loop_borrows_parsed(parsed: &ParsedIr<'_>) -> Vec<String> {
    let globals = &parsed.global_decls;
    let funcs = &parsed.funcs;
    let writers = storage_writers(funcs, globals);
    let user: HashSet<&str> = funcs.iter().map(|f| f.name).collect();
    let mut problems = Vec::new();
    for f in funcs {
        let (name, body) = (f.name, &f.body);
        for &(var, from) in &f.borrows {
        let Some(start) = body.iter().position(|ins| ins[0] == "store" && ins.get(1) == Some(&var)) else { continue };
        // the loop's head is the label before the first read of its index;
        // the body runs from the variable's store to the jmp back there
        let fid = var.trim_start_matches("__loopvar_").split('_').next().unwrap_or("");
        let idx_slot = format!("__for_idx_{fid}");
        let Some(head) = body
            .iter()
            .position(|ins| ins[0] == "load" && ins.get(2) == Some(&idx_slot.as_str()))
            .and_then(|i| body[..i].iter().rposition(|ins| ins[0] == "label"))
            .and_then(|i| body[i].get(1).copied())
        else {
            continue;
        };
        let Some(end) = body.iter().rposition(|ins| ins[0] == "jmp" && ins.get(1) == Some(&head)) else { continue };
        if end <= start {
            continue;
        }
        let region = &body[start + 1..end];
        let fresh = fresh_allocations(body);
        // track registers pre-loaded from `var` (and label positions inside
        // `region` for inner-loop back-edges).
        let mut loaded_regs: HashSet<&str> = HashSet::new();
        let mut label_pos: HashMap<&str, usize> = HashMap::new();
        for (idx, ins) in region.iter().enumerate() {
            if ins[0] == "label" && ins.len() >= 2 {
                label_pos.insert(ins[1], idx);
            }
        }
        let mut inner_loops: Vec<(usize, usize)> = Vec::new();
        for (idx, ins) in region.iter().enumerate() {
            let targets: &[&str] = match ins[0] {
                "jmp" if ins.len() >= 2 => &[ins[1]],
                "brif" if ins.len() >= 4 => &[ins[2], ins[3]],
                _ => &[],
            };
            for target in targets {
                if let Some(&lpos) = label_pos.get(target) {
                    if lpos < idx {
                        inner_loops.push((lpos, idx));
                    }
                }
            }
        }
        let uses_var_or_reg = |ins: &[&str], regs: &HashSet<&str>| -> bool {
            if ins[0] == "load" && ins.get(2) == Some(&var) {
                return true;
            }
            if regs.is_empty() {
                return false;
            }
            let operands: &[&str] = match ins[0] {
                "call" if ins.len() > 5 => &ins[5..],
                "calli" if ins.len() > 5 => &ins[5..],
                "rcall" if ins.len() > 6 => &ins[6..],
                "store" if ins.len() >= 3 => &ins[2..3],
                "sstore" if ins.len() >= 4 => &[ins[1], ins[3]],
                "field" if ins.len() >= 3 => &ins[2..3],
                "optval" if ins.len() >= 3 => &ins[2..3],
                "concat" | "eq" | "neq" | "lt" | "gt" | "lte" | "gte" | "add" | "sub" | "mul" | "div" if ins.len() >= 4 => &ins[2..4],
                "concatn" if ins.len() >= 4 => &ins[3..],
                "appendn" if ins.len() >= 4 => &ins[3..],
                "ret" if ins.len() >= 2 => &ins[1..2],
                "ret2" if ins.len() >= 3 => &ins[2..3],
                "brif" if ins.len() >= 2 => &ins[1..2],
                _ => &[],
            };
            operands.iter().any(|op| regs.contains(op))
        };
        let mut reported = false;
        let mut first_writer: Option<usize> = None;
        for (idx, ins) in region.iter().enumerate() {
            if let Some(w) = first_writer {
                if uses_var_or_reg(ins, &loaded_regs) {
                    problems.push(format!(
                        "{name}: {var} borrows from {from}, and `{}` can drop the element before a later read",
                        region[w].join(" ")
                    ));
                    reported = true;
                    break;
                }
            }
            if ins[0] == "load" && ins.len() >= 3 && ins[2] == var {
                loaded_regs.insert(ins[1]);
            }
            if first_writer.is_none() && instruction_writes(ins, &globals, &user, &writers, &fresh) {
                first_writer = Some(idx);
            }
        }
        // an inner loop containing both a write and a read of `var` can drop
        // `var` on one iteration and read it on the next via its back-edge.
        if !reported {
            for (lstart, lend) in inner_loops {
                let inner = &region[lstart..=lend];
                if let Some(w) = inner.iter().position(|ins| instruction_writes(ins, &globals, &user, &writers, &fresh)) {
                    let mut inner_regs = HashSet::new();
                    let has_read = inner.iter().any(|ins| {
                        let hit = uses_var_or_reg(ins, &inner_regs);
                        if ins[0] == "load" && ins.len() >= 3 && ins[2] == var {
                            inner_regs.insert(ins[1]);
                        }
                        hit
                    });
                    if has_read {
                        problems.push(format!(
                            "{name}: {var} borrows from {from}, and `{}` can drop the element before a later read",
                            inner[w].join(" ")
                        ));
                        break;
                    }
                }
            }
        }
        }
    }
    problems
}
