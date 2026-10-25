// map_update: rust counterpart of bench/map_update.pith.
//
// the event ledger's map update in isolation: bump the count of a key the
// map already holds, or insert it with 1, over a small pool of keys
// repeated many times. HashMap with the default SipHash hasher, keyed by
// String, Vec<u8> or i64 for the three flavors. the update is written
// with get_mut so a hit borrows the key and allocates nothing, the same
// fused read-add-write the pith map does; only a miss clones the key.
//
// build: rustc -O -o bench/map_update_rust bench/map_update.rs
// run:   ./bench/map_update_rust <flavor: string|int|bytes> <updates> <distinct keys>

use std::collections::HashMap;
use std::env;
use std::hash::Hash;

fn arg_text(i: usize, fallback: &str) -> String {
    env::args().nth(i).unwrap_or_else(|| fallback.to_string())
}

fn arg_int(i: usize, fallback: usize) -> usize {
    arg_text(i, "")
        .parse::<usize>()
        .ok()
        .filter(|&v| v > 0)
        .unwrap_or(fallback)
}

fn key_text(i: usize) -> String {
    format!("user-{i}-region")
}

fn count<K: Hash + Eq + Clone>(keys: &[K], updates: usize) -> i64 {
    let mut m: HashMap<K, i64> = HashMap::new();
    for i in 0..updates {
        let k = &keys[i % keys.len()];
        if let Some(v) = m.get_mut(k) {
            *v += 1;
        } else {
            m.insert(k.clone(), 1);
        }
    }
    m.values().sum()
}

fn main() {
    let flavor = arg_text(1, "string");
    let updates = arg_int(2, 200000);
    let distinct = arg_int(3, 64);
    let total = match flavor.as_str() {
        "int" => count(&(0..distinct as i64).collect::<Vec<_>>(), updates),
        "bytes" => count(&(0..distinct).map(|i| key_text(i).into_bytes()).collect::<Vec<_>>(), updates),
        _ => count(&(0..distinct).map(key_text).collect::<Vec<_>>(), updates),
    };
    println!("map_update flavor={flavor} updates={updates} distinct={distinct}");
    println!("total={total}");
}
