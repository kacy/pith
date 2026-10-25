// hash_kernels: rust counterpart of bench/hash_kernels.pith.
//
// fills a buffer with xorshift64 output written eight bytes at a time,
// little-endian, then runs one hash or checksum over it and prints the
// digest, which matches the pith and go versions. rust's standard library
// has none of these kernels, so this uses the crates a rust team would reach
// for: sha1 and sha2 from rustcrypto (pure rust, with sha-ni and other cpu
// extensions picked at run time), crc32fast (pure rust, pclmulqdq when the
// cpu has it) and adler2 (pure rust). 32-bit fnv-1a is four lines, so it is
// written out here, byte at a time, as its definition reads.
//
// build: cargo build --release --manifest-path bench/hash_kernels_rust/Cargo.toml
// run:   ./bench/hash_kernels_rust/target/release/hash_kernels_rust sha256 256
//        ./bench/hash_kernels_rust/target/release/hash_kernels_rust none 256

use sha1::Sha1;
use sha2::{Digest, Sha224, Sha256, Sha384, Sha512};

fn hex<D: Digest>(data: &[u8]) -> String {
    D::digest(data).iter().map(|b| format!("{:02x}", b)).collect()
}

fn fnv1a(data: &[u8]) -> u32 {
    let mut h: u32 = 2166136261;
    for &b in data {
        h = (h ^ b as u32).wrapping_mul(16777619);
    }
    h
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let kernel = argv.get(1).map(String::as_str).unwrap_or("sha256");
    let mb: usize = argv.get(2).and_then(|s| s.parse().ok()).unwrap_or(4);
    let total = mb * 1048576;
    let mut data: Vec<u8> = Vec::with_capacity(total);
    let mut state: u64 = 88172645463325252;
    while data.len() < total {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        data.extend_from_slice(&state.to_le_bytes());
    }
    println!("len={}", data.len());
    let digest = match kernel {
        "none" => "none".to_string(),
        "sha1" => hex::<Sha1>(&data),
        "sha224" => hex::<Sha224>(&data),
        "sha256" => hex::<Sha256>(&data),
        "sha384" => hex::<Sha384>(&data),
        "sha512" => hex::<Sha512>(&data),
        "fnv1a" => fnv1a(&data).to_string(),
        "crc32" => crc32fast::hash(&data).to_string(),
        "adler32" => adler2::adler32_slice(&data).to_string(),
        _ => {
            println!(
                "unknown kernel {}; one of none sha1 sha224 sha256 sha384 sha512 fnv1a crc32 adler32",
                kernel
            );
            std::process::exit(2);
        }
    };
    println!("digest={}", digest);
}
