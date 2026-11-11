// csv_encode: c counterpart of bench/csv_encode.pith.
//
// the encoder of the rust version: a field is quoted when it holds a comma,
// a double quote or a newline, quotes inside it are doubled, and rows are
// joined by "\n" with none after the last. each round encodes into a fresh
// growable buffer. the checksum is the encoded byte count folded with every
// 97th byte.
//
// build: cc -O2 -o bench/csv_encode_c bench/csv_encode.c
// run:   ./bench/csv_encode_c <rows> <rounds>
#define _GNU_SOURCE
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define FIELDS 10

struct buf {
    char *data;
    size_t len, cap;
};

static void reserve(struct buf *b, size_t extra) {
    if (b->len + extra <= b->cap) return;
    size_t cap = b->cap ? b->cap * 2 : 64;
    while (cap < b->len + extra) cap *= 2;
    b->data = realloc(b->data, cap);
    if (!b->data) abort();
    b->cap = cap;
}

static void push(struct buf *b, char c) {
    reserve(b, 1);
    b->data[b->len++] = c;
}

// a heap string from a format, like format! in the rust version.
static char *fmt(const char *f, ...) {
    char *out;
    va_list ap;
    va_start(ap, f);
    if (vasprintf(&out, f, ap) < 0) abort();
    va_end(ap);
    return out;
}

static const char *region_for(size_t i) {
    static const char *r[] = {"north", "south", "east", "west"};
    return r[i % 4];
}

static void encode(char **rows, size_t nrows, struct buf *out) {
    for (size_t r = 0; r < nrows; r++) {
        if (r > 0) push(out, '\n');
        for (int i = 0; i < FIELDS; i++) {
            if (i > 0) push(out, ',');
            const char *f = rows[r * FIELDS + i];
            size_t n = strlen(f);
            if (strpbrk(f, ",\"\n")) {
                reserve(out, 2 * n + 2);
                out->data[out->len++] = '"';
                for (size_t j = 0; j < n; j++) {
                    if (f[j] == '"') out->data[out->len++] = '"';
                    out->data[out->len++] = f[j];
                }
                out->data[out->len++] = '"';
            } else {
                reserve(out, n);
                memcpy(out->data + out->len, f, n);
                out->len += n;
            }
        }
    }
}

int main(int argc, char **argv) {
    long records = argc > 1 ? atol(argv[1]) : 0;
    long rounds = argc > 2 ? atol(argv[2]) : 0;
    if (records <= 0) records = 5000;
    if (rounds <= 0) rounds = 10;
    size_t nrows = (size_t)records + 1;
    char **rows = malloc(nrows * FIELDS * sizeof *rows);
    static const char *header[FIELDS] = {"id", "name", "region", "active", "score",
                                         "quota", "url", "joined", "note", "path"};
    for (int i = 0; i < FIELDS; i++) rows[i] = strdup(header[i]);
    for (long i = 0; i < records; i++) {
        char **row = rows + (i + 1) * FIELDS;
        const char *region = region_for(i);
        long score = (i * 17 + 11) % 100, quota = (i * 13 + 7) % 50;
        row[0] = fmt("%ld", i);
        row[1] = fmt("user-%ld", i);
        row[2] = strdup(region);
        row[3] = strdup(i % 3 != 0 ? "true" : "false");
        row[4] = fmt("%ld", score);
        row[5] = fmt("%ld", quota);
        row[6] = fmt("https://example.com/api/%s/users/%ld?score=%ld", region, i, score);
        row[7] = fmt("2026-%ld-%ld", i % 12 + 1, i % 28 + 1);
        row[8] = fmt("user %ld, region %s", i, region);
        row[9] = fmt("data//%s/./users/../users/%ld.json", region, i);
    }
    struct timespec t0, t1;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    unsigned long long checksum = 0;
    for (long round = 0; round < rounds; round++) {
        struct buf out = {0};
        encode(rows, nrows, &out);
        checksum += out.len;
        for (size_t i = (size_t)(round % 7); i < out.len; i += 97) checksum += (unsigned char)out.data[i];
        free(out.data);
    }
    clock_gettime(CLOCK_MONOTONIC, &t1);
    long long ms = (t1.tv_sec - t0.tv_sec) * 1000LL + (t1.tv_nsec - t0.tv_nsec) / 1000000;
    printf("csv encode benchmark\n");
    printf("rows=%ld\n", records);
    printf("rounds=%ld\n", rounds);
    printf("elapsed_ms=%lld\n", ms);
    printf("checksum=%llu\n", checksum);
    return 0;
}
