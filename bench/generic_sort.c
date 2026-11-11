// generic_sort: c counterpart of bench/generic_sort.pith.
//
// the same rows as the rust version (a seeded 31-bit lcg, built before the
// clock starts). only the `current` arm is here: copy the array of row
// pointers and qsort it on the field. qsort is not stable, but rows with
// equal keys are identical, so the checksum matches the stable sorts.
//
// build: cc -O2 -o bench/generic_sort_c bench/generic_sort.c
// run:   ./bench/generic_sort_c current <dist> <key> <n> <rounds>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

struct row {
    char *label;
    long long rank;
};

static int by_rank(const void *a, const void *b) {
    long long x = (*(struct row *const *)a)->rank, y = (*(struct row *const *)b)->rank;
    return (x > y) - (x < y);
}

static int by_label(const void *a, const void *b) {
    return strcmp((*(struct row *const *)a)->label, (*(struct row *const *)b)->label);
}

static long long checksum(struct row **rows, long long n) {
    long long total = 0;
    for (long long i = 0; i < n; i++)
        total = (total + (i + 1) * (rows[i]->rank + 1) + (long long)strlen(rows[i]->label)) % 1000000007;
    return total;
}

int main(int argc, char **argv) {
    const char *arm = argc > 1 ? argv[1] : "current";
    const char *dist = argc > 2 ? argv[2] : "random";
    const char *key = argc > 3 ? argv[3] : "int";
    long long n = argc > 4 ? atoll(argv[4]) : 1000;
    long long rounds = argc > 5 ? atoll(argv[5]) : 5;
    if (strcmp(arm, "current") != 0) {
        printf("only the current arm is implemented\n");
        return 2;
    }
    struct row *rows = malloc(n * sizeof *rows);
    struct row **refs = malloc(n * sizeof *refs);
    struct row **sorted = malloc(n * sizeof *sorted);
    long long state = 2463534242LL;
    for (long long i = 0; i < n; i++) {
        long long rank;
        if (strcmp(dist, "sorted") == 0) {
            rank = i;
        } else if (strcmp(dist, "reverse") == 0) {
            rank = n - i;
        } else {
            state = (state * 1103515245 + 12345) % 2147483648LL;
            rank = strcmp(dist, "dups") == 0 ? state % 8 : state % 1000000;
        }
        char padded[32], label[32];
        int len = snprintf(padded, sizeof padded, "0000000%lld", rank);
        snprintf(label, sizeof label, "row-%s", padded + len - 7);
        rows[i].label = strdup(label);
        rows[i].rank = rank;
        refs[i] = &rows[i];
    }
    int strings = strcmp(key, "string") == 0;
    struct timespec t0, t1;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    long long sum = 0;
    for (long long r = 0; r < rounds; r++) {
        memcpy(sorted, refs, n * sizeof *refs);
        qsort(sorted, n, sizeof *sorted, strings ? by_label : by_rank);
        sum = (sum + checksum(sorted, n)) % 1000000007;
    }
    clock_gettime(CLOCK_MONOTONIC, &t1);
    long long us = (t1.tv_sec - t0.tv_sec) * 1000000LL + (t1.tv_nsec - t0.tv_nsec) / 1000;
    printf("%s %s %s n=%lld rounds=%lld total_us=%lld per_op_us=%lld checksum=%lld\n", arm, dist, key, n, rounds, us,
           rounds > 0 ? us / rounds : us, sum);
    return 0;
}
