// substring_search: c counterpart of bench/substring_search.pith.
//
// the same cells as the rust version, searched with glibc's memmem, which is
// what a c program calls for a substring with a known length. contains is
// memmem != NULL and find maps NULL to -1, both called each round, so every
// cell costs two searches as in the other versions.
//
// build: cc -O2 -o bench/substring_search_c bench/substring_search.c
// run:   ./bench/substring_search_c 20000 [cell|all] [comma|word|repeat|all]
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

struct cell {
    char *hay;
    size_t hay_len;
    const char *needle;
    size_t needle_len;
};

// filler with no comma and no occurrence of "sitting".
static void fill(char *out, size_t len) {
    static const char pattern[] = "lorem ipsum dolor sit amet consectetur s ";
    size_t plen = sizeof pattern - 1;
    for (size_t i = 0; i < len; i++) out[i] = pattern[i % plen];
}

// lays out pad, needle, pad for one position, with pad from fill or from 'a'.
static char *haystack(size_t len, const char *pos, const char *needle, int repeat) {
    char *h = malloc(len + 1);
    size_t nl = strlen(needle);
    size_t gap = len - nl, left;
    if (strcmp(pos, "absent") == 0) {
        gap = len;
        left = len;
    } else if (strcmp(pos, "start") == 0) {
        left = 0;
    } else if (strcmp(pos, "end") == 0) {
        left = gap;
    } else {
        left = gap / 2;
    }
    if (repeat) {
        memset(h, 'a', len);
    } else {
        // each pad of the rust version restarts the filler pattern.
        fill(h, left);
        if (left < len) fill(h + left + nl, gap - left);
    }
    if (strcmp(pos, "absent") != 0) memcpy(h + left, needle, nl);
    h[len] = 0;
    return h;
}

int main(int argc, char **argv) {
    long rounds = argc > 1 ? atol(argv[1]) : 2000;
    const char *only = argc > 2 ? argv[2] : "all";
    const char *kind_only = argc > 3 ? argv[3] : "all";
    static const char *sizes[] = {"short", "medium", "long"};
    static const size_t size_len[] = {12, 80, 4000};
    static const char *poses[] = {"start", "middle", "end", "absent"};
    static const char *kinds[] = {"comma", "word", "repeat"};
    static const char *needles[] = {",", "sitting", "aaaaaab"};
    struct cell cells[36];
    int ncells = 0;
    char name[64];
    for (int s = 0; s < 3; s++)
        for (int p = 0; p < 4; p++) {
            snprintf(name, sizeof name, "%s-%s", sizes[s], poses[p]);
            if (strcmp(only, "all") != 0 && strcmp(only, name) != 0) continue;
            for (int k = 0; k < 3; k++) {
                if (strcmp(kind_only, "all") != 0 && strcmp(kind_only, kinds[k]) != 0) continue;
                cells[ncells].hay = haystack(size_len[s], poses[p], needles[k], k == 2);
                cells[ncells].hay_len = size_len[s];
                cells[ncells].needle = needles[k];
                cells[ncells].needle_len = strlen(needles[k]);
                ncells++;
            }
        }
    long long calls = (long long)ncells * rounds * 2;
    struct timespec t0, t1;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    long long sum = 0;
    for (long r = 0; r < rounds; r++)
        for (int i = 0; i < ncells; i++) {
            struct cell *c = &cells[i];
            if (memmem(c->hay, c->hay_len, c->needle, c->needle_len)) sum++;
            const char *at = memmem(c->hay, c->hay_len, c->needle, c->needle_len);
            long long index = at ? at - c->hay : -1;
            sum = (sum * 31 + index + 2) % 1000000007;
        }
    clock_gettime(CLOCK_MONOTONIC, &t1);
    long long ns = (t1.tv_sec - t0.tv_sec) * 1000000000LL + (t1.tv_nsec - t0.tv_nsec);
    printf("cells=%d rounds=%ld calls=%lld total_us=%lld per_call_ns=%lld checksum=%lld\n", ncells, rounds, calls,
           ns / 1000, calls > 0 ? ns / calls : ns, sum);
    return 0;
}
