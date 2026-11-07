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

use crate::runtime_ownership::{runtime_ownership, ArgOwnership, ResultValue};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// retains are `b` in the ownership table; the checker knows them by name
const RETAINS: &[&str] = &[
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

const SCALAR_KINDS: &[&str] = &["int", "bool", "float", "void", "uint", "byte", "char", "i32", "u8"];

/// instructions that make a plain value from plain operands
const PLAIN_OPS: &[&str] = &[
    "iconst", "fconst", "add", "sub", "mul", "div", "mod", "lt", "gt", "lte", "gte", "eq", "neq",
    "and", "or", "xor", "shl", "shr", "ushr", "not", "neg", "band", "bor", "bxor", "fadd", "fsub",
    "fmul", "fdiv", "flt", "fgt", "flte", "fgte", "feq", "fneq", "optflag", "funcref",
    "closure_ref", "strref",
];

/// the release helpers the emitter writes into each module: a tuple's
/// cascade, a fallible argument's release, and the release that dispatches
/// on a side slot's kind code. each gives up its first argument's count.
fn is_emitted_release(callee: &str) -> bool {
    callee.contains("__tuple_cascade_") || callee.contains("__result_arg_release_") || callee.contains("__kinded_release_m")
}

/// a kind a generic body left unsubstituted, such as `T`: it may be counted
/// or not, so the checker can't settle a count on it
fn kind_of_text(kind: &str) -> &str {
    let unsubstituted = kind.len() <= 2 && kind.chars().next().is_some_and(|c| c.is_ascii_uppercase());
    if unsubstituted { "unknown" } else { kind }
}

fn is_counted(kind: &str) -> bool {
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
}

impl CountReport {
    pub fn summary(&self) -> String {
        format!(
            "count check: {} balanced, {} unbalanced, {} undecided, {} without marks",
            self.passed, self.failed, self.undecided, self.unmarked
        )
    }
}

struct Func<'a> {
    name: &'a str,
    ret_kind: &'a str,
    params: Vec<&'a str>,
    kinds: HashMap<&'a str, &'a str>,
    owns: HashSet<&'a str>,
    mixed: HashSet<&'a str>,
    marked: bool,
    body: Vec<Vec<&'a str>>,
}

/// check every function in the IR text and report what was found
pub fn check_counts(lines: &[&str]) -> CountReport {
    let mut globals: HashMap<&str, &str> = HashMap::new();
    let mut user_funcs: HashSet<&str> = HashSet::new();
    let mut funcs: Vec<Func> = Vec::new();
    let mut cur: Option<Func> = None;
    for line in lines {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        match parts[0] {
            "mark" if parts.len() >= 4 && parts[1] == "global" => {
                globals.insert(parts[2], parts[3]);
            }
            "func" if parts.len() >= 2 => {
                user_funcs.insert(parts[1]);
                cur = Some(Func {
                    name: parts[1],
                    ret_kind: parts.get(3).copied().unwrap_or("void"),
                    params: Vec::new(),
                    kinds: HashMap::new(),
                    owns: HashSet::new(),
                    mixed: HashSet::new(),
                    marked: false,
                    body: Vec::new(),
                });
            }
            "endfunc" => {
                if let Some(f) = cur.take() {
                    funcs.push(f);
                }
            }
            _ => {
                let Some(f) = cur.as_mut() else { continue };
                if parts[0] == "mark" && parts.len() >= 3 {
                    f.marked = true;
                    match parts[1] {
                        "kind" if parts.len() >= 4 => {
                            f.kinds.insert(parts[2], kind_of_text(parts[3]));
                        }
                        "owns" if parts.len() >= 4 => {
                            f.owns.insert(parts[2]);
                            f.kinds.entry(parts[2]).or_insert(parts[3]);
                        }
                        "mixed" => {
                            f.mixed.insert(parts[2]);
                        }
                        _ => {}
                    }
                    continue;
                }
                if parts[0] == "param" && parts.len() >= 2 {
                    f.params.push(parts[1]);
                }
                f.body.push(parts);
            }
        }
    }

    let mut report = CountReport::default();
    for f in &funcs {
        if !f.marked {
            report.unmarked += 1;
            continue;
        }
        let mut checker = FuncCheck::new(f, &globals, &user_funcs);
        match checker.run() {
            Err(reason) => {
                report.undecided += 1;
                if !checker.join_detail.is_empty() {
                    report.join_details.push(format!("{}: {}", f.name, checker.join_detail));
                }
                *report.undecided_reasons.entry(reason).or_insert(0) += 1;
            }
            Ok(()) if checker.problems.is_empty() => {
                report.passed += 1;
                report.balanced_names.push(f.name.to_string());
            }
            Ok(()) => {
                report.failed += 1;
                for p in checker.problems {
                    report.problems.push(format!("{}: {}", f.name, p));
                }
            }
        }
    }
    report
}

#[derive(Clone, Debug, PartialEq)]
enum Val {
    /// not a counted value
    Plain,
    /// a null handle: releasing it does nothing
    Null,
    /// a value of a kind the checker can't settle
    Unknown,
    Obj(String),
}

#[derive(Clone, Copy, PartialEq)]
enum SlotClass {
    /// the cleanup releases it, or it's a global: holds one count or null
    Holder,
    /// a counted slot that holds no count of its own
    View,
    Scalar,
    /// an emitter temp with no mark: carries whatever was stored
    Carrier,
}

#[derive(Clone, Default)]
struct State {
    counts: BTreeMap<String, i64>,
    slots: BTreeMap<String, Val>,
    regs: HashMap<String, Val>,
    /// objects a null may stand in for: paths joined where one side held the
    /// object and the other a null, and releasing a null does nothing
    maybe_null: HashSet<String>,
    /// for a field read: the field it read, as "base:index", and when
    field_of: HashMap<String, (String, usize)>,
    /// fields whose value was released through a read and not yet replaced
    emptied: BTreeSet<String>,
    /// tuple shells built without a destructor: freeing one frees the shell
    /// alone, and the counts its fields held pass to whoever read them out
    shells: BTreeSet<String>,
    /// the shell fields, as "shell:index", that hold an owned payload
    owned_fields: BTreeSet<String>,
    /// a register that tests an object against null: test register ->
    /// (object, whether a set test means the object is there)
    null_tests: BTreeMap<String, (String, bool)>,
}

/// the state at a block boundary. an object a slot holds is named by its
/// slots, so two paths that put different values in the same slots agree. an
/// object only registers hold keeps the name of the register that made it:
/// each register is defined once in a function, so the name is the same on
/// every path, and a register used past a block boundary is one whose
/// definition dominates the use.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
struct Canon {
    /// slot-held objects: count held, and whether a null may stand in
    objs: BTreeMap<Vec<String>, (i64, bool)>,
    /// register-only objects that hold a count
    regobjs: BTreeMap<String, i64>,
    /// what each live register refers to: an object's key, or "null"
    regs: BTreeMap<String, String>,
    nulls: BTreeSet<String>,
    unknowns: BTreeSet<String>,
    emptied: BTreeSet<String>,
    shells: BTreeSet<String>,
    owned_fields: BTreeSet<String>,
    /// view slots that share an owned slot's value: view -> owned slot
    aliases: BTreeMap<String, String>,
}

