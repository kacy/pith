// green_fanout - Rust counterpart of bench/green_fanout.pith.
//
// spawn a batch of 64 short tokio tasks, await each one, repeat. it reports
// throughput and peak rss from /proc/self/status in the same line format as
// the pith program. TOKIO_WORKER_THREADS plays the role of
// PITH_GREEN_WORKERS.
//
// build: cargo build --release --manifest-path bench/green_fanout_rust/Cargo.toml
// run:   ./bench/green_fanout_rust/target/release/green_fanout_rust 2000000

use std::fs;
use std::time::Instant;

const BATCH: i64 = 64;

fn tasks_from_args() -> i64 {
    std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(500000)
}

fn worker(id: i64) -> i64 {
    id * 2 + 1
}

// spawn a batch of workers, then fan in by awaiting each.
async fn run_batch(batch: i64) -> i64 {
    let tasks: Vec<_> = (0..batch)
        .map(|i| tokio::spawn(async move { worker(i) }))
        .collect();
    let mut total = 0;
    for t in tasks {
        total += t.await.unwrap();
    }
    total
}

fn peak_rss_kb() -> i64 {
    let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            if let Ok(n) = rest.replace("kB", "").trim().parse() {
                return n;
            }
        }
    }
    0
}

#[tokio::main]
async fn main() {
    let total_tasks = tasks_from_args();
    let rounds = total_tasks / BATCH;

    let start = Instant::now();
    let mut checksum = 0;
    for _ in 0..rounds {
        checksum += run_batch(BATCH).await;
    }
    let elapsed = start.elapsed().as_millis() as i64;

    let tasks_run = rounds * BATCH;
    let rate = if elapsed > 0 {
        tasks_run * 1000 / elapsed
    } else {
        0
    };
    println!(
        "green_fanout: tasks {tasks_run} batch {BATCH} time {elapsed}ms rate {rate}/s peak_rss {}kb checksum {checksum}",
        peak_rss_kb()
    );
}
