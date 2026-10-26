// string_build: rust counterpart of bench/string_build.pith.
//
// grows one string by many short appends through String::push_str.
//
// build: rustc -O -o bench/string_build_rust bench/string_build.rs
// run:   ./bench/string_build_rust 2000000
use std::fmt::Write;

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .filter(|&v| v > 0)
        .unwrap_or(1000);
    let mut s = String::new();
    for i in 0..n {
        s.push_str("item-");
        let _ = write!(s, "{}", i);
        s.push(',');
    }
    let bytes = s.as_bytes();
    let mut sum: u64 = 0;
    let mut j = 0;
    while j < bytes.len() {
        sum += bytes[j] as u64;
        j += 97;
    }
    println!("len: {} checksum: {}", s.len(), sum);
}
