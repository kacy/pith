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
                            f.kinds.insert(parts[2], parts[3]);
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
                *report.undecided_reasons.entry(reason).or_insert(0) += 1;
            }
            Ok(()) if checker.problems.is_empty() => report.passed += 1,
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
}

/// where two paths meet. a slot holding one object on one side and a null on
/// the other becomes that object or a null. a register only one side defines
/// can't be used past the join, so a count it holds there is a leak. any
/// other disagreement is one the checker can't settle.
fn join(a: &Canon, b: &Canon, leaks: &mut Vec<String>) -> Option<Canon> {
    if a == b {
        return Some(a.clone());
    }
    if a.unknowns != b.unknowns {
        return None;
    }
    let mut a_nulls = a.nulls.clone();
    let mut b_nulls = b.nulls.clone();
    let mut out = Canon { unknowns: a.unknowns.clone(), ..Canon::default() };
    let keys: BTreeSet<&Vec<String>> = a.objs.keys().chain(b.objs.keys()).collect();
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
            _ => return None,
        }
    }
    if a_nulls != b_nulls {
        return None;
    }
    out.nulls = a_nulls;
    let ids: BTreeSet<&String> = a.regobjs.keys().chain(b.regobjs.keys()).collect();
    for id in ids {
        match (a.regobjs.get(id), b.regobjs.get(id)) {
            (Some(x), Some(y)) if x == y => {
                out.regobjs.insert(id.clone(), *x);
            }
            (Some(_), Some(_)) => return None,
            (Some(n), None) | (None, Some(n)) => {
                if *n > 0 {
                    leaks.push(id.clone());
                }
            }
            (None, None) => {}
        }
    }
    for (r, key) in &a.regs {
        if b.regs.get(r) == Some(key) {
            let live = key == "null" || out.regobjs.contains_key(key) || out.objs.keys().any(|k| format!("@{}", k.join("|")) == *key);
            if live {
                out.regs.insert(r.clone(), key.clone());
            }
        }
    }
    Some(out)
}

struct FuncCheck<'a, 'f> {
    f: &'f Func<'a>,
    globals: &'f HashMap<&'a str, &'a str>,
    user_funcs: &'f HashSet<&'a str>,
    scalar_temps: HashSet<&'a str>,
    problems: Vec<String>,
    /// the instruction that made each object, for the messages
    origins: HashMap<String, String>,
    current: String,
    current_dst: String,
}

impl<'a, 'f> FuncCheck<'a, 'f> {
    fn new(f: &'f Func<'a>, globals: &'f HashMap<&'a str, &'a str>, user_funcs: &'f HashSet<&'a str>) -> Self {
        let mut c = FuncCheck { f, globals, user_funcs, scalar_temps: HashSet::new(), problems: Vec::new(), origins: HashMap::new(), current: String::new(), current_dst: String::new() };
        c.scalar_temps = c.infer_scalar_temps();
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
                out.objs.insert(names.clone(), (*n, st.maybe_null.contains(o)));
                key_of.insert(o, format!("@{}", names.join("|")));
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
        st
    }

    fn run(&mut self) -> Result<(), String> {
        if !self.f.mixed.is_empty() {
            return Err("a local whose kind changes by path".into());
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
        for (g, k) in self.globals.iter() {
            if is_counted(k) {
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
                    Some(merged) if merged == *prev => continue,
                    Some(merged) => merged,
                    None => return Err("paths join with different counts".into()),
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
            let out = self.canon(&st, &format!("the end of {}", order[b]));
            for s in &succ[b] {
                work.push((*s, out.clone()));
            }
        }
        self.problems.sort();
        self.problems.dedup();
        Ok(())
    }

    fn join_at(&mut self, a: &Canon, b: &Canon, label: &str) -> Option<Canon> {
        let mut leaks = Vec::new();
        let out = join(a, b, &mut leaks);
        for id in leaks {
            let from = self.origins.get(&id).cloned().unwrap_or(id);
            self.problems.push(format!("a count only one path holds reaches {label} (leak), from `{from}`"));
        }
        out
    }

    fn val(&self, st: &State, reg: &str) -> Val {
        st.regs.get(reg).cloned().unwrap_or(Val::Plain)
    }

    /// give up one count through `reg`
    fn take(&mut self, st: &mut State, reg: &str, why: &str) -> Result<(), String> {
        match self.val(st, reg) {
            Val::Obj(o) => {
                let n = st.counts.entry(o).or_insert(0);
                if *n <= 0 {
                    self.problems.push(format!("{why} gives up a count it doesn't hold (register {reg})"));
                }
                *n -= 1;
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
                let v = if ins[4] == "unknown" {
                    Val::Unknown
                } else if is_counted(ins[4]) {
                    Val::Obj(self.new_obj(st, 0))
                } else {
                    Val::Plain
                };
                st.regs.insert(ins[1].to_string(), v);
                Ok(false)
            }
            "sstore" if ins.len() >= 4 => {
                self.take(st, ins[3], "a struct field store")?;
                Ok(false)
            }
            "concat" | "concatn" | "appendn" if ins.len() >= 2 => {
                let o = self.new_obj(st, 1);
                st.regs.insert(ins[1].to_string(), Val::Obj(o));
                Ok(false)
            }
            "call" if ins.len() >= 5 => self.call(ins, st).map(|_| false),
            "rcall" => Err("a call returning the register pair".into()),
            "ret2" => Err("a register-pair return".into()),
            "ret" => {
                self.ret(ins, st);
                Ok(true)
            }
            _ => Err(format!("instruction {op}")),
        }
    }

    fn call(&mut self, ins: &[&str], st: &mut State) -> Result<(), String> {
        let (dst, callee, typ) = (ins[1], ins[2], ins[3]);
        let args: Vec<&str> = ins.iter().skip(5).copied().collect();
        if RETAINS.contains(&callee) {
            match self.val(st, args[0]) {
                Val::Obj(o) => *st.counts.entry(o).or_insert(0) += 1,
                Val::Unknown => return Err("a value of unknown kind meets a count".into()),
                _ => {}
            }
            st.regs.insert(dst.to_string(), Val::Plain);
            return Ok(());
        }
        let result = if let Some(row) = runtime_ownership(callee) {
            for (i, a) in row.args.iter().enumerate() {
                if *a == ArgOwnership::Consume && i < args.len() {
                    self.take(st, args[i], callee)?;
                }
            }
            match row.result.value {
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
        st.regs.insert(dst.to_string(), result);
        Ok(())
    }

    fn ret(&mut self, ins: &[&str], st: &mut State) {
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
                if self.globals.contains_key(s.as_str()) {
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
        blocks.last_mut().unwrap().push(ins.as_slice());
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

    #[test]
    fn a_function_without_marks_is_left_out() {
        let r = check("func helper 1 void\nparam p\niconst 1 0\nret 1\nendfunc\n");
        assert_eq!(r.unmarked, 1);
    }
}
