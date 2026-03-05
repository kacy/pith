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
#include <time.h>
#include <pthread.h>
#include <unistd.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <netdb.h>
#include <errno.h>
#include <sys/wait.h>
#include <signal.h>

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
// string helpers — reduce malloc+memcpy+null-terminate boilerplate
// ---------------------------------------------------------------

// allocate a string buffer of the given length (+1 for null).
// exits on OOM — safe to use without checking the return value.
static inline char *forge_str_alloc(int64_t len) {
    char *buf = (char *)malloc((size_t)len + 1);
    if (!buf) {
        fprintf(stderr, "forge: out of memory\n");
        exit(1);
    }
    return buf;
}

// convert a forge_string_t to a null-terminated C string.
// the caller is responsible for freeing the returned pointer.
static inline char *forge_cstr(forge_string_t s) {
    char *buf = forge_str_alloc(s.len);
    memcpy(buf, s.data, (size_t)s.len);
    buf[s.len] = '\0';
    return buf;
}

// ---------------------------------------------------------------
// string operations
// ---------------------------------------------------------------

static inline forge_string_t forge_string_concat(forge_string_t a, forge_string_t b) {
    if (a.len > INT64_MAX - b.len) {
        fprintf(stderr, "forge: string too large\n");
        exit(1);
    }
    int64_t new_len = a.len + b.len;
    char *buf = forge_str_alloc(new_len);
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
    char *buf = forge_str_alloc(s.len);
    for (int64_t i = 0; i < s.len; i++) {
        char c = s.data[i];
        buf[i] = (c >= 'a' && c <= 'z') ? (char)(c - 32) : c;
    }
    buf[s.len] = '\0';
    return (forge_string_t){ .data = buf, .len = s.len };
}

static inline forge_string_t forge_string_to_lower(forge_string_t s) {
    char *buf = forge_str_alloc(s.len);
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
    char *buf = forge_str_alloc(new_len);
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
    char *buf = forge_str_alloc(1);
    buf[0] = s.data[index];
    buf[1] = '\0';
    return (forge_string_t){ .data = buf, .len = 1 };
}

// chr(Int) -> String: return a single-character string for the given ASCII code.
static inline forge_string_t forge_chr(int64_t code) {
    char *buf = forge_str_alloc(1);
    buf[0] = (char)(code & 0xFF);
    buf[1] = '\0';
    return (forge_string_t){ .data = buf, .len = 1 };
}

