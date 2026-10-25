// lock_sweep - Rust counterpart of bench/lock_sweep.pith.
//
// every run does the same total number of operations, split evenly across
// `tasks` tokio tasks, and prints the elapsed milliseconds in the pith
// format. each mode maps a pith std facility to the closest rust idiom:
//
//   log        tracing::debug! with a fmt subscriber capped at info, so every
//              call is filtered out by the global level check. close: both
//              check a level held in an atomic and return.
//   spawn      each op spawns a tokio task and awaits it. close.
//   labels     a counter series looked up by name and label set on every
//              write, in an RwLock-guarded HashMap keyed by the rendered
//              series key, then a compare-and-swap float add. this is the
//              shape of the prometheus crate's with_label_values(..).inc()
//              written with std. pith also caches resolved series per task,
//              which this does not.
//   breaker    a small circuit breaker, one per task, with its own mutex,
//              mirroring std.resilience's allow() and success(). rust has no
//              standard breaker, so this is a hand port.
//   readfile   tokio::fs::read of bench/lock_sweep.pith, which runs on
//              tokio's blocking pool. close: pith runs it on its file pool.
//   trace      skipped. pith's span start and end allocate ids, stamp times
//              and push into a sharded span buffer; the rust equivalent is
//              the opentelemetry sdk, which does a different amount of work,
//              and a hand-written stand-in would not mean much.
//
// TOKIO_WORKER_THREADS plays the role of PITH_GREEN_WORKERS. run from the
// repo root so the readfile path resolves.
//
// build: cargo build --release --manifest-path bench/lock_sweep_rust/Cargo.toml
// run:   ./bench/lock_sweep_rust/target/release/lock_sweep_rust <log|spawn|labels|breaker|readfile> <tasks> <ops>

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant};

fn trivial(n: i64) -> i64 {
    n + 1
}

fn log_worker(n: i64) -> i64 {
    for _ in 0..n {
        tracing::debug!("filtered out");
    }
    n
}

async fn spawn_worker(n: i64) -> i64 {
    let mut sum = 0;
    for i in 0..n {
        sum += tokio::spawn(async move { trivial(i) }).await.unwrap();
    }
    let _ = sum;
    n
}

// --- labels: a series registry keyed by the rendered label set ---

struct CounterCell(AtomicU64);

impl CounterCell {
    fn add(&self, delta: f64) {
        let mut old = self.0.load(Ordering::Relaxed);
        loop {
            let new = (f64::from_bits(old) + delta).to_bits();
            match self
                .0
                .compare_exchange_weak(old, new, Ordering::AcqRel, Ordering::Relaxed)
            {
                Ok(_) => return,
                Err(current) => old = current,
            }
        }
    }
}

// series live for the whole process, so the registry hands out leaked
// &'static references rather than reference-counted ones.
static SERIES: LazyLock<RwLock<HashMap<String, &'static CounterCell>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

// name{k="v",...} with keys sorted, so a label set has one key whatever
// order the pairs come in.
fn series_key(name: &str, pairs: &[&str]) -> String {
    let mut kv: HashMap<&str, &str> = HashMap::new();
    let mut keys: Vec<&str> = Vec::new();
    for pair in pairs.chunks_exact(2) {
        if kv.insert(pair[0], pair[1]).is_none() {
            keys.push(pair[0]);
        }
    }
    keys.sort_unstable();
    let mut out = String::from(name);
    out.push('{');
    for (i, k) in keys.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(k);
        out.push_str("=\"");
        out.push_str(kv[k]);
        out.push('"');
    }
    out.push('}');
    out
}

fn counter_with_labels(name: &str, pairs: &[&str]) -> &'static CounterCell {
    let key = series_key(name, pairs);
    if let Some(c) = SERIES.read().unwrap().get(&key) {
        return c;
    }
    SERIES
        .write()
        .unwrap()
        .entry(key)
        .or_insert_with(|| Box::leak(Box::new(CounterCell(AtomicU64::new(0)))))
}

fn labels_worker(n: i64) -> i64 {
    for _ in 0..n {
        counter_with_labels("lock_sweep_requests", &["route", "/api", "method", "GET"]).add(1.0);
    }
    n
}

// --- breaker: closed, open after `threshold` failures, one probe after reset ---

struct BreakerState {
    failures: i64,
    open: bool,
    opened_at: Instant,
    probing: bool,
}

struct Breaker {
    threshold: i64,
    reset: Duration,
    state: Mutex<BreakerState>,
}

impl Breaker {
    fn new(failures: i64, reset_ms: u64) -> Breaker {
        Breaker {
            threshold: failures,
            reset: Duration::from_millis(reset_ms),
            state: Mutex::new(BreakerState {
                failures: 0,
                open: false,
                opened_at: Instant::now(),
                probing: false,
            }),
        }
    }

    fn allow(&self) -> bool {
        let mut s = self.state.lock().unwrap();
        if !s.open {
            return true;
        }
        if s.opened_at.elapsed() >= self.reset && !s.probing {
            s.probing = true;
            return true;
        }
        false
    }

    fn success(&self) {
        let mut s = self.state.lock().unwrap();
        s.failures = 0;
        s.open = false;
        s.probing = false;
    }

    #[allow(dead_code)]
    fn failure(&self) {
        let mut s = self.state.lock().unwrap();
        if s.open {
            s.opened_at = Instant::now();
            s.probing = false;
            return;
        }
        s.failures += 1;
        if s.failures >= self.threshold {
            s.open = true;
            s.opened_at = Instant::now();
            s.probing = false;
        }
    }
}

fn breaker_worker(n: i64) -> i64 {
    let breaker = Breaker::new(5, 1000);
    for _ in 0..n {
        if breaker.allow() {
            breaker.success();
        }
    }
    n
}

async fn readfile_worker(n: i64) -> i64 {
    for _ in 0..n {
        let _ = tokio::fs::read("bench/lock_sweep.pith").await;
    }
    n
}

#[derive(Clone, Copy)]
enum Mode {
    Log,
    Spawn,
    Labels,
    Breaker,
    Readfile,
}

async fn run_worker(mode: Mode, n: i64) -> i64 {
    match mode {
        Mode::Log => log_worker(n),
        Mode::Spawn => spawn_worker(n).await,
        Mode::Labels => labels_worker(n),
        Mode::Breaker => breaker_worker(n),
        Mode::Readfile => readfile_worker(n).await,
    }
}

#[tokio::main]
async fn main() {
    let argv: Vec<String> = std::env::args().collect();
    if argv.len() < 4 {
        println!("usage: lock_sweep <log|spawn|labels|breaker|readfile> <tasks> <ops>");
        std::process::exit(1);
    }
    let mode_name = argv[1].as_str();
    let mode = match mode_name {
        "log" => Mode::Log,
        "spawn" => Mode::Spawn,
        "labels" => Mode::Labels,
        "breaker" => Mode::Breaker,
        "readfile" => Mode::Readfile,
        _ => {
            println!("mode {mode_name} has no rust counterpart");
            std::process::exit(1);
        }
    };
    let tasks: i64 = argv[2].parse().expect("tasks must be an integer");
    let ops: i64 = argv[3].parse().expect("ops must be an integer");
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_writer(std::io::stderr)
        .init();
    let per = ops / tasks;
    let start = Instant::now();
    let running: Vec<_> = (0..tasks)
        .map(|_| tokio::spawn(run_worker(mode, per)))
        .collect();
    let mut done = 0;
    for task in running {
        done += task.await.unwrap();
    }
    println!(
        "mode={mode_name} tasks={tasks} ops={done} elapsed_ms={}",
        start.elapsed().as_millis()
    );
}
