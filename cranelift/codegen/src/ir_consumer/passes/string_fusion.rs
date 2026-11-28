//! Pre-lowering string concatenation, self-append, and int-part fusion passes.
//!
//! Run strictly in order: `fuse_concat_chains` -> `fuse_self_appends` -> `fuse_int_parts`.

use cranelift_module::{DataId, FuncId};
use std::collections::{HashMap, HashSet};

/// Rewrite each chain of string concatenations in a function body into one
/// `concatn dst count part...` instruction, or return `None` when the body has
/// no chain. The emitter lowers `a + b + c + d` to a concat per `+`, each
/// result released right after the next concat reads it, so a chain of n
/// parts made n - 1 strings and copied the leading parts over and over;
/// `concatn` makes one.
///
/// A concat continues a chain when its left operand is the result of an
/// earlier concat that nothing else reads: the register appears once as a
/// left operand, once in a `pith_cstring_release`, and nowhere else (a number
/// that merely looks like it, an immediate, only makes the check refuse). No
/// label, branch or return may come between the two. The chain's
/// intermediate results and their releases go away, and a release of one of
/// the chain's parts that sat inside the chain moves to just after the
/// `concatn`, so every part is still alive when it is read. A string is
/// immutable, so releasing it later changes nothing but when it is freed.
pub(in crate::ir_consumer) fn fuse_concat_chains(
    body: &[&str],
    param_names: &[String],
    declared_funcs: &HashMap<String, FuncId>,
    global_data: &HashMap<String, DataId>,
) -> Option<Vec<String>> {
    let parsed: Vec<Vec<&str>> = body.iter().map(|l| l.split_whitespace().collect()).collect();
    if !parsed.iter().any(|p| p.first() == Some(&"concat")) {
        return None;
    }
    let is_concat = |p: &[&str]| p.len() == 4 && p[0] == "concat";
    let release_of = |p: &[&str]| -> Option<usize> {
        if p.len() == 6 && p[0] == "call" && p[2] == "pith_cstring_release" && p[4] == "1" {
            p[5].parse().ok()
        } else {
            None
        }
    };
    let mut local_vars: HashSet<&str> = param_names.iter().map(String::as_str).collect();
    for p in &parsed {
        if p.len() >= 3 && p[0] == "store" {
            local_vars.insert(p[1]);
        }
    }
    // Only pure string-formatting / cstring-RC runtime helpers can sit between
    // two fused concats without risking mutation or deallocation of an earlier
    // borrowed string part (e.g., `h.name + " " + mutate(h)` or a temporary
    // container release). Any user call, indirect call, store, or non-string
    // release acts as a fusion barrier.
    let is_pure_concat_call = |callee: &str| -> bool {
        if declared_funcs.contains_key(callee)
            || global_data.contains_key(callee)
            || local_vars.contains(callee)
        {
            return false;
        }
        matches!(
            callee,
            "pith_cstring_release"
                | "pith_cstring_retain"
                | "int_to_string"
                | "pith_int_to_cstr"
                | "uint_to_string"
                | "pith_uint_to_cstr"
                | "bool_to_string"
                | "pith_bool_to_cstr"
                | "float_to_string"
                | "pith_float_to_cstr"
                | "float_format"
                | "pith_float_format"
                | "format_pad"
                | "pith_format_pad"
                | "char_to_string"
                | "pith_char_to_string"
                | "chr"
                | "string_len"
                | "pith_cstring_len"
                | "byte_at"
                | "pith_cstring_byte_at"
        )
    };
    let is_barrier = |p: &[&str]| match p.first().copied() {
        Some("label" | "brif" | "jmp" | "ret" | "ret2" | "store" | "sstore" | "rcall" | "calli") => {
            true
        }
        Some("call") => p.get(2).is_none_or(|callee| !is_pure_concat_call(callee)),
        _ => false,
    };

    // every token after the opcode, counted. a register's own definition and
    // its uses all show up here, and so does anything that looks like it.
    let mut mentions: HashMap<&str, usize> = HashMap::new();
    for p in &parsed {
        for t in p.iter().skip(1) {
            *mentions.entry(t).or_insert(0) += 1;
        }
    }
    let mut concat_line: HashMap<usize, usize> = HashMap::new();
    let mut release_lines: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, p) in parsed.iter().enumerate() {
        if is_concat(p) {
            if let Ok(dst) = p[1].parse::<usize>() {
                concat_line.insert(dst, i);
            }
        }
        if let Some(reg) = release_of(p) {
            release_lines.entry(reg).or_default().push(i);
        }
    }

    // prev[i] = the line of the concat whose result line i extends.
    let mut prev: HashMap<usize, usize> = HashMap::new();
    let mut barrier_before = vec![0usize; parsed.len() + 1];
    for (i, p) in parsed.iter().enumerate() {
        barrier_before[i + 1] = barrier_before[i] + usize::from(is_barrier(p));
    }
    for (i, p) in parsed.iter().enumerate() {
        if !is_concat(p) {
            continue;
        }
        let Ok(left) = p[2].parse::<usize>() else {
            continue;
        };
        let Some(&j) = concat_line.get(&left) else {
            continue;
        };
        let released_after = release_lines.get(&left).is_some_and(|r| r.len() == 1 && r[0] > i);
        if j < i
            && mentions.get(p[2]).copied() == Some(3)
            && released_after
            && barrier_before[i] == barrier_before[j + 1]
        {
            prev.insert(i, j);
        }
    }
    if prev.is_empty() {
        return None;
    }

    let extended: HashSet<usize> = prev.values().copied().collect();
    let mut chains: Vec<(Vec<usize>, Vec<&str>)> = Vec::new();
    for &last in prev.keys() {
        if extended.contains(&last) {
            continue; // not the end of its chain
        }
        let mut members = vec![last];
        let mut at = last;
        while let Some(&j) = prev.get(&at) {
            members.push(j);
            at = j;
        }
        members.reverse();
        let first = members[0];
        let mut parts: Vec<&str> = vec![parsed[first][2], parsed[first][3]];
        for &m in &members[1..] {
            parts.push(parsed[m][3]);
        }
        chains.push((members, parts));
    }
    // a register that is a part of two chains would have its release moved
    // by one and needed by the other, so neither of those chains is fused.
    let mut chains_of_part: HashMap<&str, usize> = HashMap::new();
    for (_, parts) in &chains {
        let unique: HashSet<&str> = parts.iter().copied().collect();
        for part in unique {
            *chains_of_part.entry(part).or_insert(0) += 1;
        }
    }
    chains.retain(|(_, parts)| parts.iter().all(|part| chains_of_part[part] == 1));
    if chains.is_empty() {
        return None;
    }

    let mut dropped = vec![false; parsed.len()];
    let mut replaced: HashMap<usize, String> = HashMap::new();
    let mut moved_after: HashMap<usize, Vec<usize>> = HashMap::new();
    for (members, parts) in &chains {
        let first = members[0];
        let last = members[members.len() - 1];
        for &m in &members[..members.len() - 1] {
            dropped[m] = true;
            if let Ok(dst) = parsed[m][1].parse::<usize>() {
                for &r in &release_lines[&dst] {
                    dropped[r] = true;
                }
            }
        }
        let part_regs: HashSet<usize> =
            parts.iter().filter_map(|t| t.parse().ok()).collect();
        let mut moved = Vec::new();
        for k in first + 1..last {
            if dropped[k] {
                continue;
            }
            if release_of(&parsed[k]).is_some_and(|r| part_regs.contains(&r)) {
                dropped[k] = true;
                moved.push(k);
            }
        }
        moved_after.insert(last, moved);
        replaced.insert(
            last,
            format!("concatn {} {} {}", parsed[last][1], parts.len(), parts.join(" ")),
        );
    }

    let mut out = Vec::with_capacity(body.len());
    for (i, line) in body.iter().enumerate() {
        if let Some(fused) = replaced.get(&i) {
            out.push(fused.clone());
            for &k in &moved_after[&i] {
                out.push(body[k].to_string());
            }
        } else if !dropped[i] {
            out.push(line.to_string());
        }
    }
    Some(out)
}

