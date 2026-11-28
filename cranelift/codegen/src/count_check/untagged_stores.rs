//! A borrowed value stored into a container built without an element tag.
//!
//! The balance rule can't see this. runtime_ownership.txt marks the value of
//! `map_insert` and the other plain stores `b`: the caller keeps its count and
//! the container takes one of its own. That holds only for a container built
//! tagged; an untagged one (`pith_map_new_default`, `pith_list_new_default`)
//! takes nothing. The caller's counts balance either way, and the container is
//! left pointing at a value whose only count belongs to someone else, who
//! frees it on the next overwrite.
//!
//! This pass follows each function's untagged containers through its local
//! slots and struct fields, and reports a plain store of a counted value into
//! one. A store that carries the value's kind (`map_insert_kind`) lets the
//! container adopt it, so only the plain stores are checked.
//!
//! It also reports a struct built over the error type (`struct:Bag[<error>]`):
//! an instance whose type parameter the checker never fixed. Its `= {}` and
//! `= []` defaults are built untagged, and the stores into them happen in
//! other functions, where the per-function walk above cannot follow them. A
//! generic owner's method is also emitted once as an unspecialized template
//! that nothing calls, so only a function that some instruction names, or
//! `main`, is reported.

use super::cfg::build_block_ranges;
use super::ir::ParsedIr;
use super::{is_counted, kind_of_text};
use std::collections::HashMap;

/// constructors of containers that take no count of what is stored in them
const UNTAGGED_CTORS: &[&str] = &[
    "pith_map_new_default",
    "pith_map_new_int",
    "pith_map_new_bytes",
    "pith_list_new_default",
];

/// the plain stores whose count depends on the container's tag: callee,
/// container argument, value argument
const TAG_DEPENDENT_STORES: &[(&str, usize, usize)] = &[
    ("map_insert", 0, 2),
    ("map_insert_ikey", 0, 2),
    ("map_insert_bkey", 0, 2),
    ("pith_map_insert_cstr_owned", 0, 2),
    ("pith_map_insert_ikey_owned", 0, 2),
    ("pith_map_insert_bkey_owned", 0, 2),
    ("pith_list_push_value", 0, 1),
    ("pith_list_push_value_owned", 0, 1),
    ("pith_list_set_value", 0, 2),
    ("pith_list_set_value_owned", 0, 2),
    ("pith_list_set_value_strict", 0, 2),
    ("pith_list_set_value_owned_strict", 0, 2),
    ("pith_list_set_value_checked", 0, 2),
    ("pith_list_set_value_owned_checked", 0, 2),
    ("list_insert", 0, 2),
    ("pith_list_insert_value_owned", 0, 2),
];

/// "function: problem" for each counted value stored into an untagged container
pub fn check_untagged_stores(lines: &[&str]) -> Vec<String> {
    let parsed = ParsedIr::parse(lines);
    check_untagged_stores_parsed(&parsed)
}

pub(super) fn check_untagged_stores_parsed(parsed: &ParsedIr<'_>) -> Vec<String> {
    let mut problems = Vec::new();
    let mut error_structs: Vec<(&str, String)> = Vec::new();
    for f in &parsed.funcs {
        let mut kinds = parsed.globals.clone();
        for (&k, &v) in &f.kinds {
            kinds.insert(k, v);
        }
        for p in check_function(&f.body, &kinds) {
            problems.push(format!("{}: {p}", f.name));
        }
        for ins in &f.body {
            if ins[0] == "call"
                && ins.len() >= 4
                && ins[2] == "pith_struct_alloc"
                && ins[3].contains("<error>")
            {
                error_structs.push((f.name, ins.join(" ")));
            }
        }
    }
    for (name, ins) in error_structs {
        if name == "main" || parsed.named_tokens.contains(name) {
            problems.push(format!(
                "{name}: `{ins}` builds a struct over an unresolved type parameter, whose container defaults take no count of what is stored into them"
            ));
        }
    }
    problems
}

