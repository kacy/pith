//! Whole-module reachability and dead function/global/string/tracer pruning.

use super::ir::IrFunction;
use std::collections::{HashMap, HashSet};

/// Match a 3-line cycle-collector tracer registration inside a
/// `*__cc_register_tracers*` function:
///   funcref <r1> <dtor>
///   funcref <r2> <tracer>
///   call <r3> pith_cc_register_tracer void 2 <r1> <r2>
pub(super) fn match_cc_register_tracer<'a>(lines: &[&'a str], i: usize) -> Option<(&'a str, &'a str)> {
    if i + 2 >= lines.len() {
        return None;
    }
    let p0: Vec<&str> = lines[i].split_whitespace().collect();
    let p1: Vec<&str> = lines[i + 1].split_whitespace().collect();
    let p2: Vec<&str> = lines[i + 2].split_whitespace().collect();
    if p0.len() == 3
        && p0[0] == "funcref"
        && p1.len() == 3
        && p1[0] == "funcref"
        && p2.len() == 7
        && p2[0] == "call"
        && p2[2] == "pith_cc_register_tracer"
        && p2[4] == "2"
        && p2[5] == p0[1]
        && p2[6] == p1[1]
    {
        Some((p0[2], p1[2]))
    } else {
        None
    }
}

/// Match a 2-line user-drop registration inside a `*__cc_register_tracers*`
/// function:
///   funcref <r1> <dtor>
///   call <r2> pith_cc_register_user_drop void 1 <r1>
pub(super) fn match_cc_register_user_drop<'a>(lines: &[&'a str], i: usize) -> Option<&'a str> {
    if i + 1 >= lines.len() {
        return None;
    }
    let p0: Vec<&str> = lines[i].split_whitespace().collect();
    let p1: Vec<&str> = lines[i + 1].split_whitespace().collect();
    if p0.len() == 3
        && p0[0] == "funcref"
        && p1.len() == 6
        && p1[0] == "call"
        && p1[2] == "pith_cc_register_user_drop"
        && p1[4] == "1"
        && p1[5] == p0[1]
    {
        Some(p0[2])
    } else {
        None
    }
}

/// Keep only cycle-collector registrations whose destructor function is live.
/// A destructor that no reachable function references can never be attached to
/// a runtime object, so registering its tracer or user-drop is dead work that
/// would otherwise pull hundreds of unreachable dtors/tracers into every binary.
pub(super) fn filter_tracer_reg_body<'a>(body: &[&'a str], live: &HashSet<String>) -> Vec<&'a str> {
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        if let Some((dtor, _tracer)) = match_cc_register_tracer(body, i) {
            if live.contains(dtor) {
                out.push(body[i]);
                out.push(body[i + 1]);
                out.push(body[i + 2]);
            }
            i += 3;
        } else if let Some(dtor) = match_cc_register_user_drop(body, i) {
            if live.contains(dtor) {
                out.push(body[i]);
                out.push(body[i + 1]);
            }
            i += 2;
        } else {
            out.push(body[i]);
            i += 1;
        }
    }
    out
}

/// The functions a program can run: those reachable from main and the
/// startup functions its prologue calls, through any token of a body that
/// names another function (a call, an rcall, a funcref, a closure). Only
/// main is exported, so nothing else can reach the rest. The emitter writes
/// bodies nobody calls: the unspecialized template of a generic method and
/// the copy of a method for an instance that never calls it. Those may call
/// what does not exist for their types, so they are not compiled at all.
/// Inside `*__cc_register_tracers*`, a tracer/user-drop registration only
/// becomes live once its destructor is referenced by live code.
/// IR without a main (a consumer unit test) keeps every function.
pub(super) fn live_functions(functions: &[IrFunction<'_>]) -> Option<HashSet<String>> {
    let mut bodies: HashMap<&str, &[&str]> = HashMap::new();
    for function in functions {
        bodies.entry(function.name).or_insert(&function.body);
    }
    if !bodies.contains_key("main") {
        return None;
    }
    let mut tracer_regs: Vec<&str> = Vec::new();
    let mut work: Vec<&str> = Vec::new();
    for &name in bodies.keys() {
        if name.contains("__cc_register_tracers") {
            tracer_regs.push(name);
        } else if name == "main" || name.starts_with("__init_globals") {
            work.push(name);
        }
    }
    let mut live: HashSet<String> = work
        .iter()
        .chain(tracer_regs.iter())
        .map(|name| name.to_string())
        .collect();
    // Scan any non-registration lines inside `*__cc_register_tracers*` up front
    // so conservative fallback keeps unexpected calls alive.
    for &reg_fn in &tracer_regs {
        let body = bodies[reg_fn];
        let mut i = 0;
        while i < body.len() {
            if match_cc_register_tracer(body, i).is_some() {
                i += 3;
            } else if match_cc_register_user_drop(body, i).is_some() {
                i += 2;
            } else {
                for token in body[i].split_whitespace().skip(1) {
                    if let Some((&callee, _)) = bodies.get_key_value(token) {
                        if live.insert(callee.to_string()) {
                            work.push(callee);
                        }
                    }
                }
                i += 1;
            }
        }
    }
    loop {
        while let Some(name) = work.pop() {
            for line in bodies[name] {
                for token in line.split_whitespace().skip(1) {
                    if let Some((&callee, _)) = bodies.get_key_value(token) {
                        if live.insert(callee.to_string()) {
                            work.push(callee);
                        }
                    }
                }
            }
        }
        // Activate tracer functions only for destructors that live code actually uses.
        let mut added = false;
        for &reg_fn in &tracer_regs {
            let body = bodies[reg_fn];
            let mut i = 0;
            while i < body.len() {
                if let Some((dtor, tracer)) = match_cc_register_tracer(body, i) {
                    if live.contains(dtor) {
                        if let Some((&callee, _)) = bodies.get_key_value(tracer) {
                            if live.insert(callee.to_string()) {
                                work.push(callee);
                                added = true;
                            }
                        }
                    }
                    i += 3;
                } else if match_cc_register_user_drop(body, i).is_some() {
                    i += 2;
                } else {
                    i += 1;
                }
            }
        }
        if !added {
            break;
        }
    }
    Some(live)
}


/// When dead-function elimination is active (`main` is present), collect the
/// globals and string literals live functions actually reference so unused
/// globals and dead `.rodata` string literals are omitted.
pub(super) fn collect_live_globals_and_strings(
    functions: &[IrFunction<'_>],
    live: Option<&HashSet<String>>,
) -> (Option<HashSet<String>>, Option<HashSet<String>>) {
    let Some(live_set) = live else {
        return (None, None);
    };
    let mut g_used: HashSet<String> = HashSet::new();
    let mut s_used: HashSet<String> = HashSet::new();
    let mut seen_funcs: HashSet<&str> = HashSet::new();
    for function in functions {
        if !live_set.contains(function.name) || !seen_funcs.insert(function.name) {
            continue;
        }
        let filtered: Vec<&str>;
        let body: &[&str] = if function.name.contains("__cc_register_tracers") {
            filtered = filter_tracer_reg_body(&function.body, live_set);
            &filtered
        } else {
            &function.body
        };
        for line in body {
            let mut parts = line.split_whitespace();
            let Some(op) = parts.next() else { continue };
            if op == "strref" {
                let _reg = parts.next();
                if let Some(sid) = parts.next() {
                    s_used.insert(sid.to_string());
                }
            } else {
                for tok in parts {
                    g_used.insert(tok.to_string());
                }
            }
        }
    }
    (Some(g_used), Some(s_used))
}
