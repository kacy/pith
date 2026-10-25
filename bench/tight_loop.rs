// tight_loop: rust counterpart of bench/tight_loop.pith.
//
// sums i*3 over n iterations in a function of its own and prints the sum as
// the checksum. the loop does no allocation, no call and no channel work, so
// it prices the bare loop. rust threads have no safe-point at all.
//
// llvm is free to vectorize this loop or replace it with a closed form; that
// is what an optimizing compiler does with it, and it is left alone here.
//
// build: rustc -O -o bench/tight_loop_rust bench/tight_loop.rs
// run:   ./bench/tight_loop_rust 2000000000

fn iterations_from_args() -> i64 {
    if let Some(arg) = std::env::args().nth(1) {
        if let Ok(n) = arg.parse::<i64>() {
            if n > 0 {
                return n;
            }
        }
    }
    200000000
}

#[inline(never)]
fn run(n: i64) -> i64 {
    let mut sum: i64 = 0;
    let mut i: i64 = 0;
    while i < n {
        sum += i * 3;
        i += 1;
    }
    sum
}

fn main() {
    println!("checksum={}", run(iterations_from_args()));
}