/// Rewrite `s = s + ...` on a local into `appendn dst count base part...`,
/// so the string grows in place while the local holds its only count. The
/// emitter lowers that assignment as a load of the old value (O), a second
/// load as the concat's left operand (A), the concat or concatn into D, a
/// release of O, and `store V D`. The release is the evidence that V owns a
/// count; the rewrite hands that count to `pith_cstring_append_many` instead
/// and drops the release. It applies only when, from the first of the two
/// loads to the store, nothing else loads or stores V and no label, branch
/// or return intervenes; O, A and D have no other use; and V is not a global
/// (`__g_`), which another task may be reading.
pub(in crate::ir_consumer) fn fuse_self_appends(
    body: &[&str],
    global_data: &HashMap<String, DataId>,
) -> Option<Vec<String>> {
    let parsed: Vec<Vec<&str>> = body.iter().map(|l| l.split_whitespace().collect()).collect();
    let is_join = |p: &[&str]| (p.len() == 4 && p[0] == "concat") || (p.len() >= 5 && p[0] == "concatn");
    if !parsed.iter().any(|p| is_join(p)) {
        return None;
    }
    let mut mentions: HashMap<&str, usize> = HashMap::new();
    let mut load_of: HashMap<&str, (usize, &str)> = HashMap::new();
    let mut join_line: HashMap<&str, usize> = HashMap::new();
    let mut release_line: HashMap<&str, usize> = HashMap::new();
    for (i, p) in parsed.iter().enumerate() {
        for t in p.iter().skip(1) {
            *mentions.entry(t).or_insert(0) += 1;
        }
        if p.len() == 3 && p[0] == "load" {
            load_of.insert(p[1], (i, p[2]));
        } else if is_join(p) {
            join_line.insert(p[1], i);
        } else if p.len() == 6 && p[0] == "call" && p[2] == "pith_cstring_release" && p[4] == "1" {
            release_line.insert(p[5], i);
        }
    }
    let once = |r: &str| mentions.get(r).copied() == Some(2);

    let mut dropped = vec![false; parsed.len()];
    let mut replaced: HashMap<usize, String> = HashMap::new();
    for (k, p) in parsed.iter().enumerate() {
        if p.len() != 3
            || p[0] != "store"
            || p[1].starts_with("__g_")
            || global_data.contains_key(p[1])
        {
            continue;
        }
        let (var, dst) = (p[1], p[2]);
        let Some(&c) = join_line.get(dst) else { continue };
        let join = &parsed[c];
        let (base, rest) = if join[0] == "concat" {
            (join[2], &join[3..])
        } else {
            (join[3], &join[4..])
        };
        let Some(&(a, a_var)) = load_of.get(base) else { continue };
        if a_var != var || a > c || !once(dst) || !once(base) {
            continue;
        }
        // the release of the old value, between the join and the store
        let old = (c + 1..k).find_map(|r| {
            let rp = &parsed[r];
            let is_release = rp.len() == 6 && rp[0] == "call" && rp[2] == "pith_cstring_release";
            let reg = if is_release { rp[5] } else { return None };
            match load_of.get(reg) {
                Some(&(o, v)) if v == var && once(reg) && release_line.get(reg) == Some(&r) => {
                    Some((o, r))
                }
                _ => None,
            }
        });
        let Some((o, r)) = old else { continue };
        let start = o.min(a);
        let clean = (start..=k).all(|i| {
            let q = &parsed[i];
            let barrier = matches!(q.first(), Some(&"label" | &"brif" | &"jmp" | &"ret" | &"ret2"));
            let stores_var = i != k && q.len() == 3 && q[0] == "store" && q[1] == var;
            let loads_var = q.len() == 3 && q[0] == "load" && q[2] == var && i != o && i != a;
            !barrier && !stores_var && !loads_var
        });
        if !clean {
            continue;
        }
        dropped[r] = true;
        replaced.insert(c, format!("appendn {} {} {} {}", dst, rest.len(), base, rest.join(" ")));
    }
    if replaced.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(body.len());
    for (i, line) in body.iter().enumerate() {
        if let Some(rewritten) = replaced.get(&i) {
            out.push(rewritten.clone());
        } else if !dropped[i] {
            out.push(line.to_string());
        }
    }
    Some(out)
}


