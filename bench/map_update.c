// map_update: c counterpart of bench/map_update.pith.
//
// bump the count of a key the map already holds, or insert it with 1, over a
// small pool of keys repeated many times. the c standard library has no hash
// map, so this is a small open-addressing table with linear probing, the kind
// a c program carries. byte keys are hashed with 64-bit fnv-1a and int keys
// with a multiplicative mix. string and bytes keys are the same thing in c,
// so those two flavors run the same code. a miss copies the key, as the rust
// version clones it.
//
// build: cc -O2 -o bench/map_update_c bench/map_update.c
// run:   ./bench/map_update_c <flavor: string|int|bytes> <updates> <distinct keys>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

struct slot {
    char *key; // NULL when empty; unused for int keys
    size_t len;
    int64_t ikey;
    int used;
    int64_t value;
};

struct map {
    struct slot *slots;
    size_t cap, count;
};

static uint64_t hash_bytes(const char *p, size_t n) {
    uint64_t h = 14695981039346656037ULL;
    for (size_t i = 0; i < n; i++) h = (h ^ (unsigned char)p[i]) * 1099511628211ULL;
    return h;
}

static uint64_t hash_int(int64_t k) {
    uint64_t h = (uint64_t)k * 0x9e3779b97f4a7c15ULL;
    return h ^ (h >> 32);
}

static void grow(struct map *m, int ints);

static struct slot *find(struct map *m, const char *key, size_t len, int64_t ikey, int ints) {
    if ((m->count + 1) * 4 > m->cap * 3) grow(m, ints);
    size_t mask = m->cap - 1;
    size_t i = (ints ? hash_int(ikey) : hash_bytes(key, len)) & mask;
    for (;; i = (i + 1) & mask) {
        struct slot *s = &m->slots[i];
        if (!s->used) return s;
        if (ints ? s->ikey == ikey : (s->len == len && memcmp(s->key, key, len) == 0)) return s;
    }
}

static void grow(struct map *m, int ints) {
    struct map bigger = {calloc(m->cap ? m->cap * 2 : 16, sizeof(struct slot)), m->cap ? m->cap * 2 : 16, 0};
    for (size_t i = 0; i < m->cap; i++)
        if (m->slots[i].used) {
            struct slot *s = find(&bigger, m->slots[i].key, m->slots[i].len, m->slots[i].ikey, ints);
            *s = m->slots[i];
            bigger.count++;
        }
    free(m->slots);
    *m = bigger;
}

static void bump(struct map *m, const char *key, size_t len, int64_t ikey, int ints) {
    struct slot *s = find(m, key, len, ikey, ints);
    if (s->used) {
        s->value++;
        return;
    }
    s->used = 1;
    s->ikey = ikey;
    s->len = len;
    if (!ints) {
        s->key = malloc(len);
        memcpy(s->key, key, len);
    }
    s->value = 1;
    m->count++;
}

static long arg_int(int argc, char **argv, int i, long fallback) {
    long v = argc > i ? atol(argv[i]) : 0;
    return v > 0 ? v : fallback;
}

int main(int argc, char **argv) {
    const char *flavor = argc > 1 ? argv[1] : "string";
    long updates = arg_int(argc, argv, 2, 200000);
    long distinct = arg_int(argc, argv, 3, 64);
    int ints = strcmp(flavor, "int") == 0;
    char **keys = malloc(distinct * sizeof *keys);
    size_t *lens = malloc(distinct * sizeof *lens);
    for (long i = 0; i < distinct; i++) {
        char tmp[64];
        lens[i] = (size_t)snprintf(tmp, sizeof tmp, "user-%ld-region", i);
        keys[i] = strdup(tmp);
    }
    struct map m = {0};
    for (long i = 0; i < updates; i++) {
        long k = i % distinct;
        bump(&m, keys[k], lens[k], k, ints);
    }
    int64_t total = 0;
    for (size_t i = 0; i < m.cap; i++)
        if (m.slots[i].used) total += m.slots[i].value;
    printf("map_update flavor=%s updates=%ld distinct=%ld\n", flavor, updates, distinct);
    printf("total=%lld\n", (long long)total);
    return 0;
}
