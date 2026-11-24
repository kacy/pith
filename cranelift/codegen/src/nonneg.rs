//! Which named variables of a function never hold a negative Int.
//!
//! A checked `+`, `-` or `*` needs less of a check when an operand is known
//! to be at least zero (see `emit_checked_int_arith`): `a - b` of two such
//! operands cannot overflow, `x * 3` can only overflow upward, and `a + b`
//! with `b >= 0` overflows exactly when the wrapped sum comes out below `a`.
//! These operands are mostly loads of loop counters, indexes, lengths and the
//! sums built from them, so the analysis works on variables.
//!
//! A variable is written only by `store` (and a parameter on entry), so it is
//! never negative when every value stored to it is not: a constant of at
//! least zero, a length, a load of a variable that is never negative, or a
//! checked or exact sum or product of two such values. A checked op that
//! would leave the range stops the program instead, and an exact op
//! (`addnsw`) is one the front end proved stays in range. Variables that
//! feed one another are settled together: start from every variable, drop
//! each one with a store not shown to be non-negative, and repeat until
//! nothing drops.
//!
//! This reads the text, not the Cranelift values, and does not follow the
//! order the lines are lowered in, so a register counts as non-negative only
//! when every line that could define it gives a non-negative value. The
//! lines read here define their first operand; any other line is taken to
//! define every register it names, and a parameter's register is defined on
//! entry with a value of unknown sign.

use std::collections::{HashMap, HashSet};

/// Runtime calls whose result is a length or a count, never below zero.
pub(crate) const LENGTH_CALLS: &[&str] = &[
    "pith_cstring_len",
    "pith_list_len",
    "pith_auto_len",
    "pith_bytes_len",
];

/// Whether `op`'s result is at least zero when both operands are: a checked
/// add or multiply, which stops the program rather than wrap, and an add the
/// front end proved exact.
pub(crate) fn keeps_nonneg(op: &str) -> bool {
    matches!(op, "addc" | "mulc" | "addnsw")
}

/// How a register gets its value, as far as its sign goes.
#[derive(Clone, Copy)]
enum Def<'a> {
    /// a constant of at least zero, or a length
    NonNeg,
    /// a load of the named variable
    Load(&'a str),
    /// a checked or exact sum or product of two registers
    Both(usize, usize),
    /// anything else
    Unknown,
}

fn reg_of(token: &str) -> Option<usize> {
    token.parse::<usize>().ok()
}

/// The variables of a function body that never hold a negative value.
/// `param_names` are the function's parameters, in order, and `is_global`
/// says whether a name is a module global; neither kind is a candidate,
/// since its value comes from outside the body. `is_user_function` keeps a
/// call to a module function that shares a length call's name from counting
/// as a length.
pub(crate) fn nonneg_vars(
    body_lines: &[&str],
    param_names: &[String],
    is_global: impl Fn(&str) -> bool,
    is_user_function: impl Fn(&str) -> bool,
) -> HashSet<String> {
    let mut defs: HashMap<usize, Vec<Def>> = HashMap::new();
    let mut stores: Vec<(&str, Option<usize>)> = Vec::new();
    for i in 0..param_names.len() {
        defs.entry(i).or_default().push(Def::Unknown);
    }
    for line in body_lines {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let Some(&op) = parts.first() else { continue };
        let first = parts.get(1).and_then(|t| reg_of(t));
        let def = match op {
            "store" if parts.len() >= 3 => {
                stores.push((parts[1], reg_of(parts[2])));
                continue;
            }
            // count marks state facts for the count checker and define nothing
            "mark" => continue,
            "iconst" if parts.len() >= 3 => match parts[2].parse::<i64>() {
                Ok(v) if v >= 0 => Def::NonNeg,
                _ => Def::Unknown,
            },
            "load" if parts.len() >= 3 => Def::Load(parts[2]),
            "call"
                if parts.len() >= 3
                    && LENGTH_CALLS.contains(&parts[2])
                    && !is_user_function(parts[2]) =>
            {
                Def::NonNeg
            }
            op if keeps_nonneg(op) && parts.len() >= 4 => {
                match (reg_of(parts[2]), reg_of(parts[3])) {
                    (Some(a), Some(b)) => Def::Both(a, b),
                    _ => Def::Unknown,
                }
            }
            _ => {
                for reg in parts[1..].iter().filter_map(|t| reg_of(t)) {
                    defs.entry(reg).or_default().push(Def::Unknown);
                }
                continue;
            }
        };
        if let Some(reg) = first {
            defs.entry(reg).or_default().push(def);
        }
    }

    let mut vars: HashSet<String> = stores
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !param_names.iter().any(|p| p == name) && !is_global(name))
        .map(str::to_string)
        .collect();
    loop {
        let mut memo: HashMap<usize, bool> = HashMap::new();
        let dropped: Vec<&str> = stores
            .iter()
            .filter(|(name, reg)| {
                vars.contains(*name)
                    && !reg.is_some_and(|r| {
                        reg_nonneg(r, &defs, &vars, &mut memo, &mut HashSet::new())
                    })
            })
            .map(|(name, _)| *name)
            .collect();
        if dropped.is_empty() {
            return vars;
        }
        for name in dropped {
            vars.remove(name);
        }
    }
}

