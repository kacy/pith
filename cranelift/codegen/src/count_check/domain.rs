//! Abstract ownership domain, canonical boundary state, and lattice join.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Val {
    /// not a counted value
    Plain,
    /// a null handle: releasing it does nothing
    Null,
    /// a value of a kind the checker can't settle
    Unknown,
    Obj(String),
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum SlotClass {
    /// the cleanup releases it, or it's a global: holds one count or null
    Holder,
    /// a counted slot that holds no count of its own
    View,
    Scalar,
    /// an emitter temp with no mark: carries whatever was stored
    Carrier,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum TagTest {
    ResultOk,
    ResultNotOk,
    OptSome,
    OptNone,
}

impl TagTest {
    pub(super) fn invert(self) -> Self {
        match self {
            TagTest::ResultOk => TagTest::ResultNotOk,
            TagTest::ResultNotOk => TagTest::ResultOk,
            TagTest::OptSome => TagTest::OptNone,
            TagTest::OptNone => TagTest::OptSome,
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct State {
    pub(super) counts: BTreeMap<String, i64>,
    pub(super) slots: BTreeMap<String, Val>,
    pub(super) regs: HashMap<String, Val>,
    /// objects a null may stand in for: paths joined where one side held the
    /// object and the other a null, and releasing a null does nothing
    pub(super) maybe_null: HashSet<String>,
    /// for a field read: the field it read, as "base:index", and when
    pub(super) field_of: HashMap<String, (String, usize)>,
    /// fields whose value was released through a read and not yet replaced
    pub(super) emptied: BTreeSet<String>,
    /// optionals a channel receive handed back: no shell at all when nothing
    /// came, so a clear flag means a null (runtime_ownership.txt)
    pub(super) none_is_null: BTreeSet<String>,
    /// tuple shells built without a destructor: freeing one frees the shell
    /// alone, and the counts its fields held pass to whoever read them out
    pub(super) shells: BTreeSet<String>,
    /// 3-slot result-box shells (`is_ok`, `ok`, `err`)
    pub(super) result_shells: BTreeSet<String>,
    /// stack-allocated closures (`closure_stack`): never released, so storing
    /// a counted capture via `pith_closure_set_env_rc` leaks it
    pub(super) stack_closures: BTreeSet<String>,
    /// the shell fields, as "shell:index", that hold an owned payload
    pub(super) owned_fields: BTreeSet<String>,
    /// a register that tests an object against null: test register ->
    /// (object, whether a set test means the object is there)
    pub(super) null_tests: BTreeMap<String, (String, bool)>,
    /// a send's result register -> the object it took a count of. the
    /// channel keeps that count only when the send reports success, so a
    /// branch on the result gives it back on the failing side
    pub(super) cond_takes: BTreeMap<String, String>,
    /// field reads that took a count of their own: giving that back leaves
    /// the read holding nothing, and a further release is one too many,
    /// not the release of the field's value a replace starts with
    pub(super) counted_reads: HashSet<String>,
    /// pending weak retains per register (`pith_struct_weak_retain`): each
    /// credit covers exactly one subsequent weak `sstore` (or weak capture)
    pub(super) weak_credits: BTreeMap<String, usize>,
    /// register holding an `is_ok`/`is_some`/`optflag` tag test on a shell
    pub(super) tag_tests: BTreeMap<String, (String, TagTest)>,
    /// register holding `pith_struct_strong_count(obj)` -> `obj`
    pub(super) strong_count_of: BTreeMap<String, String>,
    /// register holding `pith_struct_strong_count(obj) == 1` -> `obj`
    pub(super) unique_tests: BTreeMap<String, String>,
    /// shells known to have `strong_count > 1` on this branch (releasing one
    /// does not destroy its payloads; readers must retain explicitly)
    pub(super) shared_shells: BTreeSet<String>,
}

/// the state at a block boundary. an object a slot holds is named by its
/// slots, so two paths that put different values in the same slots agree. an
/// object only registers hold keeps the name of the register that made it:
/// each register is defined once in a function, so the name is the same on
/// every path, and a register used past a block boundary is one whose
/// definition dominates the use.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(super) struct Canon {
    /// slot-held objects: count held, and whether a null may stand in
    pub(super) objs: BTreeMap<Vec<String>, (i64, bool)>,
    /// register-only objects: the count each holds, which is 0 for a
    /// borrowed value a later retain or release can still reach
    pub(super) regobjs: BTreeMap<String, i64>,
    /// register-only field reads: object key -> (field, read order)
    pub(super) reads: BTreeMap<String, (String, usize)>,
    /// field reads that took a count of their own, by object key
    pub(super) counted_reads: BTreeSet<String>,
    /// what each live register refers to: an object's key, or "null"
    pub(super) regs: BTreeMap<String, String>,
    pub(super) nulls: BTreeSet<String>,
    pub(super) unknowns: BTreeSet<String>,
    pub(super) emptied: BTreeSet<String>,
    pub(super) shells: BTreeSet<String>,
    pub(super) result_shells: BTreeSet<String>,
    pub(super) stack_closures: BTreeSet<String>,
    pub(super) none_is_null: BTreeSet<String>,
    pub(super) owned_fields: BTreeSet<String>,
    /// view slots that share an owned slot's value: view -> owned slot
    pub(super) aliases: BTreeMap<String, String>,
    /// pending weak-retain credits per register that hold on both incoming paths
    pub(super) weak_credits: BTreeMap<String, usize>,
    /// register holding `strong_count(obj) == 1` -> canonical obj key
    pub(super) unique_tests: BTreeMap<String, String>,
    /// shells proven `strong_count > 1` on both incoming paths
    pub(super) shared_shells: BTreeSet<String>,
}

/// where two paths meet. a slot holding one object on one side and a null on
/// the other becomes that object or a null. a register only one side defines
/// can't be used past the join, so a count it holds there is a leak. any
/// other disagreement is one the checker can't settle.
pub(super) fn join(a: &Canon, b: &Canon, leaks: &mut Vec<String>, mismatches: &mut Vec<(String, i64, i64)>) -> Result<Canon, String> {
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
        result_shells: a.result_shells.union(&b.result_shells).cloned().collect(),
        stack_closures: a.stack_closures.union(&b.stack_closures).cloned().collect(),
        // a receive's no-shell none holds only where both paths got the value
        // from a receive: an optional of another kind can be a real none shell
        none_is_null: a.none_is_null.intersection(&b.none_is_null).cloned().collect(),
        owned_fields: a.owned_fields.union(&b.owned_fields).cloned().collect(),
        // a view only one path pointed at a value is dead past the join
        aliases: a.aliases.iter().filter(|(v, h)| b.aliases.get(*v) == Some(*h)).map(|(v, h)| (v.clone(), h.clone())).collect(),
        weak_credits: a
            .weak_credits
            .iter()
            .filter_map(|(r, va)| b.weak_credits.get(r).map(|vb| (r.clone(), (*va).min(*vb))))
            .filter(|(_, n)| *n > 0)
            .collect(),
        unique_tests: a
            .unique_tests
            .iter()
            .filter(|(r, obj)| b.unique_tests.get(*r) == Some(*obj))
            .map(|(r, obj)| (r.clone(), obj.clone()))
            .collect(),
        shared_shells: a.shared_shells.intersection(&b.shared_shells).cloned().collect(),
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
            // the same holders reach the join with a different number of
            // counts: one path took or gave up a count the other didn't (in
            // a loop, every pass does it again). reported, and the first
            // side's count, the block's earlier state, carried on so the walk
            // settles instead of growing the count each pass.
            (Some(x), Some(y)) => {
                mismatches.push((k.join("|"), x.0, y.0));
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
    // a view slot that aliases an owned slot on one path and borrows a value
    // of its own on the other (`mut cur := cursor`, then `cur` reassigned in
    // a loop) still holds a borrow past the join, with no count of its own
    for (x, y) in [(a, b), (b, a)] {
        for view in x.aliases.keys() {
            let own = vec![view.clone()];
            if !out.aliases.contains_key(view)
                && y.objs.get(&own).is_some_and(|v| v.0 == 0)
                && !out.objs.contains_key(&own)
            {
                out.objs.insert(own, (0, false));
            }
        }
    }
    let ids: BTreeSet<&String> = a.regobjs.keys().chain(b.regobjs.keys()).collect();
    for id in ids {
        let (x, y) = (a.regobjs.get(id).copied(), b.regobjs.get(id).copied());
        match (x, y) {
            (Some(x), Some(y)) if x == y => {
                out.regobjs.insert(id.clone(), x);
            }
            (Some(x), Some(y)) if x != 0 && y != 0 => return Err(format!("register object {id} holds {x} on one path and {y} on the other")),
            _ => {
                // at most one side holds a count. the other side either never
                // made the object or holds it borrowed, and either way holds
                // no count of it there.
                let (n, holder, other) = if x.unwrap_or(0) != 0 { (x.unwrap_or(0), a, b) } else { (y.unwrap_or(0), b, a) };
                // a count one path holds in registers the other path settled to
                // a null (a channel receive that brought nothing) is one count
                // or nothing past the join, like a slot's
                let refs: Vec<&String> = holder.regs.iter().filter(|(_, k)| *k == id).map(|(r, _)| r).collect();
                let settled = !refs.is_empty() && refs.iter().all(|r| other.regs.get(*r).is_some_and(|k| k == "null"));
                if n > 0 && settled {
                    out.regobjs.insert(id.clone(), n);
                } else if n > 0 {
                    leaks.push(id.clone());
                }
            }
        }
    }
    // a read survives where its object does, recorded the same way on both
    // sides or on the only side that made it
    for (id, read) in a.reads.iter().chain(b.reads.iter()) {
        if out.regobjs.contains_key(id) {
            out.reads.entry(id.clone()).or_insert_with(|| read.clone());
        }
    }
    out.counted_reads = a.counted_reads.union(&b.counted_reads).filter(|o| out.regobjs.contains_key(*o)).cloned().collect();
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


/// A `base:offset` field with its base object renamed to its canonical key,
/// when it has one.
pub(super) fn rekey_field(key_of: &HashMap<&str, String>, field: &str) -> String {
    let (base, off) = field.rsplit_once(':').unwrap_or((field, ""));
    let base = key_of.get(base).map(String::as_str).unwrap_or(base);
    format!("{base}:{off}")
}
