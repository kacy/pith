//! Single-pass IR and count-mark tokenizer shared by the balance,
//! loop-borrow, and untagged-store checkers.

use super::kind_of_text;
use std::collections::{HashMap, HashSet};

pub(super) struct Func<'a> {
    pub(super) name: &'a str,
    pub(super) ret_kind: &'a str,
    pub(super) params: Vec<&'a str>,
    pub(super) kinds: HashMap<&'a str, &'a str>,
    pub(super) owns: HashSet<&'a str>,
    pub(super) mixed: HashSet<&'a str>,
    pub(super) borrows: Vec<(&'a str, &'a str)>,
    pub(super) marked: bool,
    pub(super) body: Vec<Vec<&'a str>>,
}

pub(super) struct ParsedIr<'a> {
    /// Globals named by `mark global NAME KIND`, normalized via `kind_of_text`.
    pub(super) globals: HashMap<&'a str, &'a str>,
    /// Globals declared by `global NAME ...` lines (used by `check_loop_borrows`).
    pub(super) global_decls: HashSet<&'a str>,
    /// All declared user function names.
    pub(super) user_funcs: HashSet<&'a str>,
    /// Every token referenced by any non-`func`/non-`mark` line (used by
    /// `untagged_stores` to tell called functions from unspecialized templates).
    pub(super) named_tokens: HashSet<&'a str>,
    pub(super) funcs: Vec<Func<'a>>,
}

impl<'a> ParsedIr<'a> {
    pub(super) fn parse(lines: &[&'a str]) -> Self {
        let mut globals: HashMap<&'a str, &'a str> = HashMap::new();
        let mut global_decls: HashSet<&'a str> = HashSet::new();
        let mut user_funcs: HashSet<&'a str> = HashSet::new();
        let mut named_tokens: HashSet<&'a str> = HashSet::new();
        let mut funcs: Vec<Func<'a>> = Vec::new();
        let mut cur: Option<Func<'a>> = None;

        for line in lines {
            let parts: Vec<&'a str> = line.split_whitespace().collect();
            let Some(&op) = parts.first() else {
                continue;
            };
            if op != "func" && op != "mark" {
                named_tokens.extend(parts.iter().skip(1).copied());
            }
            match op {
                "global" if parts.len() >= 2 => {
                    global_decls.insert(parts[1]);
                }
                "mark" if parts.len() >= 4 && parts[1] == "global" => {
                    globals.insert(parts[2], kind_of_text(parts[3]));
                }
                "func" if parts.len() >= 2 => {
                    if let Some(f) = cur.take() {
                        funcs.push(f);
                    }
                    user_funcs.insert(parts[1]);
                    cur = Some(Func {
                        name: parts[1],
                        ret_kind: parts.get(3).copied().unwrap_or("void"),
                        params: Vec::new(),
                        kinds: HashMap::new(),
                        owns: HashSet::new(),
                        mixed: HashSet::new(),
                        borrows: Vec::new(),
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
                    let Some(f) = cur.as_mut() else {
                        continue;
                    };
                    if op == "mark" {
                        if parts.len() >= 3 {
                            f.marked = true;
                            match parts[1] {
                                // `owns` names only the family (`struct`); `kind` the type
                                "kind" if parts.len() >= 4 => {
                                    f.kinds.insert(parts[2], kind_of_text(parts[3]));
                                }
                                "owns" if parts.len() >= 4 => {
                                    f.owns.insert(parts[2]);
                                    f.kinds.entry(parts[2]).or_insert(kind_of_text(parts[3]));
                                }
                                "mixed" => {
                                    f.mixed.insert(parts[2]);
                                }
                                "borrow" if parts.len() >= 4 => {
                                    f.borrows.push((parts[2], parts[3]));
                                }
                                _ => {}
                            }
                        }
                        continue;
                    }
                    if op == "param" && parts.len() >= 2 {
                        f.params.push(parts[1]);
                    }
                    f.body.push(parts);
                }
            }
        }
        if let Some(f) = cur.take() {
            funcs.push(f);
        }

        Self {
            globals,
            global_decls,
            user_funcs,
            named_tokens,
            funcs,
        }
    }
}
