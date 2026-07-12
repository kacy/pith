// event_ledger — Rust counterpart of bench/event_ledger.pith.
//
// Rust's std has neither JSON nor crypto, so this pulls the serde_json,
// sha2, and hmac crates — the honest cost of a minimal standard library.
// The deterministic event stream, aggregation, and HMAC-signed summary
// match the Pith, Go, and Zig versions byte for byte.
//
// build: cargo build --release --manifest-path bench/event_ledger_rust/Cargo.toml
// run:   ./bench/event_ledger_rust/target/release/event_ledger_rust 200000

use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::time::Instant;

type HmacSha256 = Hmac<Sha256>;

fn lcg_next(state: i64) -> i64 {
    (state * 1103515245 + 12345) % 2147483648
}

fn action_name(k: i64) -> &'static str {
    match k {
        0 => "view",
        1 => "click",
        2 => "buy",
        _ => "refund",
    }
}

fn region_name(k: i64) -> &'static str {
    match k {
        0 => "north",
        1 => "south",
        2 => "east",
        _ => "west",
    }
}

fn generate_events(count: usize) -> String {
    let mut out = String::new();
    let mut state: i64 = 20260711;
    for i in 0..count {
        state = lcg_next(state);
        let user = (state / 256) % 1000;
        state = lcg_next(state);
        let action = action_name((state / 256) % 4);
        state = lcg_next(state);
        let amount = (state / 256) % 500;
        state = lcg_next(state);
        let region = region_name((state / 256) % 4);
        if i > 0 {
            out.push('\n');
        }
        let _ = write!(
            out,
            "{{\"id\":{},\"user\":{},\"action\":\"{}\",\"amount\":{},\"region\":\"{}\"}}",
            i, user, action, amount, region
        );
    }
    out
}

#[derive(Deserialize)]
struct Event {
    user: i64,
    action: String,
    amount: i64,
    region: String,
}

// parse the stream into an in-memory vec of events with serde_json.
fn parse_events(stream: &str) -> Vec<Event> {
    let mut events = Vec::new();
    for line in stream.split('\n') {
        if line.is_empty() {
            continue;
        }
        if let Ok(e) = serde_json::from_str::<Event>(line) {
            events.push(e);
        }
    }
    events
}

struct Analysis {
    region_amount: HashMap<String, i64>,
    action_count: HashMap<String, i64>,
    unique_users: HashSet<i64>,
    high_value: i64,
    top_user: i64,
    top_user_total: i64,
    total_amount: i64,
    record_count: i64,
}

// the analyze phase: several maps, a set, and a per-user rollup with a
// top-spender scan tracked inline as the totals grow.
fn analyze(events: &[Event]) -> Analysis {
    let mut a = Analysis {
        region_amount: HashMap::new(),
        action_count: HashMap::new(),
        unique_users: HashSet::new(),
        high_value: 0,
        top_user: -1,
        top_user_total: -1,
        total_amount: 0,
        record_count: events.len() as i64,
    };
    let mut user_total: HashMap<i64, i64> = HashMap::new();
    for e in events {
        *a.region_amount.entry(e.region.clone()).or_insert(0) += e.amount;
        *a.action_count.entry(e.action.clone()).or_insert(0) += 1;
        let running = {
            let t = user_total.entry(e.user).or_insert(0);
            *t += e.amount;
            *t
        };
        a.unique_users.insert(e.user);
        if e.amount >= 400 {
            a.high_value += 1;
        }
        a.total_amount += e.amount;
        if running > a.top_user_total || (running == a.top_user_total && e.user < a.top_user) {
            a.top_user = e.user;
            a.top_user_total = running;
        }
    }
    a
}

fn build_summary(a: &Analysis) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut regions: Vec<&String> = a.region_amount.keys().collect();
    regions.sort();
    for r in regions {
        parts.push(format!("region:{}={}", r, a.region_amount[r]));
    }
    let mut actions: Vec<&String> = a.action_count.keys().collect();
    actions.sort();
    for act in actions {
        parts.push(format!("action:{}={}", act, a.action_count[act]));
    }
    parts.push(format!("users:{}", a.unique_users.len()));
    parts.push(format!("hivalue:{}", a.high_value));
    parts.push(format!("topuser:{}={}", a.top_user, a.top_user_total));
    parts.push(format!("total:{}", a.total_amount));
    parts.push(format!("records:{}", a.record_count));
    parts.join(";")
}

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{:02x}", b);
    }
    s
}

fn digest_score(digest: &str) -> i64 {
    digest.bytes().map(|c| c as i64).sum()
}

fn now_millis(start: Instant) -> i64 {
    start.elapsed().as_millis() as i64
}

fn main() {
    let events: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .filter(|&v: &usize| v > 0)
        .unwrap_or(200000);

    let total_start = Instant::now();

    let start = Instant::now();
    let stream = generate_events(events);
    let gen_ms = now_millis(start);

    let start = Instant::now();
    let parsed = parse_events(&stream);
    let parse_ms = now_millis(start);

    let start = Instant::now();
    let a = analyze(&parsed);
    let analyze_ms = now_millis(start);

    let start = Instant::now();
    let summary = build_summary(&a);
    let mut mac = HmacSha256::new_from_slice(b"pith-bench-key").unwrap();
    mac.update(summary.as_bytes());
    let digest = to_hex(&mac.finalize().into_bytes());
    let sign_ms = now_millis(start);

    let total_ms = now_millis(total_start);

    let checksum = a.total_amount + a.record_count + a.unique_users.len() as i64 * 31
        + a.high_value + a.top_user_total + digest_score(&digest);

    println!("event ledger benchmark");
    println!("events={}", events);
    println!("gen_ms={}", gen_ms);
    println!("parse_ms={}", parse_ms);
    println!("analyze_ms={}", analyze_ms);
    println!("sign_ms={}", sign_ms);
    println!("total_ms={}", total_ms);
    println!("unique_users={}", a.unique_users.len());
    println!("digest={}", digest);
    println!("checksum={}", checksum);
}
