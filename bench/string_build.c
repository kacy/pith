// string_build: c counterpart of bench/string_build.pith.
//
// grows one heap buffer by many short appends, doubling its capacity when
// it fills, the way a c program builds a string of unknown length.
//
// build: cc -O2 -o bench/string_build_c bench/string_build.c
// run:   ./bench/string_build_c 2000000
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

struct buf {
    char *data;
    size_t len, cap;
};

static void append(struct buf *b, const char *s, size_t n) {
    if (b->len + n > b->cap) {
        size_t cap = b->cap ? b->cap * 2 : 16;
        while (cap < b->len + n) cap *= 2;
        b->data = realloc(b->data, cap);
        if (!b->data) abort();
        b->cap = cap;
    }
    memcpy(b->data + b->len, s, n);
    b->len += n;
}

int main(int argc, char **argv) {
    long n = argc > 1 ? atol(argv[1]) : 0;
    if (n <= 0) n = 1000;
    struct buf s = {0};
    char num[24];
    for (long i = 0; i < n; i++) {
        append(&s, "item-", 5);
        append(&s, num, (size_t)snprintf(num, sizeof num, "%ld", i));
        append(&s, ",", 1);
    }
    unsigned long long sum = 0;
    for (size_t j = 0; j < s.len; j += 97) sum += (unsigned char)s.data[j];
    printf("len: %zu checksum: %llu\n", s.len, sum);
    free(s.data);
    return 0;
}
