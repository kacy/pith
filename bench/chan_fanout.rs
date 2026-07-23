// chan_fanout — Rust counterpart of bench/chan_fanout.pith.
//
// Four producer threads push messages into one bounded channel and four
// consumer threads drain it. The per-message work is two LCG rounds and
// the aggregate is a sum modulo a prime, so the checksum is
// order-independent and matches the Pith, Go, and Zig versions.
//
// std's mpsc channel is multi-producer but single-consumer, so the
// receiver is shared behind a Mutex. That is the std-only way to fan a
// channel out to several consumers, and its cost is part of what this
// measures.
//
// build: rustc -O -o bench/chan_fanout_rust bench/chan_fanout.rs
// run:   ./bench/chan_fanout_rust 1000000

use std::fs;
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

const PRODUCERS: i64 = 4;
const CONSUMERS: i64 = 4;
const CAPACITY: usize = 256;
const MOD: i64 = 1000000007;

fn messages_from_args() -> i64 {
    if let Some(arg) = std::env::args().nth(1) {
        if let Ok(n) = arg.parse::<i64>() {
            if n > 0 {
                return n;
            }
        }
    }
    1000000
}

// two rounds of a 31-bit LCG (POSIX constants), masked so it never
// overflows and every language reproduces it exactly.
fn mix(value: i64) -> i64 {
    let mut x = (value * 1103515245 + 12345) % 2147483648;
    x = (x * 1103515245 + 12345) % 2147483648;
    x
}

fn peak_rss_kb() -> i64 {
    let status = match fs::read_to_string("/proc/self/status") {
        Ok(s) => s,
        Err(_) => return 0,
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            let digits = rest.replace("kB", "");
            if let Ok(n) = digits.trim().parse::<i64>() {
                return n;
            }
        }
    }
    0
}

fn main() {
    let requested = messages_from_args();
    let per = requested / PRODUCERS;
    let messages = per * PRODUCERS;

    let (tx, rx) = sync_channel::<i64>(CAPACITY);
    let rx = Arc::new(Mutex::new(rx));

    let start = Instant::now();

    // consumers start first and block on recv until work shows up.
    let mut consumers = Vec::new();
    for _ in 0..CONSUMERS {
        let rx = Arc::clone(&rx);
        consumers.push(thread::spawn(move || {
            let mut sum: i64 = 0;
            let mut seen: i64 = 0;
            loop {
                // take the value under the lock, then release it before
                // doing the per-message work.
                let msg = rx.lock().unwrap().recv();
                match msg {
                    Ok(value) => {
                        sum = (sum + mix(value)) % MOD;
                        seen += 1;
                    }
                    Err(_) => break,
                }
            }
            (sum, seen)
        }));
    }

    // each producer owns a disjoint slice of the id space, and reports
    // how many messages it pushed.
    let mut producers = Vec::new();
    for id in 0..PRODUCERS {
        let tx = tx.clone();
        producers.push(thread::spawn(move || {
            for i in 0..per {
                tx.send(id * per + i).unwrap();
            }
            per
        }));
    }
    // the last sender has to go for the consumers' recv to report the
    // channel as finished.
    drop(tx);

    let mut sent: i64 = 0;
    for p in producers {
        sent += p.join().unwrap();
    }

    let mut checksum: i64 = 0;
    let mut received: i64 = 0;
    for c in consumers {
        let (sum, seen) = c.join().unwrap();
        checksum = (checksum + sum) % MOD;
        received += seen;
    }

    let elapsed = start.elapsed().as_millis() as i64;
    let rate = if elapsed > 0 {
        messages * 1000 / elapsed
    } else {
        0
    };

    println!("chan fanout benchmark");
    println!("messages={}", messages);
    println!("producers={}", PRODUCERS);
    println!("consumers={}", CONSUMERS);
    println!("sent={}", sent);
    println!("received={}", received);
    println!("elapsed_ms={}", elapsed);
    println!("rate_per_sec={}", rate);
    println!("peak_rss_kb={}", peak_rss_kb());
    println!("checksum={}", checksum);
}
