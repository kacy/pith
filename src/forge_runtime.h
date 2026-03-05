// forge_runtime.h — minimal C runtime for forge-generated code
//
// provides string type, print, and basic helpers. this is intentionally
// simple — just enough to get programs running. memory management is
// "leak everything" for now; ARC comes later.

#ifndef FORGE_RUNTIME_H
#define FORGE_RUNTIME_H

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <stdbool.h>
#include <inttypes.h>

// ---------------------------------------------------------------
// closure type (function pointer + captured environment)
// ---------------------------------------------------------------

typedef struct {
    void *fn_ptr;
    void *env_ptr;
} forge_closure_t;

// ---------------------------------------------------------------
// string type
// ---------------------------------------------------------------

typedef struct {
    const char *data;
    int64_t len;
} forge_string_t;

// create a string from a C string literal (compile-time known length)
#define FORGE_STRING_LIT(s) ((forge_string_t){ .data = (s), .len = sizeof(s) - 1 })

// create a string from a pointer and length
static inline forge_string_t forge_string_from(const char *data, int64_t len) {
    return (forge_string_t){ .data = data, .len = len };
}

// empty string constant
static const forge_string_t forge_string_empty = { .data = "", .len = 0 };

// ---------------------------------------------------------------
// string operations
// ---------------------------------------------------------------

static inline forge_string_t forge_string_concat(forge_string_t a, forge_string_t b) {
    if (a.len > INT64_MAX - b.len) {
        fprintf(stderr, "forge: string too large\n");
        exit(1);
    }
    int64_t new_len = a.len + b.len;
    char *buf = (char *)malloc((size_t)new_len + 1);
    if (!buf) {
        fprintf(stderr, "forge: out of memory\n");
        exit(1);
    }
    memcpy(buf, a.data, (size_t)a.len);
    memcpy(buf + a.len, b.data, (size_t)b.len);
    buf[new_len] = '\0';
    return (forge_string_t){ .data = buf, .len = new_len };
}

static inline bool forge_string_eq(forge_string_t a, forge_string_t b) {
    if (a.len != b.len) return false;
    return memcmp(a.data, b.data, (size_t)a.len) == 0;
}

static inline bool forge_string_neq(forge_string_t a, forge_string_t b) {
    return !forge_string_eq(a, b);
}

static inline bool forge_string_lt(forge_string_t a, forge_string_t b) {
    int64_t min_len = a.len < b.len ? a.len : b.len;
    int cmp = memcmp(a.data, b.data, (size_t)min_len);
    if (cmp != 0) return cmp < 0;
    return a.len < b.len;
}

static inline bool forge_string_gt(forge_string_t a, forge_string_t b) {
    return forge_string_lt(b, a);
}

static inline bool forge_string_lte(forge_string_t a, forge_string_t b) {
    return !forge_string_gt(a, b);
}

static inline bool forge_string_gte(forge_string_t a, forge_string_t b) {
    return !forge_string_lt(a, b);
}

// ---------------------------------------------------------------
// string methods
// ---------------------------------------------------------------

static inline bool forge_string_contains(forge_string_t haystack, forge_string_t needle) {
    if (needle.len == 0) return true;
    if (needle.len > haystack.len) return false;
    for (int64_t i = 0; i <= haystack.len - needle.len; i++) {
        if (memcmp(haystack.data + i, needle.data, (size_t)needle.len) == 0)
            return true;
    }
    return false;
}

static inline bool forge_string_starts_with(forge_string_t s, forge_string_t prefix) {
    if (prefix.len > s.len) return false;
    return memcmp(s.data, prefix.data, (size_t)prefix.len) == 0;
}

static inline bool forge_string_ends_with(forge_string_t s, forge_string_t suffix) {
    if (suffix.len > s.len) return false;
    return memcmp(s.data + s.len - suffix.len, suffix.data, (size_t)suffix.len) == 0;
}

static inline forge_string_t forge_string_trim(forge_string_t s) {
    const char *start = s.data;
    const char *end = s.data + s.len;
    while (start < end && (*start == ' ' || *start == '\t' || *start == '\n' || *start == '\r'))
        start++;
    while (end > start && (*(end - 1) == ' ' || *(end - 1) == '\t' || *(end - 1) == '\n' || *(end - 1) == '\r'))
        end--;
    return forge_string_from(start, (int64_t)(end - start));
}

static inline forge_string_t forge_string_to_upper(forge_string_t s) {
    char *buf = (char *)malloc((size_t)s.len + 1);
    if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    for (int64_t i = 0; i < s.len; i++) {
        char c = s.data[i];
        buf[i] = (c >= 'a' && c <= 'z') ? (char)(c - 32) : c;
    }
    buf[s.len] = '\0';
    return (forge_string_t){ .data = buf, .len = s.len };
}

