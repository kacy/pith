// path_clean_count: rust counterpart of bench/path_clean_count.pith.
//
// std::path normalizes repeated separators and "." but never resolves
// "..", so there is no std equivalent of a lexical clean. this is the same
// counting algorithm std.os.path.clean_part_count uses: one pass over the
// bytes with a depth counter for real segments and a climb counter for the
// ".." a relative path keeps, with nothing allocated.
//
// build: rustc -O -o bench/path_clean_count_rust bench/path_clean_count.rs
// run:   ./bench/path_clean_count_rust <rounds>

use std::env;
use std::time::Instant;

fn clean_part_count(path: &str) -> usize {
    let bytes = path.as_bytes();
    let n = bytes.len();
    if n == 0 {
        return 0;
    }
    let absolute = bytes[0] == b'/';
    let mut depth = 0;
    let mut climbs = 0;
    let mut start = 0;
    for i in 0..=n {
        if i == n || bytes[i] == b'/' {
            let seg = &bytes[start..i];
            if seg == b".." {
                if depth > 0 {
                    depth -= 1;
                } else if !absolute {
                    climbs += 1;
                }
            } else if !seg.is_empty() && seg != b"." {
                depth += 1;
            }
            start = i + 1;
        }
    }
    depth + climbs
}

fn main() {
    let rounds = env::args()
        .nth(1)
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(20000);
    let paths: Vec<String> = [
        "data//north/./users/../users/123.json",
        "/var/log/../lib/./pith/build/",
        "../../escape/../still/../../up",
        "/../root/./sees/no/parent",
        "plain/relative/path",
        "a/b/c/d/e/f/g/h/../../../../x",
        "",
        "/",
        ".",
        "./only/dots/./././end/.",
        "trailing/slashes///",
        "/abs//double/../../../gone",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let start = Instant::now();
    let mut checksum = 0;
    for _ in 0..rounds {
        for (i, p) in paths.iter().enumerate() {
            checksum += clean_part_count(p) * (i + 1);
        }
    }
    let elapsed = start.elapsed().as_millis();
    println!("path clean_part_count benchmark");
    println!("rounds={rounds}");
    println!("elapsed_ms={elapsed}");
    println!("checksum={checksum}");
}