/// where two paths meet. a slot holding one object on one side and a null on
/// the other becomes that object or a null. a register only one side defines
/// can't be used past the join, so a count it holds there is a leak. any
/// other disagreement is one the checker can't settle.
fn join(a: &Canon, b: &Canon, leaks: &mut Vec<String>) -> Result<Canon, String> {
    if a == b {
        return Ok(a.clone());
    }
    if a.unknowns != b.unknowns {
        return Err(format!("slots of unknown kind differ: {:?} vs {:?}", a.unknowns, b.unknowns));
    }
    if a.emptied != b.emptied {
        return Err(format!("emptied fields differ: {:?} vs {:?}", a.emptied, b.emptied));
    }
    let mut a_nulls = a.nulls.clone();
    let mut b_nulls = b.nulls.clone();
    let mut out = Canon {
        unknowns: a.unknowns.clone(),
        emptied: a.emptied.clone(),
        shells: a.shells.union(&b.shells).cloned().collect(),
        owned_fields: a.owned_fields.union(&b.owned_fields).cloned().collect(),
        // a view only one path pointed at a value is dead past the join
        aliases: a.aliases.iter().filter(|(v, h)| b.aliases.get(*v) == Some(*h)).map(|(v, h)| (v.clone(), h.clone())).collect(),
        ..Canon::default()
    };
    // a slot that holds a different value with one count on each path
    // (`x := f() catch fresh()`, the payload on one path and the default on
    // the other) holds one count past the join, whichever value it is. the
    // slots the two values share keep it; the others held the value on
    // their path only and can't be read past here.
    let one_count_only = |x: &Canon, y: &Canon| -> Vec<Vec<String>> {
        x.objs.iter().filter(|(k, v)| v.0 == 1 && !y.objs.contains_key(*k)).map(|(k, _)| k.clone()).collect()
    };
    let (a_one, b_one) = (one_count_only(a, b), one_count_only(b, a));
    let mut paired: HashSet<Vec<String>> = HashSet::new();
    // what a register naming either paired value names past the join
    let mut renamed: HashMap<String, String> = HashMap::new();
    for ka in &a_one {
        let partners: Vec<&Vec<String>> = b_one.iter().filter(|kb| kb.iter().any(|s| ka.contains(s))).collect();
        if let [kb] = partners[..] {
            let shared: Vec<String> = ka.iter().filter(|s| kb.contains(s)).cloned().collect();
            if out.objs.contains_key(&shared) || paired.contains(kb) {
                continue;
            }
            let key = format!("@{}", shared.join("|"));
            renamed.insert(format!("@{}", ka.join("|")), key.clone());
            renamed.insert(format!("@{}", kb.join("|")), key);
            out.objs.insert(shared, (1, a.objs[ka].1 || b.objs[kb].1));
            paired.insert(ka.clone());
            paired.insert(kb.clone());
        }
    }
    let keys: BTreeSet<&Vec<String>> = a.objs.keys().chain(b.objs.keys()).filter(|k| !paired.contains(*k)).collect();
    for k in keys {
        match (a.objs.get(k), b.objs.get(k)) {
            (Some(x), Some(y)) if x.0 == y.0 => {
                out.objs.insert(k.clone(), (x.0, x.1 || y.1));
            }
            (Some(x), None) if k.len() == 1 && x.0 <= 1 && b_nulls.remove(&k[0]) => {
                out.objs.insert(k.clone(), (x.0, true));
            }
            (None, Some(y)) if k.len() == 1 && y.0 <= 1 && a_nulls.remove(&k[0]) => {
                out.objs.insert(k.clone(), (y.0, true));
            }
            // a borrow one path made and the other never did: it holds no
            // count, and a slot only one path wrote can't be read past here
            (Some(x), None) | (None, Some(x)) if x.0 == 0 && !k.iter().any(|s| a_nulls.contains(s) || b_nulls.contains(s)) => {}
            // a count one path holds in slots the other never wrote, or wrote
            // a plain value to (a literal): a slot survives the join and can be
            // read past it, so the count is kept as one count or nothing. one
            // never given up is still a leak at the return.
            (Some(x), None) | (None, Some(x)) if x.0 == 1 && !k.iter().any(|s| a_nulls.contains(s) || b_nulls.contains(s)) => {
                out.objs.insert(k.clone(), (1, true));
            }
            (x, y) => return Err(format!("{} holds {:?} on one path and {:?} on the other", k.join("|"), x, y)),
        }
    }
    // a null holds nothing, so a slot null on one path and never written on
    // the other agrees; the slot is kept null only where both paths say so.
    // a slot one path left null and the other gave a count is the holder
    // case handled above.
    let held_keys: BTreeSet<&String> = out.objs.keys().flatten().collect();
    if a_nulls.iter().chain(b_nulls.iter()).any(|n| held_keys.contains(n)) {
        return Err(format!("nulls differ: {:?} vs {:?}", a_nulls, b_nulls));
    }
    out.nulls = a_nulls.intersection(&b_nulls).cloned().collect();
    let ids: BTreeSet<&String> = a.regobjs.keys().chain(b.regobjs.keys()).collect();
    for id in ids {
        match (a.regobjs.get(id), b.regobjs.get(id)) {
            (Some(x), Some(y)) if x == y => {
                out.regobjs.insert(id.clone(), *x);
            }
            (Some(x), Some(y)) => return Err(format!("register object {id} holds {x} on one path and {y} on the other")),
            (Some(n), None) | (None, Some(n)) => {
                if *n > 0 {
                    leaks.push(id.clone());
                }
            }
            (None, None) => {}
        }
    }
    let obj_keys: HashSet<String> = out.objs.keys().map(|k| format!("@{}", k.join("|"))).collect();
    let live = |key: &String| key == "null" || out.regobjs.contains_key(key) || obj_keys.contains(key);
    // values the join made "one count or nothing"
    let maybe: HashSet<String> = out.objs.iter().filter(|(_, v)| v.1).map(|(k, _)| format!("@{}", k.join("|"))).collect();
    let mut regs = BTreeMap::new();
    let names: BTreeSet<&String> = a.regs.keys().chain(b.regs.keys()).collect();
    let follow = |k: Option<&String>| k.map(|k| renamed.get(k).unwrap_or(k).clone());
    for r in names {
        let merged = match (follow(a.regs.get(r)).as_ref(), follow(b.regs.get(r)).as_ref()) {
            (Some(x), Some(y)) if x == y => Some(x.clone()),
            // a register read from a slot that held a null on one path and a
            // value on the other: the join made that value "one count or a
            // null", and the register follows it
            (Some(x), Some(y)) if x == "null" && live(y) => Some(y.clone()),
            (Some(x), Some(y)) if y == "null" && live(x) => Some(x.clone()),
            // a register's definition dominates its uses, so one missing on
            // one side held a plain value there (a literal read from the same
            // slot). it follows the value the same way a null does.
            (Some(x), None) | (None, Some(x)) if maybe.contains(x) => Some(x.clone()),
            _ => None,
        };
        if let Some(k) = merged {
            if live(&k) {
                regs.insert(r.clone(), k);
            }
        }
    }
    out.regs = regs;
    Ok(out)
}

struct FuncCheck<'a, 'f> {
    f: &'f Func<'a>,
    globals: &'f HashMap<&'a str, &'a str>,
    user_funcs: &'f HashSet<&'a str>,
    scalar_temps: HashSet<&'a str>,
    /// the integer constant each `iconst` register holds, for the slot number
    /// a thread-local call names
    consts: HashMap<&'a str, &'a str>,
    problems: Vec<String>,
    /// the instruction that made each object, for the messages
    origins: HashMap<String, String>,
    current: String,
    current_dst: String,
    /// field reads so far, to order them
    reads: usize,
    /// what disagreed at the join that left the function undecided
    join_detail: String,
}

impl<'a, 'f> FuncCheck<'a, 'f> {
    fn new(f: &'f Func<'a>, globals: &'f HashMap<&'a str, &'a str>, user_funcs: &'f HashSet<&'a str>) -> Self {
        let mut c = FuncCheck { f, globals, user_funcs, scalar_temps: HashSet::new(), consts: HashMap::new(), problems: Vec::new(), origins: HashMap::new(), current: String::new(), current_dst: String::new(), reads: 0, join_detail: String::new() };
        c.scalar_temps = c.infer_scalar_temps();
        c.consts = f.body.iter().filter(|ins| ins[0] == "iconst" && ins.len() >= 3).map(|ins| (ins[1], ins[2])).collect();
        c
    }