static inline forge_string_t forge_string_to_lower(forge_string_t s) {
    char *buf = (char *)malloc((size_t)s.len + 1);
    if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    for (int64_t i = 0; i < s.len; i++) {
        char c = s.data[i];
        buf[i] = (c >= 'A' && c <= 'Z') ? (char)(c + 32) : c;
    }
    buf[s.len] = '\0';
    return (forge_string_t){ .data = buf, .len = s.len };
}

static inline forge_string_t forge_string_substring(forge_string_t s, int64_t start, int64_t end) {
    if (start < 0) start = 0;
    if (end > s.len) end = s.len;
    if (start >= end) return forge_string_empty;
    int64_t new_len = end - start;
    char *buf = (char *)malloc((size_t)new_len + 1);
    if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    memcpy(buf, s.data + start, (size_t)new_len);
    buf[new_len] = '\0';
    return (forge_string_t){ .data = buf, .len = new_len };
}

// index a single character by position. returns a 1-char string.
static inline forge_string_t forge_string_char_at(forge_string_t s, int64_t index) {
    if (index < 0 || index >= s.len) {
        fprintf(stderr, "forge: string index out of bounds (index %" PRId64 ", length %" PRId64 ")\n", index, s.len);
        exit(1);
    }
    char *buf = (char *)malloc(2);
    if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    buf[0] = s.data[index];
    buf[1] = '\0';
    return (forge_string_t){ .data = buf, .len = 1 };
}

// chr(Int) -> String: return a single-character string for the given ASCII code.
static inline forge_string_t forge_chr(int64_t code) {
    char *buf = (char *)malloc(2);
    if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    buf[0] = (char)(code & 0xFF);
    buf[1] = '\0';
    return (forge_string_t){ .data = buf, .len = 1 };
}

// replace all occurrences of `old` with `new_s` in `s`.
static inline forge_string_t forge_string_replace(forge_string_t s, forge_string_t old, forge_string_t new_s) {
    if (old.len == 0) {
        // empty pattern — return a copy
        char *buf = (char *)malloc((size_t)s.len + 1);
        if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
        memcpy(buf, s.data, (size_t)s.len);
        buf[s.len] = '\0';
        return (forge_string_t){ .data = buf, .len = s.len };
    }
    // first pass: count occurrences
    int64_t count = 0;
    for (int64_t i = 0; i + old.len <= s.len; i++) {
        if (memcmp(s.data + i, old.data, (size_t)old.len) == 0) {
            count++;
            i += old.len - 1;
        }
    }
    if (count == 0) {
        char *buf = (char *)malloc((size_t)s.len + 1);
        if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
        memcpy(buf, s.data, (size_t)s.len);
        buf[s.len] = '\0';
        return (forge_string_t){ .data = buf, .len = s.len };
    }
    // second pass: build result
    int64_t new_len = s.len + count * (new_s.len - old.len);
    char *buf = (char *)malloc((size_t)new_len + 1);
    if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    int64_t pos = 0;
    for (int64_t i = 0; i < s.len; ) {
        if (i + old.len <= s.len && memcmp(s.data + i, old.data, (size_t)old.len) == 0) {
            memcpy(buf + pos, new_s.data, (size_t)new_s.len);
            pos += new_s.len;
            i += old.len;
        } else {
            buf[pos++] = s.data[i++];
        }
    }
    buf[new_len] = '\0';
    return (forge_string_t){ .data = buf, .len = new_len };
}

// split uses a forward-declared list type — defined after collection types
// (see forge_string_split below)

// ---------------------------------------------------------------
// conversions to string
// ---------------------------------------------------------------

static inline forge_string_t forge_int_to_string(int64_t n) {
    char buf[32];
    int len = snprintf(buf, sizeof(buf), "%" PRId64, n);
    char *result = (char *)malloc((size_t)len + 1);
    if (!result) {
        fprintf(stderr, "forge: out of memory\n");
        exit(1);
    }
    memcpy(result, buf, (size_t)len + 1);
    return (forge_string_t){ .data = result, .len = len };
}

static inline forge_string_t forge_float_to_string(double n) {
    char buf[64];
    int len = snprintf(buf, sizeof(buf), "%g", n);
    char *result = (char *)malloc((size_t)len + 1);
    if (!result) {
        fprintf(stderr, "forge: out of memory\n");
        exit(1);
    }
    memcpy(result, buf, (size_t)len + 1);
    return (forge_string_t){ .data = result, .len = len };
}

static inline forge_string_t forge_bool_to_string(bool b) {
    return b ? FORGE_STRING_LIT("true") : FORGE_STRING_LIT("false");
}

// ---------------------------------------------------------------
// collection types
// ---------------------------------------------------------------

// List[T] — ordered collection backed by a contiguous array.
// stores elements as raw bytes; callers use typed macros to access.
typedef struct {
    void *data;
    int64_t len;
} forge_list_t;