/// Fold `n.to_string()` into the join that reads it, so `"id=" + n.to_string()`
/// and `"id={n}"` write the number straight into the joined string instead of
/// making a string for it first. The emitter lowers the conversion as
/// `call R int_to_string string 1 X`, uses R as one part of a `concat`, and
/// releases R afterwards. When R has no other mention and no label, branch or
/// return sits between its definition and its release, the conversion and the
/// release go away and the join takes X itself, marked as an Int in the count
/// word (`pith_runtime::JOIN_INT_SHIFT`). A plain `concat` becomes a
/// two-part `concatn`. A join of more parts than one runtime call takes is
/// left alone, since the Int bits describe a single call.
pub(in crate::ir_consumer) fn fuse_int_parts(
    body: &[&str],
    param_names: &[String],
    declared_funcs: &HashMap<String, FuncId>,
    global_data: &HashMap<String, DataId>,
) -> Option<Vec<String>> {
    if declared_funcs.contains_key("int_to_string")
        || global_data.contains_key("int_to_string")
        || param_names.iter().any(|n| n == "int_to_string")
    {
        return None;
    }
    let parsed: Vec<Vec<&str>> = body.iter().map(|l| l.split_whitespace().collect()).collect();
    if parsed
        .iter()
        .any(|p| p.len() >= 3 && p[0] == "store" && p[1] == "int_to_string")
    {
        return None;
    }
    // R -> (line of its definition, X)
    let mut made: HashMap<&str, (usize, &str)> = HashMap::new();
    for (i, p) in parsed.iter().enumerate() {
        if p.len() == 6 && p[0] == "call" && p[2] == "int_to_string" && p[4] == "1" && p[5].parse::<usize>().is_ok() {
            made.insert(p[1], (i, p[5]));
        }
    }
    if made.is_empty() {
        return None;
    }
    let mut mentions: HashMap<&str, usize> = HashMap::new();
    let mut release_lines: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut barriers_before = vec![0usize; parsed.len() + 1];
    for (i, p) in parsed.iter().enumerate() {
        for t in p.iter().skip(1) {
            *mentions.entry(t).or_insert(0) += 1;
        }
        if p.len() == 6 && p[0] == "call" && p[2] == "pith_cstring_release" && p[4] == "1" {
            release_lines.entry(p[5]).or_default().push(i);
        }
        let barrier = matches!(p.first(), Some(&"label" | &"brif" | &"jmp" | &"ret" | &"ret2"));
        barriers_before[i + 1] = barriers_before[i] + usize::from(barrier);
    }

    let mut dropped = vec![false; parsed.len()];
    let mut replaced: HashMap<usize, String> = HashMap::new();
    for (j, p) in parsed.iter().enumerate() {
        // (the tokens before the parts, the parts, the most one call takes)
        let (head, parts, max) = match p.first() {
            Some(&"concat") if p.len() == 4 => (vec!["concatn", p[1]], &p[2..], pith_runtime::CONCAT_MANY_MAX),
            Some(&"concatn") if p.len() >= 5 => (vec!["concatn", p[1]], &p[3..], pith_runtime::CONCAT_MANY_MAX),
            Some(&"appendn") if p.len() >= 5 => (vec!["appendn", p[1]], &p[4..], pith_runtime::APPEND_MANY_MAX),
            _ => continue,
        };
        if parts.len() > max {
            continue;
        }
        let mut ints = 0usize;
        let mut joined: Vec<&str> = parts.to_vec();
        for (i, part) in parts.iter().enumerate() {
            let Some(&(made_at, number)) = made.get(part) else { continue };
            let Some(&[released_at]) = release_lines.get(part).map(Vec::as_slice) else { continue };
            if made_at < j
                && j < released_at
                && mentions.get(part).copied() == Some(3)
                && barriers_before[released_at] == barriers_before[made_at]
            {
                ints |= 1 << i;
                joined[i] = number;
                dropped[made_at] = true;
                dropped[released_at] = true;
            }
        }
        if ints == 0 {
            continue;
        }
        let count = parts.len() | ints << pith_runtime::JOIN_INT_SHIFT;
        let base = if head[0] == "appendn" { format!(" {}", p[3]) } else { String::new() };
        replaced.insert(j, format!("{} {} {}{} {}", head[0], head[1], count, base, joined.join(" ")));
    }
    if replaced.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(body.len());
    for (i, line) in body.iter().enumerate() {
        if let Some(rewritten) = replaced.get(&i) {
            out.push(rewritten.clone());
        } else if !dropped[i] {
            out.push(line.to_string());
        }
    }
    Some(out)
}

/// The Int bits of a `concatn` or `appendn` count word, already shifted into
/// place for the runtime call. `fuse_int_parts` sets them only on a join of
/// at most `max` parts, so a longer join's word is a plain part count: 256
/// parts would otherwise read as Int bit 0 set and print the first part of
/// every call as a number.
pub(in crate::ir_consumer) fn join_int_bits(count_word: &str, parts: usize, max: usize) -> i64 {
    if parts > max {
        return 0;
    }
    let word = count_word.parse::<i64>().unwrap_or(0);
    word >> pith_runtime::JOIN_INT_SHIFT << pith_runtime::JOIN_INT_SHIFT
}