fn check_function(body: &[Vec<&str>], kinds: &HashMap<&str, &str>) -> Vec<String> {
    // the kind of each register, from what defined it
    let mut reg_kind: HashMap<&str, &str> = HashMap::new();
    for ins in body {
        match ins[0] {
            "call" if ins.len() >= 4 => {
                reg_kind.insert(ins[1], kind_of_text(ins[3]));
            }
            "calli" if ins.len() >= 4 => {
                reg_kind.insert(ins[1], kind_of_text(ins[3]));
            }
            "rcall" if ins.len() >= 5 => {
                reg_kind.insert(ins[2], kind_of_text(ins[4]));
            }
            "optval" if ins.len() >= 4 => {
                reg_kind.insert(ins[1], kind_of_text(ins[3]));
            }
            "concat" | "concatn" | "appendn" if ins.len() >= 2 => {
                reg_kind.insert(ins[1], "string");
            }
            "closure_ref" if ins.len() >= 2 => {
                reg_kind.insert(ins[1], "closure");
            }
            "field" if ins.len() >= 5 => {
                reg_kind.insert(ins[1], kind_of_text(ins[4]));
            }
            "load" if ins.len() >= 3 => {
                if let Some(k) = kinds.get(ins[2]) {
                    reg_kind.insert(ins[1], k);
                }
            }
            _ => {}
        }
    }
    // a struct field is named by its struct and its index. an allocation
    // names the type `struct:Name` and a slot's mark plain `Name`
    let field_key = |base: &str, index: i64| {
        reg_kind
            .get(base)
            .map(|k| format!("{}:{index}", k.trim_start_matches("struct:")))
    };
    // Build basic blocks so local slot overwrites (`store xs tagged`) kill a
    // prior untagged store on the same path, while CFG joins use may-analysis
    // (union: a slot or field is untagged if ANY incoming path can store an
    // untagged container into it).
    let (ranges, succs) = build_block_ranges(body);
    let nblocks = ranges.len();

    #[derive(Clone, Default)]
    struct FlowState {
        // register -> origin instruction string when it may hold an untagged container
        regs: HashMap<String, String>,
        // slot name or "StructName:index" -> origin instruction string
        slots: HashMap<String, String>,
    }

    let transfer = |ins: &[&str], st: &mut FlowState| {
        match ins[0] {
            "call" if ins.len() >= 3 && UNTAGGED_CTORS.contains(&ins[2]) => {
                st.regs.insert(ins[1].to_string(), ins.join(" "));
            }
            "load" if ins.len() >= 3 => {
                if let Some(from) = st.slots.get(ins[2]).cloned() {
                    st.regs.insert(ins[1].to_string(), from);
                } else {
                    st.regs.remove(ins[1]);
                }
            }
            "field" if ins.len() >= 5 => {
                let from = ins[3]
                    .parse::<i64>()
                    .ok()
                    .and_then(|b| field_key(ins[2], b / 8))
                    .and_then(|k| st.slots.get(&k))
                    .cloned();
                if let Some(from) = from {
                    st.regs.insert(ins[1].to_string(), from);
                } else {
                    st.regs.remove(ins[1]);
                }
            }
            "store" if ins.len() >= 3 => {
                if let Some(from) = st.regs.get(ins[2]).cloned() {
                    st.slots.insert(ins[1].to_string(), from);
                } else {
                    // overwriting a local slot with a tagged/other value kills
                    // the previous untagged container on this path
                    st.slots.remove(ins[1]);
                }
            }
            "sstore" if ins.len() >= 4 => {
                if let Some(target) = ins[2].parse().ok().and_then(|i| field_key(ins[1], i)) {
                    if let Some(from) = st.regs.get(ins[3]).cloned() {
                        st.slots.insert(target, from);
                    }
                    // struct fields are keyed by type+index (multiple instances may
                    // coexist), so keep weak-update semantics on tagged sstore
                }
            }
            // Any other instruction that defines a register clears untagged status
            "call" | "calli" | "optval" | "concat" | "concatn" | "appendn" | "closure_ref"
            | "iconst" | "fconst" | "strref" | "funcref" | "tuple_stack" | "closure_stack"
            | "optflag"
                if ins.len() >= 2 =>
            {
                st.regs.remove(ins[1]);
            }
            "rcall" if ins.len() >= 3 => {
                st.regs.remove(ins[1]);
                st.regs.remove(ins[2]);
            }
            _ => {}
        }
    };

    let mut in_states: Vec<Option<FlowState>> = vec![None; nblocks];
    let mut worklist: Vec<usize> = Vec::new();
    if nblocks > 0 {
        in_states[0] = Some(FlowState::default());
        worklist.push(0);
    }
    while let Some(b) = worklist.pop() {
        let mut st = in_states[b].clone().unwrap_or_default();
        let (start, end) = ranges[b];
        for ins in &body[start..end] {
            transfer(ins, &mut st);
        }
        for &s in &succs[b] {
            let changed = match &mut in_states[s] {
                None => {
                    in_states[s] = Some(st.clone());
                    true
                }
                Some(dst) => {
                    let mut grew = false;
                    for (k, v) in &st.regs {
                        if !dst.regs.contains_key(k) {
                            dst.regs.insert(k.clone(), v.clone());
                            grew = true;
                        }
                    }
                    for (k, v) in &st.slots {
                        if !dst.slots.contains_key(k) {
                            dst.slots.insert(k.clone(), v.clone());
                            grew = true;
                        }
                    }
                    grew
                }
            };
            if changed && !worklist.contains(&s) {
                worklist.push(s);
            }
        }
    }

    let mut problems = Vec::new();
    for b in 0..nblocks {
        let Some(mut st) = in_states[b].clone() else {
            continue;
        };
        let (start, end) = ranges[b];
        for ins in &body[start..end] {
            if ins[0] == "call" && ins.len() >= 5 {
                if let Some(&(_, c, v)) = TAG_DEPENDENT_STORES.iter().find(|(n, _, _)| *n == ins[2])
                {
                    if let (Some(container), Some(value)) = (ins.get(5 + c), ins.get(5 + v)) {
                        let value_kind = reg_kind.get(value).copied().unwrap_or("unknown");
                        if let Some(from) = st.regs.get(*container) {
                            if is_counted(value_kind) {
                                problems.push(format!(
                                    "`{}` stores a borrowed {value_kind} into a container built untagged by `{from}`, which takes no count of it",
                                    ins.join(" "),
                                ));
                            }
                        }
                    }
                }
            }
            transfer(ins, &mut st);
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(ir: &str) -> Vec<String> {
        let lines: Vec<&str> = ir
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        check_untagged_stores(&lines)
    }

    // struct Saved: lists: Map[String, String]
    // saved := Saved(lists: {}); saved.lists.insert("a", src.name)
    // the `{}` was built from the field's kind alone, as an untagged map
    fn field_store(ctor: &str) -> String {
        format!(
            "
            func main 0 void
            mark kind saved Saved
            mark kind src Src
            mark owns saved struct
            param src
            call 56 {ctor} map 0
            iconst 59 1
            call 58 pith_struct_alloc struct:Saved 1 59
            sstore 58 0 56
            store saved 58
            load 87 saved
            field 88 87 0 map lists
            strref 89 m0s3
            load 90 src
            field 91 90 0 string name
            call 92 map_insert void 3 88 89 91
            ret
            endfunc
            "
        )
    }

    #[test]
    fn a_borrowed_string_stored_into_an_untagged_field_map_is_reported() {
        let problems = check(&field_store("pith_map_new_default"));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("pith_map_new_default"), "{problems:?}");
    }

    #[test]
    fn a_borrowed_string_stored_into_a_tagged_field_map_is_fine() {
        assert!(check(&field_store("pith_map_new_cstr_val")).is_empty());
    }

    // b := Bag() in a generic body, before the instance came from its target
    #[test]
    fn a_struct_built_over_the_error_type_is_reported() {
        let ir = "
            func make__string 0 struct:Bag
            call 0 pith_map_new_default map 0
            iconst 2 1
            call 1 pith_struct_alloc struct:Bag[<error>] 1 2
            sstore 1 0 0
            ret 1
            endfunc
            func main 0 void
            call 3 make__string struct:Bag 0
            ret
            endfunc
        ";
        let problems = check(ir);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("Bag[<error>]"), "{problems:?}");
    }

    // a generic owner's unspecialized method template, which nothing calls
    #[test]
    fn an_uncalled_template_over_the_error_type_is_not_reported() {
        let ir = "
            func Pair__swapped 1 struct:Pair
            param self
            iconst 6 2
            call 5 pith_struct_alloc struct:Pair[<error>] 1 6
            ret 5
            endfunc
        ";
        assert!(check(ir).is_empty());
    }

    #[test]
    fn a_kind_carrying_store_into_an_untagged_list_is_fine() {
        let ir = "
            func f 1 void
            param s
            mark kind s string
            call 1 pith_list_new_default list 0
            store xs 1
            load 2 xs
            load 3 s
            iconst 4 1
            call 5 list_push_kind void 3 2 3 4
            load 6 xs
            iconst 7 9
            call 8 pith_list_push_value void 2 6 7
            ret
            endfunc
        ";
        assert!(check(ir).is_empty());
    }

    #[test]
    fn a_slot_that_also_holds_a_tagged_list_is_not_reported() {
        let ir = "
            func f 1 void
            param s
            mark kind s string
            call 1 pith_list_new_default list 0
            store xs 1
            call 2 pith_list_new_cstr list 0
            store xs 2
            load 3 xs
            load 4 s
            call 5 pith_list_push_value void 2 3 4
            ret
            endfunc
        ";
        assert!(check(ir).is_empty());
    }

    #[test]
    fn a_branch_holding_an_untagged_list_on_one_path_is_reported() {
        let ir = "
            func f 2 void
            param cond
            param s
            mark kind s string
            load 1 cond
            brif 1 L1 L2
            label L1
            call 2 pith_list_new_default list 0
            store xs 2
            jmp L3
            label L2
            call 3 pith_list_new_cstr list 0
            store xs 3
            jmp L3
            label L3
            load 4 xs
            load 5 s
            call 6 pith_list_push_value_owned void 2 4 5
            ret
            endfunc
        ";
        let problems = check(ir);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("pith_list_push_value_owned"), "{problems:?}");
        assert!(problems[0].contains("pith_list_new_default"), "{problems:?}");
    }

    #[test]
    fn concat_rcall_and_closure_kinds_stored_into_untagged_containers_are_reported() {
        let ir = "
            func f 1 void
            param s
            mark kind s string
            call 1 pith_list_new_default list 0
            load 2 s
            concat 3 2 2
            call 4 pith_list_push_value void 2 1 3
            rcall 5 6 make_str string 0
            call 7 pith_list_set_value_owned void 3 1 5 6
            closure_ref 8 f
            call 9 list_insert void 3 1 5 8
            ret
            endfunc
        ";
        let problems = check(ir);
        assert_eq!(problems.len(), 3, "{problems:?}");
    }
}
