// closure_calls: c counterpart of bench/closure_calls.pith.
//
// c has no closures; the usual stand-in is a function pointer with a context
// struct, which is what this builds. the struct lives on the stack, so the
// build phase allocates nothing, like the rust and c++ versions. the
// compiler may see through the pointer and inline the call.
//
// build: cc -O2 -o bench/closure_calls_c bench/closure_calls.c
// run:   ./bench/closure_calls_c 50000000
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

struct adder {
    long long (*fn)(const struct adder *, long long);
    long long base;
};

static long long add_base(const struct adder *self, long long x) { return self->base + x; }

static long long direct_add(long long base, long long x) { return base + x; }

static long long bench_build(long long n) {
    long long total = 0;
    for (long long i = 0; i < n; i++) {
        struct adder f = {add_base, i % 13};
        total += f.fn(&f, i);
    }
    return total;
}

static long long bench_call(long long n) {
    struct adder f = {add_base, 7};
    long long total = 0;
    for (long long i = 0; i < n; i++) total += f.fn(&f, i);
    return total;
}

static long long bench_direct(long long n) {
    long long total = 0;
    for (long long i = 0; i < n; i++) total += direct_add(7, i);
    return total;
}

static long long now_ms(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec * 1000LL + t.tv_nsec / 1000000;
}

int main(int argc, char **argv) {
    long long n = argc > 1 ? atoll(argv[1]) : 2000000;
    long long t0 = now_ms();
    long long build = bench_build(n);
    long long t1 = now_ms();
    long long call = bench_call(n);
    long long t2 = now_ms();
    long long direct = bench_direct(n);
    long long t3 = now_ms();
    printf("closure calls benchmark\n");
    printf("iterations=%lld\n", n);
    printf("build_ms=%lld\n", t1 - t0);
    printf("call_ms=%lld\n", t2 - t1);
    printf("direct_ms=%lld\n", t3 - t2);
    printf("checksum=%lld\n", build + call + direct);
    return 0;
}
