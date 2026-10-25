// closure_calls: rust counterpart of bench/closure_calls.pith.
//
// splits the cost of a closure into three phases, each folded into one
// checksum:
//
//   build   n closures, each built, called once, and dropped
//   call    one closure called n times
//   direct  a plain function called n times, the floor
//
// a rust closure is an unboxed value whose call is static and usually
// inlined, so these phases measure what rust does with the idiomatic form.
// a Box<dyn Fn> per iteration would be the closer analogue of a heap
// closure with dynamic dispatch, and is not what a rust developer writes
// here.
//
// build: rustc -O -o bench/closure_calls_rust bench/closure_calls.rs
// run:   ./bench/closure_calls_rust 50000000

use std::time::Instant;

fn direct_add(base: i64, x: i64) -> i64 {
    base + x
}

fn bench_build(n: i64) -> i64 {
    let mut total = 0;
    let mut i = 0;
    while i < n {
        let base = i % 13;
        let f = move |x: i64| base + x;
        total += f(i);
        i += 1;
    }
    total
}

fn bench_call(n: i64) -> i64 {
    let base = 7;
    let f = move |x: i64| base + x;
    let mut total = 0;
    let mut i = 0;
    while i < n {
        total += f(i);
        i += 1;
    }
    total
}

fn bench_direct(n: i64) -> i64 {
    let mut total = 0;
    let mut i = 0;
    while i < n {
        total += direct_add(7, i);
        i += 1;
    }
    total
}

fn main() {
    let mut n: i64 = 2000000;
    if let Some(arg) = std::env::args().nth(1) {
        if let Ok(v) = arg.parse::<i64>() {
            n = v;
        }
    }

    let t0 = Instant::now();
    let build = bench_build(n);
    let t1 = Instant::now();
    let call = bench_call(n);
    let t2 = Instant::now();
    let direct = bench_direct(n);
    let t3 = Instant::now();

    println!("closure calls benchmark");
    println!("iterations={}", n);
    println!("build_ms={}", (t1 - t0).as_millis());
    println!("call_ms={}", (t2 - t1).as_millis());
    println!("direct_ms={}", (t3 - t2).as_millis());
    println!("checksum={}", build + call + direct);
}