    fn kind_of(&self, name: &str) -> Option<&'a str> {
        self.globals.get(name).copied().or_else(|| self.f.kinds.get(name).copied())
    }

    /// an unmarked slot whose every store is a plain value is a scalar the
    /// emitter made for itself, such as a short-circuit flag
    fn infer_scalar_temps(&self) -> HashSet<&'a str> {
        let mut plain_regs: HashSet<&str> = HashSet::new();
        let mut loads: Vec<(&str, &str)> = Vec::new();
        let mut stores: HashMap<&str, Vec<&str>> = HashMap::new();
        for ins in &self.f.body {
            let op = ins[0];
            if PLAIN_OPS.contains(&op) && op != "strref" && op != "closure_ref" && ins.len() > 1 {
                plain_regs.insert(ins[1]);
            } else if op == "call" && ins.len() > 3 {
                if !is_counted(ins[3]) && ins[3] != "unknown" {
                    plain_regs.insert(ins[1]);
                }
            } else if op == "field" && ins.len() > 4 {
                if !is_counted(ins[4]) && ins[4] != "unknown" {
                    plain_regs.insert(ins[1]);
                }
            } else if op == "load" && ins.len() > 2 {
                match self.kind_of(ins[2]) {
                    Some(k) if !is_counted(k) && k != "unknown" => {
                        plain_regs.insert(ins[1]);
                    }
                    None => loads.push((ins[1], ins[2])),
                    _ => {}
                }
            } else if op == "store" && ins.len() > 2 {
                stores.entry(ins[1]).or_default().push(ins[2]);
            }
        }
        let mut scalars: HashSet<&str> = HashSet::new();
        loop {
            for (reg, slot) in &loads {
                if scalars.contains(slot) {
                    plain_regs.insert(reg);
                }
            }
            let now: HashSet<&str> = stores
                .iter()
                .filter(|(slot, regs)| self.kind_of(slot).is_none() && regs.iter().all(|r| plain_regs.contains(r)))
                .map(|(slot, _)| *slot)
                .collect();
            if now == scalars {
                return scalars;
            }
            scalars = now;
        }
    }

    fn class_of(&self, name: &str) -> SlotClass {
        if self.globals.contains_key(name) {
            return if is_counted(self.globals[name]) { SlotClass::Holder } else { SlotClass::Scalar };
        }
        if self.f.owns.contains(name) {
            return SlotClass::Holder;
        }
        match self.f.kinds.get(name) {
            Some(k) if is_counted(k) => SlotClass::View,
            Some(k) if *k != "unknown" => SlotClass::Scalar,
            Some(_) => SlotClass::Carrier,
            None if self.scalar_temps.contains(name) => SlotClass::Scalar,
            None => SlotClass::Carrier,
        }
    }

    /// the object the current instruction makes, named by its destination
    /// register
    fn new_obj(&mut self, st: &mut State, count: i64) -> String {
        let o = format!("r{}", self.current_dst);
        let slot_held = st.slots.values().any(|v| *v == Val::Obj(o.clone()));
        if !slot_held && st.counts.get(&o).copied().unwrap_or(0) > 0 {
            let from = self.origins.get(&o).cloned().unwrap_or_default();
            self.problems.push(format!("a count is still held when `{from}` runs again (leak)"));
        }
        st.counts.insert(o.clone(), count);
        st.maybe_null.remove(&o);
        self.origins.insert(o.clone(), self.current.clone());
        o
    }

    // ---- canonical state at a block boundary ----

    fn canon(&mut self, st: &State, at: &str) -> Canon {
        let mut holders: BTreeMap<&String, Vec<String>> = BTreeMap::new();
        for (slot, v) in &st.slots {
            if let Val::Obj(o) = v {
                holders.entry(o).or_default().push(slot.clone());
            }
        }
        let referenced: HashSet<&String> = st
            .regs
            .values()
            .filter_map(|v| if let Val::Obj(o) = v { Some(o) } else { None })
            .collect();
        let mut out = Canon::default();
        let mut key_of: HashMap<&String, String> = HashMap::new();
        for (o, n) in &st.counts {
            if let Some(names) = holders.get(o) {
                let maybe_null = st.maybe_null.contains(o);
                let owners: Vec<&String> = names.iter().filter(|s| self.class_of(s) == SlotClass::Holder).collect();
                if names.len() >= 2 && !owners.is_empty() && *n == owners.len() as i64 {
                    // several slots share one value and each owned slot has a
                    // count of its own (`name = value` with a retain): each
                    // owned slot is its own count, and a view slot sharing the
                    // value is an alias of the first. a path where the slots
                    // hold separate values agrees with this one.
                    for slot in &owners {
                        out.objs.insert(vec![(*slot).clone()], (1, maybe_null));
                    }
                    for slot in names.iter().filter(|s| !owners.contains(s)) {
                        out.aliases.insert(slot.clone(), owners[0].clone());
                    }
                    key_of.insert(o, format!("@{}", owners[0]));
                } else {
                    out.objs.insert(names.clone(), (*n, maybe_null));
                    key_of.insert(o, format!("@{}", names.join("|")));
                }
            } else if *n != 0 && referenced.contains(o) {
                out.regobjs.insert(o.clone(), *n);
                key_of.insert(o, o.clone());
            } else if *n > 0 {
                let from = self.origins.get(o).cloned().unwrap_or_else(|| o.clone());
                self.problems.push(format!("a count is held by nothing at {at} (leak), from `{from}`"));
            } else if *n < 0 {
                self.problems.push(format!("a count was given up twice before {at}"));
            }
        }
        for (r, v) in &st.regs {
            match v {
                Val::Obj(o) => {
                    if let Some(k) = key_of.get(o) {
                        out.regs.insert(r.clone(), k.clone());
                    }
                }
                Val::Null => {
                    out.regs.insert(r.clone(), "null".into());
                }
                _ => {}
            }
        }
        for (slot, v) in &st.slots {
            match v {
                Val::Null => {
                    out.nulls.insert(slot.clone());
                }
                Val::Unknown => {
                    out.unknowns.insert(slot.clone());
                }
                _ => {}
            }
        }
        for o in &st.shells {
            if let Some(k) = key_of.get(o) {
                out.shells.insert(k.clone());
            }
        }
        for field in &st.owned_fields {
            let (base, off) = field.rsplit_once(':').unwrap_or((field.as_str(), ""));
            if let Some(k) = key_of.get(&base.to_string()) {
                out.owned_fields.insert(format!("{k}:{off}"));
            }
        }
        for field in &st.emptied {
            let (base, off) = field.rsplit_once(':').unwrap_or((field.as_str(), ""));
            let base = key_of.get(&base.to_string()).cloned().unwrap_or_else(|| base.to_string());
            out.emptied.insert(format!("{base}:{off}"));
        }
        out
    }

    fn from_canon(&mut self, canon: &Canon) -> State {
        let mut st = State::default();
        for (names, (n, maybe_null)) in &canon.objs {
            let o = format!("@{}", names.join("|"));
            st.counts.insert(o.clone(), *n);
            if *maybe_null {
                st.maybe_null.insert(o.clone());
            }
            for s in names {
                st.slots.insert(s.clone(), Val::Obj(o.clone()));
            }
        }
        for (view, owner) in &canon.aliases {
            if let Some(v) = st.slots.get(owner).cloned() {
                st.slots.insert(view.clone(), v);
            }
        }
        for (id, n) in &canon.regobjs {
            st.counts.insert(id.clone(), *n);
        }
        for (r, key) in &canon.regs {
            let v = if key == "null" { Val::Null } else { Val::Obj(key.clone()) };
            st.regs.insert(r.clone(), v);
        }
        for s in &canon.nulls {
            st.slots.insert(s.clone(), Val::Null);
        }
        for s in &canon.unknowns {
            st.slots.insert(s.clone(), Val::Unknown);
        }
        st.emptied = canon.emptied.clone();
        st.shells = canon.shells.clone();
        st.owned_fields = canon.owned_fields.clone();
        st
    }

    fn run(&mut self) -> Result<(), String> {
        if !self.f.mixed.is_empty() {
            return Err("a local whose kind changes by path".into());
        }
        // a generated JSON decoder keeps its struct in a slot it records as
        // a plain word, so the struct's count can't be followed
        if self.f.kinds.keys().any(|k| k.starts_with("__jf_")) {
            return Err("a JSON decoder's raw word slots".into());
        }
        let (blocks, succ, order) = split_blocks(&self.f.body);
        let mut st = State::default();
        for p in &self.f.params {
            match self.class_of(p) {
                SlotClass::Scalar => {}
                SlotClass::Carrier => {
                    st.slots.insert(p.to_string(), Val::Unknown);
                }
                _ => {
                    // a parameter is borrowed: the caller keeps its count
                    let o = format!("param:{p}");
                    st.counts.insert(o.clone(), 0);
                    st.slots.insert(p.to_string(), Val::Obj(o));
                }
            }
        }
        // the thread-local slots this function names, each holding one count
        // of its value from the start, like a global
        for ins in &self.f.body {
            if let Some(slot) = self.counted_tls_slot(ins) {
                if !st.slots.contains_key(&slot) {
                    st.counts.insert(slot.clone(), 1);
                    st.maybe_null.insert(slot.clone());
                    st.slots.insert(slot.clone(), Val::Obj(slot.clone()));
                }
            }
        }
        // only the globals this function names: the rest never change here,
        // and carrying every global of the program through every block was
        // most of the checker's time on a large program
        let named: HashSet<&str> = self
            .f
            .body
            .iter()
            .filter(|ins| (ins[0] == "load" || ins[0] == "store") && ins.len() >= 3)
            .map(|ins| if ins[0] == "load" { ins[2] } else { ins[1] })
            .collect();
        for (g, k) in self.globals.iter() {
            if is_counted(k) && named.contains(g) {
                // a global holds one count of its value, or a null
                let o = format!("global:{g}");
                st.counts.insert(o.clone(), 1);
                st.maybe_null.insert(o.clone());
                st.slots.insert(g.to_string(), Val::Obj(o));
            }
        }
        let entry = self.canon(&st, "entry");
        let mut seen: HashMap<usize, Canon> = HashMap::new();
        let mut work: Vec<(usize, Canon)> = vec![(0, entry)];
        let mut steps = 0usize;
        while let Some((b, key)) = work.pop() {
            steps += 1;
            if steps > 200_000 {
                return Err("no fixed point".into());
            }
            let key = match seen.get(&b) {
                Some(prev) => match self.join_at(prev, &key, &order[b]) {
                    Ok(merged) if merged == *prev => continue,
                    Ok(merged) => merged,
                    Err(why) => {
                        self.join_detail = format!("at {}: {}", order[b], why);
                        return Err("paths join with different counts".into());
                    }
                },
                None => key,
            };
            seen.insert(b, key.clone());
            let mut st = self.from_canon(&key);
            let mut ended = false;
            for ins in &blocks[b] {
                if self.step(ins, &mut st)? {
                    ended = true;
                    break;
                }
            }
            if ended {
                continue;
            }
            // a branch on whether a value is null settles its count: the null
            // side holds none
            let last = blocks[b].last().copied().unwrap_or(&[]);
            if last.first() == Some(&"brif") && last.len() >= 4 && succ[b].len() == 2 {
                // (object, count when the test is set, count when it isn't)
                let mut settle: Option<(String, Option<i64>, Option<i64>)> = None;
                if let Some((obj, set_means_there)) = st.null_tests.get(last[1]).cloned() {
                    // the null side holds no count; the other keeps its own
                    settle = Some(if set_means_there { (obj, None, Some(0)) } else { (obj, Some(0), None) });
                } else if let Val::Obj(obj) = self.val(&st, last[1]) {
                    settle = Some((obj, None, Some(0)));
                }
                if let Some((obj, when_set, when_clear)) = settle {
                    for (i, s) in succ[b].iter().enumerate() {
                        let mut side = st.clone();
                        if let Some(n) = if i == 0 { when_set } else { when_clear } {
                            side.counts.insert(obj.clone(), n);
                        }
                        side.null_tests.clear();
                        let out = self.canon(&side, &format!("the end of {}", order[b]));
                        work.push((*s, out));
                    }
                    continue;
                }
            }
            let out = self.canon(&st, &format!("the end of {}", order[b]));
            for s in &succ[b] {
                work.push((*s, out.clone()));
            }
        }
        self.problems.sort();
        self.problems.dedup();
        Ok(())
    }

    fn join_at(&mut self, a: &Canon, b: &Canon, label: &str) -> Result<Canon, String> {
        let mut leaks = Vec::new();
        let out = join(a, b, &mut leaks);
        for id in leaks {
            let from = self.origins.get(&id).cloned().unwrap_or(id);
            self.problems.push(format!("a count only one path holds reaches {label} (leak), from `{from}`"));
        }
        out
    }

    /// what a field's base refers to, so a read and a store through two
    /// loads of the same struct name the same field
    fn base_key(&self, st: &State, reg: &str) -> String {
        match self.val(st, reg) {
            Val::Obj(o) => o,
            _ => format!("reg{reg}"),
        }
    }

    fn val(&self, st: &State, reg: &str) -> Val {
        st.regs.get(reg).cloned().unwrap_or(Val::Plain)
    }

    /// give up one count through `reg`
    fn take(&mut self, st: &mut State, reg: &str, why: &str) -> Result<(), String> {
        match self.val(st, reg) {
            Val::Obj(o) => {
                if st.counts.get(&o).copied().unwrap_or(0) <= 0 {
                    if let Some((field, _)) = st.field_of.get(&o).cloned() {
                        if st.owned_fields.remove(&field) {
                            // a destructor-less shell's payload, taken over
                            // and given up here: the shell no longer owns it
                            st.field_of.remove(&o);
                            return Ok(());
                        }
                        let base = field.rsplit_once(':').map(|(b, _)| b.to_string()).unwrap_or_default();
                        if !st.shells.contains(&base) {
                            // the struct's count on the field's value, given
                            // up to replace it: the field must be stored again
                            st.emptied.insert(field);
                            return Ok(());
                        }
                    }
                }
                let n = st.counts.entry(o.clone()).or_insert(0);
                if *n <= 0 {
                    self.problems.push(format!("{why} gives up a count it doesn't hold (register {reg})"));
                }
                *n -= 1;
                if *n == 0 && st.shells.contains(&o) {
                    let prefix = format!("{o}:");
                    let readers: Vec<String> = st
                        .field_of
                        .iter()
                        .filter(|(r, (f, _))| {
                            f.starts_with(&prefix) && st.owned_fields.contains(f) && st.counts.get(*r).copied().unwrap_or(0) == 0
                        })
                        .map(|(r, _)| r.clone())
                        .collect();
                    for r in readers {
                        st.counts.insert(r.clone(), 1);
                        st.field_of.remove(&r);
                    }
                }
                Ok(())
            }
            Val::Unknown => Err("a value of unknown kind meets a count".into()),
            _ => Ok(()),
        }
    }

    fn step(&mut self, ins: &[&str], st: &mut State) -> Result<bool, String> {
        let op = ins[0];
        self.current = ins.join(" ");
        self.current_dst = ins.get(1).copied().unwrap_or("").to_string();
        match op {
            "param" | "label" | "jmp" | "brif" | "runtime_abi" => Ok(false),
            "iconst" if ins.len() >= 3 => {
                let v = if ins[2] == "0" { Val::Null } else { Val::Plain };
                st.regs.insert(ins[1].to_string(), v);
                Ok(false)
            }
            "eq" | "neq" if ins.len() >= 4 => {
                // `neq t x 0`: t says whether x is there
                let (a, b) = (self.val(st, ins[2]), self.val(st, ins[3]));
                let obj = match (a, b) {
                    (Val::Obj(o), Val::Null) | (Val::Null, Val::Obj(o)) => Some(o),
                    _ => None,
                };
                if let Some(o) = obj {
                    st.null_tests.insert(ins[1].to_string(), (o, op == "neq"));
                }
                st.regs.insert(ins[1].to_string(), Val::Plain);
                Ok(false)
            }
            // a channel receive hands back no shell at all when nothing
            // came (runtime_ownership.txt), so its flag says whether the
            // value is there, the same as a null test. other optionals can
            // be a real shell with the flag clear.
            "optflag" if ins.len() >= 3 => {
                if let Val::Obj(o) = self.val(st, ins[2]) {
                    let origin = self.origins.get(&o).map(String::as_str).unwrap_or("");
                    if origin.contains(" pith_channel_try_recv ") || origin.contains(" pith_channel_recv ") {
                        st.null_tests.insert(ins[1].to_string(), (o, true));
                    }
                }
                st.regs.insert(ins[1].to_string(), Val::Plain);
                Ok(false)
            }
            _ if PLAIN_OPS.contains(&op) => {
                if ins.len() > 1 {
                    st.regs.insert(ins[1].to_string(), Val::Plain);
                }
                Ok(false)
            }
            "load" if ins.len() >= 3 => {
                let v = match self.class_of(ins[2]) {
                    SlotClass::Scalar => Val::Plain,
                    _ => match st.slots.get(ins[2]) {
                        Some(v) => v.clone(),
                        None if self.class_of(ins[2]) == SlotClass::Carrier => Val::Plain,
                        None => return Err("a counted slot is read before it is written".into()),
                    },
                };
                st.regs.insert(ins[1].to_string(), v);
                Ok(false)
            }
            "store" if ins.len() >= 3 => {
                let slot = ins[1].to_string();
                let v = self.val(st, ins[2]);
                match self.class_of(ins[1]) {
                    SlotClass::Scalar => {}
                    SlotClass::Holder => {
                        // a holder has one count, or a null its cleanup
                        // releases for nothing
                        let held = match v {
                            Val::Plain => Val::Null,
                            other => other,
                        };
                        st.slots.insert(slot, held);
                    }
                    SlotClass::View | SlotClass::Carrier => {
                        st.slots.insert(slot, v);
                    }
                }
                Ok(false)
            }
            "field" if ins.len() >= 5 => {
                let v = if kind_of_text(ins[4]) == "unknown" {
                    Val::Unknown
                } else if is_counted(ins[4]) {
                    let o = self.new_obj(st, 0);
                    // a read names its field by byte offset, a store by index
                    let index = ins[3].parse::<i64>().map(|b| b / 8).unwrap_or(-1);
                    let field = format!("{}:{}", self.base_key(st, ins[2]), index);
                    self.reads += 1;
                    st.field_of.insert(o.clone(), (field, self.reads));
                    Val::Obj(o)
                } else {
                    Val::Plain
                };
                st.regs.insert(ins[1].to_string(), v);
                Ok(false)
            }
            "sstore" if ins.len() >= 4 => {
                let field = format!("{}:{}", self.base_key(st, ins[1]), ins[2]);
                let into_shell = matches!(self.val(st, ins[1]), Val::Obj(ref b) if st.shells.contains(b));
                let held = match self.val(st, ins[3]) {
                    Val::Obj(o) => st.counts.get(&o).copied().unwrap_or(0) > 0,
                    _ => false,
                };
                if into_shell && !held {
                    // a destructor-less shell can hold a borrowed payload:
                    // freeing the shell leaves the payload alone
                    return Ok(false);
                }
                self.take(st, ins[3], "a struct field store")?;
                if into_shell {
                    st.owned_fields.insert(field.clone());
                }
                if !st.emptied.remove(&field) {
                    // the store displaces the old value: the latest read of
                    // it that held no count now carries the struct's one
                    // count, for this function to give up
                    let displaced = st
                        .field_of
                        .iter()
                        .filter(|(o, (f, _))| *f == field && st.counts.get(*o).copied().unwrap_or(0) == 0)
                        .max_by_key(|(_, (_, seq))| *seq)
                        .map(|(o, _)| o.clone());
                    if let Some(o) = displaced {
                        st.counts.insert(o.clone(), 1);
                    }
                    st.field_of.retain(|_, (f, _)| *f != field);
                }
                Ok(false)
            }
            "concat" | "concatn" | "appendn" if ins.len() >= 2 => {
                let o = self.new_obj(st, 1);
                st.regs.insert(ins[1].to_string(), Val::Obj(o));
                Ok(false)
            }
            "call" if ins.len() >= 5 => self.call(ins, st).map(|_| false),
            // the register-pair abi carries an error payload whose count
            // depends on the flag, and the error handling around it isn't
            // modelled yet
            "rcall" => Err("a call returning the register pair".into()),
            "ret2" => Err("a register-pair return".into()),
            "optval" => Err("instruction optval".into()),
            "tuple_stack" if ins.len() >= 2 => {
                // a shell in the frame: never released, and its payloads go
                // through the stack cascade, which gives up what it took
                let o = self.new_obj(st, 0);
                st.shells.insert(o.clone());
                st.regs.insert(ins[1].to_string(), Val::Obj(o));
                Ok(false)
            }
            "closure_stack" if ins.len() >= 2 => {
                let o = self.new_obj(st, 0);
                st.regs.insert(ins[1].to_string(), Val::Obj(o));
                Ok(false)
            }
            "ret" => {
                self.ret(ins, st);
                Ok(true)
            }
            _ => Err(format!("instruction {op}")),
        }
    }

    /// the thread-local slot a getter or setter names, when its value is
    /// counted. a tagged call says so by its last argument, the element tag
    /// of the value's kind (0 for an uncounted value); an untagged getter by
    /// its result type. an untagged setter alone does not tell.
    fn counted_tls_slot(&self, ins: &[&str]) -> Option<String> {
        if ins[0] != "call" || ins.len() < 6 || !ins[2].starts_with("pith_tls_") {
            return None;
        }
        let counted = if ins[2].ends_with("_tagged") {
            ins.last().and_then(|t| self.consts.get(t)).is_some_and(|t| *t != "0")
        } else {
            ins[2].starts_with("pith_tls_get_or_init") && is_counted(ins[3])
        };
        let slot = self.consts.get(ins[5])?;
        counted.then(|| format!("tls:{slot}"))
    }

    fn call(&mut self, ins: &[&str], st: &mut State) -> Result<(), String> {
        let (dst, callee, typ) = (ins[1], ins[2], ins[3]);
        let args: Vec<&str> = ins.iter().skip(5).copied().collect();
        // a weak reference keeps a second count the checker doesn't model
        // yet, and an upgrade turns it into a strong one
        if callee.starts_with("pith_struct_weak_") {
            return Err("a weak reference".into());
        }
        if callee.contains("__tuple_stack_cascade_") {
            st.regs.insert(dst.to_string(), Val::Plain);
            return Ok(());
        }
        // a thread-local slot holds one count of its value, like a global:
        // the getter hands out that value, and the setter puts a new value
        // in after the old one was released through a getter's result
        if callee.starts_with("pith_tls_get_or_init") || callee.starts_with("pith_tls_set") {
            // a slot of an uncounted type is never seeded and holds nothing
            if let Some(slot) = args.first().and_then(|a| self.consts.get(a)).map(|v| format!("tls:{v}")).filter(|s| st.slots.contains_key(s)) {
                if callee.starts_with("pith_tls_set") {
                    let v = args.get(1).map(|a| self.val(st, a)).unwrap_or(Val::Plain);
                    let held = match v {
                        Val::Null | Val::Plain => Val::Obj(self.new_obj(st, 1)),
                        other => other,
                    };
                    st.slots.insert(slot, held);
                    st.regs.insert(dst.to_string(), Val::Plain);
                    return Ok(());
                }
                if is_counted(typ) {
                    if !st.slots.contains_key(&slot) {
                        st.counts.insert(slot.clone(), 1);
                        st.maybe_null.insert(slot.clone());
                        st.slots.insert(slot.clone(), Val::Obj(slot.clone()));
                    }
                    let v = st.slots.get(&slot).cloned().unwrap_or(Val::Plain);
                    st.regs.insert(dst.to_string(), v);
                    return Ok(());
                }
            }
        }
        if is_emitted_release(callee) {
            if let Some(a) = args.first() {
                self.take(st, a, callee)?;
            }
            st.regs.insert(dst.to_string(), Val::Plain);
            return Ok(());
        }
        if RETAINS.contains(&callee) {
            match self.val(st, args[0]) {
                Val::Obj(o) => *st.counts.entry(o).or_insert(0) += 1,
                Val::Unknown => return Err("a value of unknown kind meets a count".into()),
                _ => {}
            }
            st.regs.insert(dst.to_string(), Val::Plain);
            return Ok(());
        }
        if callee == "pith_struct_set_dtor" {
            if let Some(Val::Obj(o)) = args.first().map(|a| self.val(st, a)) {
                st.shells.remove(&o);
            }
        }
        let result = if let Some(row) = runtime_ownership(callee) {
            for (i, a) in row.args.iter().enumerate() {
                if *a == ArgOwnership::Consume && i < args.len() {
                    self.take(st, args[i], callee)?;
                }
            }
            match row.result.value {
                // a generic runtime call (an await, an element get) says on
                // the call line when this use of it yields a plain value
                _ if SCALAR_KINDS.contains(&typ) => Val::Plain,
                // an await of an unknown kind may be an Int; a count on it
                // can't be settled either way
                ResultValue::Own if typ == "unknown" => Val::Unknown,
                // a row that can't name where its counted result is borrowed
                // from (the thread-local getters hand back the slot's value)
                ResultValue::Plain if is_counted(typ) => Val::Obj(self.new_obj(st, 0)),
                ResultValue::Plain => Val::Plain,
                ResultValue::Own => Val::Obj(self.new_obj(st, 1)),
                ResultValue::Borrow(_) => Val::Obj(self.new_obj(st, 0)),
            }
        } else if self.user_funcs.contains(callee) {
            // a user function borrows its arguments and hands back a count
            // on a counted result
            if typ == "unknown" {
                Val::Unknown
            } else if is_counted(typ) {
                Val::Obj(self.new_obj(st, 1))
            } else {
                Val::Plain
            }
        } else {
            return Err("a call the checker has no summary for".into());
        };
        if callee == "pith_struct_alloc" && (typ == "struct:tuple" || typ == "tuple") {
            if let Val::Obj(o) = &result {
                st.shells.insert(o.clone());
            }
        }
        st.regs.insert(dst.to_string(), result);
        Ok(())
    }

    fn ret(&mut self, ins: &[&str], st: &mut State) {
        if !st.emptied.is_empty() {
            self.problems.push("releases a field's value and returns without storing a new one (it dangles)".into());
        }
        let mut counts = st.counts.clone();
        // a function returning a tuple has `unknown` on its func line, but
        // its callers still take the result as owned, so a return hands off
        // one count either way
        if ins.len() > 1 && (is_counted(self.f.ret_kind) || self.f.ret_kind == "unknown") {
            if let Val::Obj(o) = self.val(st, ins[1]) {
                *counts.entry(o).or_insert(0) -= 1;
            }
        }
        // each global holding an object keeps one count of it
        let mut global_holds: HashMap<&String, i64> = HashMap::new();
        for (s, v) in &st.slots {
            if let Val::Obj(o) = v {
                if self.globals.contains_key(s.as_str()) || s.starts_with("tls:") {
                    *global_holds.entry(o).or_insert(0) += 1;
                }
            }
        }
        for (o, n) in counts {
            let expected = global_holds.get(&o).copied().unwrap_or(0);
            let from = self.origins.get(&o).cloned().unwrap_or_else(|| o.clone());
            if n > expected {
                self.problems.push(format!("returns holding {} count(s) it never gave up (leak), from `{from}`", n - expected));
            } else if n < expected {
                self.problems.push(format!("gives up {} count(s) more than it took, on `{from}`", expected - n));
            }
        }
    }
}