// replace all occurrences of `old` with `new_s` in `s`.
static inline forge_string_t forge_string_replace(forge_string_t s, forge_string_t old, forge_string_t new_s) {
    if (old.len == 0) {
        // empty pattern — return a copy
        char *buf = forge_str_alloc(s.len);
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
        char *buf = forge_str_alloc(s.len);
        memcpy(buf, s.data, (size_t)s.len);
        buf[s.len] = '\0';
        return (forge_string_t){ .data = buf, .len = s.len };
    }
    // second pass: build result
    int64_t new_len = s.len + count * (new_s.len - old.len);
    char *buf = forge_str_alloc(new_len);
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

// index_of: find first occurrence of needle in haystack. returns -1 if not found.
static inline int64_t forge_string_index_of(forge_string_t haystack, forge_string_t needle) {
    if (needle.len == 0) return 0;
    if (needle.len > haystack.len) return -1;
    for (int64_t i = 0; i <= haystack.len - needle.len; i++) {
        if (memcmp(haystack.data + i, needle.data, (size_t)needle.len) == 0)
            return i;
    }
    return -1;
}

// last_index_of: find last occurrence of needle in haystack. returns -1 if not found.
static inline int64_t forge_string_last_index_of(forge_string_t haystack, forge_string_t needle) {
    if (needle.len == 0) return haystack.len;
    if (needle.len > haystack.len) return -1;
    for (int64_t i = haystack.len - needle.len; i >= 0; i--) {
        if (memcmp(haystack.data + i, needle.data, (size_t)needle.len) == 0)
            return i;
    }
    return -1;
}

// repeat: repeat string n times.
static inline forge_string_t forge_string_repeat(forge_string_t s, int64_t n) {
    if (n <= 0 || s.len == 0) return forge_string_empty;
    int64_t new_len = s.len * n;
    char *buf = (char *)malloc((size_t)new_len + 1);
    if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    for (int64_t i = 0; i < n; i++) {
        memcpy(buf + i * s.len, s.data, (size_t)s.len);
    }
    buf[new_len] = '\0';
    return (forge_string_t){ .data = buf, .len = new_len };
}

// pad_left: pad string to given width with fill character (left-padded).
static inline forge_string_t forge_string_pad_left(forge_string_t s, int64_t width, forge_string_t fill) {
    if (s.len >= width || fill.len == 0) {
        char *buf = (char *)malloc((size_t)s.len + 1);
        if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
        memcpy(buf, s.data, (size_t)s.len);
        buf[s.len] = '\0';
        return (forge_string_t){ .data = buf, .len = s.len };
    }
    int64_t pad_len = width - s.len;
    char *buf = (char *)malloc((size_t)width + 1);
    if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    for (int64_t i = 0; i < pad_len; i++) {
        buf[i] = fill.data[i % fill.len];
    }
    memcpy(buf + pad_len, s.data, (size_t)s.len);
    buf[width] = '\0';
    return (forge_string_t){ .data = buf, .len = width };
}

// pad_right: pad string to given width with fill character (right-padded).
static inline forge_string_t forge_string_pad_right(forge_string_t s, int64_t width, forge_string_t fill) {
    if (s.len >= width || fill.len == 0) {
        char *buf = (char *)malloc((size_t)s.len + 1);
        if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
        memcpy(buf, s.data, (size_t)s.len);
        buf[s.len] = '\0';
        return (forge_string_t){ .data = buf, .len = s.len };
    }
    int64_t pad_len = width - s.len;
    char *buf = (char *)malloc((size_t)width + 1);
    if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    memcpy(buf, s.data, (size_t)s.len);
    for (int64_t i = 0; i < pad_len; i++) {
        buf[s.len + i] = fill.data[i % fill.len];
    }
    buf[width] = '\0';
    return (forge_string_t){ .data = buf, .len = width };
}

// split uses a forward-declared list type — defined after collection types
// (see forge_string_split below and forge_string_chars below)

// ---------------------------------------------------------------
// conversions to string
// ---------------------------------------------------------------

static inline forge_string_t forge_int_to_string(int64_t n) {
    char buf[32];
    int len = snprintf(buf, sizeof(buf), "%" PRId64, n);
    char *result = forge_str_alloc(len);
    memcpy(result, buf, (size_t)len + 1);
    return (forge_string_t){ .data = result, .len = len };
}

static inline forge_string_t forge_float_to_string(double n) {
    char buf[64];
    int len = snprintf(buf, sizeof(buf), "%g", n);
    char *result = forge_str_alloc(len);
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

// string — chars(): split into a list of single-character strings.
static inline forge_list_t forge_string_chars(forge_string_t s) {
    forge_list_t result = { .data = NULL, .len = 0 };
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

// ---------------------------------------------------------------
// list — index_of, slice, sort
// ---------------------------------------------------------------

// list — find first occurrence of element. returns -1 if not found.
static inline int64_t forge_list_index_of(forge_list_t list, const void *elem, int64_t elem_size) {
    for (int64_t i = 0; i < list.len; i++) {
        if (memcmp((char *)list.data + i * elem_size, elem, (size_t)elem_size) == 0)
            return i;
    }
    return -1;
}

// list — find first occurrence of string element. returns -1 if not found.
static inline int64_t forge_list_index_of_string(forge_list_t list, forge_string_t s) {
    forge_string_t *items = (forge_string_t *)list.data;
    for (int64_t i = 0; i < list.len; i++) {
        if (forge_string_eq(items[i], s)) return i;
    }
    return -1;
}

// list — slice: return a new list from start to end (exclusive).
static inline forge_list_t forge_list_slice(forge_list_t list, int64_t start, int64_t end, int64_t elem_size) {
    if (start < 0) start = 0;
    if (end > list.len) end = list.len;
    if (start >= end) return (forge_list_t){ .data = NULL, .len = 0 };
    int64_t new_len = end - start;
    int64_t total = new_len * elem_size;
    void *buf = malloc((size_t)total);
    if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    memcpy(buf, (char *)list.data + start * elem_size, (size_t)total);
    return (forge_list_t){ .data = buf, .len = new_len };
}

// qsort comparators for sort
static int forge_cmp_int(const void *a, const void *b) {
    int64_t va = *(const int64_t *)a;
    int64_t vb = *(const int64_t *)b;
    return (va > vb) - (va < vb);
}

static int forge_cmp_float(const void *a, const void *b) {
    double va = *(const double *)a;
    double vb = *(const double *)b;
    return (va > vb) - (va < vb);
}

static int forge_cmp_string(const void *a, const void *b) {
    const forge_string_t *sa = (const forge_string_t *)a;
    const forge_string_t *sb = (const forge_string_t *)b;
    int64_t min_len = sa->len < sb->len ? sa->len : sb->len;
    int cmp = memcmp(sa->data, sb->data, (size_t)min_len);
    if (cmp != 0) return cmp;
    return (sa->len > sb->len) - (sa->len < sb->len);
}

// list — sort: return a new sorted copy. type_tag: 0=int, 1=float, 2=string.
static inline forge_list_t forge_list_sort(forge_list_t list, int64_t elem_size, int type_tag) {
    if (list.len <= 1) {
        forge_list_t copy = { .data = NULL, .len = list.len };
        if (list.len == 1) {
            copy.data = malloc((size_t)elem_size);
            if (!copy.data) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
            memcpy(copy.data, list.data, (size_t)elem_size);
        }
        return copy;
    }
    int64_t total = list.len * elem_size;
    void *buf = malloc((size_t)total);
    if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    memcpy(buf, list.data, (size_t)total);
    int (*cmp)(const void *, const void *) = NULL;
    if (type_tag == 0) cmp = forge_cmp_int;
    else if (type_tag == 1) cmp = forge_cmp_float;
    else cmp = forge_cmp_string;
    qsort(buf, (size_t)list.len, (size_t)elem_size, cmp);
    return (forge_list_t){ .data = buf, .len = list.len };
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
    char *path = forge_cstr((forge_string_t){ .data = path_data, .len = path_len });
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
    char *path = forge_cstr((forge_string_t){ .data = path_data, .len = path_len });
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
    char *name = forge_cstr((forge_string_t){ .data = name_data, .len = name_len });
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
    char *cstr = forge_cstr(cmd);
    int result = system(cstr);
    free(cstr);
#ifdef _WIN32
    return (int64_t)result;
#else
    return (int64_t)WEXITSTATUS(result);
#endif
}

// ---------------------------------------------------------------
// time, sleep, random, exec_output, input
// ---------------------------------------------------------------

// time() -> epoch milliseconds
static inline int64_t forge_time(void) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    return (int64_t)ts.tv_sec * 1000 + (int64_t)(ts.tv_nsec / 1000000);
}

// sleep(ms) -> Void
static inline void forge_sleep(int64_t ms) {
    struct timespec ts;
    ts.tv_sec = ms / 1000;
    ts.tv_nsec = (ms % 1000) * 1000000;
    nanosleep(&ts, NULL);
}

// random — lazy seed on first call
static int __forge_rng_seeded = 0;
static inline void __forge_seed_rng(void) {
    if (!__forge_rng_seeded) {
        srand48((long)time(NULL));
        __forge_rng_seeded = 1;
    }
}

// random_int(min, max) -> Int (inclusive range)
static inline int64_t forge_random_int(int64_t min, int64_t max) {
    __forge_seed_rng();
    if (min >= max) return min;
    int64_t range = max - min + 1;
    return min + (lrand48() % range);
}

// random_float() -> Float in [0.0, 1.0)
static inline double forge_random_float(void) {
    __forge_seed_rng();
    return drand48();
}

// random_seed(Int) -> Void — set RNG seed for reproducible output
static inline void forge_random_seed(int64_t seed) {
    srand48((long)seed);
    __forge_rng_seeded = 1;
}

// random_string(Int) -> String — N random alphanumeric chars
static inline forge_string_t forge_random_string(int64_t n) {
    static const char alphanum[] = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    __forge_seed_rng();
    if (n < 0) n = 0;
    char *buf = forge_str_alloc(n);
    for (int64_t i = 0; i < n; i++)
        buf[i] = alphanum[lrand48() % 62];
    buf[n] = '\0';
    return (forge_string_t){ .data = buf, .len = n };
}

// format_time(epoch_ms, format_string) -> String — strftime wrapper
static inline forge_string_t forge_format_time(int64_t epoch_ms, forge_string_t fmt) {
    time_t secs = (time_t)(epoch_ms / 1000);
    struct tm *t = localtime(&secs);
    char *fmt_cstr = forge_cstr(fmt);
    char buf[256];
    size_t len = strftime(buf, sizeof(buf), fmt_cstr, t);
    free(fmt_cstr);
    char *result = forge_str_alloc((int64_t)len);
    memcpy(result, buf, len);
    result[len] = '\0';
    return (forge_string_t){ .data = result, .len = (int64_t)len };
}

// exec_output — internal impl, returns false on error.
// codegen emits a wrapper that returns forge_result_forge_string_t.
static inline bool forge_exec_output_impl(forge_string_t cmd, forge_string_t *out) {
    char *cstr = forge_cstr(cmd);
    FILE *fp = popen(cstr, "r");
    free(cstr);
    if (!fp) return false;

    // read all output into a dynamic buffer
    int64_t cap = 1024;
    int64_t len = 0;
    char *buf = (char *)malloc((size_t)cap);
    if (!buf) { pclose(fp); return false; }

    while (1) {
        size_t n = fread(buf + len, 1, (size_t)(cap - len), fp);
        if (n == 0) break;
        len += (int64_t)n;
        if (len >= cap) {
            cap *= 2;
            char *newbuf = (char *)realloc(buf, (size_t)cap);
            if (!newbuf) { free(buf); pclose(fp); return false; }
            buf = newbuf;
        }
    }
    pclose(fp);

    // trim trailing newline (like shell $() does)
    if (len > 0 && buf[len - 1] == '\n') len--;

    buf[len] = '\0';
    out->data = buf;
    out->len = len;
    return true;
}

// input() -> String — read a line from stdin
static inline forge_string_t forge_input(void) {
    char buf[4096];
    if (!fgets(buf, sizeof(buf), stdin)) {
        return forge_string_from("", 0);
    }
    int64_t len = (int64_t)strlen(buf);
    if (len > 0 && buf[len - 1] == '\n') len--;
    char *copy = (char *)malloc((size_t)len + 1);
    if (!copy) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    memcpy(copy, buf, (size_t)len);
    copy[len] = '\0';
    return (forge_string_t){ .data = copy, .len = len };
}

// ---------------------------------------------------------------
// math functions
// ---------------------------------------------------------------

#include <math.h>

static inline double forge_math_pow(double base, double exp) {
    return pow(base, exp);
}

static inline double forge_math_sqrt(double x) {
    return sqrt(x);
}

static inline int64_t forge_math_floor(double x) {
    return (int64_t)floor(x);
}

static inline int64_t forge_math_ceil(double x) {
    return (int64_t)ceil(x);
}

static inline int64_t forge_math_round(double x) {
    return (int64_t)round(x);
}

// ---------------------------------------------------------------
// formatting functions
// ---------------------------------------------------------------

// fmt_hex(Int) -> String: format integer as hexadecimal
static inline forge_string_t forge_fmt_hex(int64_t n) {
    char buf[32];
    int len = snprintf(buf, sizeof(buf), "%" PRIx64, (uint64_t)n);
    char *result = (char *)malloc((size_t)len + 1);
    if (!result) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    memcpy(result, buf, (size_t)len + 1);
    return (forge_string_t){ .data = result, .len = len };
}

// fmt_oct(Int) -> String: format integer as octal
static inline forge_string_t forge_fmt_oct(int64_t n) {
    char buf[32];
    int len = snprintf(buf, sizeof(buf), "%" PRIo64, (uint64_t)n);
    char *result = (char *)malloc((size_t)len + 1);
    if (!result) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    memcpy(result, buf, (size_t)len + 1);
    return (forge_string_t){ .data = result, .len = len };
}

// fmt_bin(Int) -> String: format integer as binary
static inline forge_string_t forge_fmt_bin(int64_t n) {
    if (n == 0) {
        char *z = (char *)malloc(2);
        if (!z) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
        z[0] = '0'; z[1] = '\0';
        return (forge_string_t){ .data = z, .len = 1 };
    }
    char buf[65];
    int pos = 64;
    buf[pos] = '\0';
    uint64_t v = (uint64_t)n;
    while (v > 0) {
        buf[--pos] = '0' + (char)(v & 1);
        v >>= 1;
    }
    int len = 64 - pos;
    char *result = (char *)malloc((size_t)len + 1);
    if (!result) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    memcpy(result, buf + pos, (size_t)len + 1);
    return (forge_string_t){ .data = result, .len = len };
}

// fmt_float(Float, Int) -> String: format float with fixed decimal places
static inline forge_string_t forge_fmt_float(double n, int64_t decimals) {
    char buf[64];
    int len = snprintf(buf, sizeof(buf), "%.*f", (int)decimals, n);
    char *result = (char *)malloc((size_t)len + 1);
    if (!result) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    memcpy(result, buf, (size_t)len + 1);
    return (forge_string_t){ .data = result, .len = len };
}

// ---------------------------------------------------------------
// JSON — opaque handle-based API
// ---------------------------------------------------------------

typedef enum {
    FORGE_JSON_NULL,
    FORGE_JSON_BOOL,
    FORGE_JSON_INT,
    FORGE_JSON_FLOAT,
    FORGE_JSON_STRING,
    FORGE_JSON_ARRAY,
    FORGE_JSON_OBJECT
} forge_json_type_t;

typedef struct {
    forge_json_type_t type;
    union {
        bool bool_val;
        int64_t int_val;
        double float_val;
        forge_string_t string_val;
        struct { int64_t *items; int64_t len; } array_val;
        struct { forge_string_t *keys; int64_t *vals; int64_t len; } object_val;
    };
} forge_json_node_t;

// global node pool
static forge_json_node_t *forge_json_pool = NULL;
static int64_t forge_json_pool_len = 0;
static int64_t forge_json_pool_cap = 0;

static inline int64_t forge_json_alloc(void) {
    if (forge_json_pool_len >= forge_json_pool_cap) {
        int64_t new_cap = forge_json_pool_cap == 0 ? 64 : forge_json_pool_cap * 2;
        forge_json_node_t *new_pool = (forge_json_node_t *)realloc(
            forge_json_pool, (size_t)new_cap * sizeof(forge_json_node_t));
        if (!new_pool) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
        forge_json_pool = new_pool;
        forge_json_pool_cap = new_cap;
    }
    int64_t idx = forge_json_pool_len++;
    memset(&forge_json_pool[idx], 0, sizeof(forge_json_node_t));
    return idx;
}

// --- json parser ---

typedef struct {
    const char *data;
    int64_t len;
    int64_t pos;
    bool error;
} forge_json_parser_t;

static inline void forge_json_skip_ws(forge_json_parser_t *p) {
    while (p->pos < p->len) {
        char c = p->data[p->pos];
        if (c == ' ' || c == '\t' || c == '\n' || c == '\r') p->pos++;
        else break;
    }
}

static inline char forge_json_peek(forge_json_parser_t *p) {
    forge_json_skip_ws(p);
    if (p->pos >= p->len) return '\0';
    return p->data[p->pos];
}

static inline bool forge_json_match(forge_json_parser_t *p, const char *s, int64_t slen) {
    if (p->pos + slen > p->len) return false;
    if (memcmp(p->data + p->pos, s, (size_t)slen) != 0) return false;
    p->pos += slen;
    return true;
}

// forward declarations
static int64_t forge_json_parse_value(forge_json_parser_t *p);
static inline void forge_json_array_push(int64_t handle, int64_t val);

static inline forge_string_t forge_json_parse_string_raw(forge_json_parser_t *p) {
    if (p->data[p->pos] != '"') { p->error = true; return forge_string_empty; }
    p->pos++; // skip opening quote
    // first pass: compute length
    int64_t start = p->pos;
    int64_t escaped_len = 0;
    while (p->pos < p->len && p->data[p->pos] != '"') {
        if (p->data[p->pos] == '\\') { p->pos++; escaped_len++; }
        p->pos++;
        escaped_len++;
    }
    if (p->pos >= p->len) { p->error = true; return forge_string_empty; }
    // build the string with escape handling
    char *buf = (char *)malloc((size_t)escaped_len + 1);
    if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    int64_t out = 0;
    int64_t i = start;
    while (i < p->pos) {
        if (p->data[i] == '\\') {
            i++;
            if (i < p->pos) {
                switch (p->data[i]) {
                    case '"': buf[out++] = '"'; break;
                    case '\\': buf[out++] = '\\'; break;
                    case '/': buf[out++] = '/'; break;
                    case 'n': buf[out++] = '\n'; break;
                    case 'r': buf[out++] = '\r'; break;
                    case 't': buf[out++] = '\t'; break;
                    case 'b': buf[out++] = '\b'; break;
                    case 'f': buf[out++] = '\f'; break;
                    default: buf[out++] = p->data[i]; break;
                }
                i++;
            }
        } else {
            buf[out++] = p->data[i++];
        }
    }
    buf[out] = '\0';
    p->pos++; // skip closing quote
    return (forge_string_t){ .data = buf, .len = out };
}

static inline int64_t forge_json_parse_number(forge_json_parser_t *p) {
    int64_t start = p->pos;
    bool is_float = false;
    if (p->data[p->pos] == '-') p->pos++;
    while (p->pos < p->len && p->data[p->pos] >= '0' && p->data[p->pos] <= '9') p->pos++;
    if (p->pos < p->len && p->data[p->pos] == '.') {
        is_float = true;
        p->pos++;
        while (p->pos < p->len && p->data[p->pos] >= '0' && p->data[p->pos] <= '9') p->pos++;
    }
    if (p->pos < p->len && (p->data[p->pos] == 'e' || p->data[p->pos] == 'E')) {
        is_float = true;
        p->pos++;
        if (p->pos < p->len && (p->data[p->pos] == '+' || p->data[p->pos] == '-')) p->pos++;
        while (p->pos < p->len && p->data[p->pos] >= '0' && p->data[p->pos] <= '9') p->pos++;
    }
    // extract the number string
    int64_t nlen = p->pos - start;
    char tmp[64];
    if (nlen >= 64) nlen = 63;
    memcpy(tmp, p->data + start, (size_t)nlen);
    tmp[nlen] = '\0';

    int64_t idx = forge_json_alloc();
    if (is_float) {
        forge_json_pool[idx].type = FORGE_JSON_FLOAT;
        forge_json_pool[idx].float_val = strtod(tmp, NULL);
    } else {
        forge_json_pool[idx].type = FORGE_JSON_INT;
        forge_json_pool[idx].int_val = strtoll(tmp, NULL, 10);
    }
    return idx;
}

static int64_t forge_json_parse_value(forge_json_parser_t *p) {
    if (p->error) return -1;
    char c = forge_json_peek(p);
    if (c == '\0') { p->error = true; return -1; }

    // string
    if (c == '"') {
        forge_string_t s = forge_json_parse_string_raw(p);
        if (p->error) return -1;
        int64_t idx = forge_json_alloc();
        forge_json_pool[idx].type = FORGE_JSON_STRING;
        forge_json_pool[idx].string_val = s;
        return idx;
    }
    // number
    if (c == '-' || (c >= '0' && c <= '9')) {
        return forge_json_parse_number(p);
    }
    // true
    if (c == 't') {
        if (!forge_json_match(p, "true", 4)) { p->error = true; return -1; }
        int64_t idx = forge_json_alloc();
        forge_json_pool[idx].type = FORGE_JSON_BOOL;
        forge_json_pool[idx].bool_val = true;
        return idx;
    }
    // false
    if (c == 'f') {
        if (!forge_json_match(p, "false", 5)) { p->error = true; return -1; }
        int64_t idx = forge_json_alloc();
        forge_json_pool[idx].type = FORGE_JSON_BOOL;
        forge_json_pool[idx].bool_val = false;
        return idx;
    }
    // null
    if (c == 'n') {
        if (!forge_json_match(p, "null", 4)) { p->error = true; return -1; }
        int64_t idx = forge_json_alloc();
        forge_json_pool[idx].type = FORGE_JSON_NULL;
        return idx;
    }
    // array
    if (c == '[') {
        p->pos++;
        int64_t idx = forge_json_alloc();
        forge_json_pool[idx].type = FORGE_JSON_ARRAY;
        forge_json_pool[idx].array_val.items = NULL;
        forge_json_pool[idx].array_val.len = 0;
        if (forge_json_peek(p) == ']') { p->pos++; return idx; }
        while (1) {
            int64_t val = forge_json_parse_value(p);
            if (p->error) return -1;
            forge_json_array_push(idx, val);
            if (forge_json_peek(p) == ',') { p->pos++; continue; }
            if (forge_json_peek(p) == ']') { p->pos++; break; }
            p->error = true; return -1;
        }
        return idx;
    }
    // object
    if (c == '{') {
        p->pos++;
        int64_t idx = forge_json_alloc();
        forge_json_pool[idx].type = FORGE_JSON_OBJECT;
        forge_json_pool[idx].object_val.keys = NULL;
        forge_json_pool[idx].object_val.vals = NULL;
        forge_json_pool[idx].object_val.len = 0;
        if (forge_json_peek(p) == '}') { p->pos++; return idx; }
        while (1) {
            forge_json_skip_ws(p);
            forge_string_t key = forge_json_parse_string_raw(p);
            if (p->error) return -1;
            forge_json_skip_ws(p);
            if (p->pos >= p->len || p->data[p->pos] != ':') { p->error = true; return -1; }
            p->pos++;
            int64_t val = forge_json_parse_value(p);
            if (p->error) return -1;
            // grow parallel arrays
            int64_t new_len = forge_json_pool[idx].object_val.len + 1;
            forge_string_t *new_keys = (forge_string_t *)realloc(
                forge_json_pool[idx].object_val.keys, (size_t)new_len * sizeof(forge_string_t));
            int64_t *new_vals = (int64_t *)realloc(
                forge_json_pool[idx].object_val.vals, (size_t)new_len * sizeof(int64_t));
            if (!new_keys || !new_vals) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
            new_keys[new_len - 1] = key;
            new_vals[new_len - 1] = val;
            forge_json_pool[idx].object_val.keys = new_keys;
            forge_json_pool[idx].object_val.vals = new_vals;
            forge_json_pool[idx].object_val.len = new_len;
            if (forge_json_peek(p) == ',') { p->pos++; continue; }
            if (forge_json_peek(p) == '}') { p->pos++; break; }
            p->error = true; return -1;
        }
        return idx;
    }

    p->error = true;
    return -1;
}

// --- public API ---

// json_parse(String) -> Int (handle, -1 on error)
static inline int64_t forge_json_parse(forge_string_t input) {
    forge_json_parser_t p = { .data = input.data, .len = input.len, .pos = 0, .error = false };
    int64_t result = forge_json_parse_value(&p);
    if (p.error) return -1;
    return result;
}

// json_type(handle) -> String
static inline forge_string_t forge_json_type(int64_t handle) {
    if (handle < 0 || handle >= forge_json_pool_len) return FORGE_STRING_LIT("invalid");
    switch (forge_json_pool[handle].type) {
        case FORGE_JSON_NULL:   return FORGE_STRING_LIT("null");
        case FORGE_JSON_BOOL:   return FORGE_STRING_LIT("bool");
        case FORGE_JSON_INT:    return FORGE_STRING_LIT("int");
        case FORGE_JSON_FLOAT:  return FORGE_STRING_LIT("float");
        case FORGE_JSON_STRING: return FORGE_STRING_LIT("string");
        case FORGE_JSON_ARRAY:  return FORGE_STRING_LIT("array");
        case FORGE_JSON_OBJECT: return FORGE_STRING_LIT("object");
    }
    return FORGE_STRING_LIT("unknown");
}

// json_get_bool(handle) -> Bool
static inline bool forge_json_get_bool(int64_t handle) {
    if (handle < 0 || handle >= forge_json_pool_len) return false;
    if (forge_json_pool[handle].type != FORGE_JSON_BOOL) return false;
    return forge_json_pool[handle].bool_val;
}

// json_get_int(handle) -> Int
static inline int64_t forge_json_get_int(int64_t handle) {
    if (handle < 0 || handle >= forge_json_pool_len) return 0;
    if (forge_json_pool[handle].type == FORGE_JSON_INT)
        return forge_json_pool[handle].int_val;
    if (forge_json_pool[handle].type == FORGE_JSON_FLOAT)
        return (int64_t)forge_json_pool[handle].float_val;
    return 0;
}

// json_get_float(handle) -> Float
static inline double forge_json_get_float(int64_t handle) {
    if (handle < 0 || handle >= forge_json_pool_len) return 0.0;
    if (forge_json_pool[handle].type == FORGE_JSON_FLOAT)
        return forge_json_pool[handle].float_val;
    if (forge_json_pool[handle].type == FORGE_JSON_INT)
        return (double)forge_json_pool[handle].int_val;
    return 0.0;
}

// json_get_string(handle) -> String
static inline forge_string_t forge_json_get_string(int64_t handle) {
    if (handle < 0 || handle >= forge_json_pool_len) return forge_string_empty;
    if (forge_json_pool[handle].type != FORGE_JSON_STRING) return forge_string_empty;
    return forge_json_pool[handle].string_val;
}

// json_array_len(handle) -> Int
static inline int64_t forge_json_array_len(int64_t handle) {
    if (handle < 0 || handle >= forge_json_pool_len) return 0;
    if (forge_json_pool[handle].type != FORGE_JSON_ARRAY) return 0;
    return forge_json_pool[handle].array_val.len;
}

// json_array_get(handle, index) -> Int (handle)
static inline int64_t forge_json_array_get(int64_t handle, int64_t index) {
    if (handle < 0 || handle >= forge_json_pool_len) return -1;
    if (forge_json_pool[handle].type != FORGE_JSON_ARRAY) return -1;
    if (index < 0 || index >= forge_json_pool[handle].array_val.len) return -1;
    return forge_json_pool[handle].array_val.items[index];
}

// json_object_get(handle, key) -> Int (handle, -1 if not found)
static inline int64_t forge_json_object_get(int64_t handle, forge_string_t key) {
    if (handle < 0 || handle >= forge_json_pool_len) return -1;
    if (forge_json_pool[handle].type != FORGE_JSON_OBJECT) return -1;
    for (int64_t i = 0; i < forge_json_pool[handle].object_val.len; i++) {
        if (forge_string_eq(forge_json_pool[handle].object_val.keys[i], key))
            return forge_json_pool[handle].object_val.vals[i];
    }
    return -1;
}

// json_object_has(handle, key) -> Bool
static inline bool forge_json_object_has(int64_t handle, forge_string_t key) {
    return forge_json_object_get(handle, key) >= 0;
}

// json_object_keys(handle) -> List[String]
static inline forge_list_t forge_json_object_keys(int64_t handle) {
    forge_list_t result = { .data = NULL, .len = 0 };
    if (handle < 0 || handle >= forge_json_pool_len) return result;
    if (forge_json_pool[handle].type != FORGE_JSON_OBJECT) return result;
    for (int64_t i = 0; i < forge_json_pool[handle].object_val.len; i++) {
        forge_list_push(&result, &forge_json_pool[handle].object_val.keys[i], sizeof(forge_string_t));
    }
    return result;
}

// --- json encoder ---

static void forge_json_encode_impl(int64_t handle, char **buf, int64_t *len, int64_t *cap) {
    // helper to append a character
    #define JAPPEND_CHAR(ch) do { \
        if (*len >= *cap) { \
            *cap = *cap == 0 ? 256 : *cap * 2; \
            *buf = (char *)realloc(*buf, (size_t)*cap); \
            if (!*buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); } \
        } \
        (*buf)[(*len)++] = (ch); \
    } while(0)
    #define JAPPEND_STR(s, slen) do { \
        while (*len + (slen) > *cap) { \
            *cap = *cap == 0 ? 256 : *cap * 2; \
            *buf = (char *)realloc(*buf, (size_t)*cap); \
            if (!*buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); } \
        } \
        memcpy(*buf + *len, (s), (size_t)(slen)); \
        *len += (slen); \
    } while(0)

    if (handle < 0 || handle >= forge_json_pool_len) {
        JAPPEND_STR("null", 4);
        return;
    }
    forge_json_node_t *node = &forge_json_pool[handle];
    switch (node->type) {
        case FORGE_JSON_NULL:
            JAPPEND_STR("null", 4);
            break;
        case FORGE_JSON_BOOL:
            if (node->bool_val) JAPPEND_STR("true", 4);
            else JAPPEND_STR("false", 5);
            break;
        case FORGE_JSON_INT: {
            char tmp[32];
            int n = snprintf(tmp, sizeof(tmp), "%" PRId64, node->int_val);
            JAPPEND_STR(tmp, n);
            break;
        }
        case FORGE_JSON_FLOAT: {
            char tmp[64];
            int n = snprintf(tmp, sizeof(tmp), "%g", node->float_val);
            JAPPEND_STR(tmp, n);
            break;
        }
        case FORGE_JSON_STRING:
            JAPPEND_CHAR('"');
            for (int64_t i = 0; i < node->string_val.len; i++) {
                char c = node->string_val.data[i];
                switch (c) {
                    case '"':  JAPPEND_CHAR('\\'); JAPPEND_CHAR('"'); break;
                    case '\\': JAPPEND_CHAR('\\'); JAPPEND_CHAR('\\'); break;
                    case '\n': JAPPEND_CHAR('\\'); JAPPEND_CHAR('n'); break;
                    case '\r': JAPPEND_CHAR('\\'); JAPPEND_CHAR('r'); break;
                    case '\t': JAPPEND_CHAR('\\'); JAPPEND_CHAR('t'); break;
                    default: JAPPEND_CHAR(c); break;
                }
            }
            JAPPEND_CHAR('"');
            break;
        case FORGE_JSON_ARRAY:
            JAPPEND_CHAR('[');
            for (int64_t i = 0; i < node->array_val.len; i++) {
                if (i > 0) JAPPEND_CHAR(',');
                forge_json_encode_impl(node->array_val.items[i], buf, len, cap);
            }
            JAPPEND_CHAR(']');
            break;
        case FORGE_JSON_OBJECT:
            JAPPEND_CHAR('{');
            for (int64_t i = 0; i < node->object_val.len; i++) {
                if (i > 0) JAPPEND_CHAR(',');
                JAPPEND_CHAR('"');
                JAPPEND_STR(node->object_val.keys[i].data, node->object_val.keys[i].len);
                JAPPEND_CHAR('"');
                JAPPEND_CHAR(':');
                forge_json_encode_impl(node->object_val.vals[i], buf, len, cap);
            }
            JAPPEND_CHAR('}');
            break;
    }
    #undef JAPPEND_CHAR
    #undef JAPPEND_STR
}

