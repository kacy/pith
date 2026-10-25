// cpu_parallel_sync - Rust counterpart of bench/cpu_parallel_sync.pith.
//
// eight tokio tasks each do a fixed amount of arithmetic in chunks and,
// between chunks, send their partial to a collector task and wait for an
// acknowledgement on a shared ack channel. both channels hold 8, as in the
// pith program. any worker may take any ack, so the ack channel needs
// several receivers; tokio's channels are single-consumer, so this uses
// async-channel, the usual multi-consumer channel for async rust. the
// checksum is the sum of the workers' partials and must equal the
// collector's total. TOKIO_WORKER_THREADS plays the role of
// PITH_GREEN_WORKERS.
//
// build: cargo build --release --manifest-path bench/cpu_parallel_sync_rust/Cargo.toml
// run:   ./bench/cpu_parallel_sync_rust/target/release/cpu_parallel_sync_rust 200 1000000

use async_channel::{bounded, Receiver, Sender};
use std::time::Instant;

const MOD: i64 = 1000000007;

async fn collector(req: Receiver<i64>, ack: Sender<i64>, expected: i64) -> i64 {
    let mut total = 0;
    let mut seen = 0;
    while seen < expected {
        match req.recv().await {
            Ok(v) => {
                total = (total + v) % MOD;
                seen += 1;
                ack.send(1).await.unwrap();
            }
            Err(_) => break,
        }
    }
    total
}

fn mix(value: i64) -> i64 {
    let mut x = (value * 1103515245 + 12345) % 2147483648;
    x = (x * 1103515245 + 12345) % 2147483648;
    x
}

async fn worker(
    id: i64,
    req: Sender<i64>,
    ack: Receiver<i64>,
    chunks: i64,
    chunk_size: i64,
) -> i64 {
    let mut partial = 0;
    for c in 0..chunks {
        let mut x = id * 7919 + c;
        for _ in 0..chunk_size {
            x = mix(x);
        }
        partial = (partial + x) % MOD;
        req.send(x).await.unwrap();
        let _ = ack.recv().await;
    }
    partial
}

fn arg(i: usize, fallback: i64) -> i64 {
    std::env::args()
        .nth(i)
        .and_then(|a| a.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(fallback)
}

#[tokio::main]
async fn main() {
    let chunks = arg(1, 200);
    let chunk_size = arg(2, 20000);
    let (req_tx, req_rx) = bounded(8);
    let (ack_tx, ack_rx) = bounded(8);
    let start = Instant::now();
    let coll = tokio::spawn(collector(req_rx, ack_tx, chunks * 8));
    let tasks: Vec<_> = (0..8)
        .map(|id| {
            tokio::spawn(worker(
                id,
                req_tx.clone(),
                ack_rx.clone(),
                chunks,
                chunk_size,
            ))
        })
        .collect();
    let mut checksum = 0;
    for t in tasks {
        checksum = (checksum + t.await.unwrap()) % MOD;
    }
    let total = coll.await.unwrap();
    let elapsed = start.elapsed().as_millis();
    println!("cpu_parallel_sync chunks={chunks} chunk_size={chunk_size} elapsed_ms={elapsed} checksum={checksum} total={total}");
}
