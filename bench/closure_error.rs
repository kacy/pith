use std::time::Instant;

fn make_adder(base: i64) -> impl Fn(i64) -> i64 {
    move |x| base + x
}

fn apply_twice<F: Fn(i64) -> i64>(f: &F, x: i64) -> i64 {
    f(f(x))
}

fn bench_closures(iterations: i64) -> i64 {
    let mut total: i64 = 0;
    for i in 0..iterations {
        let add = make_adder(i);
        let step = i % 7 + 1;
        let mul = move |x: i64| x * step;
        for j in 0..8 {
            total += add(j) - mul(j) + apply_twice(&add, j);
        }
    }
    total
}

fn checked(n: i64) -> Result<i64, String> {
    let label = format!("value-{}", n);
    let tags = ["a", "b", label.as_str()];
    if n % 5 == 0 {
        return Err(format!("rejected: {}", label));
    }
    Ok(n * 2 + tags.len() as i64)
}

fn two_layers(n: i64) -> Result<i64, String> {
    let a = checked(n)?;
    let b = checked(n + 1)?;
    Ok(a + b)
}

fn bench_errors(iterations: i64) -> i64 {
    let mut total: i64 = 0;
    for i in 0..iterations {
        total += two_layers(i).unwrap_or(0);
        total += checked(i).unwrap_or(-1);
    }
    total
}

fn main() {
    let iterations: i64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .filter(|&v: &i64| v > 0)
        .unwrap_or(200000);
    println!("closure/error benchmark");
    println!("iterations={}", iterations);

    let total_start = Instant::now();

    let t0 = Instant::now();
    let closure_total = bench_closures(iterations);
    let closure_ms = t0.elapsed().as_millis();

    let t1 = Instant::now();
    let error_total = bench_errors(iterations);
    let error_ms = t1.elapsed().as_millis();

    let total_ms = total_start.elapsed().as_millis();
    let checksum = closure_total + error_total;

    println!("closure_ms={}", closure_ms);
    println!("error_ms={}", error_ms);
    println!("total_ms={}", total_ms);
    println!("checksum={}", checksum);
}
