// closure_calls: c++ counterpart of bench/closure_calls.pith.
//
// the three phases of the rust version with c++ lambdas. a lambda is an
// unboxed value with a static call, like a rust closure; std::function
// would add type erasure and is not what this loop would use.
//
// build: c++ -O2 -std=c++20 -o bench/closure_calls_cpp bench/closure_calls.cpp
// run:   ./bench/closure_calls_cpp 50000000
#include <chrono>
#include <cstdio>
#include <cstdlib>

static long long direct_add(long long base, long long x) { return base + x; }

static long long bench_build(long long n) {
    long long total = 0;
    for (long long i = 0; i < n; i++) {
        long long base = i % 13;
        auto f = [base](long long x) { return base + x; };
        total += f(i);
    }
    return total;
}

static long long bench_call(long long n) {
    long long base = 7;
    auto f = [base](long long x) { return base + x; };
    long long total = 0;
    for (long long i = 0; i < n; i++) total += f(i);
    return total;
}

static long long bench_direct(long long n) {
    long long total = 0;
    for (long long i = 0; i < n; i++) total += direct_add(7, i);
    return total;
}

int main(int argc, char **argv) {
    using clock = std::chrono::steady_clock;
    long long n = argc > 1 ? std::atoll(argv[1]) : 2000000;
    auto ms = [](auto d) { return (long long)std::chrono::duration_cast<std::chrono::milliseconds>(d).count(); };
    auto t0 = clock::now();
    long long build = bench_build(n);
    auto t1 = clock::now();
    long long call = bench_call(n);
    auto t2 = clock::now();
    long long direct = bench_direct(n);
    auto t3 = clock::now();
    std::printf("closure calls benchmark\n");
    std::printf("iterations=%lld\n", n);
    std::printf("build_ms=%lld\n", ms(t1 - t0));
    std::printf("call_ms=%lld\n", ms(t2 - t1));
    std::printf("direct_ms=%lld\n", ms(t3 - t2));
    std::printf("checksum=%lld\n", build + call + direct);
}
