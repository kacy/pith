// tight_loop: c counterpart of bench/tight_loop.pith.
//
// sums i*3 over n iterations in a function of its own and prints the sum as
// the checksum. like llvm for the rust version, gcc and clang are free to
// vectorize the loop or replace it with a closed form.
//
// build: cc -O2 -o bench/tight_loop_c bench/tight_loop.c
// run:   ./bench/tight_loop_c 2000000000
#include <stdio.h>
#include <stdlib.h>

__attribute__((noinline)) static long long run(long long n) {
    long long sum = 0;
    for (long long i = 0; i < n; i++) sum += i * 3;
    return sum;
}

int main(int argc, char **argv) {
    long long n = argc > 1 ? atoll(argv[1]) : 0;
    if (n <= 0) n = 200000000;
    printf("checksum=%lld\n", run(n));
    return 0;
}