/// the function body as basic blocks: their instructions, successors and
/// names
fn split_blocks<'a, 'b>(body: &'b [Vec<&'a str>]) -> (Vec<Vec<&'b [&'a str]>>, Vec<Vec<usize>>, Vec<String>) {
    let mut names: Vec<String> = vec!["entry".to_string()];
    let mut blocks: Vec<Vec<&'b [&'a str]>> = vec![Vec::new()];
    for ins in body {
        if ins[0] == "label" && ins.len() >= 2 {
            names.push(ins[1].to_string());
            blocks.push(Vec::new());
            continue;
        }
        if let Some(block) = blocks.last_mut() {
            block.push(ins.as_slice());
        }
    }
    let index: HashMap<&str, usize> = names.iter().enumerate().map(|(i, n)| (n.as_str(), i)).collect();
    let mut succ = Vec::new();
    for (i, b) in blocks.iter().enumerate() {
        let next = match b.last() {
            Some(last) if last[0] == "jmp" && last.len() >= 2 => index.get(last[1]).copied().into_iter().collect(),
            Some(last) if last[0] == "brif" && last.len() >= 4 => [last[2], last[3]].iter().filter_map(|l| index.get(l).copied()).collect(),
            Some(last) if last[0] == "ret" || last[0] == "ret2" => Vec::new(),
            // falls through into the next label
            _ if i + 1 < blocks.len() => vec![i + 1],
            _ => Vec::new(),
        };
        succ.push(next);
    }
    (blocks, succ, names)
}

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
    "clear", "remove", "pop", "set_value", "take", "truncate", "drain", "store_u64", "tls_set", "set_env",
];

