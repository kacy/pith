//! The count checker's balance rule: every reference count a function takes
//! is given up exactly once on every path out of it.
//!
//! It reads the IR before the consumer drops its `mark` lines (see
//! docs/ir-contract.md, "count marks"). The marks say what the instructions
//! leave out: each slot's kind, and which locals the exit cleanup releases.
//! What each runtime call does with counts comes from runtime_ownership.txt,
//! not from the emitter, so a wrong decision in the emitter shows up as a
//! count that doesn't balance.
//!
//! The model: a counted value is an abstract object, and the function holds
//! some number of counts on it. A borrow (a parameter, a field read, an
//! element get) holds none. A retain adds one; a release, a consuming call,
//! a struct field store and a return each take one away. A slot the cleanup
//! releases, and a global, holds exactly one count of its value or a null.
//! Registers never cross a block boundary, so at a boundary every object is
//! named by the slots that hold it, and a count nothing holds is a leak.

use std::collections::BTreeMap;

mod balance;
mod cfg;
mod domain;
mod ir;
mod loop_borrows;
mod untagged_stores;

#[cfg(test)]
mod tests;

pub use balance::check_counts;
pub use loop_borrows::check_loop_borrows;
pub use untagged_stores::check_untagged_stores;

/// Run all three count/borrow/untagged-store analyses over a single parsed IR
/// representation so `PITH_CHECK_COUNTS=1` tokenizes the IR once instead of
/// three times.
pub fn check_all(lines: &[&str]) -> (CountReport, Vec<String>, Vec<String>) {
    let parsed = ir::ParsedIr::parse(lines);
    let report = balance::check_counts_parsed(&parsed);
    let loop_problems = loop_borrows::check_loop_borrows_parsed(&parsed);
    let untagged_problems = untagged_stores::check_untagged_stores_parsed(&parsed);
    (report, loop_problems, untagged_problems)
}

/// retains are `b` in the ownership table; the checker knows them by name
pub(super) const RETAINS: &[&str] = &[
    "pith_cstring_retain",
    "pith_list_retain_handle",
    "pith_map_retain_handle",
    "pith_set_retain_handle",
    "pith_closure_retain",
    "pith_struct_retain",
    "pith_bytes_retain",
    "pith_sync_retain",
    "pith_channel_retain",
];

/// the plain releases: each gives up one count of its argument
pub(super) const RELEASES: &[&str] = &[
    "pith_cstring_release",
    "pith_list_release_handle",
    "pith_map_release_handle",
    "pith_set_release_handle",
    "pith_closure_release",
    "pith_struct_release",
    "pith_bytes_release",
    "pith_sync_release",
    "pith_channel_release",
];

// a task handle is in the list too: the runtime keeps no count on it
// (runtime_ownership.txt, pith_await and pith_task_detach), so a returned
// handle that is awaited or detached has nothing to give back
// the fixed-width integer types reach the IR under their source names
// (`func f 1 UInt64`, `mark kind x Int8`), and are plain words like `int`
pub(super) const SCALAR_KINDS: &[&str] = &[
    "int", "bool", "float", "void", "uint", "byte", "char", "i32", "u8", "task", "UInt", "Int8",
    "Int16", "Int32", "Int64", "UInt8", "UInt16", "UInt32", "UInt64",
];

/// the callee name a `calli` is checked under: no function can be named this
pub(super) const CALLI_CALLEE: &str = "<calli>";

/// instructions that make a plain value from plain operands
pub(super) const PLAIN_OPS: &[&str] = &[
    "iconst", "fconst", "add", "sub", "mul", "div", "mod", "lt", "gt", "lte", "gte", "eq", "neq",
    "addc", "subc", "mulc", "uaddc", "usubc", "umulc", "addnsw", "udiv", "urem", "ult", "ugt",
    "ulte", "ugte", "and", "or", "xor", "shl", "shr", "ushr", "not", "neg", "band", "bor", "bxor",
    "fadd", "fsub", "fmul", "fdiv", "flt", "fgt", "flte", "fgte", "feq", "fneq", "optflag",
    "funcref", "closure_ref", "strref",
];

/// the release helpers the emitter writes into each module: a tuple's
/// cascade, a fallible argument's release, and the release that dispatches
/// on a side slot's kind code. each gives up its first argument's count.
pub(super) fn is_emitted_release(callee: &str) -> bool {
    callee.contains("__tuple_cascade_")
        || callee.contains("__result_arg_release_")
        || callee.contains("__kinded_release_m")
}

/// the helper the emitter writes into each module to hand back a result box
/// the caller owns alone (`ir_emit_result_box_unshare`)
pub(super) fn is_emitted_unshare(callee: &str) -> bool {
    callee.contains("__result_unshare_")
}

/// a kind a generic body left unsubstituted, such as `T`: it may be counted
/// or not, so the checker can't settle a count on it
pub(super) fn kind_of_text(kind: &str) -> &str {
    let unsubstituted =
        kind.len() <= 2 && kind.chars().next().is_some_and(|c| c.is_ascii_uppercase());
    if unsubstituted {
        "unknown"
    } else {
        kind
    }
}

pub(super) fn is_counted(kind: &str) -> bool {
    !SCALAR_KINDS.contains(&kind) && kind != "unknown" && !kind.starts_with("result_reg")
}

#[derive(Default, Debug)]
pub struct CountReport {
    pub passed: usize,
    pub failed: usize,
    pub undecided: usize,
    pub unmarked: usize,
    /// "function: problem" for each failing function
    pub problems: Vec<String>,
    /// why the undecided functions couldn't be settled, with counts
    pub undecided_reasons: BTreeMap<String, usize>,
    /// for each function undecided at a join, what disagreed there
    pub join_details: Vec<String>,
    /// the functions that balance, for tools that check the checker
    pub balanced_names: Vec<String>,
    /// "function: reason, at `instruction`" for each undecided function
    pub undecided_names: Vec<String>,
}

impl CountReport {
    pub fn summary(&self) -> String {
        format!(
            "count check: {} balanced, {} unbalanced, {} undecided, {} without marks",
            self.passed, self.failed, self.undecided, self.unmarked
        )
    }
}
