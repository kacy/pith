//! Per-function reference-count balance analysis.

use super::cfg::split_blocks;
use super::domain::{join, rekey_field, Canon, SlotClass, State, TagTest, Val};
use super::ir::{Func, ParsedIr};
use super::{
    is_counted, is_emitted_release, is_emitted_unshare, kind_of_text, CountReport, CALLI_CALLEE,
    PLAIN_OPS, RELEASES, RETAINS, SCALAR_KINDS,
};
use crate::runtime_ownership::{runtime_ownership, ArgOwnership, ResultValue};
use std::collections::{BTreeMap, HashMap, HashSet};

/// check every function in the IR text and report what was found
pub fn check_counts(lines: &[&str]) -> CountReport {
    let parsed = ParsedIr::parse(lines);
    check_counts_parsed(&parsed)
}

pub(super) fn check_counts_parsed(parsed: &ParsedIr<'_>) -> CountReport {
    let globals = &parsed.globals;
    let user_funcs = &parsed.user_funcs;
    let funcs = &parsed.funcs;
    let mut report = CountReport::default();
    for f in funcs {
        if !f.marked {
            report.unmarked += 1;
            continue;
        }
        let mut checker = FuncCheck::new(f, &globals, &user_funcs);
        let outcome = checker.run();
        // a count given up that the path never held is a finding even when a
        // later join leaves the rest of the function unsettled: the doubled
        // release is what made the paths disagree
        let over: Vec<String> = checker
            .problems
            .iter()
            .filter(|p| {
                p.contains("doesn't hold")
                    || p.contains("given up twice")
                    || p.contains("more than it took")
            })
            .cloned()
            .collect();
        match outcome {
            Err(_) if !over.is_empty() => {
                report.failed += 1;
                for p in over {
                    report.problems.push(format!("{}: {}", f.name, p));
                }
            }
            Err(reason) => {
                report.undecided += 1;
                if !checker.join_detail.is_empty() {
                    report.join_details.push(format!("{}: {}", f.name, checker.join_detail));
                }
                report.undecided_names.push(format!("{}: {}, at `{}`", f.name, reason, checker.current));
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

struct FuncCheck<'a, 'f> {
    f: &'f Func<'a>,
    globals: &'f HashMap<&'a str, &'a str>,
    user_funcs: &'f HashSet<&'a str>,
    scalar_temps: HashSet<&'a str>,
    /// the integer constant each `iconst` register holds, for the slot number
    /// a thread-local call names
    consts: HashMap<&'a str, &'a str>,
    /// objects an rcall made for a plain ok payload: they hold a count only
    /// on the error side
    plain_ok_payloads: HashSet<String>,
    problems: Vec<String>,
    /// the instruction that made each object, for the messages
    origins: HashMap<String, String>,
    current: String,
    current_dst: String,
    /// field reads so far, to order them
    reads: usize,
    /// what disagreed at the join that left the function undecided
    join_detail: String,
    /// registers a retain or a plain release names: a value of a kind the
    /// marks leave open (a generic body's `T`) that the emitter counts
    counted_regs: HashSet<&'a str>,
    /// the slots this function stores to: a call through one is a call of
    /// the closure it holds
    stored_slots: HashSet<&'a str>,
}

impl<'a, 'f> FuncCheck<'a, 'f> {
    fn new(
        f: &'f Func<'a>,
        globals: &'f HashMap<&'a str, &'a str>,
        user_funcs: &'f HashSet<&'a str>,
    ) -> Self {
        let mut c = FuncCheck {
            f,
            globals,
            user_funcs,
            scalar_temps: HashSet::new(),
            consts: HashMap::new(),
            plain_ok_payloads: HashSet::new(),
            problems: Vec::new(),
            origins: HashMap::new(),
            current: String::new(),
            current_dst: String::new(),
            reads: 0,
            join_detail: String::new(),
            counted_regs: HashSet::new(),
            stored_slots: HashSet::new(),
        };
        c.scalar_temps = c.infer_scalar_temps();
        for ins in &f.body {
            if ins[0] == "call"
                && ins.len() >= 6
                && (RETAINS.contains(&ins[2]) || RELEASES.contains(&ins[2]))
            {
                c.counted_regs.insert(ins[5]);
            } else if ins[0] == "store" && ins.len() >= 3 {
                c.stored_slots.insert(ins[1]);
            }
        }
        // and a value stored into an unmarked slot that a counted register
        // is later loaded from (an awaited `T` kept in a temp, then released)
        let counted_slots: HashSet<&str> = f
            .body
            .iter()
            .filter(|ins| {
                ins[0] == "load"
                    && ins.len() >= 3
                    && c.counted_regs.contains(ins[1])
                    && f.kinds.get(ins[2]).map_or(true, |k| *k == "unknown")
            })
            .map(|ins| ins[2])
            .collect();
        for ins in &f.body {
            if ins[0] == "store" && ins.len() >= 3 && counted_slots.contains(ins[1]) {
                c.counted_regs.insert(ins[2]);
            }
        }
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
        let mut key_of: HashMap<&str, String> = HashMap::new();
        for (o, n) in &st.counts {
            if let Some(names) = holders.get(o) {
                let maybe_null = st.maybe_null.contains(o);
                let mut owners: Vec<&String> = names
                    .iter()
                    .filter(|s| self.class_of(s) == SlotClass::Holder)
                    .collect();
                // an emitter temp that carries a count of its own, such as the
                // slot an `unwrap_or` or a match leaves its value in before the
                // binding retains it, counts as one more owner when the count
                // says every such temp holds one
                let carriers: Vec<&String> = names
                    .iter()
                    .filter(|s| self.class_of(s) == SlotClass::Carrier)
                    .collect();
                if !owners.is_empty()
                    && !carriers.is_empty()
                    && *n == (owners.len() + carriers.len()) as i64
                {
                    owners.extend(carriers);
                }
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
                    key_of.insert(o.as_str(), format!("@{}", owners[0]));
                } else {
                    out.objs.insert(names.clone(), (*n, maybe_null));
                    key_of.insert(o.as_str(), format!("@{}", names.join("|")));
                }
            } else if referenced.contains(o) {
                // a borrowed value keeps its place too: a retain or a release
                // past the boundary has to find it, not a plain word. an
                // object named for the slots that held it at an earlier
                // boundary takes a key of its own, or it would be confused
                // with what those slots hold now
                let key = if o.starts_with('@') && !o.ends_with('~') { format!("{o}~") } else { o.clone() };
                out.regobjs.insert(key.clone(), *n);
                key_of.insert(o.as_str(), key);
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
                    if let Some(k) = key_of.get(o.as_str()) {
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
        for o in &st.none_is_null {
            if let Some(k) = key_of.get(o.as_str()) {
                out.none_is_null.insert(k.clone());
            }
        }
        for (o, (field, seq)) in &st.field_of {
            let Some(k) = key_of.get(o.as_str()).filter(|k| out.regobjs.contains_key(*k)) else {
                continue;
            };
            out.reads.insert(k.clone(), (rekey_field(&key_of, field), *seq));
        }
        for o in &st.counted_reads {
            if let Some(k) = key_of.get(o.as_str()).filter(|k| out.regobjs.contains_key(*k)) {
                out.counted_reads.insert(k.clone());
            }
        }
        for o in &st.shells {
            if let Some(k) = key_of.get(o.as_str()) {
                out.shells.insert(k.clone());
            }
        }
        for o in &st.result_shells {
            if let Some(k) = key_of.get(o.as_str()) {
                out.result_shells.insert(k.clone());
            }
        }
        for o in &st.stack_closures {
            if let Some(k) = key_of.get(o.as_str()) {
                out.stack_closures.insert(k.clone());
            }
        }
        for field in &st.owned_fields {
            let (base, off) = field.rsplit_once(':').unwrap_or((field.as_str(), ""));
            if let Some(k) = key_of.get(base) {
                out.owned_fields.insert(format!("{k}:{off}"));
            }
        }
        for field in &st.emptied {
            out.emptied.insert(rekey_field(&key_of, field));
        }
        out.weak_credits = st.weak_credits.clone();
        for (r, o) in &st.unique_tests {
            if let Some(k) = key_of.get(o.as_str()) {
                out.unique_tests.insert(r.clone(), k.clone());
            }
        }
        for o in &st.shared_shells {
            if let Some(k) = key_of.get(o.as_str()) {
                out.shared_shells.insert(k.clone());
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
        for (view, owner) in &canon.aliases {
            if let Some(v) = st.slots.get(owner).cloned() {
                st.slots.insert(view.clone(), v);
            }
        }
        for (id, n) in &canon.regobjs {
            st.counts.insert(id.clone(), *n);
        }
        for (id, read) in &canon.reads {
            st.field_of.insert(id.clone(), read.clone());
        }
        st.counted_reads = canon.counted_reads.iter().cloned().collect();
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
        st.result_shells = canon.result_shells.clone();
        st.stack_closures = canon.stack_closures.clone();
        st.none_is_null = canon.none_is_null.clone();
        st.owned_fields = canon.owned_fields.clone();
        st.weak_credits = canon.weak_credits.clone();
        st.unique_tests = canon.unique_tests.clone();
        st.shared_shells = canon.shared_shells.clone();
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
                // a parameter of an open kind the body retains or releases
                // is a counted one, and borrowed like any other
                SlotClass::Carrier
                    if self.f.body.iter().any(|ins| {
                        ins[0] == "load"
                            && ins.len() >= 3
                            && ins[2] == *p
                            && self.counted_regs.contains(ins[1])
                    }) =>
                {
                    let o = format!("param:{p}");
                    st.counts.insert(o.clone(), 0);
                    st.slots.insert(p.to_string(), Val::Obj(o));
                }
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
            // a branch on whether a value is null, a result/optional tag, or a
            // shell's strong_count refines each successor side.
            let last = blocks[b].last().copied().unwrap_or(&[]);
            if last.first() == Some(&"brif") && last.len() >= 4 && succ[b].len() == 2 {
                let cond_take = st.cond_takes.get(last[1]).cloned();
                let mut settle: Option<(String, Option<i64>, Option<i64>)> = None;
                if let Some((obj, set_means_there)) = st.null_tests.get(last[1]).cloned() {
                    // the null side holds no count; the other keeps its own
                    settle = Some(if set_means_there { (obj, None, Some(0)) } else { (obj, Some(0), None) });
                } else if let Val::Obj(obj) = self.val(&st, last[1]) {
                    settle = Some((obj, None, Some(0)));
                }
                let tag_test = st.tag_tests.get(last[1]).cloned();
                let unique_test = st.unique_tests.get(last[1]).cloned();
                if cond_take.is_some() || settle.is_some() || tag_test.is_some() || unique_test.is_some() {
                    for (i, s) in succ[b].iter().enumerate() {
                        let mut side = st.clone();
                        if let Some(ref obj) = cond_take {
                            if i == 1 {
                                *side.counts.entry(obj.clone()).or_insert(0) += 1;
                            }
                        }
                        if let Some((ref obj, when_set, when_clear)) = settle {
                            if let Some(n) = if i == 0 { when_set } else { when_clear } {
                                side.counts.insert(obj.clone(), n);
                                // on the null side the registers and slots holding
                                // it hold a null, and releasing one does nothing
                                if n == 0 {
                                    for v in side.regs.values_mut() {
                                        if *v == Val::Obj(obj.clone()) {
                                            *v = Val::Null;
                                        }
                                    }
                                    for v in side.slots.values_mut() {
                                        if *v == Val::Obj(obj.clone()) {
                                            *v = Val::Null;
                                        }
                                    }
                                }
                            }
                        }
                        if let Some((ref obj, kind)) = tag_test {
                            match (kind, i) {
                                (TagTest::ResultOk, 0) | (TagTest::ResultNotOk, 1) => {
                                    side.owned_fields.remove(&format!("{obj}:2"));
                                }
                                (TagTest::ResultOk, 1) | (TagTest::ResultNotOk, 0) => {
                                    side.owned_fields.remove(&format!("{obj}:1"));
                                }
                                (TagTest::OptSome, 1) | (TagTest::OptNone, 0) => {
                                    side.owned_fields.remove(&format!("{obj}:1"));
                                }
                                _ => {}
                            }
                        }
                        if let Some(ref obj) = unique_test {
                            if i == 1 {
                                side.shared_shells.insert(obj.clone());
                            }
                        }
                        side.cond_takes.clear();
                        side.null_tests.clear();
                        side.tag_tests.clear();
                        side.strong_count_of.clear();
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
        let mut mismatches = Vec::new();
        let out = join(a, b, &mut leaks, &mut mismatches);
        for (k, x, y) in mismatches {
            self.problems.push(format!("paths reach {label} holding {x} and {y} count(s) of `@{k}`"));
        }
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
                if st.counts.get(&o).copied().unwrap_or(0) <= 0 && !st.counted_reads.contains(&o) {
                    if let Some((field, _)) = st.field_of.get(&o).cloned() {
                        if st.owned_fields.remove(&field) {
                            // a destructor-less shell's payload, taken over
                            // and given up here: the shell no longer owns it
                            st.field_of.remove(&o);
                            return Ok(());
                        }
                        let base = field.rsplit_once(':').map(|(b, _)| b.to_string()).unwrap_or_default();
                        if !st.shells.contains(&base) && !st.emptied.contains(&field) {
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
                // a plain release frees the shell alone and its payloads pass
                // to whoever read them out; an emitted release helper drops
                // them with it, and the emitted unshare hands the box on with
                // its payload still in it
                if *n == 0
                    && st.shells.contains(&o)
                    && !is_emitted_release(why)
                    && !is_emitted_unshare(why)
                {
                    let prefix = format!("{o}:");
                    if st.shared_shells.contains(&o) {
                        // shell is proven strong_count > 1 on this path: our
                        // release decrements the shell refcount without freeing
                        // its payloads (any reader retained explicitly).
                        st.owned_fields.retain(|f| !f.starts_with(&prefix));
                        st.field_of.retain(|_, (rf, _)| !rf.starts_with(&prefix));
                    } else {
                        // each owned field transfers its single count to at most
                        // one reader (the most recent); unread owned fields leak.
                        let owned_in_shell: Vec<String> = st
                            .owned_fields
                            .iter()
                            .filter(|f| f.starts_with(&prefix))
                            .cloned()
                            .collect();
                        for f in owned_in_shell {
                            st.owned_fields.remove(&f);
                            let winner = st
                                .field_of
                                .iter()
                                .filter(|(r, (rf, _))| {
                                    *rf == f && st.counts.get(*r).copied().unwrap_or(0) == 0
                                })
                                .max_by_key(|(_, (_, seq))| *seq)
                                .map(|(r, _)| r.clone());
                            st.field_of.retain(|_, (rf, _)| *rf != f);
                            if let Some(r) = winner {
                                st.counts.insert(r, 1);
                            } else if !st.result_shells.contains(&o) {
                                // Unread owned fields on 2-slot optional/tuple shells
                                // are reported immediately; 3-slot result boxes are
                                // deferred until emitter result-reassignment & typed
                                // error-enum rcall annotations land in self-host.
                                self.problems.push(format!(
                                    "{why} frees shell `{o}` without releasing owned field `{f}` (leak)"
                                ));
                            }
                        }
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
                // Propagate result/optional tag tests through `eq/neq` against 0 or 1.
                for (lhs, rhs) in [(ins[2], ins[3]), (ins[3], ins[2])] {
                    if let Some(&(ref o, kind)) = st.tag_tests.get(lhs) {
                        match (op, self.consts.get(rhs).copied()) {
                            ("eq", Some("0")) | ("neq", Some("1")) => {
                                st.tag_tests.insert(ins[1].to_string(), (o.clone(), kind.invert()));
                            }
                            ("eq", Some("1")) | ("neq", Some("0")) => {
                                st.tag_tests.insert(ins[1].to_string(), (o.clone(), kind));
                            }
                            _ => {}
                        }
                    }
                    if op == "eq" && self.consts.get(rhs).copied() == Some("1") {
                        if let Some(o) = st.strong_count_of.get(lhs).cloned() {
                            st.unique_tests.insert(ins[1].to_string(), o);
                        }
                    }
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
                    if st.none_is_null.contains(&o) {
                        st.null_tests.insert(ins[1].to_string(), (o.clone(), true));
                    }
                    st.tag_tests.insert(ins[1].to_string(), (o, TagTest::OptSome));
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
                let counted = self.counted_regs.contains(ins[1]);
                let field_name = ins.get(5).copied().unwrap_or("");
                if ins[3] == "0" && (field_name == "is_ok" || field_name == "is_some") {
                    if let Val::Obj(o) = self.val(st, ins[2]) {
                        let kind = if field_name == "is_ok" {
                            TagTest::ResultOk
                        } else {
                            TagTest::OptSome
                        };
                        st.tag_tests.insert(ins[1].to_string(), (o, kind));
                    }
                }
                let v = if kind_of_text(ins[4]) == "unknown" && !counted {
                    Val::Unknown
                } else if counted || is_counted(ins[4]) {
                    let o = self.new_obj(st, 0);
                    // a read names its field by byte offset, a store by index
                    let index = ins[3].parse::<i64>().map(|b| b / 8).unwrap_or(-1);
                    let base = self.base_key(st, ins[2]);
                    let field = format!("{base}:{index}");
                    // reading `ok` or `err` names a result box (an optional's
                    // payload is `value`). a box this function holds has no
                    // destructor and owns its payload, so freeing the box hands
                    // that count to the read (docs/ir-contract.md)
                    let name = ins.get(5).copied().unwrap_or("");
                    // a box keeps `ok` in its second word and `err` in its
                    // third, and only a tuple slot holds one: a struct of
                    // the program's own may name a field `ok` too
                    let box_slot = (name == "ok" && ins[3] == "8") || (name == "err" && ins[3] == "16");
                    let in_struct_slot = st.slots.iter().any(|(s, v)| {
                        matches!(v, Val::Obj(b) if *b == base) && self.f.kinds.get(s.as_str()).is_some_and(|k| *k != "tuple")
                    });
                    if box_slot && !in_struct_slot && st.counts.get(&base).copied().unwrap_or(0) > 0 {
                        st.shells.insert(base.clone());
                        st.result_shells.insert(base);
                        st.owned_fields.insert(field.clone());
                    }
                    self.reads += 1;
                    st.field_of.insert(o.clone(), (field, self.reads));
                    Val::Obj(o)
                } else {
                    Val::Plain
                };
                st.regs.insert(ins[1].to_string(), v);
                Ok(false)
            }
            // a weak field keeps the single weak count taken for it by a prior
            // `pith_struct_weak_retain` on this register
            "sstore" if ins.len() >= 4 && st.weak_credits.get(ins[3]).copied().unwrap_or(0) > 0 => {
                if let Some(c) = st.weak_credits.get_mut(ins[3]) {
                    *c -= 1;
                    if *c == 0 {
                        st.weak_credits.remove(ins[3]);
                    }
                }
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
            "concat" | "concatn" if ins.len() >= 2 => {
                let o = self.new_obj(st, 1);
                st.regs.insert(ins[1].to_string(), Val::Obj(o));
                Ok(false)
            }
            "appendn" if ins.len() >= 4 => {
                self.take(st, ins[3], "appendn")?;
                let o = self.new_obj(st, 1);
                st.regs.insert(ins[1].to_string(), Val::Obj(o));
                Ok(false)
            }
            "call" if ins.len() >= 5 => self.call(ins, st).map(|_| false),
            // `calli REG PTR KIND N ARGS`: a call through a function pointer,
            // which follows the user-function convention
            "calli" if ins.len() >= 5 => {
                let mut as_call = vec!["call", ins[1], CALLI_CALLEE];
                as_call.extend_from_slice(&ins[3..]);
                self.call(&as_call, st).map(|_| false)
            }
            "rcall" if ins.len() >= 6 => self.rcall(ins, st).map(|_| false),
            "ret2" if ins.len() >= 3 => {
                self.ret(&["ret", ins[2]], st, Some(ins[1]));
                Ok(true)
            }
            // `optval REG OPT KIND NAME`: the payload of an optional, read
            // the way a field is (slot 1 of the shell), or 0 for none
            "optval" if ins.len() >= 4 => {
                let counted = self.counted_regs.contains(ins[1]);
                let v = if kind_of_text(ins[3]) == "unknown" && !counted {
                    Val::Unknown
                } else if counted || is_counted(ins[3]) {
                    let o = self.new_obj(st, 0);
                    let field = format!("{}:1", self.base_key(st, ins[2]));
                    self.reads += 1;
                    st.field_of.insert(o.clone(), (field, self.reads));
                    Val::Obj(o)
                } else {
                    Val::Plain
                };
                st.regs.insert(ins[1].to_string(), v);
                Ok(false)
            }
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
                st.stack_closures.insert(o.clone());
                st.regs.insert(ins[1].to_string(), Val::Obj(o));
                Ok(false)
            }
            "ret" => {
                self.ret(ins, st, None);
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

    /// a call through a fn value in a slot follows the user-function
    /// convention whatever the slot is named, even when the name is also a
    /// runtime function's (`write`): a local or a global marked `closure`, or
    /// an emitter temp this function stores the value in (`__map_fn_L3`)
    fn is_closure_call(&self, callee: &str) -> bool {
        if self.kind_of(callee).is_some_and(|k| k == "closure") {
            return true;
        }
        self.stored_slots.contains(callee)
            && !self.user_funcs.contains(callee)
            && runtime_ownership(callee).is_none()
    }

    fn call(&mut self, ins: &[&str], st: &mut State) -> Result<(), String> {
        let (dst, callee, typ) = (ins[1], ins[2], ins[3]);
        let args: Vec<&str> = ins.iter().skip(5).copied().collect();
        // a weak count is a second count the checker leaves out: it never
        // keeps the value alive. an upgrade hands back a strong count on the
        // target, or a null when it is gone, which a branch on the result
        // settles the way it settles any null test. the ir types the upgrade
        // `int`, since its result is tested before it is used.
        if callee == "pith_struct_weak_upgrade" || callee == "pith_struct_weak_load_field" {
            let o = self.new_obj(st, 1);
            st.regs.insert(dst.to_string(), Val::Obj(o));
            return Ok(());
        }
        if callee.contains("__tuple_stack_cascade_") {
            if let Some(Val::Obj(o)) = args.first().map(|a| self.val(st, a)) {
                let prefix = format!("{o}:");
                st.owned_fields.retain(|f| !f.starts_with(&prefix));
                st.field_of.retain(|_, (f, _)| !f.starts_with(&prefix));
            }
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
        // the emitted unshare takes the count it is handed on a result box
        // and hands back a box the caller owns alone: the same one when that
        // count was the only one, a fresh copy otherwise
        if is_emitted_unshare(callee) {
            if let Some(a) = args.first() {
                self.take(st, a, callee)?;
            }
            let o = self.new_obj(st, 1);
            st.regs.insert(dst.to_string(), Val::Obj(o));
            return Ok(());
        }
        if RETAINS.contains(&callee) {
            if let Some(arg0) = args.first() {
            match self.val(st, arg0) {
                Val::Obj(o) => {
                    if st.field_of.contains_key(&o) {
                        st.counted_reads.insert(o.clone());
                    }
                    *st.counts.entry(o).or_insert(0) += 1
                }
                Val::Unknown => return Err("a value of unknown kind meets a count".into()),
                _ => {}
            }
            }
            st.regs.insert(dst.to_string(), Val::Plain);
            return Ok(());
        }
        // a null destructor clears one (`ir_emit_clear_result_box_dtor`) and
        // leaves the shell as it was
        let dtor_cleared = args.get(1).is_some_and(|a| self.val(st, a) == Val::Null);
        if callee == "pith_struct_set_dtor" && !dtor_cleared {
            if let Some(Val::Obj(o)) = args.first().map(|a| self.val(st, a)) {
                // the destructor releases the fields from now on, so freeing
                // the struct hands nobody a payload's count
                let prefix = format!("{o}:");
                let payload = format!("{o}:1");
                let owned = st.owned_fields.contains(&payload);
                st.owned_fields.retain(|f| !f.starts_with(&prefix));
                st.shells.remove(&o);
                // an optional the function keeps around a borrowed element
                // (`m.get(k)` bound or returned) has its payload read and
                // retained, then the destructor: the shell takes that count
                if !owned {
                    let reader = st
                        .field_of
                        .iter()
                        .filter(|(r, (f, _))| {
                            *f == payload && st.counts.get(*r).copied().unwrap_or(0) > 0
                        })
                        .max_by_key(|(_, (_, seq))| *seq)
                        .map(|(r, _)| r.clone());
                    if let Some(r) = reader {
                        *st.counts.entry(r.clone()).or_insert(0) -= 1;
                        st.counted_reads.remove(&r);
                    }
                }
            }
        }
        let closure_call = self.is_closure_call(callee);
        // a function the program defines wins over a runtime function of
        // the same name, as it does in the consumer: a user `second` is not
        // the runtime's `second`, which borrows its result
        let result = if let Some(row) =
            runtime_ownership(callee).filter(|_| !closure_call && !self.user_funcs.contains(callee))
        {
            if callee == "pith_struct_strong_count" {
                if let Some(Val::Obj(o)) = args.first().map(|a| self.val(st, a)) {
                    st.strong_count_of.insert(dst.to_string(), o);
                }
            }
            if callee == "pith_struct_weak_retain" {
                if let Some(a) = args.first() {
                    *st.weak_credits.entry((*a).to_string()).or_insert(0) += 1;
                }
            }
            if callee == "pith_closure_set_env_rc" {
                let tag_const = args.get(3).and_then(|t| self.consts.get(t)).copied();
                if tag_const == Some("8") {
                    if let Some(val_reg) = args.get(2) {
                        if let Some(c) = st.weak_credits.get_mut(*val_reg) {
                            *c = c.saturating_sub(1);
                            if *c == 0 {
                                st.weak_credits.remove(*val_reg);
                            }
                        }
                    }
                }
                if tag_const != Some("0") {
                    if let Some(Val::Obj(c)) = args.first().map(|a| self.val(st, a)) {
                        if st.stack_closures.contains(&c) {
                            self.problems.push(format!(
                                "{callee} stores a counted capture into a stack closure that is never released (leak)"
                            ));
                        }
                    }
                }
            }
            // a weak capture (tag 8) takes over a weak count, not a strong one;
            // tag 0 stores a plain uncounted value.
            let weak_or_plain_capture = callee == "pith_closure_set_env_rc"
                && matches!(
                    args.get(3).and_then(|t| self.consts.get(t)).copied(),
                    Some("0") | Some("8")
                );
            for (i, a) in row.args.iter().enumerate() {
                if *a == ArgOwnership::Consume && i < args.len() && !weak_or_plain_capture {
                    self.take(st, args[i], callee)?;
                    // these keep the value only when they report success
                    // (runtime_ownership.txt), so the failing side of a branch
                    // on the result has the count back
                    if callee == "pith_channel_send" || callee == "pith_channel_try_send" || callee == "pith_list_set_value_owned_checked" {
                        if let Val::Obj(o) = self.val(st, args[i]) {
                            st.cond_takes.insert(dst.to_string(), o);
                        }
                    }
                }
            }
            match row.result.value {
                // a generic runtime call (an await, an element get) says on
                // the call line when this use of it yields a plain value
                _ if SCALAR_KINDS.contains(&typ) => Val::Plain,
                // an await of an unknown kind may be an Int; a count on it
                // can't be settled either way
                ResultValue::Own if typ == "unknown" && !self.counted_regs.contains(dst) => {
                    Val::Unknown
                }
                // a row that can't name where its counted result is borrowed
                // from (the thread-local getters hand back the slot's value)
                ResultValue::Plain if is_counted(typ) => Val::Obj(self.new_obj(st, 0)),
                ResultValue::Plain => Val::Plain,
                ResultValue::Own => Val::Obj(self.new_obj(st, 1)),
                ResultValue::Borrow(_) => Val::Obj(self.new_obj(st, 0)),
            }
        } else if closure_call || callee == CALLI_CALLEE || self.user_funcs.contains(callee) {
            // a user function borrows its arguments and hands back a count
            // on a counted result
            if typ == "unknown" && !self.counted_regs.contains(dst) {
                Val::Unknown
            } else if typ == "unknown" || is_counted(typ) {
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
                if args.first().and_then(|a| self.consts.get(a)).copied() == Some("3") {
                    st.result_shells.insert(o.clone());
                }
            }
        }
        if callee == "pith_channel_recv" || callee == "pith_channel_try_recv" {
            if let Val::Obj(o) = &result {
                st.none_is_null.insert(o.clone());
                st.shells.insert(o.clone());
            }
        }
        if callee == "env_opt" || callee == "pith_cstring_char_at_opt" {
            if let Val::Obj(o) = &result {
                st.none_is_null.insert(o.clone());
                st.shells.insert(o.clone());
                st.owned_fields.insert(format!("{o}:1"));
            }
        }
        st.regs.insert(dst.to_string(), result);
        Ok(())
    }

    /// `rcall FLAG VAL CALLEE KIND N ARGS`: a call returning the register
    /// pair (docs/ir-contract.md). its arguments follow the rules of a plain
    /// call. VAL holds one count: the ok payload's when the flag is set and
    /// KIND is counted, the error value's when it is clear. with a plain ok
    /// payload the count is there only when the flag is clear, which the
    /// flag says the way a null test does. a runtime pair that borrows its
    /// payload hands out no count on either side.
    fn rcall(&mut self, ins: &[&str], st: &mut State) -> Result<(), String> {
        let (flag, val, callee, kind) = (ins[1], ins[2], ins[3], ins[4]);
        let args: Vec<&str> = ins.iter().skip(6).copied().collect();
        self.current_dst = val.to_string();
        if kind == "unknown" {
            return Err("a value of unknown kind meets a count".into());
        }
        let closure_call = self.is_closure_call(callee);
        let mut owned = true;
        if let Some(row) =
            runtime_ownership(callee).filter(|_| !closure_call && !self.user_funcs.contains(callee))
        {
            for (i, a) in row.args.iter().enumerate() {
                if *a == ArgOwnership::Consume && i < args.len() {
                    self.take(st, args[i], callee)?;
                }
            }
            if matches!(row.result.value, ResultValue::Borrow(_)) {
                owned = false;
            }
        } else if !closure_call && !self.user_funcs.contains(callee) {
            return Err("a call the checker has no summary for".into());
        }
        st.regs.insert(flag.to_string(), Val::Plain);
        if !owned {
            let v = if is_counted(kind) { Val::Obj(self.new_obj(st, 0)) } else { Val::Plain };
            st.regs.insert(val.to_string(), v);
            return Ok(());
        }
        let o = self.new_obj(st, 1);
        st.regs.insert(val.to_string(), Val::Obj(o.clone()));
        if !is_counted(kind) {
            // a set flag means a plain ok payload, so no count
            st.null_tests.insert(flag.to_string(), (o.clone(), false));
            self.plain_ok_payloads.insert(o);
        }
        Ok(())
    }

    /// a return. `pair_flag` is `Some(flag_reg)` for `ret2`, whose payload is
    /// handed off whatever the function's return kind, except when `flag_reg`
    /// is known to be ok (`1`) on a plain ok payload forwarded from a pair call.
    fn ret(&mut self, ins: &[&str], st: &mut State, pair_flag: Option<&str>) {
        let pair = pair_flag.is_some();
        if !st.emptied.is_empty() {
            self.problems.push("releases a field's value and returns without storing a new one (it dangles)".into());
        }
        let mut counts = st.counts.clone();
        // a function returning a tuple has `unknown` on its func line, but
        // its callers still take the result as owned, so a return hands off
        // one count either way
        if ins.len() > 1 && (pair || is_counted(self.f.ret_kind) || self.f.ret_kind == "unknown") {
            if let Val::Obj(o) = self.val(st, ins[1]) {
                let flag_is_ok = pair_flag
                    .and_then(|f| self.consts.get(f))
                    .is_some_and(|c| *c != "0");
                let forwarded_plain = pair
                    && flag_is_ok
                    && self.plain_ok_payloads.contains(&o)
                    && counts.get(&o).copied().unwrap_or(0) == 0;
                if !forwarded_plain {
                    *counts.entry(o).or_insert(0) -= 1;
                }
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
