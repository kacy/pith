// csv_encode: rust counterpart of bench/csv_encode.pith.
//
// rust's standard library has no csv module, so the encoder is written
// here with std, following std.csv's rules: a field is quoted when it holds
// a comma, a double quote or a newline, quotes inside it are doubled, and
// rows are joined by "\n" with none after the last. each round encodes into
// a fresh Vec<u8>, as std.csv does into a fresh buffer. the checksum is the
// encoded byte count folded with every 97th byte.
//
// build: rustc -O -o bench/csv_encode_rust bench/csv_encode.rs
// run:   ./bench/csv_encode_rust <rows> <rounds>

use std::env;
use std::time::Instant;

fn arg_or(index: usize, fallback: usize) -> usize {
    env::args()
        .nth(index)
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(fallback)
}

fn region_for(i: usize) -> &'static str {
    match i % 4 {
        0 => "north",
        1 => "south",
        2 => "east",
        _ => "west",
    }
}

fn make_rows(records: usize) -> Vec<Vec<String>> {
    let header = ["id", "name", "region", "active", "score", "quota", "url", "joined", "note", "path"];
    let mut rows = vec![header.iter().map(|s| s.to_string()).collect::<Vec<_>>()];
    for i in 0..records {
        let region = region_for(i);
        let score = (i * 17 + 11) % 100;
        let quota = (i * 13 + 7) % 50;
        rows.push(vec![
            i.to_string(),
            format!("user-{i}"),
            region.to_string(),
            (i % 3 != 0).to_string(),
            score.to_string(),
            quota.to_string(),
            format!("https://example.com/api/{region}/users/{i}?score={score}"),
            format!("2026-{}-{}", i % 12 + 1, i % 28 + 1),
            format!("user {i}, region {region}"),
            format!("data//{region}/./users/../users/{i}.json"),
        ]);
    }
    rows
}

fn needs_quoting(field: &str) -> bool {
    field.bytes().any(|b| b == b',' || b == b'"' || b == b'\n')
}

fn encode(rows: &[Vec<String>]) -> Vec<u8> {
    let mut out = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        if r > 0 {
            out.push(b'\n');
        }
        for (i, field) in row.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            if needs_quoting(field) {
                out.push(b'"');
                for &b in field.as_bytes() {
                    if b == b'"' {
                        out.push(b'"');
                    }
                    out.push(b);
                }
                out.push(b'"');
            } else {
                out.extend_from_slice(field.as_bytes());
            }
        }
    }
    out
}

fn main() {
    let records = arg_or(1, 5000);
    let rounds = arg_or(2, 10);
    let rows = make_rows(records);
    let start = Instant::now();
    let mut checksum: u64 = 0;
    for round in 0..rounds {
        let encoded = encode(&rows);
        checksum += encoded.len() as u64;
        let mut i = round % 7;
        while i < encoded.len() {
            checksum += encoded[i] as u64;
            i += 97;
        }
    }
    let elapsed = start.elapsed().as_millis();
    println!("csv encode benchmark");
    println!("rows={records}");
    println!("rounds={rounds}");
    println!("elapsed_ms={elapsed}");
    println!("checksum={checksum}");
}