// Map[K,V] — hash-indexed key-value collection.
// dense keys/values arrays preserve insertion order and codegen compatibility.
// a hash bucket index on top gives O(1) lookups via open addressing.
typedef struct {
    void *keys;           // dense array of keys (codegen reads this directly)
    void *values;         // dense array of values (codegen reads this directly)
    int64_t len;          // entry count (codegen reads this directly)
    int32_t *buckets;     // hash index: bucket -> dense array index, -1 = empty
    int32_t cap;          // bucket count (power of 2)
    int32_t _pad;         // alignment padding
} forge_map_t;

// Set[T] — unique element collection. same layout as list for now.
typedef forge_list_t forge_set_t;

// ---------------------------------------------------------------
// hash functions (for map hash index)
// ---------------------------------------------------------------

// splitmix64 finalizer — good integer hash with full avalanche
static inline uint64_t forge_hash_int(int64_t key) {
    uint64_t x = (uint64_t)key;
    x ^= x >> 30;
    x *= 0xbf58476d1ce4e5b9ULL;
    x ^= x >> 27;
    x *= 0x94d049bb133111ebULL;
    x ^= x >> 31;
    return x;
}

// FNV-1a hash over raw bytes
static inline uint64_t forge_hash_bytes(const char *data, int64_t len) {
    uint64_t h = 0xcbf29ce484222325ULL;
    for (int64_t i = 0; i < len; i++) {
        h ^= (uint8_t)data[i];
        h *= 0x100000001b3ULL;
    }
    return h;
}

// hash a forge string
static inline uint64_t forge_hash_string(forge_string_t s) {
    return forge_hash_bytes(s.data, s.len);
}

// ---------------------------------------------------------------
// map hash index helpers
// ---------------------------------------------------------------

// allocate bucket array filled with -1 (empty)
static inline int32_t *forge_map_alloc_buckets(int32_t cap) {
    int32_t *b = (int32_t *)malloc((size_t)cap * sizeof(int32_t));
    if (!b) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    for (int32_t i = 0; i < cap; i++) b[i] = -1;
    return b;
}

// rebuild the bucket index from the dense arrays (string keys)
static inline void forge_map_rebuild_string(forge_map_t *map) {
    int32_t mask = map->cap - 1;
    for (int32_t i = 0; i < map->cap; i++) map->buckets[i] = -1;
    forge_string_t *keys = (forge_string_t *)map->keys;
    for (int32_t i = 0; i < (int32_t)map->len; i++) {
        uint64_t h = forge_hash_string(keys[i]);
        int32_t slot = (int32_t)(h & (uint64_t)mask);
        while (map->buckets[slot] != -1) slot = (slot + 1) & mask;
        map->buckets[slot] = i;
    }
}

// rebuild the bucket index from the dense arrays (integer keys)
static inline void forge_map_rebuild_int(forge_map_t *map) {
    int32_t mask = map->cap - 1;
    for (int32_t i = 0; i < map->cap; i++) map->buckets[i] = -1;
    int64_t *keys = (int64_t *)map->keys;
    for (int32_t i = 0; i < (int32_t)map->len; i++) {
        uint64_t h = forge_hash_int(keys[i]);
        int32_t slot = (int32_t)(h & (uint64_t)mask);
        while (map->buckets[slot] != -1) slot = (slot + 1) & mask;
        map->buckets[slot] = i;
    }
}

// ensure the map has a hash index with room for at least one more entry.
// string key variant — grows at 75% load.
static inline void forge_map_ensure_index_string(forge_map_t *map) {
    if (map->cap == 0) {
        map->cap = 8;
        map->buckets = forge_map_alloc_buckets(8);
        forge_map_rebuild_string(map);
        return;
    }
    // grow at 75% load: len * 4 >= cap * 3
    if ((int32_t)map->len * 4 >= map->cap * 3) {
        free(map->buckets);
        map->cap *= 2;
        map->buckets = forge_map_alloc_buckets(map->cap);
        forge_map_rebuild_string(map);
    }
}

// integer key variant
static inline void forge_map_ensure_index_int(forge_map_t *map) {
    if (map->cap == 0) {
        map->cap = 8;
        map->buckets = forge_map_alloc_buckets(8);
        forge_map_rebuild_int(map);
        return;
    }
    if ((int32_t)map->len * 4 >= map->cap * 3) {
        free(map->buckets);
        map->cap *= 2;
        map->buckets = forge_map_alloc_buckets(map->cap);
        forge_map_rebuild_int(map);
    }
}

// ---------------------------------------------------------------
// collection creation
// ---------------------------------------------------------------

