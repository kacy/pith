// metrics_contention - Rust counterpart of bench/metrics_contention.pith.
//
// every trial does the same total number of writes, split evenly across
// `tasks` tokio tasks, all writing the same series, and prints
// "<trial> <ms> <ops>". rust's standard library has no metrics registry, so
// this carries a small one shaped like std.metrics: a name-to-series map
// behind an RwLock, float cells updated by compare-and-swap, and histograms
// with fixed per-boundary buckets plus sum, min and max.
//
// modes:
//   counter    inc() on a counter resolved once per task
//   gauge      set() on a gauge resolved once per task
//   histogram  observe() on a histogram resolved once per task
//   labels     the series looked up by name and label set on every write,
//              the shape of the prometheus crate's with_label_values(..).inc().
//              pith also caches resolved series per task; this does not.
//
// TOKIO_WORKER_THREADS plays the role of PITH_GREEN_WORKERS.
//
// build: cargo build --release --manifest-path bench/metrics_contention_rust/Cargo.toml
// run:   ./bench/metrics_contention_rust/target/release/metrics_contention_rust 5 4000000 32 labels

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{LazyLock, RwLock};
use std::time::Instant;

const COUNTER_NAME: &str = "bench_hits_total";
const GAUGE_NAME: &str = "bench_level";
const HISTOGRAM_NAME: &str = "bench_latency_ms";
const LABELED_NAME: &str = "bench_labeled_total";

// an f64 held as its bit pattern so it can be updated atomically.
struct FloatCell(AtomicU64);

impl FloatCell {
    fn new(v: f64) -> FloatCell {
        FloatCell(AtomicU64::new(v.to_bits()))
    }

    // a sequentially consistent store, like go's atomic Store. with a
    // weaker ordering llvm merges the stores of gauge_worker's loop into
    // one, and the mode would no longer measure writers sharing a line.
    fn set(&self, v: f64) {
        self.0.store(v.to_bits(), Ordering::SeqCst);
    }

    fn add(&self, delta: f64) {
        self.update(|cur| Some(cur + delta));
    }

    // replace the value with f(current) until a compare-and-swap lands, or
    // stop when f returns None.
    fn update(&self, f: impl Fn(f64) -> Option<f64>) {
        let mut old = self.0.load(Ordering::Relaxed);
        loop {
            let Some(new) = f(f64::from_bits(old)) else {
                return;
            };
            match self.0.compare_exchange_weak(
                old,
                new.to_bits(),
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(current) => old = current,
            }
        }
    }
}

const DEFAULT_BUCKETS: [f64; 10] = [
    1.0, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 5000.0,
];

struct Histogram {
    sum: FloatCell,
    lo: FloatCell,
    hi: FloatCell,
    bounds: Vec<f64>,
    buckets: Vec<AtomicI64>,
    overflow: AtomicI64,
}

impl Histogram {
    fn new() -> Histogram {
        Histogram {
            sum: FloatCell::new(0.0),
            lo: FloatCell::new(f64::INFINITY),
            hi: FloatCell::new(f64::NEG_INFINITY),
            bounds: DEFAULT_BUCKETS.to_vec(),
            buckets: DEFAULT_BUCKETS.iter().map(|_| AtomicI64::new(0)).collect(),
            overflow: AtomicI64::new(0),
        }
    }

    // sum, min and max first, then the one bucket the value lands in.
    fn observe(&self, v: f64) {
        self.sum.add(v);
        self.lo.update(|cur| (v < cur).then_some(v));
        self.hi.update(|cur| (v > cur).then_some(v));
        match self.bounds.iter().position(|&b| v <= b) {
            Some(i) => self.buckets[i].fetch_add(1, Ordering::AcqRel),
            None => self.overflow.fetch_add(1, Ordering::AcqRel),
        };
    }
}

// a find-or-create map from series key to instrument. series live for the
// whole process, so it hands out leaked &'static references.
struct Registry<T: 'static> {
    series: RwLock<HashMap<String, &'static T>>,
    make: fn() -> T,
}

impl<T: Sync> Registry<T> {
    fn new(make: fn() -> T) -> Registry<T> {
        Registry {
            series: RwLock::new(HashMap::new()),
            make,
        }
    }

    fn get(&self, key: &str) -> &'static T {
        if let Some(v) = self.series.read().unwrap().get(key) {
            return v;
        }
        self.series
            .write()
            .unwrap()
            .entry(key.to_string())
            .or_insert_with(|| Box::leak(Box::new((self.make)())))
    }
}

static COUNTERS: LazyLock<Registry<FloatCell>> =
    LazyLock::new(|| Registry::new(|| FloatCell::new(0.0)));
static GAUGES: LazyLock<Registry<FloatCell>> =
    LazyLock::new(|| Registry::new(|| FloatCell::new(0.0)));
static HISTOGRAMS: LazyLock<Registry<Histogram>> = LazyLock::new(|| Registry::new(Histogram::new));

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

fn count_worker(n: i64) -> i64 {
    let c = COUNTERS.get(COUNTER_NAME);
    for _ in 0..n {
        c.add(1.0);
    }
    n
}

fn gauge_worker(n: i64) -> i64 {
    let g = GAUGES.get(GAUGE_NAME);
    for i in 0..n {
        g.set(i as f64);
    }
    n
}

fn histogram_worker(n: i64) -> i64 {
    let h = HISTOGRAMS.get(HISTOGRAM_NAME);
    for _ in 0..n {
        h.observe(7.0);
    }
    n
}

fn labeled_worker(n: i64) -> i64 {
    for _ in 0..n {
        COUNTERS
            .get(&series_key(
                LABELED_NAME,
                &["route", "/api", "method", "GET"],
            ))
            .add(1.0);
    }
    n
}

fn worker_for(mode: &str) -> fn(i64) -> i64 {
    match mode {
        "gauge" => gauge_worker,
        "histogram" => histogram_worker,
        "labels" => labeled_worker,
        _ => count_worker,
    }
}

// one trial: `tasks` writers share `ops` writes to one series, timed end to end.
async fn trial(mode: &str, ops: i64, tasks: i64) -> u128 {
    let per = ops / tasks;
    let run = worker_for(mode);
    let start = Instant::now();
    let running: Vec<_> = (0..tasks)
        .map(|_| tokio::spawn(async move { run(per) }))
        .collect();
    let mut done = 0;
    for task in running {
        done += task.await.unwrap();
    }
    let elapsed = start.elapsed().as_millis();
    if done != per * tasks {
        println!("MISMATCH done={done} expected={}", per * tasks);
    }
    elapsed
}

fn positive_arg(index: usize, fallback: i64) -> i64 {
    std::env::args()
        .nth(index)
        .and_then(|a| a.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(fallback)
}

#[tokio::main]
async fn main() {
    let trials = positive_arg(1, 9);
    let ops = positive_arg(2, 400000);
    let tasks = positive_arg(3, 1);
    let mode = std::env::args()
        .nth(4)
        .unwrap_or_else(|| "counter".to_string());
    let per = ops / tasks;
    for i in 0..trials {
        let ms = trial(&mode, ops, tasks).await;
        println!("{i} {ms} {}", per * tasks);
    }
}