// json_encode(handle) -> String
static inline forge_string_t forge_json_encode(int64_t handle) {
    char *buf = NULL;
    int64_t len = 0, cap = 0;
    forge_json_encode_impl(handle, &buf, &len, &cap);
    if (!buf) return forge_string_empty;
    buf = (char *)realloc(buf, (size_t)len + 1);
    buf[len] = '\0';
    return (forge_string_t){ .data = buf, .len = len };
}

// --- json constructors (for building JSON from Forge) ---

static inline int64_t forge_json_new_null(void) {
    int64_t idx = forge_json_alloc();
    forge_json_pool[idx].type = FORGE_JSON_NULL;
    return idx;
}

static inline int64_t forge_json_new_bool(bool val) {
    int64_t idx = forge_json_alloc();
    forge_json_pool[idx].type = FORGE_JSON_BOOL;
    forge_json_pool[idx].bool_val = val;
    return idx;
}

static inline int64_t forge_json_new_int(int64_t val) {
    int64_t idx = forge_json_alloc();
    forge_json_pool[idx].type = FORGE_JSON_INT;
    forge_json_pool[idx].int_val = val;
    return idx;
}

static inline int64_t forge_json_new_float(double val) {
    int64_t idx = forge_json_alloc();
    forge_json_pool[idx].type = FORGE_JSON_FLOAT;
    forge_json_pool[idx].float_val = val;
    return idx;
}

static inline int64_t forge_json_new_string(forge_string_t val) {
    int64_t idx = forge_json_alloc();
    forge_json_pool[idx].type = FORGE_JSON_STRING;
    forge_json_pool[idx].string_val = val;
    return idx;
}

static inline int64_t forge_json_new_array(void) {
    int64_t idx = forge_json_alloc();
    forge_json_pool[idx].type = FORGE_JSON_ARRAY;
    forge_json_pool[idx].array_val.items = NULL;
    forge_json_pool[idx].array_val.len = 0;
    return idx;
}

static inline void forge_json_array_push(int64_t handle, int64_t val) {
    if (handle < 0 || handle >= forge_json_pool_len) return;
    if (forge_json_pool[handle].type != FORGE_JSON_ARRAY) return;
    int64_t new_len = forge_json_pool[handle].array_val.len + 1;
    int64_t *new_items = (int64_t *)realloc(
        forge_json_pool[handle].array_val.items, (size_t)new_len * sizeof(int64_t));
    if (!new_items) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    new_items[new_len - 1] = val;
    forge_json_pool[handle].array_val.items = new_items;
    forge_json_pool[handle].array_val.len = new_len;
}

