// task_pingpong - Rust counterpart of bench/task_pingpong.pith.
//
// two tokio tasks pass a token back and forth over a pair of capacity-1
// channels, so the time is the cost of one task-to-task handoff and its
// reply. both sides are spawned, as in the pith program, and main only
// awaits; like pith's main, tokio's main future runs outside the worker
// pool. TOKIO_WORKER_THREADS plays the role of PITH_GREEN_WORKERS.
//
// build: cargo build --release --manifest-path bench/task_pingpong_rust/Cargo.toml
// run:   ./bench/task_pingpong_rust/target/release/task_pingpong 1000000

use std::time::Instant;
use task_pingpong_rust::{pinger, ponger, positive_arg};
use tokio::sync::mpsc::channel;

#[tokio::main]
async fn main() {
    let rounds = positive_arg(1, 200000);
    let (req_tx, req_rx) = channel(1);
    let (rsp_tx, rsp_rx) = channel(1);

    let start = Instant::now();
    let pong = tokio::spawn(ponger(req_rx, rsp_tx));
    let ping = tokio::spawn(pinger(req_tx, rsp_rx, rounds));
    let sum = ping.await.unwrap();
    let seen = pong.await.unwrap();
    let elapsed_ms = start.elapsed().as_millis();

    let expected = rounds * (rounds + 1) / 2;
    if sum != expected || seen != rounds {
        println!("MISMATCH sum={sum} expected={expected} seen={seen}");
        std::process::exit(1);
    }
    println!("rounds={rounds} elapsed_ms={elapsed_ms}");
}