// create a list from an initializer array. copies the data.
static inline forge_list_t forge_list_create(int64_t len, int64_t elem_size, const void *init) {
    forge_list_t list;
    list.len = len;
    if (len == 0 || !init) {
        list.data = NULL;
        return list;
    }
    if (elem_size > 0 && (size_t)len > SIZE_MAX / (size_t)elem_size) {
        fprintf(stderr, "forge: list too large\n");
        exit(1);
    }
    list.data = malloc((size_t)(len * elem_size));
    if (!list.data) {
        fprintf(stderr, "forge: out of memory\n");
        exit(1);
    }
    memcpy(list.data, init, (size_t)(len * elem_size));
    return list;
}

// create a map from parallel key/value arrays. copies both and builds hash index.
// key_size is used to distinguish string keys (sizeof(forge_string_t)) from int keys.
static inline forge_map_t forge_map_create(int64_t len, int64_t key_size, int64_t val_size,
                                           const void *init_keys, const void *init_vals) {
    forge_map_t map;
    map.len = len;
    map.buckets = NULL;
    map.cap = 0;
    map._pad = 0;
    if (len == 0) {
        map.keys = NULL;
        map.values = NULL;
        return map;
    }
    if (key_size > 0 && len > (int64_t)(SIZE_MAX / (size_t)key_size)) {
        fprintf(stderr, "forge: map too large\n");
        exit(1);
    }
    if (val_size > 0 && len > (int64_t)(SIZE_MAX / (size_t)val_size)) {
        fprintf(stderr, "forge: map too large\n");
        exit(1);
    }
    map.keys = malloc((size_t)(len * key_size));
    map.values = malloc((size_t)(len * val_size));
    if (!map.keys || !map.values) {
        fprintf(stderr, "forge: out of memory\n");
        exit(1);
    }
    memcpy(map.keys, init_keys, (size_t)(len * key_size));
    memcpy(map.values, init_vals, (size_t)(len * val_size));
    // pick initial capacity: next power of 2 that keeps load < 75%
    int32_t needed = (int32_t)((len * 4 + 2) / 3); // ceil(len / 0.75)
    int32_t cap = 8;
    while (cap < needed) cap *= 2;
    map.cap = cap;
    map.buckets = forge_map_alloc_buckets(cap);
    if (key_size == (int64_t)sizeof(forge_string_t)) {
        forge_map_rebuild_string(&map);
    } else {
        forge_map_rebuild_int(&map);
    }
    return map;
}

// create a set (same as list — just unique elements).
static inline forge_set_t forge_set_create(int64_t len, int64_t elem_size, const void *init) {
    return forge_list_create(len, elem_size, init);
}

// ---------------------------------------------------------------
// collection access
// ---------------------------------------------------------------

// bounds check — exits with a clear error on out-of-range index
static inline void forge_bounds_check(int64_t idx, int64_t len) {
    if (idx < 0 || idx >= len) {
        fprintf(stderr, "forge: index out of bounds (index %" PRId64 ", length %" PRId64 ")\n", idx, len);
        exit(1);
    }
}

// typed element access for lists: FORGE_LIST_GET(list, int64_t, 0)
#define FORGE_LIST_GET(list, type, idx) \
    (forge_bounds_check((idx), (list).len), ((type *)(list).data)[(idx)])

// look up a value in a map by integer key. returns pointer to the value slot,
// or NULL if not found. O(1) average via hash probing.
static inline void *forge_map_get_by_int(forge_map_t map, int64_t key, int64_t val_size) {
    if (map.cap == 0) return NULL;
    int32_t mask = map.cap - 1;
    uint64_t h = forge_hash_int(key);
    int32_t slot = (int32_t)(h & (uint64_t)mask);
    int64_t *keys = (int64_t *)map.keys;
    while (1) {
        int32_t idx = map.buckets[slot];
        if (idx == -1) return NULL;
        if (keys[idx] == key) return (char *)map.values + idx * val_size;
        slot = (slot + 1) & mask;
    }
}

// look up a value in a map by string key. returns pointer to the value slot,
// or NULL if not found. O(1) average via hash probing.
static inline void *forge_map_get_by_string(forge_map_t map, forge_string_t key, int64_t val_size) {
    if (map.cap == 0) return NULL;
    int32_t mask = map.cap - 1;
    uint64_t h = forge_hash_string(key);
    int32_t slot = (int32_t)(h & (uint64_t)mask);
    forge_string_t *keys = (forge_string_t *)map.keys;
    while (1) {
        int32_t idx = map.buckets[slot];
        if (idx == -1) return NULL;
        if (forge_string_eq(keys[idx], key)) return (char *)map.values + idx * val_size;
        slot = (slot + 1) & mask;
    }
}

// checked dereference for map lookups — exits if key was not found
static inline void *forge_map_get_checked(void *ptr) {
    if (!ptr) {
        fprintf(stderr, "forge: map key not found\n");
        exit(1);
    }
    return ptr;
}

// ---------------------------------------------------------------
// collection mutation
// ---------------------------------------------------------------