/// Whether every definition of `reg` is non-negative, given that the
/// variables in `vars` are. A register reached again through its own
/// definitions (which a well-formed body never does) counts as unknown.
fn reg_nonneg(
    reg: usize,
    defs: &HashMap<usize, Vec<Def>>,
    vars: &HashSet<String>,
    memo: &mut HashMap<usize, bool>,
    visiting: &mut HashSet<usize>,
) -> bool {
    if let Some(&known) = memo.get(&reg) {
        return known;
    }
    if !visiting.insert(reg) {
        return false;
    }
    let result = match defs.get(&reg) {
        None => false,
        Some(list) => list.iter().all(|def| match *def {
            Def::NonNeg => true,
            Def::Load(name) => vars.contains(name),
            Def::Both(a, b) => {
                reg_nonneg(a, defs, vars, memo, visiting)
                    && reg_nonneg(b, defs, vars, memo, visiting)
            }
            Def::Unknown => false,
        }),
    };
    visiting.remove(&reg);
    memo.insert(reg, result);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(body: &str, params: &[&str]) -> Vec<String> {
        let lines: Vec<&str> = body
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        let params: Vec<String> = params.iter().map(|p| p.to_string()).collect();
        let mut out: Vec<String> = nonneg_vars(
            &lines,
            &params,
            |name| name.starts_with("__g_"),
            |name| name == "pith_bytes_len",
        )
        .into_iter()
        .collect();
        out.sort();
        out
    }

    /// bench/tight_loop.pith's `run`: a counter from zero with an exact step,
    /// and a sum of non-negative products
    #[test]
    fn counter_and_sum_are_nonneg() {
        let body = "
            iconst 1 0
            store sum 1
            iconst 2 0
            store i 2
            label L11
            load 3 i
            load 4 n
            lt 5 3 4
            brif 5 L12 L13
            label L12
            load 6 sum
            load 7 i
            iconst 8 3
            mulc 9 7 8
            addc 10 6 9
            store sum 10
            load 11 i
            iconst 12 1
            addnsw 13 11 12
            store i 13
            jmp L11
            label L13
            load 14 sum
            ret 14
        ";
        assert_eq!(vars(body, &["n"]), ["i", "sum"]);
    }

    /// a plain add may wrap (`wrapping_add` lowers to one), so it proves nothing
    #[test]
    fn plain_add_is_unknown() {
        let body = "
            iconst 1 0
            store i 1
            load 2 i
            iconst 3 1
            add 4 2 3
            store i 4
        ";
        assert!(vars(body, &[]).is_empty());
    }

    /// one negative store anywhere, even after a return, drops the variable,
    /// and a variable fed from it drops too
    #[test]
    fn negative_store_drops_dependents() {
        let body = "
            iconst 1 0
            store x 1
            load 2 x
            store y 2
            ret 2
            iconst 3 -5
            store x 3
        ";
        assert!(vars(body, &[]).is_empty());
    }

    /// a subtraction, a parameter, a global, an unknown call and a module
    /// function named like a length call are all of unknown sign; a length is
    /// not
    #[test]
    fn unknown_sources() {
        let body = "
            load 1 n
            store a 1
            load 2 __g_total
            store b 2
            iconst 3 4
            iconst 4 1
            subc 5 3 4
            store c 5
            call 6 parse int 0
            store d 6
            call 7 pith_list_len int 1 0
            store e 7
            call 8 pith_bytes_len int 1 0
            store f 8
            iconst 9 2
            store __g_total 9
        ";
        assert_eq!(vars(body, &["n"]), ["e"]);
    }

    /// a parameter's register is defined on entry, so a body that also
    /// defines it does not make it non-negative
    #[test]
    fn parameter_register_is_unknown() {
        let body = "
            store a 0
            iconst 0 1
            store b 0
        ";
        assert!(vars(body, &["n"]).is_empty());
    }

    /// a register an unrecognized line names counts as redefined there
    #[test]
    fn register_named_elsewhere_is_unknown() {
        let body = "
            iconst 1 7
            rcall 2 1 f int 0
            store a 1
            iconst 3 7
            store b 3
        ";
        assert_eq!(vars(body, &[]), ["b"]);
    }

    /// two variables that feed each other are settled together
    #[test]
    fn mutual_dependence() {
        let body = "
            iconst 1 0
            store a 1
            load 2 b
            iconst 3 2
            mulc 4 2 3
            store a 4
            load 5 a
            load 6 a
            addc 7 5 6
            store b 7
        ";
        assert_eq!(vars(body, &[]), ["a", "b"]);
        let poisoned = format!("{body}\niconst 8 -1\nstore b 8");
        assert!(vars(&poisoned, &[]).is_empty());
    }
}
