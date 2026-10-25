// substring_search: rust counterpart of bench/substring_search.pith.
//
// str::contains and str::find over haystacks of 12, 80 and 4000 bytes with
// the needle at the start, in the middle, at the end, or absent, for three
// needles: "comma" (one byte), "word" (seven bytes whose first byte is common
// in the filler) and "repeat" (a run of one byte against a needle that
// differs only in its last byte). find's None maps to -1, as index_of
// returns. every answer folds into one checksum, which matches the pith and
// go versions for the same arguments.
//
// build: rustc -O -o bench/substring_search_rust bench/substring_search.rs
// run:   ./bench/substring_search_rust 20000 [cell|all] [comma|word|repeat|all]

use std::time::Instant;

fn arg_at(argv: &[String], at: usize, fallback: &str) -> String {
    argv.get(at).cloned().unwrap_or_else(|| fallback.to_string())
}

fn int_arg(argv: &[String], at: usize, fallback: i64) -> i64 {
    argv.get(at).and_then(|s| s.parse().ok()).unwrap_or(fallback)
}

// filler with no comma and no occurrence of "sitting".
fn filler(len: usize) -> String {
    let pattern = "lorem ipsum dolor sit amet consectetur s ";
    let mut out = String::new();
    while out.len() < len {
        out.push_str(pattern);
    }
    out.truncate(len);
    out
}

fn size_of(size: &str) -> usize {
    match size {
        "short" => 12,
        "medium" => 80,
        _ => 4000,
    }
}

fn haystack_for(len: usize, pos: &str, needle: &str) -> String {
    if pos == "absent" {
        return filler(len);
    }
    let gap = len - needle.len();
    match pos {
        "start" => format!("{}{}", needle, filler(gap)),
        "end" => format!("{}{}", filler(gap), needle),
        _ => {
            let left = gap / 2;
            format!("{}{}{}", filler(left), needle, filler(gap - left))
        }
    }
}

fn needle_for(kind: &str) -> &'static str {
    match kind {
        "comma" => ",",
        "repeat" => "aaaaaab",
        _ => "sitting",
    }
}

fn repeat_haystack(len: usize, pos: &str) -> String {
    if pos == "absent" {
        return "a".repeat(len);
    }
    let gap = len - 7;
    match pos {
        "start" => format!("aaaaaab{}", "a".repeat(gap)),
        "end" => format!("{}aaaaaab", "a".repeat(gap)),
        _ => {
            let left = gap / 2;
            format!("{}aaaaaab{}", "a".repeat(left), "a".repeat(gap - left))
        }
    }
}

#[allow(dead_code)]
struct Cell {
    name: String,
    haystack: String,
    needle: String,
}

fn build_cells(only: &str, needle_kind: &str) -> Vec<Cell> {
    let mut cells = Vec::new();
    for size in ["short", "medium", "long"] {
        for pos in ["start", "middle", "end", "absent"] {
            let name = format!("{}-{}", size, pos);
            if only != "all" && only != name {
                continue;
            }
            for kind in ["comma", "word", "repeat"] {
                if needle_kind != "all" && needle_kind != kind {
                    continue;
                }
                let needle = needle_for(kind);
                let haystack = if kind == "repeat" {
                    repeat_haystack(size_of(size), pos)
                } else {
                    haystack_for(size_of(size), pos, needle)
                };
                cells.push(Cell {
                    name: format!("{}-{}", name, kind),
                    haystack,
                    needle: needle.to_string(),
                });
            }
        }
    }
    cells
}

fn run(cells: &[Cell], rounds: i64) -> i64 {
    let mut sum: i64 = 0;
    for _ in 0..rounds {
        for cell in cells {
            if cell.haystack.contains(cell.needle.as_str()) {
                sum += 1;
            }
            let index = cell.haystack.find(cell.needle.as_str()).map_or(-1, |i| i as i64);
            sum = (sum * 31 + index + 2) % 1000000007;
        }
    }
    sum
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let rounds = int_arg(&argv, 1, 2000);
    let only = arg_at(&argv, 2, "all");
    let needle_kind = arg_at(&argv, 3, "all");
    let cells = build_cells(&only, &needle_kind);
    let calls = cells.len() as i64 * rounds * 2;

    let t0 = Instant::now();
    let sum = run(&cells, rounds);
    let elapsed_ns = t0.elapsed().as_nanos() as i64;

    let per_call_ns = if calls > 0 { elapsed_ns / calls } else { elapsed_ns };
    println!(
        "cells={} rounds={} calls={} total_us={} per_call_ns={} checksum={}",
        cells.len(),
        rounds,
        calls,
        elapsed_ns / 1000,
        per_call_ns,
        sum
    );
}