static inline int64_t forge_json_new_object(void) {
    int64_t idx = forge_json_alloc();
    forge_json_pool[idx].type = FORGE_JSON_OBJECT;
    forge_json_pool[idx].object_val.keys = NULL;
    forge_json_pool[idx].object_val.vals = NULL;
    forge_json_pool[idx].object_val.len = 0;
    return idx;
}

static inline void forge_json_object_set(int64_t handle, forge_string_t key, int64_t val) {
    if (handle < 0 || handle >= forge_json_pool_len) return;
    if (forge_json_pool[handle].type != FORGE_JSON_OBJECT) return;
    // check if key already exists
    for (int64_t i = 0; i < forge_json_pool[handle].object_val.len; i++) {
        if (forge_string_eq(forge_json_pool[handle].object_val.keys[i], key)) {
            forge_json_pool[handle].object_val.vals[i] = val;
            return;
        }
    }
    int64_t new_len = forge_json_pool[handle].object_val.len + 1;
    forge_string_t *new_keys = (forge_string_t *)realloc(
        forge_json_pool[handle].object_val.keys, (size_t)new_len * sizeof(forge_string_t));
    int64_t *new_vals = (int64_t *)realloc(
        forge_json_pool[handle].object_val.vals, (size_t)new_len * sizeof(int64_t));
    if (!new_keys || !new_vals) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    new_keys[new_len - 1] = key;
    new_vals[new_len - 1] = val;
    forge_json_pool[handle].object_val.keys = new_keys;
    forge_json_pool[handle].object_val.vals = new_vals;
    forge_json_pool[handle].object_val.len = new_len;
}

// ---------------------------------------------------------------
// file system operations
// ---------------------------------------------------------------

#include <sys/stat.h>
#include <unistd.h>
#include <dirent.h>

// file_exists(path) -> Bool
static inline bool forge_file_exists(forge_string_t path) {
    char *cpath = forge_cstr(path);
    int r = access(cpath, F_OK);
    free(cpath);
    return r == 0;
}

// dir_exists(path) -> Bool
static inline bool forge_dir_exists(forge_string_t path) {
    char *cpath = forge_cstr(path);
    struct stat st;
    int r = stat(cpath, &st);
    free(cpath);
    return r == 0 && S_ISDIR(st.st_mode);
}

// mkdir(path) -> Bool
static inline bool forge_mkdir(forge_string_t path) {
    char *cpath = forge_cstr(path);
    int r = mkdir(cpath, 0755);
    free(cpath);
    return r == 0;
}

// remove_file(path) -> Bool
static inline bool forge_remove_file(forge_string_t path) {
    char *cpath = forge_cstr(path);
    int r = unlink(cpath);
    free(cpath);
    return r == 0;
}

// rename_file(old, new) -> Bool
static inline bool forge_rename_file(forge_string_t old_path, forge_string_t new_path) {
    char *cold = forge_cstr(old_path);
    char *cnew = forge_cstr(new_path);
    int r = rename(cold, cnew);
    free(cold);
    free(cnew);
    return r == 0;
}

// append_file(path, data) -> Bool
static inline bool forge_append_file_impl(const char *path_data, int64_t path_len,
                                           const char *data, int64_t data_len) {
    char *path = forge_cstr((forge_string_t){ .data = path_data, .len = path_len });
    FILE *f = fopen(path, "ab");
    free(path);
    if (!f) return false;
    size_t written = fwrite(data, 1, (size_t)data_len, f);
    fclose(f);
    return written == (size_t)data_len;
}

// list_dir(path) -> List[String]
static inline forge_list_t forge_list_dir(forge_string_t path) {
    forge_list_t result = { .data = NULL, .len = 0 };
    char *cpath = forge_cstr(path);
    DIR *d = opendir(cpath);
    free(cpath);
    if (!d) return result;
    struct dirent *entry;
    while ((entry = readdir(d)) != NULL) {
        // skip . and ..
        if (entry->d_name[0] == '.' &&
            (entry->d_name[1] == '\0' ||
             (entry->d_name[1] == '.' && entry->d_name[2] == '\0')))
            continue;
        int64_t nlen = (int64_t)strlen(entry->d_name);
        char *buf = (char *)malloc((size_t)nlen + 1);
        if (!buf) continue;
        memcpy(buf, entry->d_name, (size_t)nlen + 1);
        forge_string_t s = { .data = buf, .len = nlen };
        forge_list_push(&result, &s, sizeof(forge_string_t));
    }
    closedir(d);
    return result;
}

// ---------------------------------------------------------------
// path manipulation — pure string operations
// ---------------------------------------------------------------

// join two path segments with /
static inline forge_string_t forge_path_join(forge_string_t a, forge_string_t b) {
    if (a.len == 0) return b;
    if (b.len == 0) return a;
    // strip trailing / from a
    int64_t alen = a.len;
    while (alen > 0 && a.data[alen - 1] == '/') alen--;
    // strip leading / from b
    const char *bdata = b.data;
    int64_t blen = b.len;
    while (blen > 0 && *bdata == '/') { bdata++; blen--; }
    int64_t total = alen + 1 + blen;
    char *buf = (char *)malloc((size_t)total + 1);
    if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    memcpy(buf, a.data, (size_t)alen);
    buf[alen] = '/';
    memcpy(buf + alen + 1, bdata, (size_t)blen);
    buf[total] = '\0';
    return (forge_string_t){ .data = buf, .len = total };
}

// directory component: "/foo/bar.txt" -> "/foo"
static inline forge_string_t forge_path_dir(forge_string_t path) {
    int64_t i = path.len - 1;
    while (i >= 0 && path.data[i] != '/') i--;
    if (i < 0) return FORGE_STRING_LIT(".");
    if (i == 0) return FORGE_STRING_LIT("/");
    char *buf = (char *)malloc((size_t)i + 1);
    if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    memcpy(buf, path.data, (size_t)i);
    buf[i] = '\0';
    return (forge_string_t){ .data = buf, .len = i };
}

// base name: "/foo/bar.txt" -> "bar.txt"
static inline forge_string_t forge_path_base(forge_string_t path) {
    int64_t i = path.len - 1;
    while (i >= 0 && path.data[i] != '/') i--;
    int64_t start = i + 1;
    int64_t len = path.len - start;
    if (len == 0) return FORGE_STRING_LIT("");
    char *buf = (char *)malloc((size_t)len + 1);
    if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    memcpy(buf, path.data + start, (size_t)len);
    buf[len] = '\0';
    return (forge_string_t){ .data = buf, .len = len };
}

// file extension: "/foo/bar.txt" -> ".txt"
static inline forge_string_t forge_path_ext(forge_string_t path) {
    // search from end, stop at / boundary
    int64_t i = path.len - 1;
    while (i >= 0 && path.data[i] != '.' && path.data[i] != '/') i--;
    if (i < 0 || path.data[i] == '/') return FORGE_STRING_LIT("");
    int64_t len = path.len - i;
    char *buf = (char *)malloc((size_t)len + 1);
    if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    memcpy(buf, path.data + i, (size_t)len);
    buf[len] = '\0';
    return (forge_string_t){ .data = buf, .len = len };
}

// stem: "/foo/bar.txt" -> "bar"
static inline forge_string_t forge_path_stem(forge_string_t path) {
    // find base first
    int64_t slash = path.len - 1;
    while (slash >= 0 && path.data[slash] != '/') slash--;
    int64_t start = slash + 1;
    // find last dot in base
    int64_t dot = path.len - 1;
    while (dot > start && path.data[dot] != '.') dot--;
    int64_t end = (dot > start) ? dot : path.len;
    int64_t len = end - start;
    if (len == 0) return FORGE_STRING_LIT("");
    char *buf = (char *)malloc((size_t)len + 1);
    if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    memcpy(buf, path.data + start, (size_t)len);
    buf[len] = '\0';
    return (forge_string_t){ .data = buf, .len = len };
}

// ---------------------------------------------------------------
// structured logging — level-prefixed stderr output with timestamps
// ---------------------------------------------------------------

static inline void forge_log_impl(const char *level, forge_string_t msg) {
    time_t now = time(NULL);
    struct tm *t = localtime(&now);
    char ts[20];
    strftime(ts, sizeof(ts), "%Y-%m-%d %H:%M:%S", t);
    fprintf(stderr, "%s [%s] %.*s\n", ts, level, (int)msg.len, msg.data);
}

static inline void forge_log_info(forge_string_t msg)  { forge_log_impl("INFO",  msg); }
static inline void forge_log_warn(forge_string_t msg)  { forge_log_impl("WARN",  msg); }
static inline void forge_log_error(forge_string_t msg) { forge_log_impl("ERROR", msg); }
static inline void forge_log_debug(forge_string_t msg) { forge_log_impl("DEBUG", msg); }

// ---------------------------------------------------------------
// encoding — base64 and hex
// ---------------------------------------------------------------

static const char forge_b64_enc[] =
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

static inline forge_string_t forge_base64_encode(forge_string_t input) {
    int64_t out_len = 4 * ((input.len + 2) / 3);
    char *buf = forge_str_alloc(out_len);
    int64_t i = 0, j = 0;
    while (i + 2 < input.len) {
        uint8_t a = (uint8_t)input.data[i], b = (uint8_t)input.data[i+1], c = (uint8_t)input.data[i+2];
        buf[j++] = forge_b64_enc[a >> 2];
        buf[j++] = forge_b64_enc[((a & 3) << 4) | (b >> 4)];
        buf[j++] = forge_b64_enc[((b & 15) << 2) | (c >> 6)];
        buf[j++] = forge_b64_enc[c & 63];
        i += 3;
    }
    if (i < input.len) {
        uint8_t a = (uint8_t)input.data[i];
        buf[j++] = forge_b64_enc[a >> 2];
        if (i + 1 < input.len) {
            uint8_t b = (uint8_t)input.data[i+1];
            buf[j++] = forge_b64_enc[((a & 3) << 4) | (b >> 4)];
            buf[j++] = forge_b64_enc[(b & 15) << 2];
        } else {
            buf[j++] = forge_b64_enc[(a & 3) << 4];
            buf[j++] = '=';
        }
        buf[j++] = '=';
    }
    buf[j] = '\0';
    return (forge_string_t){ .data = buf, .len = j };
}

// decode table: maps ASCII byte to 6-bit value, 255 = invalid
static const uint8_t forge_b64_dec[256] = {
    ['A']=0,['B']=1,['C']=2,['D']=3,['E']=4,['F']=5,['G']=6,['H']=7,
    ['I']=8,['J']=9,['K']=10,['L']=11,['M']=12,['N']=13,['O']=14,['P']=15,
    ['Q']=16,['R']=17,['S']=18,['T']=19,['U']=20,['V']=21,['W']=22,['X']=23,
    ['Y']=24,['Z']=25,['a']=26,['b']=27,['c']=28,['d']=29,['e']=30,['f']=31,
    ['g']=32,['h']=33,['i']=34,['j']=35,['k']=36,['l']=37,['m']=38,['n']=39,
    ['o']=40,['p']=41,['q']=42,['r']=43,['s']=44,['t']=45,['u']=46,['v']=47,
    ['w']=48,['x']=49,['y']=50,['z']=51,['0']=52,['1']=53,['2']=54,['3']=55,
    ['4']=56,['5']=57,['6']=58,['7']=59,['8']=60,['9']=61,['+']=62,['/']=63,
};

// returns false on invalid input, true on success
static inline bool forge_base64_decode_impl(forge_string_t input, forge_string_t *out) {
    // strip whitespace and count padding
    if (input.len % 4 != 0) return false;
    int64_t pad = 0;
    if (input.len > 0 && input.data[input.len-1] == '=') pad++;
    if (input.len > 1 && input.data[input.len-2] == '=') pad++;
    int64_t out_len = (input.len / 4) * 3 - pad;
    char *buf = forge_str_alloc(out_len);
    int64_t i = 0, j = 0;
    while (i < input.len) {
        uint8_t a = forge_b64_dec[(uint8_t)input.data[i]];
        uint8_t b = forge_b64_dec[(uint8_t)input.data[i+1]];
        uint8_t c = (input.data[i+2] == '=') ? 0 : forge_b64_dec[(uint8_t)input.data[i+2]];
        uint8_t d = (input.data[i+3] == '=') ? 0 : forge_b64_dec[(uint8_t)input.data[i+3]];
        buf[j++] = (char)((a << 2) | (b >> 4));
        if (input.data[i+2] != '=') buf[j++] = (char)(((b & 15) << 4) | (c >> 2));
        if (input.data[i+3] != '=') buf[j++] = (char)(((c & 3) << 6) | d);
        i += 4;
    }
    buf[j] = '\0';
    *out = (forge_string_t){ .data = buf, .len = out_len };
    return true;
}

static const char forge_hex_chars[] = "0123456789abcdef";

