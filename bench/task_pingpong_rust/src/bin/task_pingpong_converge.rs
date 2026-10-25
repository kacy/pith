// task_pingpong_converge - Rust counterpart of bench/task_pingpong_converge.pith.
//
// one ping-pong pair of tokio tasks runs `windows` batches of `per` round
// trips over the same two capacity-1 channels and prints the time of each
// window, so the handoff cost is sampled across the lifetime of one pair.
// TOKIO_WORKER_THREADS plays the role of PITH_GREEN_WORKERS.
//
// build: cargo build --release --manifest-path bench/task_pingpong_rust/Cargo.toml
// run:   ./bench/task_pingpong_rust/target/release/task_pingpong_converge 10 100000

use std::time::Instant;
use task_pingpong_rust::{ponger, positive_arg};
use tokio::sync::mpsc::{channel, Receiver, Sender};

async fn pinger(req: Sender<i64>, mut rsp: Receiver<i64>, windows: i64, per: i64) -> i64 {
    for w in 0..windows {
        let start = Instant::now();
        let mut i = 0;
        while i < per {
            req.send(i).await.unwrap();
            if rsp.recv().await.is_none() {
                break;
            }
            i += 1;
        }
        println!("window {w} {}ms", start.elapsed().as_millis());
    }
    req.send(-1).await.unwrap();
    windows
}

#[tokio::main]
async fn main() {
    let windows = positive_arg(1, 10);
    let per = positive_arg(2, 20000);
    let (req_tx, req_rx) = channel(1);
    let (rsp_tx, rsp_rx) = channel(1);
    let pong = tokio::spawn(ponger(req_rx, rsp_tx));
    let ping = tokio::spawn(pinger(req_tx, rsp_rx, windows, per));
    let done = ping.await.unwrap();
    let seen = pong.await.unwrap();
    println!("done windows={done} seen={seen}");
}
