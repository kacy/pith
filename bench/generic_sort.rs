// generic_sort: rust counterpart of bench/generic_sort.pith.
//
// same rows (a seeded 31-bit lcg, built before the clock starts), same
// arms and same output line, so bench/generic_sort_bench.sh can drive this
// binary with BIN=./bench/generic_sort_rust. pith lists hold references to
// their structs, so both arms sort a Vec<&Row> and no label is copied.
// `legacy` is the selection sort (int key) and insertion sort (string key)
// the pith program carries, written as generic functions over a key
// closure. `current` is what a rust program would call: collect the
// references and sort_by_key / sort_by on the field. both are stable, like
// the pith merge sort, and the checksum folds the whole result so a
// different order shows up as a different number.
//
// build: rustc -O -o bench/generic_sort_rust bench/generic_sort.rs
// run:   ./bench/generic_sort_rust <arm> <dist> <key> <n> <rounds>
//          arm   legacy | current
//          dist  sorted | reverse | random | dups
//          key   int | string

use std::collections::HashSet;
use std::env;
use std::time::Instant;

struct Row {
    label: String,
    rank: i64,
}

// selection sort, one key call per surviving element per round.
fn legacy_sort_by_key<T: Copy>(items: &[T], key: impl Fn(T) -> i64) -> Vec<T> {
    let mut result = Vec::new();
    let mut used = HashSet::new();
    while result.len() < items.len() {
        let mut found = false;
        let mut best_idx = 0;
        let mut best_key = 0;
        for (i, &item) in items.iter().enumerate() {
            if used.contains(&i) {
                continue;
            }
            let k = key(item);
            if !found || k < best_key {
                best_idx = i;
                best_key = k;
                found = true;
            }
        }
        if !found {
            return result;
        }
        result.push(items[best_idx]);
        used.insert(best_idx);
    }
    result
}

// insertion sort, re-reading the key of every element it probes.
fn legacy_sort_by_key_string<'a, T: Copy + 'a>(items: &[T], key: impl Fn(T) -> &'a str) -> Vec<T> {
    let mut out: Vec<T> = Vec::new();
    for &item in items {
        let k = key(item);
        let mut placed = false;
        let mut i = 0;
        while i < out.len() && !placed {
            if k < key(out[i]) {
                out.insert(i, item);
                placed = true;
            }
            i += 1;
        }
        if !placed {
            out.push(item);
        }
    }
    out
}

fn label_for(rank: i64) -> String {
    let padded = format!("0000000{rank}");
    format!("row-{}", &padded[padded.len() - 7..])
}

fn rows_for(dist: &str, n: i64) -> Vec<Row> {
    let mut state: i64 = 2463534242;
    let mut rows = Vec::with_capacity(n.max(0) as usize);
    for i in 0..n {
        let rank = match dist {
            "sorted" => i,
            "reverse" => n - i,
            "dups" => {
                state = (state * 1103515245 + 12345) % 2147483648;
                state % 8
            }
            _ => {
                state = (state * 1103515245 + 12345) % 2147483648;
                state % 1000000
            }
        };
        rows.push(Row { label: label_for(rank), rank });
    }
    rows
}

fn checksum(rows: &[&Row]) -> i64 {
    let mut total = 0i64;
    for (i, r) in rows.iter().enumerate() {
        total = (total + (i as i64 + 1) * (r.rank + 1) + r.label.len() as i64) % 1000000007;
    }
    total
}

fn main() {
    let argv: Vec<String> = env::args().collect();
    let arg_at = |at: usize, fallback: &str| argv.get(at).cloned().unwrap_or_else(|| fallback.to_string());
    let int_arg = |at: usize, fallback: i64| argv.get(at).and_then(|s| s.parse().ok()).unwrap_or(fallback);
    let arm = arg_at(1, "current");
    let dist = arg_at(2, "random");
    let key_kind = arg_at(3, "int");
    let n = int_arg(4, 1000);
    let rounds = int_arg(5, 5);

    let rows = rows_for(&dist, n);
    let refs: Vec<&Row> = rows.iter().collect();

    let mut sum = 0i64;
    let t0 = Instant::now();
    for _ in 0..rounds {
        let sorted: Vec<&Row> = if key_kind == "string" {
            if arm == "legacy" {
                legacy_sort_by_key_string(&refs, |r: &Row| r.label.as_str())
            } else {
                let mut out = refs.clone();
                out.sort_by(|a, b| a.label.cmp(&b.label));
                out
            }
        } else if arm == "legacy" {
            legacy_sort_by_key(&refs, |r: &Row| r.rank)
        } else {
            let mut out = refs.clone();
            out.sort_by_key(|r| r.rank);
            out
        };
        sum = (sum + checksum(&sorted)) % 1000000007;
    }
    let elapsed_us = t0.elapsed().as_micros() as i64;

    let per_op_us = if rounds > 0 { elapsed_us / rounds } else { elapsed_us };
    println!("{arm} {dist} {key_kind} n={n} rounds={rounds} total_us={elapsed_us} per_op_us={per_op_us} checksum={sum}");
}