// append an element to a list. grows the backing array via realloc.
static inline void forge_list_push(forge_list_t *list, const void *elem, int64_t elem_size) {
    int64_t new_len = list->len + 1;
    if (elem_size > 0 && (size_t)new_len > SIZE_MAX / (size_t)elem_size) {
        fprintf(stderr, "forge: list too large\n");
        exit(1);
    }
    void *new_data = realloc(list->data, (size_t)(new_len * elem_size));
    if (!new_data) {
        fprintf(stderr, "forge: out of memory\n");
        exit(1);
    }
    list->data = new_data;
    memcpy((char *)list->data + list->len * elem_size, elem, (size_t)elem_size);
    list->len = new_len;
}

// insert or update a key-value pair in a map (string keys).
// if the key already exists, updates the value in place.
// O(1) average via hash probing.
static inline void forge_map_set_by_string(forge_map_t *map, forge_string_t key,
                                            const void *val, int64_t key_size, int64_t val_size) {
    // check for existing key via hash probe
    if (map->cap > 0) {
        int32_t mask = map->cap - 1;
        uint64_t h = forge_hash_string(key);
        int32_t slot = (int32_t)(h & (uint64_t)mask);
        forge_string_t *keys = (forge_string_t *)map->keys;
        while (1) {
            int32_t idx = map->buckets[slot];
            if (idx == -1) break;
            if (forge_string_eq(keys[idx], key)) {
                memcpy((char *)map->values + idx * val_size, val, (size_t)val_size);
                return;
            }
            slot = (slot + 1) & mask;
        }
    }
    // ensure hash index has room
    forge_map_ensure_index_string(map);
    // grow dense arrays
    int64_t new_len = map->len + 1;
    void *new_keys = realloc(map->keys, (size_t)(new_len * key_size));
    if (!new_keys) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    map->keys = new_keys;
    void *new_vals = realloc(map->values, (size_t)(new_len * val_size));
    if (!new_vals) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    map->values = new_vals;
    memcpy((char *)map->keys + map->len * key_size, &key, (size_t)key_size);
    memcpy((char *)map->values + map->len * val_size, val, (size_t)val_size);
    // insert into hash index
    int32_t mask = map->cap - 1;
    uint64_t h = forge_hash_string(key);
    int32_t slot = (int32_t)(h & (uint64_t)mask);
    while (map->buckets[slot] != -1) slot = (slot + 1) & mask;
    map->buckets[slot] = (int32_t)map->len;
    map->len = new_len;
}

// insert or update a key-value pair in a map (integer keys).
// O(1) average via hash probing.
static inline void forge_map_set_by_int(forge_map_t *map, int64_t key,
                                         const void *val, int64_t key_size, int64_t val_size) {
    // check for existing key via hash probe
    if (map->cap > 0) {
        int32_t mask = map->cap - 1;
        uint64_t h = forge_hash_int(key);
        int32_t slot = (int32_t)(h & (uint64_t)mask);
        int64_t *keys = (int64_t *)map->keys;
        while (1) {
            int32_t idx = map->buckets[slot];
            if (idx == -1) break;
            if (keys[idx] == key) {
                memcpy((char *)map->values + idx * val_size, val, (size_t)val_size);
                return;
            }
            slot = (slot + 1) & mask;
        }
    }
    // ensure hash index has room
    forge_map_ensure_index_int(map);
    // grow dense arrays
    int64_t new_len = map->len + 1;
    void *new_keys = realloc(map->keys, (size_t)(new_len * key_size));
    if (!new_keys) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    map->keys = new_keys;
    void *new_vals = realloc(map->values, (size_t)(new_len * val_size));
    if (!new_vals) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    map->values = new_vals;
    memcpy((char *)map->keys + map->len * key_size, &key, (size_t)key_size);
    memcpy((char *)map->values + map->len * val_size, val, (size_t)val_size);
    // insert into hash index
    int32_t mask = map->cap - 1;
    uint64_t h = forge_hash_int(key);
    int32_t slot = (int32_t)(h & (uint64_t)mask);
    while (map->buckets[slot] != -1) slot = (slot + 1) & mask;
    map->buckets[slot] = (int32_t)map->len;
    map->len = new_len;
}

// add an element to a set (no-op if already present).
// uses linear scan for deduplication — fine for small sets.
static inline void forge_set_add(forge_set_t *set, const void *elem, int64_t elem_size) {
    // check if element already exists
    for (int64_t i = 0; i < set->len; i++) {
        if (memcmp((char *)set->data + i * elem_size, elem, (size_t)elem_size) == 0) {
            return; // already present
        }
    }
    forge_list_push(set, elem, elem_size);
}

// ---------------------------------------------------------------
// collection methods
// ---------------------------------------------------------------

