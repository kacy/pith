// task_pingpong_trials - Rust counterpart of bench/task_pingpong_trials.pith.
//
// runs many ping-pong trials inside one process, each with fresh channels
// and a fresh pair of tokio tasks, and prints "<index> <ms>" per trial. in
// pith each trial is a new placement decision for the pair; tokio makes its
// own, so the per-trial spread is what to compare. TOKIO_WORKER_THREADS
// plays the role of PITH_GREEN_WORKERS.
//
// build: cargo build --release --manifest-path bench/task_pingpong_rust/Cargo.toml
// run:   ./bench/task_pingpong_rust/target/release/task_pingpong_trials 200 20000

use std::time::Instant;
use task_pingpong_rust::{pinger, ponger, positive_arg};
use tokio::sync::mpsc::channel;

// one trial: fresh channels, fresh pair, timed round trip.
async fn trial(rounds: i64) -> u128 {
    let (req_tx, req_rx) = channel(1);
    let (rsp_tx, rsp_rx) = channel(1);
    let start = Instant::now();
    let pong = tokio::spawn(ponger(req_rx, rsp_tx));
    let ping = tokio::spawn(pinger(req_tx, rsp_rx, rounds));
    ping.await.unwrap();
    let seen = pong.await.unwrap();
    let elapsed = start.elapsed().as_millis();
    if seen != rounds {
        println!("MISMATCH seen={seen} rounds={rounds}");
    }
    elapsed
}

#[tokio::main]
async fn main() {
    let trials = positive_arg(1, 100);
    let rounds = positive_arg(2, 20000);
    for i in 0..trials {
        let ms = trial(rounds).await;
        println!("{i} {ms}");
    }
}
