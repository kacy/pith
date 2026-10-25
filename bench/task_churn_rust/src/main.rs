// task_churn - Rust counterpart of bench/task_churn.pith.
//
// one short task after another, each allocating a batch of structs into a
// vec and dropping them at return; the caller awaits each task before
// starting the next. every row is boxed so it is its own heap allocation,
// the work the pith program puts on its struct pool.
//
// by default each task is a tokio task on the multi-thread runtime, the
// analogue of pith's green backend; TOKIO_WORKER_THREADS plays the role of
// PITH_GREEN_WORKERS. with BENCH_OS_THREADS=1 each task is a fresh
// std::thread instead, the analogue of PITH_GREEN=0, which is the arm the
// pith benchmark was written for.
//
// build: cargo build --release --manifest-path bench/task_churn_rust/Cargo.toml
// run:   ./bench/task_churn_rust/target/release/task_churn_rust 200000
//        BENCH_OS_THREADS=1 ./bench/task_churn_rust/target/release/task_churn_rust 200000

use std::time::Instant;

const PER_TASK: i64 = 400;

struct Blob {
    #[allow(dead_code)]
    a: i64,
    b: i64,
}

fn worker(seed: i64, per_task: i64) -> i64 {
    let mut rows: Vec<Box<Blob>> = Vec::new();
    for i in 0..per_task {
        rows.push(Box::new(Blob { a: seed, b: i }));
    }
    rows.iter().map(|r| r.b).sum()
}

#[tokio::main]
async fn main() {
    let tasks: i64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(2000);
    let os_threads = std::env::var("BENCH_OS_THREADS").is_ok_and(|v| v == "1");
    let start = Instant::now();
    let mut acc = 0i64;
    for i in 0..tasks {
        acc += if os_threads {
            std::thread::spawn(move || worker(i, PER_TASK))
                .join()
                .unwrap()
        } else {
            tokio::spawn(async move { worker(i, PER_TASK) })
                .await
                .unwrap()
        };
    }
    let elapsed = start.elapsed().as_millis();
    println!("task churn benchmark");
    println!("tasks={tasks}");
    println!("structs_per_task={PER_TASK}");
    println!("elapsed_ms={elapsed}");
    println!("checksum={acc}");
}