static inline forge_string_t forge_hex_encode(forge_string_t input) {
    int64_t out_len = input.len * 2;
    char *buf = forge_str_alloc(out_len);
    for (int64_t i = 0; i < input.len; i++) {
        uint8_t byte = (uint8_t)input.data[i];
        buf[i * 2] = forge_hex_chars[byte >> 4];
        buf[i * 2 + 1] = forge_hex_chars[byte & 15];
    }
    buf[out_len] = '\0';
    return (forge_string_t){ .data = buf, .len = out_len };
}

// returns false on invalid input (odd length or non-hex chars)
static inline bool forge_hex_decode_impl(forge_string_t input, forge_string_t *out) {
    if (input.len % 2 != 0) return false;
    int64_t out_len = input.len / 2;
    char *buf = forge_str_alloc(out_len);
    for (int64_t i = 0; i < out_len; i++) {
        uint8_t hi, lo;
        char ch = input.data[i * 2];
        char cl = input.data[i * 2 + 1];
        if (ch >= '0' && ch <= '9') hi = (uint8_t)(ch - '0');
        else if (ch >= 'a' && ch <= 'f') hi = (uint8_t)(ch - 'a' + 10);
        else if (ch >= 'A' && ch <= 'F') hi = (uint8_t)(ch - 'A' + 10);
        else { free(buf); return false; }
        if (cl >= '0' && cl <= '9') lo = (uint8_t)(cl - '0');
        else if (cl >= 'a' && cl <= 'f') lo = (uint8_t)(cl - 'a' + 10);
        else if (cl >= 'A' && cl <= 'F') lo = (uint8_t)(cl - 'A' + 10);
        else { free(buf); return false; }
        buf[i] = (char)((hi << 4) | lo);
    }
    buf[out_len] = '\0';
    *out = (forge_string_t){ .data = buf, .len = out_len };
    return true;
}

// ---------------------------------------------------------------
// hashing — SHA-256 and FNV-1a
// ---------------------------------------------------------------

// SHA-256 constants (FIPS 180-4)
static const uint32_t forge_sha256_k[64] = {
    0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
    0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
    0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
    0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
    0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
    0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
    0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
    0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2,
};

#define FORGE_SHA256_ROTR(x,n) (((x)>>(n))|((x)<<(32-(n))))

static inline forge_string_t forge_hash_sha256(forge_string_t input) {
    // initial hash values
    uint32_t h0=0x6a09e667, h1=0xbb67ae85, h2=0x3c6ef372, h3=0xa54ff53a;
    uint32_t h4=0x510e527f, h5=0x9b05688c, h6=0x1f83d9ab, h7=0x5be0cd19;

    // pre-processing: pad message to 64-byte blocks
    uint64_t bit_len = (uint64_t)input.len * 8;
    // message + 1 byte (0x80) + padding + 8 bytes (length)
    int64_t padded_len = ((input.len + 9 + 63) / 64) * 64;
    uint8_t *msg = (uint8_t *)calloc(1, (size_t)padded_len);
    if (!msg) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    memcpy(msg, input.data, (size_t)input.len);
    msg[input.len] = 0x80;
    // big-endian 64-bit length at the end
    for (int i = 0; i < 8; i++)
        msg[padded_len - 1 - i] = (uint8_t)(bit_len >> (i * 8));

    // process each 64-byte block
    for (int64_t block = 0; block < padded_len; block += 64) {
        uint32_t w[64];
        for (int i = 0; i < 16; i++)
            w[i] = ((uint32_t)msg[block+i*4]<<24) | ((uint32_t)msg[block+i*4+1]<<16)
                  | ((uint32_t)msg[block+i*4+2]<<8) | (uint32_t)msg[block+i*4+3];
        for (int i = 16; i < 64; i++) {
            uint32_t s0 = FORGE_SHA256_ROTR(w[i-15],7) ^ FORGE_SHA256_ROTR(w[i-15],18) ^ (w[i-15]>>3);
            uint32_t s1 = FORGE_SHA256_ROTR(w[i-2],17) ^ FORGE_SHA256_ROTR(w[i-2],19) ^ (w[i-2]>>10);
            w[i] = w[i-16] + s0 + w[i-7] + s1;
        }
        uint32_t a=h0, b=h1, c=h2, d=h3, e=h4, f=h5, g=h6, h=h7;
        for (int i = 0; i < 64; i++) {
            uint32_t S1 = FORGE_SHA256_ROTR(e,6) ^ FORGE_SHA256_ROTR(e,11) ^ FORGE_SHA256_ROTR(e,25);
            uint32_t ch = (e & f) ^ (~e & g);
            uint32_t temp1 = h + S1 + ch + forge_sha256_k[i] + w[i];
            uint32_t S0 = FORGE_SHA256_ROTR(a,2) ^ FORGE_SHA256_ROTR(a,13) ^ FORGE_SHA256_ROTR(a,22);
            uint32_t maj = (a & b) ^ (a & c) ^ (b & c);
            uint32_t temp2 = S0 + maj;
            h=g; g=f; f=e; e=d+temp1; d=c; c=b; b=a; a=temp1+temp2;
        }
        h0+=a; h1+=b; h2+=c; h3+=d; h4+=e; h5+=f; h6+=g; h7+=h;
    }
    free(msg);

    // produce 64-char hex string
    char *buf = forge_str_alloc(64);
    uint32_t hash[8] = { h0, h1, h2, h3, h4, h5, h6, h7 };
    for (int i = 0; i < 8; i++) {
        for (int j = 0; j < 8; j++)
            buf[i*8+j] = forge_hex_chars[(hash[i] >> (28 - j*4)) & 0xf];
    }
    buf[64] = '\0';
    return (forge_string_t){ .data = buf, .len = 64 };
}

// FNV-1a hash exposed as Int (wraps existing forge_hash_bytes)
static inline int64_t forge_hash_fnv1a(forge_string_t input) {
    return (int64_t)forge_hash_bytes(input.data, input.len);
}

// ---------------------------------------------------------------
// URL parsing
// ---------------------------------------------------------------

typedef struct {
    forge_string_t scheme;
    forge_string_t host;
    int64_t port;
    forge_string_t path;
    forge_string_t query;
    forge_string_t fragment;
} forge_url_t;

static forge_url_t *forge_url_pool = NULL;
static int64_t forge_url_pool_len = 0;
static int64_t forge_url_pool_cap = 0;

static inline int64_t forge_url_alloc(void) {
    if (forge_url_pool_len >= forge_url_pool_cap) {
        int64_t new_cap = forge_url_pool_cap == 0 ? 16 : forge_url_pool_cap * 2;
        forge_url_t *p = (forge_url_t *)realloc(forge_url_pool, (size_t)new_cap * sizeof(forge_url_t));
        if (!p) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
        forge_url_pool = p;
        forge_url_pool_cap = new_cap;
    }
    int64_t idx = forge_url_pool_len++;
    memset(&forge_url_pool[idx], 0, sizeof(forge_url_t));
    return idx;
}

// parse URL: scheme://host:port/path?query#fragment
static inline int64_t forge_url_parse(forge_string_t input) {
    int64_t idx = forge_url_alloc();
    forge_url_t *u = &forge_url_pool[idx];
    u->port = -1;
    const char *s = input.data;
    int64_t len = input.len;
    int64_t pos = 0;

    // scheme: everything before "://"
    int64_t scheme_end = -1;
    for (int64_t i = 0; i + 2 < len; i++) {
        if (s[i] == ':' && s[i+1] == '/' && s[i+2] == '/') { scheme_end = i; break; }
    }
    if (scheme_end > 0) {
        char *buf = forge_str_alloc(scheme_end);
        memcpy(buf, s, (size_t)scheme_end);
        buf[scheme_end] = '\0';
        u->scheme = (forge_string_t){ .data = buf, .len = scheme_end };
        pos = scheme_end + 3;
    }

    // host (and optional port)
    int64_t host_start = pos;
    int64_t host_end = len;
    for (int64_t i = pos; i < len; i++) {
        if (s[i] == '/' || s[i] == '?' || s[i] == '#') { host_end = i; break; }
    }
    // check for port in host part
    int64_t colon = -1;
    for (int64_t i = host_start; i < host_end; i++) {
        if (s[i] == ':') { colon = i; break; }
    }
    if (colon >= 0) {
        int64_t hlen = colon - host_start;
        char *hbuf = forge_str_alloc(hlen);
        memcpy(hbuf, s + host_start, (size_t)hlen);
        hbuf[hlen] = '\0';
        u->host = (forge_string_t){ .data = hbuf, .len = hlen };
        // parse port
        int64_t port = 0;
        for (int64_t i = colon + 1; i < host_end; i++) {
            if (s[i] >= '0' && s[i] <= '9') port = port * 10 + (s[i] - '0');
            else break;
        }
        u->port = port;
    } else {
        int64_t hlen = host_end - host_start;
        char *hbuf = forge_str_alloc(hlen);
        memcpy(hbuf, s + host_start, (size_t)hlen);
        hbuf[hlen] = '\0';
        u->host = (forge_string_t){ .data = hbuf, .len = hlen };
    }
    pos = host_end;

    // path: everything before ? or #
    int64_t path_start = pos;
    int64_t path_end = len;
    for (int64_t i = pos; i < len; i++) {
        if (s[i] == '?' || s[i] == '#') { path_end = i; break; }
    }
    int64_t plen = path_end - path_start;
    if (plen > 0) {
        char *pbuf = forge_str_alloc(plen);
        memcpy(pbuf, s + path_start, (size_t)plen);
        pbuf[plen] = '\0';
        u->path = (forge_string_t){ .data = pbuf, .len = plen };
    }
    pos = path_end;

    // query: after ? before #
    if (pos < len && s[pos] == '?') {
        pos++;
        int64_t qstart = pos;
        int64_t qend = len;
        for (int64_t i = pos; i < len; i++) {
            if (s[i] == '#') { qend = i; break; }
        }
        int64_t qlen = qend - qstart;
        char *qbuf = forge_str_alloc(qlen);
        memcpy(qbuf, s + qstart, (size_t)qlen);
        qbuf[qlen] = '\0';
        u->query = (forge_string_t){ .data = qbuf, .len = qlen };
        pos = qend;
    }

    // fragment: after #
    if (pos < len && s[pos] == '#') {
        pos++;
        int64_t flen = len - pos;
        char *fbuf = forge_str_alloc(flen);
        memcpy(fbuf, s + pos, (size_t)flen);
        fbuf[flen] = '\0';
        u->fragment = (forge_string_t){ .data = fbuf, .len = flen };
    }

    return idx;
}

static inline forge_string_t forge_url_scheme(int64_t handle) {
    if (handle < 0 || handle >= forge_url_pool_len) return forge_string_empty;
    return forge_url_pool[handle].scheme;
}

static inline forge_string_t forge_url_host(int64_t handle) {
    if (handle < 0 || handle >= forge_url_pool_len) return forge_string_empty;
    return forge_url_pool[handle].host;
}

static inline int64_t forge_url_port(int64_t handle) {
    if (handle < 0 || handle >= forge_url_pool_len) return -1;
    return forge_url_pool[handle].port;
}

static inline forge_string_t forge_url_path(int64_t handle) {
    if (handle < 0 || handle >= forge_url_pool_len) return forge_string_empty;
    return forge_url_pool[handle].path;
}

static inline forge_string_t forge_url_query(int64_t handle) {
    if (handle < 0 || handle >= forge_url_pool_len) return forge_string_empty;
    return forge_url_pool[handle].query;
}

static inline forge_string_t forge_url_fragment(int64_t handle) {
    if (handle < 0 || handle >= forge_url_pool_len) return forge_string_empty;
    return forge_url_pool[handle].fragment;
}

// reconstruct URL from components
static inline forge_string_t forge_url_to_string(int64_t handle) {
    if (handle < 0 || handle >= forge_url_pool_len) return forge_string_empty;
    forge_url_t *u = &forge_url_pool[handle];
    // calculate total length
    int64_t total = 0;
    if (u->scheme.len > 0) total += u->scheme.len + 3; // scheme://
    total += u->host.len;
    char port_buf[16] = {0};
    int port_len = 0;
    if (u->port >= 0) {
        port_len = snprintf(port_buf, sizeof(port_buf), "%" PRId64, u->port);
        total += 1 + port_len; // :port
    }
    total += u->path.len;
    if (u->query.len > 0) total += 1 + u->query.len; // ?query
    if (u->fragment.len > 0) total += 1 + u->fragment.len; // #fragment
    char *buf = forge_str_alloc(total);
    int64_t pos = 0;
    if (u->scheme.len > 0) {
        memcpy(buf + pos, u->scheme.data, (size_t)u->scheme.len); pos += u->scheme.len;
        memcpy(buf + pos, "://", 3); pos += 3;
    }
    memcpy(buf + pos, u->host.data, (size_t)u->host.len); pos += u->host.len;
    if (u->port >= 0) {
        buf[pos++] = ':';
        memcpy(buf + pos, port_buf, (size_t)port_len); pos += port_len;
    }
    memcpy(buf + pos, u->path.data, (size_t)u->path.len); pos += u->path.len;
    if (u->query.len > 0) {
        buf[pos++] = '?';
        memcpy(buf + pos, u->query.data, (size_t)u->query.len); pos += u->query.len;
    }
    if (u->fragment.len > 0) {
        buf[pos++] = '#';
        memcpy(buf + pos, u->fragment.data, (size_t)u->fragment.len); pos += u->fragment.len;
    }
    buf[pos] = '\0';
    return (forge_string_t){ .data = buf, .len = pos };
}

