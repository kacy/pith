// path_clean_count: c counterpart of bench/path_clean_count.pith.
//
// the same one-pass counting algorithm as the rust version, over the same
// twelve paths, with nothing allocated. c++ would be the same code, so there
// is no c++ version.
//
// build: cc -O2 -o bench/path_clean_count_c bench/path_clean_count.c
// run:   ./bench/path_clean_count_c <rounds>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static size_t clean_part_count(const char *p, size_t n) {
    if (n == 0) return 0;
    int absolute = p[0] == '/';
    size_t depth = 0, climbs = 0, start = 0;
    for (size_t i = 0; i <= n; i++) {
        if (i == n || p[i] == '/') {
            size_t len = i - start;
            const char *seg = p + start;
            if (len == 2 && seg[0] == '.' && seg[1] == '.') {
                if (depth > 0)
                    depth--;
                else if (!absolute)
                    climbs++;
            } else if (len > 0 && !(len == 1 && seg[0] == '.')) {
                depth++;
            }
            start = i + 1;
        }
    }
    return depth + climbs;
}

int main(int argc, char **argv) {
    long rounds = argc > 1 ? atol(argv[1]) : 0;
    if (rounds <= 0) rounds = 20000;
    // the paths are copied to the heap, as the rust version holds Strings,
    // so the compiler cannot fold the counts at build time.
    static const char *literals[] = {
        "data//north/./users/../users/123.json",
        "/var/log/../lib/./pith/build/",
        "../../escape/../still/../../up",
        "/../root/./sees/no/parent",
        "plain/relative/path",
        "a/b/c/d/e/f/g/h/../../../../x",
        "",
        "/",
        ".",
        "./only/dots/./././end/.",
        "trailing/slashes///",
        "/abs//double/../../../gone",
    };
    enum { npaths = sizeof literals / sizeof literals[0] };
    char *paths[npaths];
    size_t lens[npaths];
    for (int i = 0; i < npaths; i++) {
        paths[i] = strdup(literals[i]);
        lens[i] = strlen(paths[i]);
    }
    struct timespec t0, t1;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    size_t checksum = 0;
    for (long r = 0; r < rounds; r++)
        for (int i = 0; i < npaths; i++) checksum += clean_part_count(paths[i], lens[i]) * (size_t)(i + 1);
    clock_gettime(CLOCK_MONOTONIC, &t1);
    long long ms = (t1.tv_sec - t0.tv_sec) * 1000LL + (t1.tv_nsec - t0.tv_nsec) / 1000000;
    printf("path clean_part_count benchmark\n");
    printf("rounds=%ld\n", rounds);
    printf("elapsed_ms=%lld\n", ms);
    printf("checksum=%zu\n", checksum);
    return 0;
}
