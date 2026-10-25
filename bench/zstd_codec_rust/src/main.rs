// zstd_codec: rust counterpart of bench/zstd_codec.pith.
//
// the same corpus (README.md, docs/*.md, std/*.pith, generated json log
// lines, one run-heavy case), the same 400ms wall-clock budget per phase,
// and the same report lines. the two arms map onto the pith ones:
//
//   kernel: the `zstd` crate, bindings to the c reference library at level
//           3. this is the same crate and level the pith kernel calls, so it
//           is a c-binding arm, not native rust.
//   pure:   `ruzstd`, a pure rust codec. its decoder reads the kernel's level
//           3 frame, as the pith pure decoder does. its encoder implements
//           only CompressionLevel::Fastest (roughly zstd level 1; Default,
//           Better and Best are unimplemented in 0.9.1), so the encode size
//           column compares a level 1 class encoder against level 3.
//
// run it from the repository root, since the corpus paths are relative.
//
// build: cargo build --release --manifest-path bench/zstd_codec_rust/Cargo.toml
// run:   ./bench/zstd_codec_rust/target/release/zstd_codec_rust
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::Instant;

use ruzstd::decoding::StreamingDecoder;
use ruzstd::encoding::{compress_to_vec, CompressionLevel};

const BUDGET_MS: u128 = 400;
const KERNEL_LEVEL: i32 = 3;

// pre-order walk in directory-read order, the order pith's fs.walk uses.
fn walk(path: &Path, out: &mut Vec<(String, bool)>) {
    let is_dir = path.is_dir();
    out.push((path.to_string_lossy().into_owned(), is_dir));
    if is_dir {
        for entry in fs::read_dir(path).expect("read_dir").flatten() {
            walk(&entry.path(), out);
        }
    }
}

// concatenate every file under `dir` ending in `suffix`, up to `limit` bytes.
fn gather(dir: &str, suffix: &str, limit: usize) -> Vec<u8> {
    let mut entries = Vec::new();
    walk(Path::new(dir), &mut entries);
    let mut buf = Vec::new();
    for (path, is_dir) in entries {
        if buf.len() >= limit {
            break;
        }
        if is_dir || !path.ends_with(suffix) {
            continue;
        }
        buf.extend_from_slice(&fs::read(&path).expect("read"));
    }
    buf.truncate(limit);
    buf
}

// structured log lines: repetitive keys, high-cardinality trace ids.
fn synth_logs(count: i64) -> Vec<u8> {
    let levels = ["INFO", "WARN", "ERROR", "DEBUG"];
    let paths = ["/api/v1/users", "/api/v1/orders", "/health", "/metrics"];
    let mut buf = String::new();
    for i in 0..count {
        let trace = i * 2654435761 + 1013904223;
        buf.push_str(&format!(
            "{{\"ts\":\"2026-08-01T18:{}:{}Z\",\"level\":\"{}\",\"svc\":\"checkout\",\"trace\":\"{}\",\"path\":\"{}\",\"status\":{},\"ms\":{}}}\n",
            i % 60,
            (i * 7) % 60,
            levels[(i % 4) as usize],
            trace,
            paths[(i % 4) as usize],
            200 + (i % 5) * 76,
            i % 250
        ));
    }
    buf.into_bytes()
}

fn kernel_decompress(frame: &[u8]) -> Vec<u8> {
    zstd::stream::decode_all(frame).expect("kernel decode")
}

fn pure_decompress(frame: &[u8]) -> Vec<u8> {
    let mut source = frame;
    let mut decoder = StreamingDecoder::new(&mut source).expect("pure frame header");
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).expect("pure decode");
    out
}

fn kernel_compress(raw: &[u8]) -> Vec<u8> {
    zstd::bulk::compress(raw, KERNEL_LEVEL).expect("kernel encode")
}

fn pure_compress(raw: &[u8]) -> Vec<u8> {
    compress_to_vec(raw, CompressionLevel::Fastest)
}

// run `f` until the budget is spent; returns (reps, elapsed ms).
fn run_for<F: FnMut()>(mut f: F) -> (usize, usize) {
    let mut reps = 0;
    let t0 = Instant::now();
    while t0.elapsed().as_millis() < BUDGET_MS {
        f();
        reps += 1;
    }
    (reps, t0.elapsed().as_millis() as usize)
}

fn bench(label: &str, raw: &[u8]) {
    let frame = kernel_compress(raw);

    // both decoders must agree before either is timed
    let a = kernel_decompress(&frame);
    let b = pure_decompress(&frame);
    assert!(a == b && a.len() == raw.len(), "decoders disagree on {}", label);

    let (kernel_reps, kernel_ms) = run_for(|| assert!(!kernel_decompress(&frame).is_empty(), "empty decode"));
    let (pure_reps, pure_ms) = run_for(|| assert!(!pure_decompress(&frame).is_empty(), "empty decode"));

    // the pure frame must survive the c decoder before it is timed
    let pure_frame = pure_compress(raw);
    assert!(kernel_decompress(&pure_frame) == raw, "pure-encoded frame decodes wrong on {}", label);

    let (enc_kernel_reps, enc_kernel_ms) = run_for(|| assert!(!kernel_compress(raw).is_empty(), "empty encode"));
    let (enc_pure_reps, enc_pure_ms) = run_for(|| assert!(!pure_compress(raw).is_empty(), "empty encode"));

    // integer math throughout, as in the pith version
    let n = raw.len();
    let kb = n * kernel_reps / (kernel_ms + 1);
    let pb = n * pure_reps / (pure_ms + 1);
    let ratio = kb * 100 / (pb + 1);
    let ekb = n * enc_kernel_reps / (enc_kernel_ms + 1);
    let epb = n * enc_pure_reps / (enc_pure_ms + 1);
    let eratio = ekb * 100 / (epb + 1);
    let size_ratio = pure_frame.len() * 100 / (frame.len() + 1);
    let packed = n * 10 / frame.len();
    println!("{}: {}KB at {}.{}:1", label, n / 1024, packed / 10, packed % 10);
    println!(
        "  decode: kernel {} MB/s ({} reps)   pure {} MB/s ({} reps)   ratio {}.{}x",
        kb * 1000 / 1048576, kernel_reps, pb * 1000 / 1048576, pure_reps, ratio / 100, ratio % 100
    );
    println!(
        "  encode: kernel {} MB/s ({} reps)   pure {} MB/s ({} reps)   ratio {}.{}x   size {}% of kernel's",
        ekb * 1000 / 1048576, enc_kernel_reps, epb * 1000 / 1048576, enc_pure_reps, eratio / 100, eratio % 100, size_ratio
    );
}

fn main() {
    bench("text  ", &fs::read("README.md").expect("README.md"));
    bench("prose ", &gather("docs", ".md", 300000));
    bench("source", &gather("std", ".pith", 600000));
    bench("json  ", &synth_logs(4000));

    // not a realistic workload: a single long run decodes through the
    // match copy and says nothing about entropy decoding.
    let runs: Vec<u8> = (0..80000).map(|i| (97 + (i / 4096) % 26) as u8).collect();
    bench("runs  ", &runs);
}
