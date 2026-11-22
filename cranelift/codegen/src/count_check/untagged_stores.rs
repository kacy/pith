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

use super::{is_counted, kind_of_text};
use std::collections::{HashMap, HashSet};

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
    ("pith_list_push_value", 0, 1),
    ("pith_list_set_value", 0, 2),
    ("pith_list_set_value_strict", 0, 2),
    ("pith_list_set_value_checked", 0, 2),
    ("list_insert", 0, 2),
];

/// "function: problem" for each counted value stored into an untagged container
pub fn check_untagged_stores(lines: &[&str]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut name = "";
    let mut globals: HashMap<&str, &str> = HashMap::new();
    let mut kinds: HashMap<&str, &str> = HashMap::new();
    let mut body: Vec<Vec<&str>> = Vec::new();
    for line in lines {
        let parts: Vec<&str> = line.split_whitespace().collect();
        match parts.first() {
            Some(&"func") if parts.len() >= 2 => {
                name = parts[1];
                kinds = globals.clone();
                body.clear();
            }
            Some(&"mark") if parts.len() >= 4 && parts[1] == "global" => {
                globals.insert(parts[2], kind_of_text(parts[3]));
            }
            // `owns` names only the family (`struct`); `kind` the type
            Some(&"mark") if parts.len() >= 4 && parts[1] == "kind" => {
                kinds.insert(parts[2], kind_of_text(parts[3]));
            }
            Some(&"mark") if parts.len() >= 4 && parts[1] == "owns" => {
                kinds.entry(parts[2]).or_insert(kind_of_text(parts[3]));
            }
            Some(&"endfunc") => {
                for p in check_function(&body, &kinds) {
                    problems.push(format!("{name}: {p}"));
                }
            }
            Some(&"mark") | None => {}
            Some(_) => body.push(parts),
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
    // registers holding an untagged container, grown to a fixed point: a slot
    // or field is untagged when every store into it in this function is
    let mut untagged: HashSet<&str> = HashSet::new();
    let mut origin: HashMap<&str, String> = HashMap::new();
    for ins in body {
        if ins[0] == "call" && ins.len() >= 3 && UNTAGGED_CTORS.contains(&ins[2]) {
            untagged.insert(ins[1]);
            origin.insert(ins[1], ins.join(" "));
        }
    }
    loop {
        // slot names and "struct:Name:index" share one map: a slot name has
        // no colon in it
        let mut stores: HashMap<String, (bool, String)> = HashMap::new();
        for ins in body {
            let (target, value) = match ins[0] {
                "store" if ins.len() >= 3 => (Some(ins[1].to_string()), ins[2]),
                "sstore" if ins.len() >= 4 => (
                    ins[2].parse().ok().and_then(|i| field_key(ins[1], i)),
                    ins[3],
                ),
                _ => continue,
            };
            let Some(target) = target else { continue };
            let from = origin.get(value).cloned().unwrap_or_default();
            let e = stores.entry(target).or_insert((true, from));
            e.0 &= untagged.contains(value);
        }
        let mut grew = false;
        for ins in body {
            let from = match ins[0] {
                "load" if ins.len() >= 3 => stores.get(ins[2]),
                "field" if ins.len() >= 5 => ins[3]
                    .parse::<i64>()
                    .ok()
                    .and_then(|b| field_key(ins[2], b / 8))
                    .and_then(|k| stores.get(&k)),
                _ => None,
            };
            if let Some((true, from)) = from {
                if untagged.insert(ins[1]) {
                    origin.insert(ins[1], from.clone());
                    grew = true;
                }
            }
        }
        if !grew {
            break;
        }
    }
    let mut problems = Vec::new();
    for ins in body {
        if ins[0] != "call" || ins.len() < 5 {
            continue;
        }
        let Some(&(_, c, v)) = TAG_DEPENDENT_STORES.iter().find(|(n, _, _)| *n == ins[2]) else {
            continue;
        };
        let (Some(container), Some(value)) = (ins.get(5 + c), ins.get(5 + v)) else {
            continue;
        };
        let value_kind = reg_kind.get(value).copied().unwrap_or("unknown");
        if untagged.contains(container) && is_counted(value_kind) {
            problems.push(format!(
                "`{}` stores a borrowed {value_kind} into a container built untagged by `{}`, which takes no count of it",
                ins.join(" "),
                origin.get(container).map(String::as_str).unwrap_or("?")
            ));
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
}