// percent-encode: encodes non-unreserved chars (RFC 3986)
static inline forge_string_t forge_percent_encode(forge_string_t input) {
    // count how many bytes need encoding
    int64_t count = 0;
    for (int64_t i = 0; i < input.len; i++) {
        uint8_t c = (uint8_t)input.data[i];
        if ((c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') ||
            (c >= '0' && c <= '9') || c == '-' || c == '_' || c == '.' || c == '~')
            count += 1;
        else
            count += 3;
    }
    char *buf = forge_str_alloc(count);
    int64_t j = 0;
    for (int64_t i = 0; i < input.len; i++) {
        uint8_t c = (uint8_t)input.data[i];
        if ((c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') ||
            (c >= '0' && c <= '9') || c == '-' || c == '_' || c == '.' || c == '~') {
            buf[j++] = (char)c;
        } else {
            buf[j++] = '%';
            buf[j++] = forge_hex_chars[c >> 4];
            buf[j++] = forge_hex_chars[c & 15];
        }
    }
    buf[j] = '\0';
    return (forge_string_t){ .data = buf, .len = count };
}

// percent-decode: decode %XX sequences
static inline forge_string_t forge_percent_decode(forge_string_t input) {
    char *buf = forge_str_alloc(input.len); // output <= input length
    int64_t j = 0;
    for (int64_t i = 0; i < input.len; i++) {
        if (input.data[i] == '%' && i + 2 < input.len) {
            uint8_t hi, lo;
            char ch = input.data[i+1], cl = input.data[i+2];
            if (ch >= '0' && ch <= '9') hi = (uint8_t)(ch - '0');
            else if (ch >= 'a' && ch <= 'f') hi = (uint8_t)(ch - 'a' + 10);
            else if (ch >= 'A' && ch <= 'F') hi = (uint8_t)(ch - 'A' + 10);
            else { buf[j++] = input.data[i]; continue; }
            if (cl >= '0' && cl <= '9') lo = (uint8_t)(cl - '0');
            else if (cl >= 'a' && cl <= 'f') lo = (uint8_t)(cl - 'a' + 10);
            else if (cl >= 'A' && cl <= 'F') lo = (uint8_t)(cl - 'A' + 10);
            else { buf[j++] = input.data[i]; continue; }
            buf[j++] = (char)((hi << 4) | lo);
            i += 2;
        } else if (input.data[i] == '+') {
            buf[j++] = ' ';
        } else {
            buf[j++] = input.data[i];
        }
    }
    buf[j] = '\0';
    return (forge_string_t){ .data = buf, .len = j };
}

// ---------------------------------------------------------------
// channels — typed message-passing with unbounded buffer
// ---------------------------------------------------------------

typedef struct {
    void *buffer;           // dynamic array of elements
    int64_t elem_size;
    int64_t len;            // current number of elements
    int64_t cap;            // buffer capacity
    int64_t read_pos;       // next read position
    int64_t write_pos;      // next write position
    bool closed;
    pthread_mutex_t mu;
    pthread_cond_t not_empty;
} forge_channel_t;

static inline forge_channel_t* forge_channel_create(int64_t elem_size) {
    forge_channel_t *ch = (forge_channel_t *)malloc(sizeof(forge_channel_t));
    if (!ch) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    ch->elem_size = elem_size;
    ch->len = 0;
    ch->cap = 16;
    ch->buffer = malloc((size_t)(ch->cap * elem_size));
    if (!ch->buffer) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    ch->read_pos = 0;
    ch->write_pos = 0;
    ch->closed = false;
    pthread_mutex_init(&ch->mu, NULL);
    pthread_cond_init(&ch->not_empty, NULL);
    return ch;
}

// send: enqueue element (unbounded — grows buffer if needed, never blocks)
static inline void forge_channel_send(forge_channel_t *ch, const void *elem) {
    pthread_mutex_lock(&ch->mu);
    if (ch->closed) { pthread_mutex_unlock(&ch->mu); return; }
    // grow if full
    if (ch->len >= ch->cap) {
        int64_t new_cap = ch->cap * 2;
        void *new_buf = malloc((size_t)(new_cap * ch->elem_size));
        if (!new_buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
        // copy elements in order
        for (int64_t i = 0; i < ch->len; i++) {
            int64_t src = ((ch->read_pos + i) % ch->cap) * ch->elem_size;
            int64_t dst = i * ch->elem_size;
            memcpy((char *)new_buf + dst, (char *)ch->buffer + src, (size_t)ch->elem_size);
        }
        free(ch->buffer);
        ch->buffer = new_buf;
        ch->read_pos = 0;
        ch->write_pos = ch->len;
        ch->cap = new_cap;
    }
    memcpy((char *)ch->buffer + ch->write_pos * ch->elem_size, elem, (size_t)ch->elem_size);
    ch->write_pos = (ch->write_pos + 1) % ch->cap;
    ch->len++;
    pthread_cond_signal(&ch->not_empty);
    pthread_mutex_unlock(&ch->mu);
}

// recv: dequeue element, blocks until available. returns false when closed and empty.
static inline bool forge_channel_recv(forge_channel_t *ch, void *out) {
    pthread_mutex_lock(&ch->mu);
    while (ch->len == 0 && !ch->closed) {
        pthread_cond_wait(&ch->not_empty, &ch->mu);
    }
    if (ch->len == 0) {
        pthread_mutex_unlock(&ch->mu);
        return false;
    }
    memcpy(out, (char *)ch->buffer + ch->read_pos * ch->elem_size, (size_t)ch->elem_size);
    ch->read_pos = (ch->read_pos + 1) % ch->cap;
    ch->len--;
    pthread_mutex_unlock(&ch->mu);
    return true;
}

static inline void forge_channel_close(forge_channel_t *ch) {
    pthread_mutex_lock(&ch->mu);
    ch->closed = true;
    pthread_cond_broadcast(&ch->not_empty);
    pthread_mutex_unlock(&ch->mu);
}

static inline int64_t forge_channel_len(forge_channel_t *ch) {
    pthread_mutex_lock(&ch->mu);
    int64_t n = ch->len;
    pthread_mutex_unlock(&ch->mu);
    return n;
}

// ---------------------------------------------------------------
// TOML parsing — mirrors the JSON API shape
// ---------------------------------------------------------------

enum forge_toml_type {
    FORGE_TOML_STRING = 0,
    FORGE_TOML_INT,
    FORGE_TOML_FLOAT,
    FORGE_TOML_BOOL,
    FORGE_TOML_TABLE,
    FORGE_TOML_ARRAY,
    FORGE_TOML_INVALID,
};

typedef struct {
    enum forge_toml_type type;
    forge_string_t str_val;
    int64_t int_val;
    double float_val;
    bool bool_val;
    // table: parallel arrays of keys and value handles
    forge_string_t *keys;
    int64_t *values;
    int64_t count;
    int64_t cap;
    // array: handles
    int64_t *elements;
    int64_t elem_count;
    int64_t elem_cap;
} forge_toml_node_t;

static forge_toml_node_t *forge_toml_pool = NULL;
static int64_t forge_toml_pool_len = 0;
static int64_t forge_toml_pool_cap = 0;

static inline int64_t forge_toml_alloc(void) {
    if (forge_toml_pool_len >= forge_toml_pool_cap) {
        int64_t new_cap = forge_toml_pool_cap == 0 ? 64 : forge_toml_pool_cap * 2;
        forge_toml_node_t *p = (forge_toml_node_t *)realloc(
            forge_toml_pool, (size_t)new_cap * sizeof(forge_toml_node_t));
        if (!p) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
        forge_toml_pool = p;
        forge_toml_pool_cap = new_cap;
    }
    int64_t idx = forge_toml_pool_len++;
    memset(&forge_toml_pool[idx], 0, sizeof(forge_toml_node_t));
    return idx;
}

static inline void forge_toml_table_set(int64_t handle, forge_string_t key, int64_t val) {
    forge_toml_node_t *n = &forge_toml_pool[handle];
    // check for existing key
    for (int64_t i = 0; i < n->count; i++) {
        if (n->keys[i].len == key.len && memcmp(n->keys[i].data, key.data, (size_t)key.len) == 0) {
            n->values[i] = val;
            return;
        }
    }
    if (n->count >= n->cap) {
        int64_t new_cap = n->cap == 0 ? 8 : n->cap * 2;
        n->keys = (forge_string_t *)realloc(n->keys, (size_t)new_cap * sizeof(forge_string_t));
        n->values = (int64_t *)realloc(n->values, (size_t)new_cap * sizeof(int64_t));
        n->cap = new_cap;
    }
    n->keys[n->count] = key;
    n->values[n->count] = val;
    n->count++;
}

static inline void forge_toml_array_push(int64_t handle, int64_t val) {
    forge_toml_node_t *n = &forge_toml_pool[handle];
    if (n->elem_count >= n->elem_cap) {
        int64_t new_cap = n->elem_cap == 0 ? 8 : n->elem_cap * 2;
        n->elements = (int64_t *)realloc(n->elements, (size_t)new_cap * sizeof(int64_t));
        n->elem_cap = new_cap;
    }
    n->elements[n->elem_count++] = val;
}

typedef struct {
    const char *data;
    int64_t len;
    int64_t pos;
    bool error;
} forge_toml_parser_t;

static inline void forge_toml_skip_ws(forge_toml_parser_t *p) {
    while (p->pos < p->len) {
        char c = p->data[p->pos];
        if (c == ' ' || c == '\t' || c == '\r') p->pos++;
        else break;
    }
}

static inline void forge_toml_skip_comment(forge_toml_parser_t *p) {
    if (p->pos < p->len && p->data[p->pos] == '#') {
        while (p->pos < p->len && p->data[p->pos] != '\n') p->pos++;
    }
}

static inline void forge_toml_skip_line(forge_toml_parser_t *p) {
    forge_toml_skip_ws(p);
    forge_toml_skip_comment(p);
    if (p->pos < p->len && p->data[p->pos] == '\n') p->pos++;
}

static inline forge_string_t forge_toml_parse_key(forge_toml_parser_t *p) {
    forge_toml_skip_ws(p);
    if (p->pos < p->len && p->data[p->pos] == '"') {
        p->pos++; // skip opening quote
        int64_t start = p->pos;
        while (p->pos < p->len && p->data[p->pos] != '"') p->pos++;
        int64_t klen = p->pos - start;
        if (p->pos < p->len) p->pos++; // skip closing quote
        char *buf = forge_str_alloc(klen);
        memcpy(buf, p->data + start, (size_t)klen);
        buf[klen] = '\0';
        return (forge_string_t){ .data = buf, .len = klen };
    }
    int64_t start = p->pos;
    while (p->pos < p->len) {
        char c = p->data[p->pos];
        if ((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') ||
            (c >= '0' && c <= '9') || c == '-' || c == '_')
            p->pos++;
        else break;
    }
    int64_t klen = p->pos - start;
    char *buf = forge_str_alloc(klen);
    memcpy(buf, p->data + start, (size_t)klen);
    buf[klen] = '\0';
    return (forge_string_t){ .data = buf, .len = klen };
}

static inline int64_t forge_toml_parse_value(forge_toml_parser_t *p) {
    forge_toml_skip_ws(p);
    if (p->pos >= p->len) { p->error = true; return -1; }
    char c = p->data[p->pos];

    // string
    if (c == '"') {
        p->pos++;
        int64_t start = p->pos;
        while (p->pos < p->len && p->data[p->pos] != '"') {
            if (p->data[p->pos] == '\\') p->pos++; // skip escape
            p->pos++;
        }
        int64_t slen = p->pos - start;
        if (p->pos < p->len) p->pos++; // closing quote
        char *buf = forge_str_alloc(slen);
        memcpy(buf, p->data + start, (size_t)slen);
        buf[slen] = '\0';
        int64_t idx = forge_toml_alloc();
        forge_toml_pool[idx].type = FORGE_TOML_STRING;
        forge_toml_pool[idx].str_val = (forge_string_t){ .data = buf, .len = slen };
        return idx;
    }

    // boolean
    if (p->pos + 4 <= p->len && memcmp(p->data + p->pos, "true", 4) == 0) {
        p->pos += 4;
        int64_t idx = forge_toml_alloc();
        forge_toml_pool[idx].type = FORGE_TOML_BOOL;
        forge_toml_pool[idx].bool_val = true;
        return idx;
    }
    if (p->pos + 5 <= p->len && memcmp(p->data + p->pos, "false", 5) == 0) {
        p->pos += 5;
        int64_t idx = forge_toml_alloc();
        forge_toml_pool[idx].type = FORGE_TOML_BOOL;
        forge_toml_pool[idx].bool_val = false;
        return idx;
    }

    // array
    if (c == '[') {
        p->pos++;
        int64_t arr = forge_toml_alloc();
        forge_toml_pool[arr].type = FORGE_TOML_ARRAY;
        forge_toml_skip_ws(p);
        while (p->pos < p->len && p->data[p->pos] != ']') {
            forge_toml_skip_ws(p);
            if (p->pos < p->len && p->data[p->pos] == '\n') { p->pos++; continue; }
            if (p->pos < p->len && p->data[p->pos] == '#') { forge_toml_skip_comment(p); continue; }
            int64_t val = forge_toml_parse_value(p);
            if (p->error) return -1;
            forge_toml_array_push(arr, val);
            forge_toml_skip_ws(p);
            if (p->pos < p->len && p->data[p->pos] == ',') p->pos++;
            forge_toml_skip_ws(p);
            while (p->pos < p->len && p->data[p->pos] == '\n') p->pos++;
        }
        if (p->pos < p->len) p->pos++; // skip ]
        return arr;
    }

    // number (int or float)
    if ((c >= '0' && c <= '9') || c == '-' || c == '+') {
        int64_t start = p->pos;
        bool is_float = false;
        if (c == '-' || c == '+') p->pos++;
        while (p->pos < p->len) {
            char d = p->data[p->pos];
            if (d == '.' || d == 'e' || d == 'E') is_float = true;
            if ((d >= '0' && d <= '9') || d == '.' || d == 'e' || d == 'E' || d == '+' || d == '-' || d == '_')
                p->pos++;
            else break;
        }
        char tmp[128];
        int64_t nlen = p->pos - start;
        if (nlen > 127) nlen = 127;
        // copy, skipping underscores
        int64_t j = 0;
        for (int64_t i = 0; i < nlen && j < 127; i++) {
            if (p->data[start + i] != '_') tmp[j++] = p->data[start + i];
        }
        tmp[j] = '\0';
        int64_t idx = forge_toml_alloc();
        if (is_float) {
            forge_toml_pool[idx].type = FORGE_TOML_FLOAT;
            forge_toml_pool[idx].float_val = strtod(tmp, NULL);
        } else {
            forge_toml_pool[idx].type = FORGE_TOML_INT;
            forge_toml_pool[idx].int_val = strtoll(tmp, NULL, 10);
        }
        return idx;
    }

    p->error = true;
    return -1;
}

// find or create a nested table from a dotted key path within root
static inline int64_t forge_toml_ensure_table(int64_t root, const char *path, int64_t path_len) {
    int64_t current = root;
    int64_t start = 0;
    for (int64_t i = 0; i <= path_len; i++) {
        if (i == path_len || path[i] == '.') {
            int64_t klen = i - start;
            forge_string_t key = { .data = path + start, .len = klen };
            // look up existing
            forge_toml_node_t *n = &forge_toml_pool[current];
            int64_t found = -1;
            for (int64_t j = 0; j < n->count; j++) {
                if (n->keys[j].len == klen && memcmp(n->keys[j].data, key.data, (size_t)klen) == 0) {
                    found = n->values[j]; break;
                }
            }
            if (found < 0) {
                found = forge_toml_alloc();
                forge_toml_pool[found].type = FORGE_TOML_TABLE;
                char *kbuf = forge_str_alloc(klen);
                memcpy(kbuf, key.data, (size_t)klen);
                kbuf[klen] = '\0';
                forge_toml_table_set(current, (forge_string_t){ .data = kbuf, .len = klen }, found);
            }
            current = found;
            start = i + 1;
        }
    }
    return current;
}

static inline int64_t forge_toml_parse(forge_string_t input) {
    forge_toml_parser_t p = { .data = input.data, .len = input.len, .pos = 0, .error = false };
    int64_t root = forge_toml_alloc();
    forge_toml_pool[root].type = FORGE_TOML_TABLE;
    int64_t current_table = root;

    while (p.pos < p.len) {
        forge_toml_skip_ws(&p);
        if (p.pos >= p.len) break;
        char c = p.data[p.pos];

        // skip blank lines and comments
        if (c == '\n') { p.pos++; continue; }
        if (c == '#') { forge_toml_skip_comment(&p); continue; }

        // table header [name] or [a.b.c]
        if (c == '[') {
            p.pos++;
            forge_toml_skip_ws(&p);
            int64_t start = p.pos;
            while (p.pos < p.len && p.data[p.pos] != ']') p.pos++;
            int64_t klen = p.pos - start;
            if (p.pos < p.len) p.pos++; // skip ]
            current_table = forge_toml_ensure_table(root, p.data + start, klen);
            forge_toml_skip_line(&p);
            continue;
        }

        // key = value
        forge_string_t key = forge_toml_parse_key(&p);
        forge_toml_skip_ws(&p);
        if (p.pos < p.len && p.data[p.pos] == '=') p.pos++;
        else { p.error = true; break; }
        int64_t val = forge_toml_parse_value(&p);
        if (p.error) return -1;
        forge_toml_table_set(current_table, key, val);
        forge_toml_skip_line(&p);
    }

    if (p.error) return -1;
    return root;
}

static inline forge_string_t forge_toml_type(int64_t handle) {
    if (handle < 0 || handle >= forge_toml_pool_len) return FORGE_STRING_LIT("invalid");
    switch (forge_toml_pool[handle].type) {
        case FORGE_TOML_STRING: return FORGE_STRING_LIT("string");
        case FORGE_TOML_INT:    return FORGE_STRING_LIT("int");
        case FORGE_TOML_FLOAT:  return FORGE_STRING_LIT("float");
        case FORGE_TOML_BOOL:   return FORGE_STRING_LIT("bool");
        case FORGE_TOML_TABLE:  return FORGE_STRING_LIT("table");
        case FORGE_TOML_ARRAY:  return FORGE_STRING_LIT("array");
        default:                return FORGE_STRING_LIT("invalid");
    }
}

static inline forge_string_t forge_toml_get_string(int64_t handle, forge_string_t key) {
    if (handle < 0 || handle >= forge_toml_pool_len) return forge_string_empty;
    forge_toml_node_t *n = &forge_toml_pool[handle];
    for (int64_t i = 0; i < n->count; i++) {
        if (n->keys[i].len == key.len && memcmp(n->keys[i].data, key.data, (size_t)key.len) == 0) {
            int64_t v = n->values[i];
            if (v >= 0 && v < forge_toml_pool_len && forge_toml_pool[v].type == FORGE_TOML_STRING)
                return forge_toml_pool[v].str_val;
        }
    }
    return forge_string_empty;
}

static inline int64_t forge_toml_get_int(int64_t handle, forge_string_t key) {
    if (handle < 0 || handle >= forge_toml_pool_len) return 0;
    forge_toml_node_t *n = &forge_toml_pool[handle];
    for (int64_t i = 0; i < n->count; i++) {
        if (n->keys[i].len == key.len && memcmp(n->keys[i].data, key.data, (size_t)key.len) == 0) {
            int64_t v = n->values[i];
            if (v >= 0 && v < forge_toml_pool_len && forge_toml_pool[v].type == FORGE_TOML_INT)
                return forge_toml_pool[v].int_val;
        }
    }
    return 0;
}

static inline double forge_toml_get_float(int64_t handle, forge_string_t key) {
    if (handle < 0 || handle >= forge_toml_pool_len) return 0.0;
    forge_toml_node_t *n = &forge_toml_pool[handle];
    for (int64_t i = 0; i < n->count; i++) {
        if (n->keys[i].len == key.len && memcmp(n->keys[i].data, key.data, (size_t)key.len) == 0) {
            int64_t v = n->values[i];
            if (v >= 0 && v < forge_toml_pool_len && forge_toml_pool[v].type == FORGE_TOML_FLOAT)
                return forge_toml_pool[v].float_val;
        }
    }
    return 0.0;
}

static inline bool forge_toml_get_bool(int64_t handle, forge_string_t key) {
    if (handle < 0 || handle >= forge_toml_pool_len) return false;
    forge_toml_node_t *n = &forge_toml_pool[handle];
    for (int64_t i = 0; i < n->count; i++) {
        if (n->keys[i].len == key.len && memcmp(n->keys[i].data, key.data, (size_t)key.len) == 0) {
            int64_t v = n->values[i];
            if (v >= 0 && v < forge_toml_pool_len && forge_toml_pool[v].type == FORGE_TOML_BOOL)
                return forge_toml_pool[v].bool_val;
        }
    }
    return false;
}

static inline int64_t forge_toml_get_table(int64_t handle, forge_string_t key) {
    if (handle < 0 || handle >= forge_toml_pool_len) return -1;
    forge_toml_node_t *n = &forge_toml_pool[handle];
    for (int64_t i = 0; i < n->count; i++) {
        if (n->keys[i].len == key.len && memcmp(n->keys[i].data, key.data, (size_t)key.len) == 0) {
            int64_t v = n->values[i];
            if (v >= 0 && v < forge_toml_pool_len && forge_toml_pool[v].type == FORGE_TOML_TABLE)
                return v;
        }
    }
    return -1;
}

static inline int64_t forge_toml_get_array(int64_t handle, forge_string_t key) {
    if (handle < 0 || handle >= forge_toml_pool_len) return -1;
    forge_toml_node_t *n = &forge_toml_pool[handle];
    for (int64_t i = 0; i < n->count; i++) {
        if (n->keys[i].len == key.len && memcmp(n->keys[i].data, key.data, (size_t)key.len) == 0) {
            int64_t v = n->values[i];
            if (v >= 0 && v < forge_toml_pool_len && forge_toml_pool[v].type == FORGE_TOML_ARRAY)
                return v;
        }
    }
    return -1;
}

static inline int64_t forge_toml_array_len(int64_t handle) {
    if (handle < 0 || handle >= forge_toml_pool_len) return 0;
    return forge_toml_pool[handle].elem_count;
}

static inline int64_t forge_toml_array_get(int64_t handle, int64_t index) {
    if (handle < 0 || handle >= forge_toml_pool_len) return -1;
    if (index < 0 || index >= forge_toml_pool[handle].elem_count) return -1;
    return forge_toml_pool[handle].elements[index];
}

static inline forge_list_t forge_toml_keys(int64_t handle) {
    forge_list_t list = forge_list_create(0, sizeof(forge_string_t), NULL);
    if (handle < 0 || handle >= forge_toml_pool_len) return list;
    forge_toml_node_t *n = &forge_toml_pool[handle];
    for (int64_t i = 0; i < n->count; i++) {
        forge_list_push(&list, &n->keys[i], sizeof(forge_string_t));
    }
    return list;
}

static inline bool forge_toml_has(int64_t handle, forge_string_t key) {
    if (handle < 0 || handle >= forge_toml_pool_len) return false;
    forge_toml_node_t *n = &forge_toml_pool[handle];
    for (int64_t i = 0; i < n->count; i++) {
        if (n->keys[i].len == key.len && memcmp(n->keys[i].data, key.data, (size_t)key.len) == 0)
            return true;
    }
    return false;
}

// ---------------------------------------------------------------
// networking — TCP and DNS (POSIX sockets, blocking I/O)
// ---------------------------------------------------------------

// tcp_connect(host, port) -> Int! (fd)
static inline bool forge_tcp_connect_impl(forge_string_t host, int64_t port, int64_t *fd_out) {
    char *host_cstr = forge_cstr(host);
    char port_str[16];
    snprintf(port_str, sizeof(port_str), "%" PRId64, port);

    struct addrinfo hints, *res;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;

    int rc = getaddrinfo(host_cstr, port_str, &hints, &res);
    free(host_cstr);
    if (rc != 0) return false;

    int fd = socket(res->ai_family, res->ai_socktype, res->ai_protocol);
    if (fd < 0) { freeaddrinfo(res); return false; }

    if (connect(fd, res->ai_addr, res->ai_addrlen) < 0) {
        close(fd);
        freeaddrinfo(res);
        return false;
    }
    freeaddrinfo(res);
    *fd_out = (int64_t)fd;
    return true;
}

// tcp_listen(host, port) -> Int! (server fd)
static inline bool forge_tcp_listen_impl(forge_string_t host, int64_t port, int64_t *fd_out) {
    char *host_cstr = forge_cstr(host);
    char port_str[16];
    snprintf(port_str, sizeof(port_str), "%" PRId64, port);

    struct addrinfo hints, *res;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_INET;
    hints.ai_socktype = SOCK_STREAM;
    hints.ai_flags = AI_PASSIVE;

    int rc = getaddrinfo(host_cstr[0] ? host_cstr : NULL, port_str, &hints, &res);
    free(host_cstr);
    if (rc != 0) return false;

    int fd = socket(res->ai_family, res->ai_socktype, res->ai_protocol);
    if (fd < 0) { freeaddrinfo(res); return false; }

    int opt = 1;
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &opt, sizeof(opt));

    if (bind(fd, res->ai_addr, res->ai_addrlen) < 0) {
        close(fd);
        freeaddrinfo(res);
        return false;
    }
    freeaddrinfo(res);

    if (listen(fd, 128) < 0) { close(fd); return false; }
    *fd_out = (int64_t)fd;
    return true;
}

// tcp_accept(server_fd) -> Int! (client fd)
static inline bool forge_tcp_accept_impl(int64_t server_fd, int64_t *client_fd) {
    int fd = accept((int)server_fd, NULL, NULL);
    if (fd < 0) return false;
    *client_fd = (int64_t)fd;
    return true;
}

// tcp_read(fd, max_bytes) -> String!
static inline bool forge_tcp_read_impl(int64_t fd, int64_t max_bytes, forge_string_t *out) {
    if (max_bytes <= 0) max_bytes = 4096;
    char *buf = forge_str_alloc(max_bytes);
    ssize_t n = read((int)fd, buf, (size_t)max_bytes);
    if (n < 0) { free(buf); return false; }
    buf[n] = '\0';
    *out = (forge_string_t){ .data = buf, .len = (int64_t)n };
    return true;
}

// tcp_write(fd, data) -> Int! (bytes written)
static inline bool forge_tcp_write_impl(int64_t fd, forge_string_t data, int64_t *written) {
    ssize_t n = write((int)fd, data.data, (size_t)data.len);
    if (n < 0) return false;
    *written = (int64_t)n;
    return true;
}

// tcp_close(fd) -> Void
static inline void forge_tcp_close(int64_t fd) {
    close((int)fd);
}

// dns_resolve(hostname) -> String! (first IP address)
static inline bool forge_dns_resolve_impl(forge_string_t hostname, forge_string_t *out) {
    char *host_cstr = forge_cstr(hostname);
    struct addrinfo hints, *res;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;

    int rc = getaddrinfo(host_cstr, NULL, &hints, &res);
    free(host_cstr);
    if (rc != 0) return false;

    char ip_buf[INET6_ADDRSTRLEN];
    if (res->ai_family == AF_INET) {
        struct sockaddr_in *addr = (struct sockaddr_in *)res->ai_addr;
        inet_ntop(AF_INET, &addr->sin_addr, ip_buf, sizeof(ip_buf));
    } else {
        struct sockaddr_in6 *addr = (struct sockaddr_in6 *)res->ai_addr;
        inet_ntop(AF_INET6, &addr->sin6_addr, ip_buf, sizeof(ip_buf));
    }
    freeaddrinfo(res);

    int64_t len = (int64_t)strlen(ip_buf);
    char *buf = forge_str_alloc(len);
    memcpy(buf, ip_buf, (size_t)len);
    buf[len] = '\0';
    *out = (forge_string_t){ .data = buf, .len = len };
    return true;
}

// ---------------------------------------------------------------
// concurrency — task header and sync primitives
// ---------------------------------------------------------------

// task header — shared by all Task[T] instantiations. each spawned task
// gets a per-type struct containing this header plus the return value.
typedef struct {
    pthread_t thread;
} forge_task_header_t;

// Mutex — simple pthread mutex wrapper
typedef struct {
    pthread_mutex_t __inner;
} forge_mutex_t;

static inline forge_mutex_t forge_mutex_create(void) {
    forge_mutex_t m;
    pthread_mutex_init(&m.__inner, NULL);
    return m;
}

static inline void forge_mutex_lock(forge_mutex_t *m) {
    pthread_mutex_lock(&m->__inner);
}

static inline void forge_mutex_unlock(forge_mutex_t *m) {
    pthread_mutex_unlock(&m->__inner);
}

// WaitGroup — counter with condition variable for waiting on completion
typedef struct {
    int64_t __count;
    pthread_mutex_t __mutex;
    pthread_cond_t __cond;
} forge_waitgroup_t;

static inline forge_waitgroup_t forge_waitgroup_create(void) {
    forge_waitgroup_t wg;
    wg.__count = 0;
    pthread_mutex_init(&wg.__mutex, NULL);
    pthread_cond_init(&wg.__cond, NULL);
    return wg;
}

static inline void forge_waitgroup_add(forge_waitgroup_t *wg, int64_t n) {
    pthread_mutex_lock(&wg->__mutex);
    wg->__count += n;
    pthread_mutex_unlock(&wg->__mutex);
}

static inline void forge_waitgroup_done(forge_waitgroup_t *wg) {
    pthread_mutex_lock(&wg->__mutex);
    wg->__count--;
    if (wg->__count <= 0) {
        pthread_cond_broadcast(&wg->__cond);
    }
    pthread_mutex_unlock(&wg->__mutex);
}

static inline void forge_waitgroup_wait(forge_waitgroup_t *wg) {
    pthread_mutex_lock(&wg->__mutex);
    while (wg->__count > 0) {
        pthread_cond_wait(&wg->__cond, &wg->__mutex);
    }
    pthread_mutex_unlock(&wg->__mutex);
}

// Semaphore — counting semaphore with condition variable
typedef struct {
    int64_t __permits;
    pthread_mutex_t __mutex;
    pthread_cond_t __cond;
} forge_semaphore_t;

static inline forge_semaphore_t forge_semaphore_create(int64_t permits) {
    forge_semaphore_t s;
    s.__permits = permits;
    pthread_mutex_init(&s.__mutex, NULL);
    pthread_cond_init(&s.__cond, NULL);
    return s;
}

static inline void forge_semaphore_acquire(forge_semaphore_t *s) {
    pthread_mutex_lock(&s->__mutex);
    while (s->__permits <= 0) {
        pthread_cond_wait(&s->__cond, &s->__mutex);
    }
    s->__permits--;
    pthread_mutex_unlock(&s->__mutex);
}

static inline void forge_semaphore_release(forge_semaphore_t *s) {
    pthread_mutex_lock(&s->__mutex);
    s->__permits++;
    pthread_cond_signal(&s->__cond);
    pthread_mutex_unlock(&s->__mutex);
}

// --- process management ---
// opaque handle pool: each handle stores pid + pipe file descriptors.

typedef struct {
    pid_t pid;
    int stdin_fd;   // write end — parent writes to child's stdin
    int stdout_fd;  // read end — parent reads child's stdout
    int stderr_fd;  // read end — parent reads child's stderr
    bool alive;
} forge_process_t;

#define FORGE_MAX_PROCESSES 64
static forge_process_t forge_process_pool[FORGE_MAX_PROCESSES];
static int64_t forge_process_count = 0;

// spawn a child process. returns handle index, or -1 on error.
// the command string is split on spaces (no shell interpretation).
static inline bool forge_process_spawn_impl(forge_string_t cmd, int64_t *out_handle) {
    if (forge_process_count >= FORGE_MAX_PROCESSES) {
        *out_handle = -1;
        return false;
    }

    int stdin_pipe[2], stdout_pipe[2], stderr_pipe[2];
    if (pipe(stdin_pipe) < 0 || pipe(stdout_pipe) < 0 || pipe(stderr_pipe) < 0) {
        *out_handle = -1;
        return false;
    }

    // parse command into argv (simple space split)
    char *cstr = forge_cstr(cmd);
    char *argv[128];
    int argc = 0;
    char *tok = strtok(cstr, " ");
    while (tok && argc < 127) {
        argv[argc++] = tok;
        tok = strtok(NULL, " ");
    }
    argv[argc] = NULL;

    pid_t pid = fork();
    if (pid < 0) {
        free(cstr);
        close(stdin_pipe[0]); close(stdin_pipe[1]);
        close(stdout_pipe[0]); close(stdout_pipe[1]);
        close(stderr_pipe[0]); close(stderr_pipe[1]);
        *out_handle = -1;
        return false;
    }

    if (pid == 0) {
        // child
        close(stdin_pipe[1]);
        close(stdout_pipe[0]);
        close(stderr_pipe[0]);
        dup2(stdin_pipe[0], STDIN_FILENO);
        dup2(stdout_pipe[1], STDOUT_FILENO);
        dup2(stderr_pipe[1], STDERR_FILENO);
        close(stdin_pipe[0]);
        close(stdout_pipe[1]);
        close(stderr_pipe[1]);
        execvp(argv[0], argv);
        _exit(127);
    }

    // parent
    free(cstr);
    close(stdin_pipe[0]);
    close(stdout_pipe[1]);
    close(stderr_pipe[1]);

    int64_t handle = forge_process_count++;
    forge_process_pool[handle].pid = pid;
    forge_process_pool[handle].stdin_fd = stdin_pipe[1];
    forge_process_pool[handle].stdout_fd = stdout_pipe[0];
    forge_process_pool[handle].stderr_fd = stderr_pipe[0];
    forge_process_pool[handle].alive = true;
    *out_handle = handle;
    return true;
}

// write string to child's stdin. returns bytes written or -1 on error.
static inline bool forge_process_write_impl(int64_t handle, forge_string_t data, int64_t *out) {
    if (handle < 0 || handle >= forge_process_count) { *out = -1; return false; }
    forge_process_t *p = &forge_process_pool[handle];
    ssize_t n = write(p->stdin_fd, data.data, (size_t)data.len);
    if (n < 0) { *out = -1; return false; }
    *out = (int64_t)n;
    return true;
}

// read from child's stdout. reads up to max_bytes.
static inline bool forge_process_read_impl(int64_t handle, int64_t max_bytes, forge_string_t *out) {
    if (handle < 0 || handle >= forge_process_count) { *out = forge_string_empty; return false; }
    forge_process_t *p = &forge_process_pool[handle];
    char *buf = (char *)malloc((size_t)max_bytes + 1);
    if (!buf) { *out = forge_string_empty; return false; }
    ssize_t n = read(p->stdout_fd, buf, (size_t)max_bytes);
    if (n < 0) { free(buf); *out = forge_string_empty; return false; }
    buf[n] = '\0';
    *out = forge_string_from(buf, (int64_t)n);
    return true;
}

// read from child's stderr. reads up to max_bytes.
static inline bool forge_process_read_err_impl(int64_t handle, int64_t max_bytes, forge_string_t *out) {
    if (handle < 0 || handle >= forge_process_count) { *out = forge_string_empty; return false; }
    forge_process_t *p = &forge_process_pool[handle];
    char *buf = (char *)malloc((size_t)max_bytes + 1);
    if (!buf) { *out = forge_string_empty; return false; }
    ssize_t n = read(p->stderr_fd, buf, (size_t)max_bytes);
    if (n < 0) { free(buf); *out = forge_string_empty; return false; }
    buf[n] = '\0';
    *out = forge_string_from(buf, (int64_t)n);
    return true;
}

// wait for child to exit. returns exit code.
static inline int64_t forge_process_wait(int64_t handle) {
    if (handle < 0 || handle >= forge_process_count) return -1;
    forge_process_t *p = &forge_process_pool[handle];
    int status = 0;
    waitpid(p->pid, &status, 0);
    p->alive = false;
    if (WIFEXITED(status)) return WEXITSTATUS(status);
    return -1;
}

// kill the child process. returns true if signal sent successfully.
static inline bool forge_process_kill(int64_t handle) {
    if (handle < 0 || handle >= forge_process_count) return false;
    forge_process_t *p = &forge_process_pool[handle];
    if (!p->alive) return false;
    return kill(p->pid, SIGTERM) == 0;
}

// close all pipe file descriptors for this process.
static inline void forge_process_close(int64_t handle) {
    if (handle < 0 || handle >= forge_process_count) return;
    forge_process_t *p = &forge_process_pool[handle];
    if (p->stdin_fd >= 0) { close(p->stdin_fd); p->stdin_fd = -1; }
    if (p->stdout_fd >= 0) { close(p->stdout_fd); p->stdout_fd = -1; }
    if (p->stderr_fd >= 0) { close(p->stderr_fd); p->stderr_fd = -1; }
}

#endif // FORGE_RUNTIME_H