// list — remove element at index
static inline void forge_list_remove(forge_list_t *list, int64_t idx, int64_t elem_size) {
    forge_bounds_check(idx, list->len);
    int64_t remaining = list->len - idx - 1;
    if (remaining > 0) {
        memmove((char *)list->data + idx * elem_size,
                (char *)list->data + (idx + 1) * elem_size,
                (size_t)(remaining * elem_size));
    }
    list->len--;
}

// list — linear scan for element (generic, uses memcmp)
static inline bool forge_list_contains(forge_list_t list, const void *elem, int64_t elem_size) {
    for (int64_t i = 0; i < list.len; i++) {
        if (memcmp((char *)list.data + i * elem_size, elem, (size_t)elem_size) == 0)
            return true;
    }
    return false;
}

// list — linear scan for string element
static inline bool forge_list_contains_string(forge_list_t list, forge_string_t s) {
    forge_string_t *items = (forge_string_t *)list.data;
    for (int64_t i = 0; i < list.len; i++) {
        if (forge_string_eq(items[i], s)) return true;
    }
    return false;
}

// list — reverse in place
static inline void forge_list_reverse(forge_list_t *list, int64_t elem_size) {
    if (list->len < 2) return;
    // use stack buffer for small elements, heap for large ones
    char stack_buf[64];
    char *tmp = (elem_size <= 64) ? stack_buf : (char *)malloc((size_t)elem_size);
    if (!tmp) return;
    for (int64_t i = 0; i < list->len / 2; i++) {
        int64_t j = list->len - 1 - i;
        char *a = (char *)list->data + i * elem_size;
        char *b = (char *)list->data + j * elem_size;
        memcpy(tmp, a, (size_t)elem_size);
        memcpy(a, b, (size_t)elem_size);
        memcpy(b, tmp, (size_t)elem_size);
    }
    if (tmp != stack_buf) free(tmp);
}

// list — clear (free data and reset)
static inline void forge_list_clear(forge_list_t *list) {
    free(list->data);
    list->data = NULL;
    list->len = 0;
}

// map — remove by string key. shifts dense arrays and rebuilds hash index.
static inline void forge_map_remove_by_string(forge_map_t *map, forge_string_t key,
                                                int64_t key_size, int64_t val_size) {
    if (map->cap == 0) return;
    // find via hash probe
    int32_t mask = map->cap - 1;
    uint64_t h = forge_hash_string(key);
    int32_t slot = (int32_t)(h & (uint64_t)mask);
    forge_string_t *keys = (forge_string_t *)map->keys;
    while (1) {
        int32_t idx = map->buckets[slot];
        if (idx == -1) return; // not found
        if (forge_string_eq(keys[idx], key)) {
            // shift dense arrays to preserve insertion order
            int64_t remaining = map->len - idx - 1;
            if (remaining > 0) {
                memmove((char *)map->keys + idx * key_size,
                        (char *)map->keys + (idx + 1) * key_size,
                        (size_t)(remaining * key_size));
                memmove((char *)map->values + idx * val_size,
                        (char *)map->values + (idx + 1) * val_size,
                        (size_t)(remaining * val_size));
            }
            map->len--;
            forge_map_rebuild_string(map);
            return;
        }
        slot = (slot + 1) & mask;
    }
}

// map — remove by integer key. shifts dense arrays and rebuilds hash index.
static inline void forge_map_remove_by_int(forge_map_t *map, int64_t key,
                                            int64_t key_size, int64_t val_size) {
    if (map->cap == 0) return;
    int32_t mask = map->cap - 1;
    uint64_t h = forge_hash_int(key);
    int32_t slot = (int32_t)(h & (uint64_t)mask);
    int64_t *keys = (int64_t *)map->keys;
    while (1) {
        int32_t idx = map->buckets[slot];
        if (idx == -1) return; // not found
        if (keys[idx] == key) {
            int64_t remaining = map->len - idx - 1;
            if (remaining > 0) {
                memmove((char *)map->keys + idx * key_size,
                        (char *)map->keys + (idx + 1) * key_size,
                        (size_t)(remaining * key_size));
                memmove((char *)map->values + idx * val_size,
                        (char *)map->values + (idx + 1) * val_size,
                        (size_t)(remaining * val_size));
            }
            map->len--;
            forge_map_rebuild_int(map);
            return;
        }
        slot = (slot + 1) & mask;
    }
}

// map — check key existence (string keys). O(1) average.
static inline bool forge_map_contains_key_string(forge_map_t map, forge_string_t key) {
    if (map.cap == 0) return false;
    int32_t mask = map.cap - 1;
    uint64_t h = forge_hash_string(key);
    int32_t slot = (int32_t)(h & (uint64_t)mask);
    forge_string_t *keys = (forge_string_t *)map.keys;
    while (1) {
        int32_t idx = map.buckets[slot];
        if (idx == -1) return false;
        if (forge_string_eq(keys[idx], key)) return true;
        slot = (slot + 1) & mask;
    }
}

