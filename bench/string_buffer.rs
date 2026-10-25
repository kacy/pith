// string_buffer: rust counterpart of bench/string_buffer.pith.
//
// two arms over the same inputs. "current" is a String grown with push_str,
// the buffer a rust developer reaches for. "legacy" mirrors the chunk-list
// storage pith's std.io used to have: every write pushes onto a Vec<String>,
// and once the list holds 32 chunks it is joined back into one. the pith
// legacy arm also looks its list up in per-thread maps keyed by a handle on
// every write; that indirection is pith-specific and is left out here.
//
// a build hands the String itself back, with no copy, as pith's take-and-
// close does not. a snapshot is an owned copy (clone) of the prefix, the
// same work pith's string() does.
//
// inputs come from the same seeded 31-bit lcg and are built before the
// clock starts. the output line and checksum match the pith and go versions
// for the same arguments (total_us, per_op_us and peak_kb aside).
//
// build: rustc -O -o bench/string_buffer_rust bench/string_buffer.rs
// run:   ./bench/string_buffer_rust <arm> <workload> <n> <chunk> <rounds> [k]
//        ./bench/string_buffer_rust current build 100000 200 20

use std::time::Instant;

const MOD: i64 = 1000000007;

#[derive(Default)]
struct LegacyBuffer {
    parts: Vec<String>,
    len: usize,
}

impl LegacyBuffer {
    fn string(&self) -> String {
        match self.parts.len() {
            0 => String::new(),
            1 => self.parts[0].clone(),
            _ => self.parts.concat(),
        }
    }

    fn write(&mut self, data: &str) {
        if data.is_empty() {
            return;
        }
        self.parts.push(data.to_string());
        self.len += data.len();
        if self.parts.len() >= 32 {
            let joined = self.parts.concat();
            self.len = joined.len();
            self.parts = vec![joined];
        }
    }
}

fn chunks_for(n: i64, chunk: i64) -> Vec<String> {
    let mut out = Vec::with_capacity(n as usize);
    let mut state: i64 = 2463534242;
    for _ in 0..n {
        let mut piece = String::with_capacity(chunk as usize);
        for _ in 0..chunk {
            state = (state * 1103515245 + 12345) % 2147483648;
            piece.push((97 + state % 26) as u8 as char);
        }
        out.push(piece);
    }
    out
}

fn fold_snapshot(sum: i64, snap: &str) -> i64 {
    let last = snap.as_bytes().last().map_or(0, |&b| b as i64);
    (sum * 31 + snap.len() as i64 * 7 + last) % MOD
}

fn legacy_build(chunks: &[String]) -> String {
    let mut buf = LegacyBuffer::default();
    for piece in chunks {
        buf.write(piece);
    }
    buf.string()
}

fn current_build(chunks: &[String]) -> String {
    let mut buf = String::new();
    for piece in chunks {
        buf.push_str(piece);
    }
    buf
}

fn legacy_snapshot(chunks: &[String], every: i64) -> i64 {
    let mut buf = LegacyBuffer::default();
    let mut sum = 0;
    for (i, piece) in chunks.iter().enumerate() {
        buf.write(piece);
        if (i as i64 + 1) % every == 0 {
            sum = fold_snapshot(sum, &buf.string());
        }
    }
    fold_snapshot(sum, &buf.string())
}

fn current_snapshot(chunks: &[String], every: i64) -> i64 {
    let mut buf = String::new();
    let mut sum = 0;
    for (i, piece) in chunks.iter().enumerate() {
        buf.push_str(piece);
        if (i as i64 + 1) % every == 0 {
            sum = fold_snapshot(sum, &buf.clone());
        }
    }
    fold_snapshot(sum, &buf.clone())
}

fn fnv1a(s: &str) -> i64 {
    let mut h: u32 = 2166136261;
    for &b in s.as_bytes() {
        h = (h ^ b as u32).wrapping_mul(16777619);
    }
    h as i64
}

// the process high-water resident set in kilobytes.
fn peak_kb() -> i64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            if let Ok(v) = rest.trim().trim_end_matches("kB").trim().parse() {
                return v;
            }
        }
    }
    0
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let arg_at = |at: usize, fallback: &str| argv.get(at).cloned().unwrap_or_else(|| fallback.to_string());
    let int_arg = |at: usize, fallback: i64| argv.get(at).and_then(|s| s.parse().ok()).unwrap_or(fallback);
    let arm = arg_at(1, "current");
    let workload = arg_at(2, "build");
    let n = int_arg(3, 10000);
    let chunk = int_arg(4, 8);
    let rounds = int_arg(5, 1);
    let mut every = int_arg(6, 100);
    if every <= 0 {
        every = 1;
    }

    let chunks = chunks_for(n, chunk);

    let mut sum = 0;
    let mut last_out = String::new();
    let t0 = Instant::now();
    for _ in 0..rounds {
        if workload == "snapshot" {
            let s = if arm == "legacy" {
                legacy_snapshot(&chunks, every)
            } else {
                current_snapshot(&chunks, every)
            };
            sum = (sum + s) % MOD;
        } else {
            last_out = if arm == "legacy" {
                legacy_build(&chunks)
            } else {
                current_build(&chunks)
            };
            sum = fold_snapshot(sum, &last_out);
        }
    }
    let elapsed_us = t0.elapsed().as_micros() as i64;
    sum = (sum + fnv1a(&last_out) % MOD) % MOD;

    let per_op_us = if rounds > 0 { elapsed_us / rounds } else { elapsed_us };
    println!(
        "{} {} n={} chunk={} rounds={} every={} total_us={} per_op_us={} peak_kb={} checksum={}",
        arm,
        workload,
        n,
        chunk,
        rounds,
        every,
        elapsed_us,
        per_op_us,
        peak_kb(),
        sum
    );
}
