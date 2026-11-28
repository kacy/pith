//! Reusable basic-block and successor-graph builders for IR function bodies.

use std::collections::HashMap;

/// Split a function body at `label` boundaries into basic blocks, their
/// successor indices, and their block names (`"entry"` plus each label name).
/// Used by the balance checker.
pub(super) fn split_blocks<'a, 'b>(
    body: &'b [Vec<&'a str>],
) -> (Vec<Vec<&'b [&'a str]>>, Vec<Vec<usize>>, Vec<String>) {
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
    let index: HashMap<&str, usize> = names
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i))
        .collect();
    let mut succ = Vec::new();
    for (i, b) in blocks.iter().enumerate() {
        let next = match b.last() {
            Some(last) if last[0] == "jmp" && last.len() >= 2 => {
                index.get(last[1]).copied().into_iter().collect()
            }
            Some(last) if last[0] == "brif" && last.len() >= 4 => [last[2], last[3]]
                .iter()
                .filter_map(|l| index.get(l).copied())
                .collect(),
            Some(last) if last[0] == "ret" || last[0] == "ret2" => Vec::new(),
            // falls through into the next label
            _ if i + 1 < blocks.len() => vec![i + 1],
            _ => Vec::new(),
        };
        succ.push(next);
    }
    (blocks, succ, names)
}

/// Split a function body at both `label` instructions and immediately after
/// terminators (`jmp`, `brif`, `ret`, `ret2`), returning `(ranges, succs)`.
/// Used by `untagged_stores` for forward may-analysis over instruction slices.
pub(super) fn build_block_ranges(body: &[Vec<&str>]) -> (Vec<(usize, usize)>, Vec<Vec<usize>>) {
    let mut block_starts: Vec<usize> = vec![0];
    let mut label_to_pos: HashMap<&str, usize> = HashMap::new();
    for (i, ins) in body.iter().enumerate() {
        if ins[0] == "label" && ins.len() >= 2 {
            label_to_pos.insert(ins[1], i);
            if i > 0 {
                block_starts.push(i);
            }
        } else if matches!(ins[0], "jmp" | "brif" | "ret" | "ret2") && i + 1 < body.len() {
            block_starts.push(i + 1);
        }
    }
    block_starts.sort_unstable();
    block_starts.dedup();
    let pos_to_block: HashMap<usize, usize> = block_starts
        .iter()
        .enumerate()
        .map(|(b, &p)| (p, b))
        .collect();
    let label_to_block: HashMap<&str, usize> = label_to_pos
        .iter()
        .filter_map(|(&lbl, &pos)| pos_to_block.get(&pos).map(|&b| (lbl, b)))
        .collect();
    let nblocks = block_starts.len();
    let mut ranges: Vec<(usize, usize)> = Vec::with_capacity(nblocks);
    let mut succs: Vec<Vec<usize>> = vec![Vec::new(); nblocks];
    for b in 0..nblocks {
        let start = block_starts[b];
        let end = block_starts.get(b + 1).copied().unwrap_or(body.len());
        ranges.push((start, end));
        if start < end {
            let last = &body[end - 1];
            match last[0] {
                "jmp" if last.len() >= 2 => {
                    if let Some(&t) = label_to_block.get(last[1]) {
                        succs[b].push(t);
                    }
                }
                "brif" if last.len() >= 4 => {
                    if let Some(&t1) = label_to_block.get(last[2]) {
                        succs[b].push(t1);
                    }
                    if let Some(&t2) = label_to_block.get(last[3]) {
                        if !succs[b].contains(&t2) {
                            succs[b].push(t2);
                        }
                    }
                }
                "ret" | "ret2" => {}
                _ => {
                    if b + 1 < nblocks {
                        succs[b].push(b + 1);
                    }
                }
            }
        } else if b + 1 < nblocks {
            succs[b].push(b + 1);
        }
    }
    (ranges, succs)
}