// map — check key existence (integer keys). O(1) average.
static inline bool forge_map_contains_key_int(forge_map_t map, int64_t key) {
    if (map.cap == 0) return false;
    int32_t mask = map.cap - 1;
    uint64_t h = forge_hash_int(key);
    int32_t slot = (int32_t)(h & (uint64_t)mask);
    int64_t *keys = (int64_t *)map.keys;
    while (1) {
        int32_t idx = map.buckets[slot];
        if (idx == -1) return false;
        if (keys[idx] == key) return true;
        slot = (slot + 1) & mask;
    }
}

// map — get all keys as a list
static inline forge_list_t forge_map_keys(forge_map_t map, int64_t key_size) {
    forge_list_t result;
    result.len = map.len;
    if (map.len == 0) {
        result.data = NULL;
        return result;
    }
    result.data = malloc((size_t)(map.len * key_size));
    if (!result.data) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    memcpy(result.data, map.keys, (size_t)(map.len * key_size));
    return result;
}

// map — get all values as a list
static inline forge_list_t forge_map_values(forge_map_t map, int64_t val_size) {
    forge_list_t result;
    result.len = map.len;
    if (map.len == 0) {
        result.data = NULL;
        return result;
    }
    result.data = malloc((size_t)(map.len * val_size));
    if (!result.data) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    memcpy(result.data, map.values, (size_t)(map.len * val_size));
    return result;
}

// map — clear (free data, hash index, and reset)
static inline void forge_map_clear(forge_map_t *map) {
    free(map->keys);
    free(map->values);
    free(map->buckets);
    map->keys = NULL;
    map->values = NULL;
    map->buckets = NULL;
    map->len = 0;
    map->cap = 0;
}

// set — remove by generic element
static inline void forge_set_remove(forge_set_t *set, const void *elem, int64_t elem_size) {
    for (int64_t i = 0; i < set->len; i++) {
        if (memcmp((char *)set->data + i * elem_size, elem, (size_t)elem_size) == 0) {
            forge_list_remove(set, i, elem_size);
            return;
        }
    }
}

// set — remove by string element
static inline void forge_set_remove_string(forge_set_t *set, forge_string_t s) {
    forge_string_t *items = (forge_string_t *)set->data;
    for (int64_t i = 0; i < set->len; i++) {
        if (forge_string_eq(items[i], s)) {
            forge_list_remove(set, i, sizeof(forge_string_t));
            return;
        }
    }
}

// set — check membership (generic, uses memcmp)
static inline bool forge_set_contains(forge_set_t set, const void *elem, int64_t elem_size) {
    return forge_list_contains(set, elem, elem_size);
}

// set — check membership (string element)
static inline bool forge_set_contains_string(forge_set_t set, forge_string_t s) {
    return forge_list_contains_string(set, s);
}

// set — clear (same as list clear)
static inline void forge_set_clear(forge_set_t *set) {
    forge_list_clear(set);
}

// ---------------------------------------------------------------
// string split (after collections, since it returns forge_list_t)
// ---------------------------------------------------------------

static inline forge_list_t forge_string_split(forge_string_t s, forge_string_t sep) {
    forge_list_t result = { .data = NULL, .len = 0 };
    if (sep.len == 0) {
        // split on empty separator: return each character
        for (int64_t i = 0; i < s.len; i++) {
            char *ch = (char *)malloc(2);
            if (!ch) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
            ch[0] = s.data[i];
            ch[1] = '\0';
            forge_string_t part = { .data = ch, .len = 1 };
            forge_list_push(&result, &part, sizeof(forge_string_t));
        }
        return result;
    }
    int64_t start = 0;
    for (int64_t i = 0; i + sep.len <= s.len; i++) {
        if (memcmp(s.data + i, sep.data, (size_t)sep.len) == 0) {
            int64_t part_len = i - start;
            char *buf = (char *)malloc((size_t)part_len + 1);
            if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
            memcpy(buf, s.data + start, (size_t)part_len);
            buf[part_len] = '\0';
            forge_string_t part = { .data = buf, .len = part_len };
            forge_list_push(&result, &part, sizeof(forge_string_t));
            i += sep.len - 1;
            start = i + 1;
        }
    }
    // remaining part after last separator
    int64_t part_len = s.len - start;
    char *buf = (char *)malloc((size_t)part_len + 1);
    if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    memcpy(buf, s.data + start, (size_t)part_len);
    buf[part_len] = '\0';
    forge_string_t part = { .data = buf, .len = part_len };
    forge_list_push(&result, &part, sizeof(forge_string_t));
    return result;
}