fn runtime_call_writes(callee: &str) -> bool {
    if callee.contains("release") || callee.contains("retain") {
        return false;
    }
    EVICTING_NAME_PARTS.iter().any(|p| callee.contains(p))
}

/// the functions that can drop a stored value, by name, worked out to a fixed
/// point over the calls between them
fn storage_writers(funcs: &[(&str, Vec<Vec<&str>>)], globals: &HashSet<&str>) -> HashSet<String> {
    let user: HashSet<&str> = funcs.iter().map(|(n, _)| *n).collect();
    let mut writers: HashSet<String> = HashSet::new();
    loop {
        let mut changed = false;
        for (name, body) in funcs {
            if writers.contains(*name) {
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
        "call" | "rcall" => {
            let callee = if ins[0] == "call" { ins.get(2) } else { ins.get(3) };
            let Some(callee) = callee else { return true };
            // an emitted release helper drops only its own argument
            if is_emitted_release(callee) || callee.contains("__tuple_stack_cascade_") {
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
    let mut globals: HashSet<&str> = HashSet::new();
    let mut funcs: Vec<(&str, Vec<Vec<&str>>)> = Vec::new();
    let mut borrows: Vec<(usize, &str, &str)> = Vec::new();
    for line in lines {
        let parts: Vec<&str> = line.split_whitespace().collect();
        match parts.first() {
            Some(&"global") if parts.len() >= 2 => {
                globals.insert(parts[1]);
            }
            Some(&"func") if parts.len() >= 2 => funcs.push((parts[1], Vec::new())),
            Some(&"mark") if parts.len() >= 4 && parts[1] == "borrow" => {
                if !funcs.is_empty() {
                    borrows.push((funcs.len() - 1, parts[2], parts[3]));
                }
            }
            Some(&"endfunc") | Some(&"mark") | None => {}
            Some(_) => {
                if let Some(f) = funcs.last_mut() {
                    f.1.push(parts);
                }
            }
        }
    }
    let writers = storage_writers(&funcs, &globals);
    let user: HashSet<&str> = funcs.iter().map(|(n, _)| *n).collect();
    let mut problems = Vec::new();
    for (fi, var, from) in borrows {
        let (name, body) = &funcs[fi];
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
        let Some(w) = region.iter().position(|ins| instruction_writes(ins, &globals, &user, &writers, &fresh)) else { continue };
        if region[w + 1..].iter().any(|ins| ins[0] == "load" && ins.get(2) == Some(&var)) {
            problems.push(format!(
                "{name}: {var} borrows from {from}, and `{}` can drop the element before a later read",
                region[w].join(" ")
            ));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(ir: &str) -> CountReport {
        let lines: Vec<&str> = ir.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        check_counts(&lines)
    }

    // fn first_name(xs: List[Node]) -> String: for n in xs: return n.name
    const FIRST_NAME: &str = "
        func first_name 1 string
        param xs
        load 1 xs
        call 2 pith_auto_len int 1 1
        iconst 3 0
        store __for_idx_0 3
        store __for_len_0 2
        store __for_iter_0 1
        label L0
        load 4 __for_idx_0
        load 5 __for_len_0
        lt 6 4 5
        brif 6 L1 L3
        label L1
        load 7 __for_iter_0
        load 8 __for_idx_0
        call 9 pith_list_get_value unknown 2 7 8
        store __loopvar_0_n 9
        load 10 __loopvar_0_n
        field 11 10 0 string name
        call 12 pith_cstring_retain void 1 11
        ret 11
        label L3
        strref 16 m0s1
        ret 16
        mark kind __for_idx_0 int
        mark kind __for_len_0 int
        mark kind __for_iter_0 list
        mark kind __loopvar_0_n struct
        mark borrow __loopvar_0_n __for_iter_0
        mark kind xs list
        endfunc
    ";


    // `return b.to_string_utf8() catch ""`: the payload slot holds the fresh
    // string on the success path and a literal on the failure path, and the
    // catch slot copies it on success and releases it on failure
    const CATCH_NULLABLE: &str = "
func plain 1 string
param b
load 1 b
call 2 bytes_to_string_utf8 string 1 1
iconst 3 0
neq 4 2 3
brif 4 L0 L1
label L0
iconst 5 1
store __lp_flag_3_0 5
store __lp_val_3_0 2
jmp L2
label L1
iconst 6 0
store __lp_flag_3_0 6
strref 7 m1s56
store __lp_val_3_0 7
label L2
load 8 __lp_flag_3_0
load 9 __lp_val_3_0
brif 8 L3 L4
label L3
store __catch_10_3 9
jmp L5
label L4
call 10 pith_cstring_release void 1 9
strref 11 m1s13
store __catch_10_3 11
label L5
load 12 __catch_10_3
ret 12
iconst 13 0
ret 13
mark kind b bytes
endfunc
";

    #[test]
    fn a_catch_over_a_nullable_string_balances() {
        let r = check(CATCH_NULLABLE);
        assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
    }


    // `return zstd_compress(d) catch bytes.empty()`: the catch slot holds the
    // payload on one path and a fresh default on the other
    const CATCH_FRESH_DEFAULT: &str = "
func make_default 1 string
param b
load 1 b
call 2 bytes_to_string_utf8 string 1 1
ret 2
mark kind b bytes
endfunc
func compress 1 string
param data
load 1 data
call 3 bytes_to_string_utf8 string 1 1
iconst 4 0
neq 5 3 4
brif 5 L9 L10
label L9
iconst 6 1
store __lp_flag_4_9 6
store __lp_val_4_9 3
jmp L11
label L10
iconst 7 0
store __lp_flag_4_9 7
strref 8 m12s93
store __lp_val_4_9 8
label L11
load 9 __lp_flag_4_9
load 10 __lp_val_4_9
brif 9 L12 L13
label L12
store __catch_11_12 10
jmp L14
label L13
call 11 pith_cstring_release void 1 10
load 15 data
call 12 make_default string 1 15
store __catch_11_12 12
label L14
load 13 __catch_11_12
ret 13
iconst 14 0
ret 14
mark kind data bytes
endfunc
";

    #[test]
    fn a_catch_with_a_fresh_default_balances() {
        let r = check(CATCH_FRESH_DEFAULT);
        assert_eq!((r.passed, r.failed, r.undecided), (2, 0, 0), "{:?}", r);
    }

    #[test]
    fn a_catch_with_a_fresh_default_that_drops_the_payload_is_a_leak() {
        let r = check(&CATCH_FRESH_DEFAULT.replace("store __catch_11_12 10\n", ""));
        assert_eq!(r.failed, 1, "{:?}", r);
    }


    // a select receive arm: the shell is there only when the flag is set
    const SELECT_RECV: &str = "
func pick 1 int
param ch
load 14 ch
call 15 pith_channel_try_recv tuple 1 14
optflag 16 15
brif 16 L11 L12
label L11
store v 15
load 18 v
call 20 pith_struct_release void 1 15
iconst 19 1
ret 19
label L12
iconst 21 0
ret 21
mark kind ch channel
endfunc
";

    #[test]
    fn a_received_shell_released_where_it_came_balances() {
        let r = check(SELECT_RECV);
        assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
    }

    #[test]
    fn a_received_shell_never_released_is_a_leak() {
        let r = check(&SELECT_RECV.replace("call 20 pith_struct_release void 1 15\n", ""));
        assert_eq!(r.failed, 1, "{:?}", r);
    }


    // `x := if c: fresh() else: p`, then another branch before x is handed
    // off: the register reading x crosses the second join
    const IF_VALUE_ACROSS_JOIN: &str = "
func pick 1 string
param p
iconst 1 1
brif 1 L1 L2
label L1
call 3 int_to_string string 1 1
store __ifexpr_1 3
jmp L3
label L2
load 4 p
call 5 pith_cstring_retain void 1 4
store __ifexpr_1 4
label L3
load 6 __ifexpr_1
iconst 7 1
brif 7 L4 L5
label L4
jmp L6
label L5
jmp L6
label L6
ret 6
mark kind p string
endfunc
";

    #[test]
    fn a_branch_value_read_across_a_later_join_balances() {
        let r = check(IF_VALUE_ACROSS_JOIN);
        assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
    }

    #[test]
    fn a_retained_borrow_returned_balances() {
        let r = check(FIRST_NAME);
        assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
    }

    #[test]
    fn a_borrow_returned_without_its_retain_is_an_over_release() {
        let r = check(&FIRST_NAME.replace("call 12 pith_cstring_retain void 1 11\n", ""));
        assert_eq!(r.failed, 1, "{:?}", r);
        assert!(r.problems[0].contains("more than it took"), "{:?}", r.problems);
    }

    // main: a list built, pushed into, passed along, released at exit
    const MAIN: &str = "
        func main 0 void
        iconst 1 0
        store xs 1
        load 2 xs
        call 3 pith_list_new_struct list 0
        call 4 pith_list_release_handle void 1 2
        store xs 3
        load 5 xs
        strref 6 m0s0
        iconst 8 1
        call 7 pith_struct_alloc struct:Node 1 8
        sstore 7 0 6
        call 11 pith_list_push_value_owned void 2 5 7
        load 16 xs
        call 17 pith_list_release_handle void 1 16
        iconst 18 0
        ret 18
        mark kind xs list
        mark owns xs list
        endfunc
    ";

    #[test]
    fn an_owned_local_released_at_exit_balances() {
        let r = check(MAIN);
        assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
    }

    #[test]
    fn a_missing_release_is_a_leak() {
        let r = check(&MAIN.replace("call 17 pith_list_release_handle void 1 16\n", ""));
        assert_eq!(r.failed, 1, "{:?}", r);
        assert!(r.problems[0].contains("leak"), "{:?}", r.problems);
    }

    #[test]
    fn a_doubled_release_gives_up_a_count_it_doesnt_hold() {
        let r = check(&MAIN.replace(
            "call 17 pith_list_release_handle void 1 16\n",
            "call 17 pith_list_release_handle void 1 16\ncall 19 pith_list_release_handle void 1 16\n",
        ));
        assert_eq!(r.failed, 1, "{:?}", r);
    }

    #[test]
    fn a_rebind_without_releasing_the_old_value_leaks() {
        // the first rebind replaces a null, which needs no release; a
        // second one without a release drops the first list's count
        let rebind = "store xs 3\nload 20 xs\ncall 21 pith_list_new_struct list 0\nstore xs 21\n";
        let r = check(&MAIN.replace("store xs 3\n", rebind));
        assert_eq!(r.failed, 1, "{:?}", r);
        assert!(r.problems.iter().any(|p| p.contains("leak")), "{:?}", r.problems);
        // releasing a null before the first store is fine either way
        let r = check(&MAIN.replace("call 4 pith_list_release_handle void 1 2\n", ""));
        assert_eq!(r.passed, 1, "{:?}", r);
    }

    // fn set_value(s: String): self.value = s
    const SET_VALUE: &str = "
        func Input_set_value 2 void
        param self
        param s
        load 1 s
        call 3 pith_cstring_retain void 1 1
        load 2 self
        field 5 2 0 string value
        call 6 pith_cstring_release void 1 5
        sstore 2 0 1
        iconst 7 0
        ret 7
        mark kind self Input
        mark kind s string
        endfunc
    ";

    #[test]
    fn a_field_replaced_through_its_read_balances() {
        let r = check(SET_VALUE);
        assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
    }

    #[test]
    fn a_field_released_and_never_stored_dangles() {
        let r = check(&SET_VALUE.replace("sstore 2 0 1\n", "call 8 pith_cstring_release void 1 1\n"));
        assert_eq!(r.failed, 1, "{:?}", r);
        assert!(r.problems.iter().any(|p| p.contains("dangles")), "{:?}", r.problems);
    }

    #[test]
    fn a_store_then_a_release_of_the_old_value_balances() {
        // the order Buffer.reset compiles to: read, store, then release
        let store_first = SET_VALUE.replace(
            "call 6 pith_cstring_release void 1 5\nsstore 2 0 1\n",
            "sstore 2 0 1\ncall 6 pith_cstring_release void 1 5\n",
        );
        let r = check(&store_first);
        assert_eq!((r.passed, r.failed), (1, 0), "{:?}", r);
        // and the displaced value, never released, is a leak
        let r = check(&store_first.replace("call 6 pith_cstring_release void 1 5\n", ""));
        assert_eq!(r.failed, 1, "{:?}", r);
    }

    // name = value with a retain on one path, name = a fresh substring on the
    // other, both released at exit: the shape parser_mark_guarded compiles to
    const ALIASED: &str = "
        func mark 1 void
        param s
        iconst 1 0
        store value 1
        iconst 2 0
        store name 2
        load 3 s
        call 4 pith_cstring_trim string 1 3
        store value 4
        load 5 value
        call 6 pith_cstring_retain void 1 5
        store name 5
        iconst 7 1
        brif 7 L1 L2
        label L1
        load 8 value
        iconst 9 0
        call 10 pith_cstring_substring string 3 8 9 9
        load 11 name
        call 12 pith_cstring_release void 1 11
        store name 10
        jmp L2
        label L2
        load 13 value
        call 14 pith_cstring_release void 1 13
        load 15 name
        call 16 pith_cstring_release void 1 15
        iconst 17 0
        ret 17
        mark kind s string
        mark kind value string
        mark kind name string
        mark owns value string
        mark owns name string
        endfunc
    ";

    #[test]
    fn slots_sharing_a_value_join_slots_holding_their_own() {
        let r = check(ALIASED);
        assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
    }

    #[test]
    fn a_missing_release_after_an_aliasing_join_is_still_a_leak() {
        let r = check(&ALIASED.replace("call 16 pith_cstring_release void 1 15\n", ""));
        assert_eq!(r.failed, 1, "{:?}", r);
        // and dropping the release of the old shared value on one path too
        let r = check(&ALIASED.replace("call 12 pith_cstring_release void 1 11\n", ""));
        assert_eq!(r.failed, 1, "{:?}", r);
    }


    // examples/graceful_shutdown.pith: a result rebound each loop pass, its old
    // value released through a register read before a branch. the slot is
    // null on the first pass and a value on later ones.
    #[test]
    fn a_result_rebound_in_a_loop_balances() {
        let ir = r#"func std_time_delay 1 void
param x
iconst 1 0
ret 1
endfunc
func await_listening 1 bool
param port
iconst 1 0
store probe 1
iconst 2 0
store waited 2
label L12
load 3 waited
iconst 4 10000
lt 5 3 4
brif 5 L13 L14
label L13
load 6 probe
strref 7 m53s59
load 8 port
call 9 tcp_connect result_int 2 7 8
call 10 pith_cstring_release void 1 7
iconst 11 0
neq 12 9 11
brif 12 L15 L16
label L15
iconst 13 1
sub 14 9 13
iconst 15 1
iconst 16 0
iconst 17 3
call 18 pith_struct_alloc struct:tuple 1 17
sstore 18 0 15
sstore 18 1 14
sstore 18 2 16
store __legacy_result_11_15 18
jmp L17
label L16
strref 19 m53s1109
iconst 20 0
iconst 21 3
call 22 pith_struct_alloc struct:tuple 1 21
sstore 22 0 20
sstore 22 1 20
sstore 22 2 19
store __legacy_result_11_15 22
label L17
load 23 __legacy_result_11_15
call 24 __tuple_cascade_0s_m53 void 1 6
store probe 23
load 25 probe
field 26 25 0 bool is_ok
brif 26 L19 L20
label L19
load 27 probe
field 28 27 8 int ok
call 29 tcp_close unknown 1 28
iconst 30 1
load 31 probe
call 32 __tuple_cascade_0s_m53 void 1 31
ret 30
label L20
label L18
iconst 33 20
call 34 std_time_delay void 1 33
load 35 waited
iconst 36 20
add 37 35 36
store waited 37
jmp L12
label L14
iconst 38 0
load 39 probe
call 40 __tuple_cascade_0s_m53 void 1 39
ret 38
load 41 probe
call 42 __tuple_cascade_0s_m53 void 1 41
iconst 43 0
ret 43
mark kind __legacy_result_11_15 tuple
mark kind waited int
mark kind port int
mark kind probe tuple
mark owns probe tuple
mark untracked waited
endfunc
"#;
        let r = check(ir);
        assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
        // without the release of the old value, the next pass leaks it
        let r = check(&ir.replace("call 24 __tuple_cascade_0s_m53 void 1 6\n", ""));
        assert_eq!(r.failed, 1, "{:?}", r);
    }


    // fn total(xs: List[Node]) -> Int: for n in xs: sum = sum + n.name.len()
    // the loop as the emitter writes it when the body keeps storage
    const SUM_NAMES: &str = "
        func total 1 int
        param xs
        iconst 1 0
        store sum 1
        load 2 xs
        call 3 pith_auto_len int 1 2
        iconst 4 0
        store __for_idx_0 4
        store __for_len_0 3
        store __for_iter_0 2
        label L0
        load 5 __for_idx_0
        load 6 __for_len_0
        lt 7 5 6
        brif 7 L1 L3
        label L1
        load 8 __for_iter_0
        load 9 __for_idx_0
        call 10 pith_list_get_value unknown 2 8 9
        store __loopvar_0_n 10
        load 11 __loopvar_0_n
        field 12 11 0 string name
        call 13 pith_cstring_len int 1 12
        load 14 sum
        add 15 14 13
        store sum 15
        label L2
        load 16 __for_idx_0
        iconst 17 1
        add 18 16 17
        store __for_idx_0 18
        jmp L0
        label L3
        load 19 sum
        ret 19
        mark kind __for_idx_0 int
        mark kind __for_len_0 int
        mark kind __for_iter_0 list
        mark kind __loopvar_0_n struct
        mark borrow __loopvar_0_n __for_iter_0
        mark kind xs list
        mark kind sum int
        endfunc
    ";

    fn borrow_problems(ir: &str) -> Vec<String> {
        let lines: Vec<&str> = ir.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        check_loop_borrows(&lines)
    }

    // the #1270 shape: the body clears the list, then reads the variable
    #[test]
    fn a_borrowed_loop_variable_read_after_a_clear_is_reported() {
        assert!(borrow_problems(SUM_NAMES).is_empty());
        let cleared = SUM_NAMES.replace(
            "load 11 __loopvar_0_n\n",
            "call 30 list_clear void 1 8\nload 11 __loopvar_0_n\n",
        );
        let problems = borrow_problems(&cleared);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("list_clear"), "{problems:?}");
    }

    #[test]
    fn a_call_into_a_function_that_writes_a_global_counts_as_a_drop() {
        let writer = "global __g_items list\nfunc bump 0 void\nload 1 __g_items\nstore __g_items 1\niconst 2 0\nret 2\nendfunc\n";
        let keeper = "func peek 0 int\niconst 1 7\nret 1\nendfunc\n";
        let calls = |callee: &str| {
            format!(
                "{writer}{keeper}{}",
                SUM_NAMES.replace("load 11 __loopvar_0_n\n", &format!("call 30 {callee} int 0\nload 11 __loopvar_0_n\n"))
            )
        };
        assert_eq!(borrow_problems(&calls("bump")).len(), 1);
        // a function that writes nothing keeps the borrow good
        assert!(borrow_problems(&calls("peek")).is_empty());
    }

    #[test]
    fn a_function_without_marks_is_left_out() {
        let r = check("func helper 1 void\nparam p\niconst 1 0\nret 1\nendfunc\n");
        assert_eq!(r.unmarked, 1);
    }
}