// join a List[String] with a separator. returns a new string.
static inline forge_string_t forge_list_join(forge_list_t list, forge_string_t sep) {
    if (list.len == 0) return forge_string_empty;
    forge_string_t *items = (forge_string_t *)list.data;
    if (list.len == 1) {
        char *buf = (char *)malloc((size_t)items[0].len + 1);
        if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
        memcpy(buf, items[0].data, (size_t)items[0].len);
        buf[items[0].len] = '\0';
        return (forge_string_t){ .data = buf, .len = items[0].len };
    }
    // compute total length
    int64_t total = 0;
    for (int64_t i = 0; i < list.len; i++) {
        total += items[i].len;
    }
    total += (list.len - 1) * sep.len;
    char *buf = (char *)malloc((size_t)total + 1);
    if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    int64_t pos = 0;
    for (int64_t i = 0; i < list.len; i++) {
        if (i > 0) {
            memcpy(buf + pos, sep.data, (size_t)sep.len);
            pos += sep.len;
        }
        memcpy(buf + pos, items[i].data, (size_t)items[i].len);
        pos += items[i].len;
    }
    buf[total] = '\0';
    return (forge_string_t){ .data = buf, .len = total };
}

// ---------------------------------------------------------------
// command-line arguments
// ---------------------------------------------------------------

static int forge_argc = 0;
static char **forge_argv = NULL;

static inline void forge_set_args(int argc, char **argv) {
    forge_argc = argc;
    forge_argv = argv;
}

static inline forge_list_t forge_get_args(void) {
    forge_list_t result = { .data = NULL, .len = 0 };
    for (int i = 0; i < forge_argc; i++) {
        forge_string_t s = forge_string_from(forge_argv[i], (int64_t)strlen(forge_argv[i]));
        forge_list_push(&result, &s, sizeof(forge_string_t));
    }
    return result;
}

// ---------------------------------------------------------------
// file I/O helpers
// ---------------------------------------------------------------

// read an entire file into a string. returns false on error.
static inline bool forge_read_file_impl(const char *path_data, int64_t path_len,
                                         forge_string_t *out) {
    // null-terminate path for fopen
    char *path = (char *)malloc((size_t)path_len + 1);
    if (!path) return false;
    memcpy(path, path_data, (size_t)path_len);
    path[path_len] = '\0';

    FILE *f = fopen(path, "rb");
    free(path);
    if (!f) return false;

    fseek(f, 0, SEEK_END);
    long file_len = ftell(f);
    fseek(f, 0, SEEK_SET);

    if (file_len < 0) { fclose(f); return false; }

    char *buf = (char *)malloc((size_t)file_len + 1);
    if (!buf) { fclose(f); return false; }

    size_t read = fread(buf, 1, (size_t)file_len, f);
    fclose(f);

    buf[read] = '\0';
    out->data = buf;
    out->len = (int64_t)read;
    return true;
}

// write a string to a file. returns false on error.
static inline bool forge_write_file_impl(const char *path_data, int64_t path_len,
                                          const char *data, int64_t data_len) {
    char *path = (char *)malloc((size_t)path_len + 1);
    if (!path) return false;
    memcpy(path, path_data, (size_t)path_len);
    path[path_len] = '\0';

    FILE *f = fopen(path, "wb");
    free(path);
    if (!f) return false;

    size_t written = fwrite(data, 1, (size_t)data_len, f);
    fclose(f);
    return written == (size_t)data_len;
}

// environment variable lookup. returns false if not set.
static inline bool forge_env_impl(const char *name_data, int64_t name_len,
                                   forge_string_t *out) {
    char *name = (char *)malloc((size_t)name_len + 1);
    if (!name) return false;
    memcpy(name, name_data, (size_t)name_len);
    name[name_len] = '\0';

    const char *val = getenv(name);
    free(name);
    if (!val) return false;

    // copy the value — getenv() returns process-owned memory that can
    // be invalidated by subsequent setenv/putenv calls.
    size_t val_len = strlen(val);
    char *copy = (char *)malloc(val_len + 1);
    if (!copy) return false;
    memcpy(copy, val, val_len + 1);

    out->data = copy;
    out->len = (int64_t)val_len;
    return true;
}

// ---------------------------------------------------------------
// built-in functions
// ---------------------------------------------------------------

static inline void forge_print(forge_string_t s) {
    fwrite(s.data, 1, (size_t)s.len, stdout);
    fputc('\n', stdout);
}

static inline int64_t forge_exec(forge_string_t cmd) {
    char *cstr = (char *)malloc((size_t)cmd.len + 1);
    if (!cstr) return -1;
    memcpy(cstr, cmd.data, (size_t)cmd.len);
    cstr[cmd.len] = '\0';
    int result = system(cstr);
    free(cstr);
#ifdef _WIN32
    return (int64_t)result;
#else
    return (int64_t)WEXITSTATUS(result);
#endif
}

#endif // FORGE_RUNTIME_H
