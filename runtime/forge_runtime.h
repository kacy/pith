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
// reference counting (ARC) infrastructure
// ---------------------------------------------------------------

// Forward declaration for cycle detection
struct forge_rc_header;

// RC header stored before each heap-allocated object
// Enhanced with cycle detection support
typedef struct forge_rc_header {
    int64_t ref_count;
    int32_t type_tag;
    int32_t flags;        // MARKED, ROOT, etc. for cycle detection
    struct forge_rc_header *next;  // For cycle collector object list
} forge_rc_header_t;

// Flags for cycle detection
#define FORGE_RC_MARKED  0x01
#define FORGE_RC_VISITED 0x02
#define FORGE_RC_IN_CYCLE 0x04

// Type tags for cycle detection
typedef enum {
    FORGE_TYPE_STRING = 1,
    FORGE_TYPE_LIST = 2,
    FORGE_TYPE_MAP = 3,
    FORGE_TYPE_CLOSURE = 4,
    FORGE_TYPE_TASK = 5
} forge_type_tag_t;

// Get pointer to RC header from object pointer
#define FORGE_RC_HEADER(ptr) ((forge_rc_header_t *)((char *)(ptr) - sizeof(forge_rc_header_t)))

// Global list of all RC-managed objects (for cycle detection)
static forge_rc_header_t *g_rc_object_list = NULL;

// Allocate with reference counting header
static inline void *forge_rc_alloc(size_t size, int32_t type_tag) {
    size_t total_size = sizeof(forge_rc_header_t) + size;
    forge_rc_header_t *header = (forge_rc_header_t *)malloc(total_size);
    if (!header) {
        fprintf(stderr, "forge: out of memory\n");
        exit(1);
    }
    header->ref_count = 1;
    header->type_tag = type_tag;
    header->flags = 0;
    // Add to global list for cycle detection
    header->next = g_rc_object_list;
    g_rc_object_list = header;
    return (char *)header + sizeof(forge_rc_header_t);
}

// Increment reference count
static inline void forge_rc_retain(void *ptr) {
    if (ptr) {
        FORGE_RC_HEADER(ptr)->ref_count++;
    }
}

// Decrement reference count, free if zero
static inline void forge_rc_release(void *ptr, void (*destructor)(void *)) {
    if (ptr) {
        forge_rc_header_t *header = FORGE_RC_HEADER(ptr);
        header->ref_count--;
        if (header->ref_count <= 0) {
            if (destructor) {
                destructor(ptr);
            }
            free(header);
        }
    }
}

// Wrapper for release without destructor (used by macros)
static inline void forge_rc_release_no_dtor(void *ptr) {
    forge_rc_release(ptr, NULL);
}

// Unified string RC helper - checks is_heap and data, then applies action
#define FORGE_STRING_RC(s, action) \
    do { if ((s).is_heap && (s).data) { action((void *)(s).data); } } while(0)

// ---------------------------------------------------------------
// string type (with RC support)
// ---------------------------------------------------------------

typedef struct {
    const char *data;
    int64_t len;
    bool is_heap;  // true if data was heap-allocated (needs RC)
} forge_string_t;

// create a string from a C string literal (compile-time known length)
#define FORGE_STRING_LIT(s) ((forge_string_t){ .data = (s), .len = sizeof(s) - 1, .is_heap = false })

// create a string from a pointer and length (heap-allocated)
static inline forge_string_t forge_string_from(const char *data, int64_t len) {
    return (forge_string_t){ .data = data, .len = len, .is_heap = true };
}

// empty string constant
static const forge_string_t forge_string_empty = { .data = "", .len = 0, .is_heap = false };

// -------------------------------------------------------------
// Cycle Detection Infrastructure (Phase 6)
// -------------------------------------------------------------

// Simple cycle detector using "trial deletion" algorithm
// When an object's RC reaches 0, we check if it's part of a cycle
// by temporarily decrementing RC of all reachable objects

static inline void forge_rc_clear_marks(void) {
    forge_rc_header_t *curr = g_rc_object_list;
    while (curr) {
        curr->flags &= ~FORGE_RC_MARKED;
        curr = curr->next;
    }
}

// Check if object might be part of a cycle
// Returns true if object's RC > external reference count
// This is a simplified check - full implementation would scan all objects
static inline bool forge_rc_might_be_cyclic(void *ptr) {
    if (!ptr) return false;
    forge_rc_header_t *header = FORGE_RC_HEADER(ptr);
    // If RC > 0 but object is only reachable from itself (cycle)
    // This is detected when RC doesn't drop to 0 after releasing external refs
    return (header->flags & FORGE_RC_IN_CYCLE) != 0;
}

// Mark an object as potentially cyclic
static inline void forge_rc_mark_cyclic(void *ptr) {
    if (!ptr) return;
    FORGE_RC_HEADER(ptr)->flags |= FORGE_RC_IN_CYCLE;
}

// String destructor (frees the data buffer)
static inline void forge_string_destroy(void *ptr) {
    forge_string_t *s = (forge_string_t *)ptr;
    if (s->data && s->len > 0) {
        // Data was heap-allocated with the string struct
        free((void *)s->data);
    }
}

// Unified string RC helper - checks is_heap and data, then applies action
#define FORGE_STRING_RC(s, action) \
    do { if ((s).is_heap && (s).data) { action((void *)(s).data); } } while(0)

// Retain a string (increment RC)
static inline void forge_string_retain(forge_string_t s) {
    FORGE_STRING_RC(s, forge_rc_retain);
}

// Release a string (decrement RC, free if zero)
static inline void forge_string_release(forge_string_t s) {
    FORGE_STRING_RC(s, forge_rc_release_no_dtor);
}

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

// allocate a string buffer with RC header (for heap-allocated strings)
// returns pointer to data (after header)
static inline char *forge_str_alloc_rc(int64_t len) {
    return (char *)forge_rc_alloc((size_t)len + 1, FORGE_TYPE_STRING);
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
    char *buf = forge_str_alloc_rc(new_len);
    memcpy(buf, a.data, (size_t)a.len);
    memcpy(buf + a.len, b.data, (size_t)b.len);
    buf[new_len] = '\0';
    return (forge_string_t){ .data = buf, .len = new_len, .is_heap = true };
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
    // Return a view into the original data, NOT a heap-allocated copy
    // This string should NOT be retained/released separately
    return (forge_string_t){ .data = start, .len = (int64_t)(end - start), .is_heap = false };
}

static inline forge_string_t forge_string_to_upper(forge_string_t s) {
    char *buf = forge_str_alloc_rc(s.len);
    for (int64_t i = 0; i < s.len; i++) {
        char c = s.data[i];
        buf[i] = (c >= 'a' && c <= 'z') ? (char)(c - 32) : c;
    }
    buf[s.len] = '\0';
    return (forge_string_t){ .data = buf, .len = s.len, .is_heap = true };
}

static inline forge_string_t forge_string_to_lower(forge_string_t s) {
    char *buf = forge_str_alloc_rc(s.len);
    for (int64_t i = 0; i < s.len; i++) {
        char c = s.data[i];
        buf[i] = (c >= 'A' && c <= 'Z') ? (char)(c + 32) : c;
    }
    buf[s.len] = '\0';
    return (forge_string_t){ .data = buf, .len = s.len, .is_heap = true };
}

static inline forge_string_t forge_string_substring(forge_string_t s, int64_t start, int64_t end) {
    if (start < 0) start = 0;
    if (end > s.len) end = s.len;
    if (start >= end) return forge_string_empty;
    int64_t new_len = end - start;
    char *buf = forge_str_alloc_rc(new_len);
    memcpy(buf, s.data + start, (size_t)new_len);
    buf[new_len] = '\0';
    return (forge_string_t){ .data = buf, .len = new_len, .is_heap = true };
}

// index a single character by position. returns a 1-char string.
static inline forge_string_t forge_string_char_at(forge_string_t s, int64_t index) {
    if (index < 0 || index >= s.len) {
        fprintf(stderr, "forge: string index out of bounds (index %" PRId64 ", length %" PRId64 ")\n", index, s.len);
        exit(1);
    }
    char *buf = forge_str_alloc_rc(1);
    buf[0] = s.data[index];
    buf[1] = '\0';
    return (forge_string_t){ .data = buf, .len = 1, .is_heap = true };
}

// chr(Int) -> String: return a single-character string for the given ASCII code.
static inline forge_string_t forge_chr(int64_t code) {
    char *buf = forge_str_alloc_rc(1);
    buf[0] = (char)(code & 0xFF);
    buf[1] = '\0';
    return (forge_string_t){ .data = buf, .len = 1, .is_heap = true };
}

// ord(String) -> Int: return the byte value of the first character.
static inline int64_t forge_ord(forge_string_t s) {
    if (s.len == 0) return 0;
    return (int64_t)(unsigned char)s.data[0];
}

// bitwise operations on integers
static inline int64_t forge_bit_and(int64_t a, int64_t b) { return a & b; }
static inline int64_t forge_bit_or(int64_t a, int64_t b)  { return a | b; }
static inline int64_t forge_bit_xor(int64_t a, int64_t b) { return a ^ b; }
static inline int64_t forge_bit_not(int64_t a)             { return ~a; }
static inline int64_t forge_bit_shl(int64_t a, int64_t b) { return a << b; }
static inline int64_t forge_bit_shr(int64_t a, int64_t b) { return (int64_t)((uint64_t)a >> b); }

// replace all occurrences of `old` with `new_s` in `s`.
static inline forge_string_t forge_string_replace(forge_string_t s, forge_string_t old, forge_string_t new_s) {
    if (old.len == 0) {
        // empty pattern — return a copy
        char *buf = forge_str_alloc_rc(s.len);
        memcpy(buf, s.data, (size_t)s.len);
        buf[s.len] = '\0';
        return (forge_string_t){ .data = buf, .len = s.len, .is_heap = true };
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
        char *buf = forge_str_alloc_rc(s.len);
        memcpy(buf, s.data, (size_t)s.len);
        buf[s.len] = '\0';
        return (forge_string_t){ .data = buf, .len = s.len, .is_heap = true };
    }
    // second pass: build result
    int64_t delta = new_s.len - old.len;
    if (delta > 0 && count > INT64_MAX / delta) {
        fprintf(stderr, "forge: string replace overflow\n");
        exit(1);
    }
    int64_t growth = count * delta;
    if (growth > 0 && s.len > INT64_MAX - growth) {
        fprintf(stderr, "forge: string replace overflow\n");
        exit(1);
    }
    int64_t new_len = s.len + growth;
    char *buf = forge_str_alloc_rc(new_len);
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
    return (forge_string_t){ .data = buf, .len = new_len, .is_heap = true };
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
    if (s.len > INT64_MAX / n) {
        fprintf(stderr, "forge: string repeat overflow\n");
        exit(1);
    }
    int64_t new_len = s.len * n;
    char *buf = (char *)malloc((size_t)new_len + 1);
    if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    for (int64_t i = 0; i < n; i++) {
        memcpy(buf + i * s.len, s.data, (size_t)s.len);
    }
    buf[new_len] = '\0';
    return (forge_string_t){ .data = buf, .len = new_len, .is_heap = true };
}

// pad_left: pad string to given width with fill character (left-padded).
static inline forge_string_t forge_string_pad_left(forge_string_t s, int64_t width, forge_string_t fill) {
    if (width < 0) width = 0;
    if (s.len >= width || fill.len == 0) {
        char *buf = (char *)malloc((size_t)s.len + 1);
        if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
        memcpy(buf, s.data, (size_t)s.len);
        buf[s.len] = '\0';
        return (forge_string_t){ .data = buf, .len = s.len, .is_heap = true };
    }
    int64_t pad_len = width - s.len;
    char *buf = (char *)malloc((size_t)width + 1);
    if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    for (int64_t i = 0; i < pad_len; i++) {
        buf[i] = fill.data[i % fill.len];
    }
    memcpy(buf + pad_len, s.data, (size_t)s.len);
    buf[width] = '\0';
    return (forge_string_t){ .data = buf, .len = width, .is_heap = true };
}

// pad_right: pad string to given width with fill character (right-padded).
static inline forge_string_t forge_string_pad_right(forge_string_t s, int64_t width, forge_string_t fill) {
    if (width < 0) width = 0;
    if (s.len >= width || fill.len == 0) {
        char *buf = (char *)malloc((size_t)s.len + 1);
        if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
        memcpy(buf, s.data, (size_t)s.len);
        buf[s.len] = '\0';
        return (forge_string_t){ .data = buf, .len = s.len, .is_heap = true };
    }
    int64_t pad_len = width - s.len;
    char *buf = (char *)malloc((size_t)width + 1);
    if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    memcpy(buf, s.data, (size_t)s.len);
    for (int64_t i = 0; i < pad_len; i++) {
        buf[s.len + i] = fill.data[i % fill.len];
    }
    buf[width] = '\0';
    return (forge_string_t){ .data = buf, .len = width, .is_heap = true };
}

// split uses a forward-declared list type — defined after collection types
// (see forge_string_split below and forge_string_chars below)

// ---------------------------------------------------------------
// conversions to string
// ---------------------------------------------------------------

static inline forge_string_t forge_int_to_string(int64_t n) {
    char buf[32];
    int len = snprintf(buf, sizeof(buf), "%" PRId64, n);
    char *result = forge_str_alloc_rc(len);
    memcpy(result, buf, (size_t)len + 1);
    return (forge_string_t){ .data = result, .len = len, .is_heap = true };
}

static inline forge_string_t forge_float_to_string(double n) {
    char buf[64];
    int len = snprintf(buf, sizeof(buf), "%g", n);
    char *result = forge_str_alloc_rc(len);
    memcpy(result, buf, (size_t)len + 1);
    return (forge_string_t){ .data = result, .len = len, .is_heap = true };
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
    int64_t cap;
} forge_list_impl_t;

typedef struct {
    forge_list_impl_t *impl;
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
} forge_map_impl_t;

typedef struct {
    forge_map_impl_t *impl;
} forge_map_t;

// Set[T] — unique element collection. same layout as list for now.
typedef forge_list_t forge_set_t;

// ---------------------------------------------------------------
// collection ARC helpers
// ---------------------------------------------------------------

// Retain an element at the given address (for string types)
static inline void forge_elem_retain_string(void *elem_addr) {
    forge_string_t *s = (forge_string_t *)elem_addr;
    if (s->is_heap && s->data) {
        forge_rc_retain((void *)s->data);
    }
}

// Release an element at the given address (for string types)
static inline void forge_elem_release_string(void *elem_addr) {
    forge_string_t *s = (forge_string_t *)elem_addr;
    if (s->is_heap && s->data) {
        forge_rc_release((void *)s->data, NULL);
    }
}

// Retain all elements in a list (for string elements)
static inline void forge_list_retain_all_strings(forge_list_t list) {
    if (!list.impl || !list.impl->data) return;
    int64_t len = list.impl->len;
    forge_string_t *items = (forge_string_t *)list.impl->data;
    for (int64_t i = 0; i < len; i++) {
        if (items[i].is_heap && items[i].data) {
            forge_rc_retain((void *)items[i].data);
        }
    }
}

// Release all elements in a list (for string elements)
static inline void forge_list_release_all_strings(forge_list_t list) {
    if (!list.impl || !list.impl->data) return;
    int64_t len = list.impl->len;
    forge_string_t *items = (forge_string_t *)list.impl->data;
    for (int64_t i = 0; i < len; i++) {
        if (items[i].is_heap && items[i].data) {
            forge_rc_release((void *)items[i].data, NULL);
        }
    }
}

// Retain all keys and values in a map (for string keys/values)
static inline void forge_map_retain_all_strings(forge_map_t map) {
    if (!map.impl) return;
    int64_t len = map.impl->len;
    // Retain keys
    forge_string_t *keys = (forge_string_t *)map.impl->keys;
    for (int64_t i = 0; i < len; i++) {
        if (keys[i].is_heap && keys[i].data) {
            forge_rc_retain((void *)keys[i].data);
        }
    }
    // Retain values
    forge_string_t *vals = (forge_string_t *)map.impl->values;
    for (int64_t i = 0; i < len; i++) {
        if (vals[i].is_heap && vals[i].data) {
            forge_rc_retain((void *)vals[i].data);
        }
    }
}

// Release all keys and values in a map (for string keys/values)
static inline void forge_map_release_all_strings(forge_map_t map) {
    if (!map.impl) return;
    int64_t len = map.impl->len;
    // Release keys
    forge_string_t *keys = (forge_string_t *)map.impl->keys;
    for (int64_t i = 0; i < len; i++) {
        if (keys[i].is_heap && keys[i].data) {
            forge_rc_release((void *)keys[i].data, NULL);
        }
    }
    // Release values
    forge_string_t *vals = (forge_string_t *)map.impl->values;
    for (int64_t i = 0; i < len; i++) {
        if (vals[i].is_heap && vals[i].data) {
            forge_rc_release((void *)vals[i].data, NULL);
        }
    }
}

static inline forge_list_t forge_list_empty(void) {
    forge_list_impl_t *impl = (forge_list_impl_t *)calloc(1, sizeof(forge_list_impl_t));
    if (!impl) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    return (forge_list_t){ .impl = impl };
}

static inline forge_map_t forge_map_empty(void) {
    forge_map_impl_t *impl = (forge_map_impl_t *)calloc(1, sizeof(forge_map_impl_t));
    if (!impl) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    return (forge_map_t){ .impl = impl };
}

static inline int64_t forge_list_len(forge_list_t list) {
    return list.impl ? list.impl->len : 0;
}

static inline void *forge_list_data(forge_list_t list) {
    return list.impl ? list.impl->data : NULL;
}

static inline int64_t forge_map_len(forge_map_t map) {
    return map.impl ? map.impl->len : 0;
}

static inline void *forge_map_keys_data(forge_map_t map) {
    return map.impl ? map.impl->keys : NULL;
}

static inline void *forge_map_values_data(forge_map_t map) {
    return map.impl ? map.impl->values : NULL;
}

static inline int32_t *forge_map_buckets_data(forge_map_t map) {
    return map.impl ? map.impl->buckets : NULL;
}

static inline int32_t forge_map_cap(forge_map_t map) {
    return map.impl ? map.impl->cap : 0;
}

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
    forge_map_impl_t *impl = map->impl;
    int32_t mask = impl->cap - 1;
    for (int32_t i = 0; i < impl->cap; i++) impl->buckets[i] = -1;
    forge_string_t *keys = (forge_string_t *)impl->keys;
    for (int32_t i = 0; i < (int32_t)impl->len; i++) {
        uint64_t h = forge_hash_string(keys[i]);
        int32_t slot = (int32_t)(h & (uint64_t)mask);
        while (impl->buckets[slot] != -1) slot = (slot + 1) & mask;
        impl->buckets[slot] = i;
    }
}

// rebuild the bucket index from the dense arrays (integer keys)
static inline void forge_map_rebuild_int(forge_map_t *map) {
    forge_map_impl_t *impl = map->impl;
    int32_t mask = impl->cap - 1;
    for (int32_t i = 0; i < impl->cap; i++) impl->buckets[i] = -1;
    int64_t *keys = (int64_t *)impl->keys;
    for (int32_t i = 0; i < (int32_t)impl->len; i++) {
        uint64_t h = forge_hash_int(keys[i]);
        int32_t slot = (int32_t)(h & (uint64_t)mask);
        while (impl->buckets[slot] != -1) slot = (slot + 1) & mask;
        impl->buckets[slot] = i;
    }
}

// ensure the map has a hash index with room for at least one more entry.
// string key variant — grows at 75% load.
static inline void forge_map_ensure_index_string(forge_map_t *map) {
    if (!map->impl) {
        map->impl = (forge_map_impl_t *)calloc(1, sizeof(forge_map_impl_t));
        if (!map->impl) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    }
    forge_map_impl_t *impl = map->impl;
    if (impl->cap == 0) {
        impl->cap = 8;
        impl->buckets = forge_map_alloc_buckets(8);
        forge_map_rebuild_string(map);
        return;
    }
    // grow at 75% load: len * 4 >= cap * 3
    if ((int32_t)impl->len * 4 >= impl->cap * 3) {
        free(impl->buckets);
        impl->cap *= 2;
        impl->buckets = forge_map_alloc_buckets(impl->cap);
        forge_map_rebuild_string(map);
    }
}

// integer key variant
static inline void forge_map_ensure_index_int(forge_map_t *map) {
    if (!map->impl) {
        map->impl = (forge_map_impl_t *)calloc(1, sizeof(forge_map_impl_t));
        if (!map->impl) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    }
    forge_map_impl_t *impl = map->impl;
    if (impl->cap == 0) {
        impl->cap = 8;
        impl->buckets = forge_map_alloc_buckets(8);
        forge_map_rebuild_int(map);
        return;
    }
    if ((int32_t)impl->len * 4 >= impl->cap * 3) {
        free(impl->buckets);
        impl->cap *= 2;
        impl->buckets = forge_map_alloc_buckets(impl->cap);
        forge_map_rebuild_int(map);
    }
}

// ---------------------------------------------------------------
// collection creation
// ---------------------------------------------------------------

// create a list from an initializer array. copies the data.
static inline forge_list_t forge_list_create(int64_t len, int64_t elem_size, const void *init) {
    forge_list_impl_t *impl = (forge_list_impl_t *)calloc(1, sizeof(forge_list_impl_t));
    if (!impl) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    forge_list_t list = { .impl = impl };
    impl->len = len;
    impl->cap = len;
    if (len == 0 || !init) return list;
    if (elem_size > 0 && (size_t)len > SIZE_MAX / (size_t)elem_size) {
        fprintf(stderr, "forge: list too large\n");
        exit(1);
    }
    impl->data = malloc((size_t)len * (size_t)elem_size);
    if (!impl->data) {
        fprintf(stderr, "forge: out of memory\n");
        exit(1);
    }
    memcpy(impl->data, init, (size_t)len * (size_t)elem_size);
    return list;
}

// create a map from parallel key/value arrays. copies both and builds hash index.
// key_size is used to distinguish string keys (sizeof(forge_string_t)) from int keys.
static inline forge_map_t forge_map_create(int64_t len, int64_t key_size, int64_t val_size,
                                           const void *init_keys, const void *init_vals) {
    forge_map_impl_t *impl = (forge_map_impl_t *)calloc(1, sizeof(forge_map_impl_t));
    if (!impl) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    forge_map_t map = { .impl = impl };
    impl->len = len;
    if (len == 0) return map;
    if (key_size > 0 && len > (int64_t)(SIZE_MAX / (size_t)key_size)) {
        fprintf(stderr, "forge: map too large\n");
        exit(1);
    }
    if (val_size > 0 && len > (int64_t)(SIZE_MAX / (size_t)val_size)) {
        fprintf(stderr, "forge: map too large\n");
        exit(1);
    }
    impl->keys = malloc((size_t)len * (size_t)key_size);
    impl->values = malloc((size_t)len * (size_t)val_size);
    if (!impl->keys || !impl->values) {
        fprintf(stderr, "forge: out of memory\n");
        exit(1);
    }
    memcpy(impl->keys, init_keys, (size_t)len * (size_t)key_size);
    memcpy(impl->values, init_vals, (size_t)len * (size_t)val_size);
    // pick initial capacity: next power of 2 that keeps load < 75%
    int32_t needed = (int32_t)((len * 4 + 2) / 3); // ceil(len / 0.75)
    int32_t cap = 8;
    while (cap < needed) cap *= 2;
    impl->cap = cap;
    impl->buckets = forge_map_alloc_buckets(cap);
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
    (forge_bounds_check((idx), forge_list_len(list)), ((type *)forge_list_data(list))[(idx)])

#define FORGE_MAP_KEY_AT(map, type, idx) \
    (forge_bounds_check((idx), forge_map_len(map)), ((type *)forge_map_keys_data(map))[(idx)])

// look up a value in a map by integer key. returns pointer to the value slot,
// or NULL if not found. O(1) average via hash probing.
static inline void *forge_map_get_by_int(forge_map_t map, int64_t key, int64_t val_size) {
    if (!map.impl || map.impl->cap == 0) return NULL;
    int32_t mask = map.impl->cap - 1;
    uint64_t h = forge_hash_int(key);
    int32_t slot = (int32_t)(h & (uint64_t)mask);
    int64_t *keys = (int64_t *)map.impl->keys;
    while (1) {
        int32_t idx = map.impl->buckets[slot];
        if (idx == -1) return NULL;
        if (keys[idx] == key) return (char *)map.impl->values + idx * val_size;
        slot = (slot + 1) & mask;
    }
}

// look up a value in a map by string key. returns pointer to the value slot,
// or NULL if not found. O(1) average via hash probing.
static inline void *forge_map_get_by_string(forge_map_t map, forge_string_t key, int64_t val_size) {
    if (!map.impl || map.impl->cap == 0) return NULL;
    int32_t mask = map.impl->cap - 1;
    uint64_t h = forge_hash_string(key);
    int32_t slot = (int32_t)(h & (uint64_t)mask);
    forge_string_t *keys = (forge_string_t *)map.impl->keys;
    while (1) {
        int32_t idx = map.impl->buckets[slot];
        if (idx == -1) return NULL;
        if (forge_string_eq(keys[idx], key)) return (char *)map.impl->values + idx * val_size;
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
    if (!list->impl) {
        list->impl = (forge_list_impl_t *)calloc(1, sizeof(forge_list_impl_t));
        if (!list->impl) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    }
    forge_list_impl_t *impl = list->impl;
    int64_t new_len = impl->len + 1;
    if (elem_size > 0 && (size_t)new_len > SIZE_MAX / (size_t)elem_size) {
        fprintf(stderr, "forge: list too large\n");
        exit(1);
    }
    void *new_data = realloc(impl->data, (size_t)(new_len * elem_size));
    if (!new_data) {
        fprintf(stderr, "forge: out of memory\n");
        exit(1);
    }
    impl->data = new_data;
    impl->cap = new_len;
    memcpy((char *)impl->data + impl->len * elem_size, elem, (size_t)elem_size);
    impl->len = new_len;
}

// insert or update a key-value pair in a map (string keys).
// if the key already exists, updates the value in place.
// O(1) average via hash probing.
static inline void forge_map_set_by_string(forge_map_t *map, forge_string_t key,
                                            const void *val, int64_t key_size, int64_t val_size) {
    if (!map->impl) {
        map->impl = (forge_map_impl_t *)calloc(1, sizeof(forge_map_impl_t));
        if (!map->impl) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    }
    forge_map_impl_t *impl = map->impl;
    // check for existing key via hash probe
    if (impl->cap > 0) {
        int32_t mask = impl->cap - 1;
        uint64_t h = forge_hash_string(key);
        int32_t slot = (int32_t)(h & (uint64_t)mask);
        forge_string_t *keys = (forge_string_t *)impl->keys;
        while (1) {
            int32_t idx = impl->buckets[slot];
            if (idx == -1) break;
            if (forge_string_eq(keys[idx], key)) {
                memcpy((char *)impl->values + idx * val_size, val, (size_t)val_size);
                return;
            }
            slot = (slot + 1) & mask;
        }
    }
    // ensure hash index has room
    forge_map_ensure_index_string(map);
    // grow dense arrays
    int64_t new_len = impl->len + 1;
    void *new_keys = realloc(impl->keys, (size_t)(new_len * key_size));
    if (!new_keys) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    impl->keys = new_keys;
    void *new_vals = realloc(impl->values, (size_t)(new_len * val_size));
    if (!new_vals) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    impl->values = new_vals;
    memcpy((char *)impl->keys + impl->len * key_size, &key, (size_t)key_size);
    memcpy((char *)impl->values + impl->len * val_size, val, (size_t)val_size);
    // insert into hash index
    int32_t mask = impl->cap - 1;
    uint64_t h = forge_hash_string(key);
    int32_t slot = (int32_t)(h & (uint64_t)mask);
    while (impl->buckets[slot] != -1) slot = (slot + 1) & mask;
    impl->buckets[slot] = (int32_t)impl->len;
    impl->len = new_len;
}

// insert or update a key-value pair in a map (integer keys).
// O(1) average via hash probing.
static inline void forge_map_set_by_int(forge_map_t *map, int64_t key,
                                         const void *val, int64_t key_size, int64_t val_size) {
    if (!map->impl) {
        map->impl = (forge_map_impl_t *)calloc(1, sizeof(forge_map_impl_t));
        if (!map->impl) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    }
    forge_map_impl_t *impl = map->impl;
    // check for existing key via hash probe
    if (impl->cap > 0) {
        int32_t mask = impl->cap - 1;
        uint64_t h = forge_hash_int(key);
        int32_t slot = (int32_t)(h & (uint64_t)mask);
        int64_t *keys = (int64_t *)impl->keys;
        while (1) {
            int32_t idx = impl->buckets[slot];
            if (idx == -1) break;
            if (keys[idx] == key) {
                memcpy((char *)impl->values + idx * val_size, val, (size_t)val_size);
                return;
            }
            slot = (slot + 1) & mask;
        }
    }
    // ensure hash index has room
    forge_map_ensure_index_int(map);
    // grow dense arrays
    int64_t new_len = impl->len + 1;
    void *new_keys = realloc(impl->keys, (size_t)(new_len * key_size));
    if (!new_keys) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    impl->keys = new_keys;
    void *new_vals = realloc(impl->values, (size_t)(new_len * val_size));
    if (!new_vals) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    impl->values = new_vals;
    memcpy((char *)impl->keys + impl->len * key_size, &key, (size_t)key_size);
    memcpy((char *)impl->values + impl->len * val_size, val, (size_t)val_size);
    // insert into hash index
    int32_t mask = impl->cap - 1;
    uint64_t h = forge_hash_int(key);
    int32_t slot = (int32_t)(h & (uint64_t)mask);
    while (impl->buckets[slot] != -1) slot = (slot + 1) & mask;
    impl->buckets[slot] = (int32_t)impl->len;
    impl->len = new_len;
}

// add an element to a set (no-op if already present).
// uses linear scan for deduplication — fine for small sets.
static inline void forge_set_add(forge_set_t *set, const void *elem, int64_t elem_size) {
    // check if element already exists
    int64_t len = forge_list_len(*set);
    void *data = forge_list_data(*set);
    for (int64_t i = 0; i < len; i++) {
        if (memcmp((char *)data + i * elem_size, elem, (size_t)elem_size) == 0) {
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
    if (!list->impl) return;
    forge_list_impl_t *impl = list->impl;
    forge_bounds_check(idx, impl->len);
    int64_t remaining = impl->len - idx - 1;
    if (remaining > 0) {
        memmove((char *)impl->data + idx * elem_size,
                (char *)impl->data + (idx + 1) * elem_size,
                (size_t)(remaining * elem_size));
    }
    impl->len--;
}

// list — linear scan for element (generic, uses memcmp)
static inline bool forge_list_contains(forge_list_t list, const void *elem, int64_t elem_size) {
    int64_t len = forge_list_len(list);
    void *data = forge_list_data(list);
    for (int64_t i = 0; i < len; i++) {
        if (memcmp((char *)data + i * elem_size, elem, (size_t)elem_size) == 0)
            return true;
    }
    return false;
}

// list — linear scan for string element
static inline bool forge_list_contains_string(forge_list_t list, forge_string_t s) {
    forge_string_t *items = (forge_string_t *)forge_list_data(list);
    for (int64_t i = 0; i < forge_list_len(list); i++) {
        if (forge_string_eq(items[i], s)) return true;
    }
    return false;
}

// list — reverse in place
static inline void forge_list_reverse(forge_list_t *list, int64_t elem_size) {
    if (!list->impl || list->impl->len < 2) return;
    forge_list_impl_t *impl = list->impl;
    // use stack buffer for small elements, heap for large ones
    char stack_buf[64];
    char *tmp = (elem_size <= 64) ? stack_buf : (char *)malloc((size_t)elem_size);
    if (!tmp) return;
    for (int64_t i = 0; i < impl->len / 2; i++) {
        int64_t j = impl->len - 1 - i;
        char *a = (char *)impl->data + i * elem_size;
        char *b = (char *)impl->data + j * elem_size;
        memcpy(tmp, a, (size_t)elem_size);
        memcpy(a, b, (size_t)elem_size);
        memcpy(b, tmp, (size_t)elem_size);
    }
    if (tmp != stack_buf) free(tmp);
}

// list — clear (free data and reset)
static inline void forge_list_clear(forge_list_t *list) {
    if (!list->impl) return;
    free(list->impl->data);
    list->impl->data = NULL;
    list->impl->len = 0;
    list->impl->cap = 0;
}

// map — remove by string key. shifts dense arrays and rebuilds hash index.
static inline void forge_map_remove_by_string(forge_map_t *map, forge_string_t key,
                                                int64_t key_size, int64_t val_size) {
    if (!map->impl || map->impl->cap == 0) return;
    forge_map_impl_t *impl = map->impl;
    // find via hash probe
    int32_t mask = impl->cap - 1;
    uint64_t h = forge_hash_string(key);
    int32_t slot = (int32_t)(h & (uint64_t)mask);
    forge_string_t *keys = (forge_string_t *)impl->keys;
    while (1) {
        int32_t idx = impl->buckets[slot];
        if (idx == -1) return; // not found
        if (forge_string_eq(keys[idx], key)) {
            // shift dense arrays to preserve insertion order
            int64_t remaining = impl->len - idx - 1;
            if (remaining > 0) {
                memmove((char *)impl->keys + idx * key_size,
                        (char *)impl->keys + (idx + 1) * key_size,
                        (size_t)(remaining * key_size));
                memmove((char *)impl->values + idx * val_size,
                        (char *)impl->values + (idx + 1) * val_size,
                        (size_t)(remaining * val_size));
            }
            impl->len--;
            forge_map_rebuild_string(map);
            return;
        }
        slot = (slot + 1) & mask;
    }
}

// map — remove by integer key. shifts dense arrays and rebuilds hash index.
static inline void forge_map_remove_by_int(forge_map_t *map, int64_t key,
                                            int64_t key_size, int64_t val_size) {
    if (!map->impl || map->impl->cap == 0) return;
    forge_map_impl_t *impl = map->impl;
    int32_t mask = impl->cap - 1;
    uint64_t h = forge_hash_int(key);
    int32_t slot = (int32_t)(h & (uint64_t)mask);
    int64_t *keys = (int64_t *)impl->keys;
    while (1) {
        int32_t idx = impl->buckets[slot];
        if (idx == -1) return; // not found
        if (keys[idx] == key) {
            int64_t remaining = impl->len - idx - 1;
            if (remaining > 0) {
                memmove((char *)impl->keys + idx * key_size,
                        (char *)impl->keys + (idx + 1) * key_size,
                        (size_t)(remaining * key_size));
                memmove((char *)impl->values + idx * val_size,
                        (char *)impl->values + (idx + 1) * val_size,
                        (size_t)(remaining * val_size));
            }
            impl->len--;
            forge_map_rebuild_int(map);
            return;
        }
        slot = (slot + 1) & mask;
    }
}

// map — check key existence (string keys). O(1) average.
static inline bool forge_map_contains_key_string(forge_map_t map, forge_string_t key) {
    if (!map.impl || map.impl->cap == 0) return false;
    int32_t mask = map.impl->cap - 1;
    uint64_t h = forge_hash_string(key);
    int32_t slot = (int32_t)(h & (uint64_t)mask);
    forge_string_t *keys = (forge_string_t *)map.impl->keys;
    while (1) {
        int32_t idx = map.impl->buckets[slot];
        if (idx == -1) return false;
        if (forge_string_eq(keys[idx], key)) return true;
        slot = (slot + 1) & mask;
    }
}

// map — check key existence (integer keys). O(1) average.
static inline bool forge_map_contains_key_int(forge_map_t map, int64_t key) {
    if (!map.impl || map.impl->cap == 0) return false;
    int32_t mask = map.impl->cap - 1;
    uint64_t h = forge_hash_int(key);
    int32_t slot = (int32_t)(h & (uint64_t)mask);
    int64_t *keys = (int64_t *)map.impl->keys;
    while (1) {
        int32_t idx = map.impl->buckets[slot];
        if (idx == -1) return false;
        if (keys[idx] == key) return true;
        slot = (slot + 1) & mask;
    }
}

// map — get all keys as a list
static inline forge_list_t forge_map_keys(forge_map_t map, int64_t key_size) {
    int64_t len = forge_map_len(map);
    if (len == 0) return forge_list_empty();
    return forge_list_create(len, key_size, forge_map_keys_data(map));
}

// map — get all values as a list
static inline forge_list_t forge_map_values(forge_map_t map, int64_t val_size) {
    int64_t len = forge_map_len(map);
    if (len == 0) return forge_list_empty();
    return forge_list_create(len, val_size, forge_map_values_data(map));
}

// map — clear (free data, hash index, and reset)
static inline void forge_map_clear(forge_map_t *map) {
    if (!map->impl) return;
    free(map->impl->keys);
    free(map->impl->values);
    free(map->impl->buckets);
    map->impl->keys = NULL;
    map->impl->values = NULL;
    map->impl->buckets = NULL;
    map->impl->len = 0;
    map->impl->cap = 0;
}

// set — remove by generic element
static inline void forge_set_remove(forge_set_t *set, const void *elem, int64_t elem_size) {
    int64_t len = forge_list_len(*set);
    void *data = forge_list_data(*set);
    for (int64_t i = 0; i < len; i++) {
        if (memcmp((char *)data + i * elem_size, elem, (size_t)elem_size) == 0) {
            forge_list_remove(set, i, elem_size);
            return;
        }
    }
}

// set — remove by string element
static inline void forge_set_remove_string(forge_set_t *set, forge_string_t s) {
    forge_string_t *items = (forge_string_t *)forge_list_data(*set);
    for (int64_t i = 0; i < forge_list_len(*set); i++) {
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
    forge_list_t result = forge_list_empty();
    if (sep.len == 0) {
        // split on empty separator: return each character
        for (int64_t i = 0; i < s.len; i++) {
            char *ch = forge_str_alloc_rc(1);
            ch[0] = s.data[i];
            ch[1] = '\0';
            forge_string_t part = { .data = ch, .len = 1, .is_heap = true };
            forge_list_push(&result, &part, sizeof(forge_string_t));
        }
        return result;
    }
    int64_t start = 0;
    for (int64_t i = 0; i + sep.len <= s.len; i++) {
        if (memcmp(s.data + i, sep.data, (size_t)sep.len) == 0) {
            int64_t part_len = i - start;
            char *buf = forge_str_alloc_rc(part_len);
            memcpy(buf, s.data + start, (size_t)part_len);
            buf[part_len] = '\0';
            forge_string_t part = { .data = buf, .len = part_len, .is_heap = true };
            forge_list_push(&result, &part, sizeof(forge_string_t));
            i += sep.len - 1;
            start = i + 1;
        }
    }
    // remaining part after last separator
    int64_t part_len = s.len - start;
    char *buf = forge_str_alloc_rc(part_len);
    memcpy(buf, s.data + start, (size_t)part_len);
    buf[part_len] = '\0';
    forge_string_t part = { .data = buf, .len = part_len, .is_heap = true };
    forge_list_push(&result, &part, sizeof(forge_string_t));
    return result;
}

// join a List[String] with a separator. returns a new string.
static inline forge_string_t forge_list_join(forge_list_t list, forge_string_t sep) {
    int64_t len = forge_list_len(list);
    if (len == 0) return forge_string_empty;
    forge_string_t *items = (forge_string_t *)forge_list_data(list);
    if (len == 1) {
        char *buf = forge_str_alloc_rc(items[0].len);
        memcpy(buf, items[0].data, (size_t)items[0].len);
        buf[items[0].len] = '\0';
        return (forge_string_t){ .data = buf, .len = items[0].len, .is_heap = true };
    }
    // compute total length
    int64_t total = 0;
    for (int64_t i = 0; i < len; i++) {
        total += items[i].len;
    }
    total += (len - 1) * sep.len;
    char *buf = forge_str_alloc_rc(total);
    int64_t pos = 0;
    for (int64_t i = 0; i < len; i++) {
        if (i > 0) {
            memcpy(buf + pos, sep.data, (size_t)sep.len);
            pos += sep.len;
        }
        memcpy(buf + pos, items[i].data, (size_t)items[i].len);
        pos += items[i].len;
    }
    buf[total] = '\0';
    return (forge_string_t){ .data = buf, .len = total, .is_heap = true };
}

// string — chars(): split into a list of single-character strings.
static inline forge_list_t forge_string_chars(forge_string_t s) {
    forge_list_t result = forge_list_empty();
    for (int64_t i = 0; i < s.len; i++) {
        char *ch = forge_str_alloc_rc(1);
        ch[0] = s.data[i];
        ch[1] = '\0';
        forge_string_t part = { .data = ch, .len = 1, .is_heap = true };
        forge_list_push(&result, &part, sizeof(forge_string_t));
    }
    return result;
}

// ---------------------------------------------------------------
// list — index_of, slice, sort
// ---------------------------------------------------------------

// list — find first occurrence of element. returns -1 if not found.
static inline int64_t forge_list_index_of(forge_list_t list, const void *elem, int64_t elem_size) {
    int64_t len = forge_list_len(list);
    void *data = forge_list_data(list);
    for (int64_t i = 0; i < len; i++) {
        if (memcmp((char *)data + i * elem_size, elem, (size_t)elem_size) == 0)
            return i;
    }
    return -1;
}

// list — find first occurrence of string element. returns -1 if not found.
static inline int64_t forge_list_index_of_string(forge_list_t list, forge_string_t s) {
    forge_string_t *items = (forge_string_t *)forge_list_data(list);
    for (int64_t i = 0; i < forge_list_len(list); i++) {
        if (forge_string_eq(items[i], s)) return i;
    }
    return -1;
}

// list — slice: return a new list from start to end (exclusive).
static inline forge_list_t forge_list_slice(forge_list_t list, int64_t start, int64_t end, int64_t elem_size) {
    int64_t len = forge_list_len(list);
    if (start < 0) start = 0;
    if (end > len) end = len;
    if (start >= end) return forge_list_empty();
    int64_t new_len = end - start;
    return forge_list_create(new_len, elem_size, (char *)forge_list_data(list) + start * elem_size);
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
    int64_t len = forge_list_len(list);
    if (len <= 1) {
        return forge_list_create(len, elem_size, forge_list_data(list));
    }
    int64_t total = len * elem_size;
    void *buf = malloc((size_t)total);
    if (!buf) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    memcpy(buf, forge_list_data(list), (size_t)total);
    int (*cmp)(const void *, const void *) = NULL;
    if (type_tag == 0) cmp = forge_cmp_int;
    else if (type_tag == 1) cmp = forge_cmp_float;
    else cmp = forge_cmp_string;
    qsort(buf, (size_t)len, (size_t)elem_size, cmp);
    forge_list_t sorted = forge_list_create(len, elem_size, buf);
    free(buf);
    return sorted;
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
    forge_list_t result = forge_list_empty();
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

static inline void forge_print_err(forge_string_t s) {
    fwrite(s.data, 1, (size_t)s.len, stderr);
    fputc('\n', stderr);
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
    char *buf = forge_str_alloc_rc(n);
    for (int64_t i = 0; i < n; i++)
        buf[i] = alphanum[lrand48() % 62];
    buf[n] = '\0';
    return (forge_string_t){ .data = buf, .len = n, .is_heap = true };
}

// format_time(epoch_ms, format_string) -> String — strftime wrapper
static inline forge_string_t forge_format_time(int64_t epoch_ms, forge_string_t fmt) {
    time_t secs = (time_t)(epoch_ms / 1000);
    struct tm *t = localtime(&secs);
    char *fmt_cstr = forge_cstr(fmt);
    char buf[256];
    size_t len = strftime(buf, sizeof(buf), fmt_cstr, t);
    free(fmt_cstr);
    char *result = forge_str_alloc_rc((int64_t)len);
    memcpy(result, buf, len);
    result[len] = '\0';
    return (forge_string_t){ .data = result, .len = (int64_t)len, .is_heap = true };
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
    return (forge_string_t){ .data = copy, .len = len, .is_heap = true };
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

// fmt_hex, fmt_oct, fmt_bin moved to std/fmt.fg (native forge)

// fmt_float(Float, Int) -> String: format float with fixed decimal places
static inline forge_string_t forge_fmt_float(double n, int64_t decimals) {
    char buf[64];
    int len = snprintf(buf, sizeof(buf), "%.*f", (int)decimals, n);
    char *result = (char *)malloc((size_t)len + 1);
    if (!result) { fprintf(stderr, "forge: out of memory\n"); exit(1); }
    memcpy(result, buf, (size_t)len + 1);
    return (forge_string_t){ .data = result, .len = len, .is_heap = true };
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
    forge_list_t result = forge_list_empty();
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
        forge_string_t s = { .data = buf, .len = nlen, .is_heap = true };
        forge_list_push(&result, &s, sizeof(forge_string_t));
    }
    closedir(d);
    return result;
}

// path manipulation moved to std/path.fg (native forge)

// logging moved to std/log.fg (native forge)


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
        if (new_cap > (int64_t)(SIZE_MAX / (size_t)ch->elem_size)) {
            fprintf(stderr, "forge: channel buffer overflow\n");
            exit(1);
        }
        void *new_buf = malloc((size_t)new_cap * (size_t)ch->elem_size);
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

// Destroy a channel and free all associated memory
static inline void forge_channel_destroy(forge_channel_t *ch) {
    pthread_mutex_lock(&ch->mu);
    free(ch->buffer);
    pthread_mutex_unlock(&ch->mu);
    pthread_mutex_destroy(&ch->mu);
    pthread_cond_destroy(&ch->not_empty);
    free(ch);
}

static inline int64_t forge_channel_len(forge_channel_t *ch) {
    pthread_mutex_lock(&ch->mu);
    int64_t n = ch->len;
    pthread_mutex_unlock(&ch->mu);
    return n;
}

// TOML parsing moved to std/toml.fg (native forge)

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
    // Security fix: validate buffer size to prevent overflow
    // Reject negative values and limit to 1MB max
    if (max_bytes <= 0 || max_bytes > 1048576) {
        *out = forge_string_empty;
        return false;
    }
    char *buf = forge_str_alloc_rc(max_bytes);
    ssize_t n = read((int)fd, buf, (size_t)max_bytes);
    if (n < 0) { free(FORGE_RC_HEADER(buf)); return false; }
    buf[n] = '\0';
    *out = (forge_string_t){ .data = buf, .len = (int64_t)n, .is_heap = true };
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
    char *buf = forge_str_alloc_rc(len);
    memcpy(buf, ip_buf, (size_t)len);
    buf[len] = '\0';
    *out = (forge_string_t){ .data = buf, .len = len, .is_heap = true };
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
    if (wg->__count <= 0) {
        pthread_mutex_unlock(&wg->__mutex);
        return;
    }
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
static pthread_mutex_t forge_process_mutex = PTHREAD_MUTEX_INITIALIZER;

// spawn a child process. returns handle index, or -1 on error.
// the command string is split on spaces (no shell interpretation).
static inline bool forge_process_spawn_impl(forge_string_t cmd, int64_t *out_handle) {
    // check capacity under lock
    pthread_mutex_lock(&forge_process_mutex);
    if (forge_process_count >= FORGE_MAX_PROCESSES) {
        pthread_mutex_unlock(&forge_process_mutex);
        *out_handle = -1;
        return false;
    }
    pthread_mutex_unlock(&forge_process_mutex);

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

    // increment counter and store under lock
    pthread_mutex_lock(&forge_process_mutex);
    int64_t handle = forge_process_count++;
    forge_process_pool[handle].pid = pid;
    forge_process_pool[handle].stdin_fd = stdin_pipe[1];
    forge_process_pool[handle].stdout_fd = stdout_pipe[0];
    forge_process_pool[handle].stderr_fd = stderr_pipe[0];
    forge_process_pool[handle].alive = true;
    pthread_mutex_unlock(&forge_process_mutex);
    *out_handle = handle;
    return true;
}

// write string to child's stdin. returns bytes written or -1 on error.
static inline bool forge_process_write_impl(int64_t handle, forge_string_t data, int64_t *out) {
    pthread_mutex_lock(&forge_process_mutex);
    if (handle < 0 || handle >= forge_process_count) { 
        pthread_mutex_unlock(&forge_process_mutex);
        *out = -1; 
        return false; 
    }
    forge_process_t *p = &forge_process_pool[handle];
    pthread_mutex_unlock(&forge_process_mutex);
    ssize_t n = write(p->stdin_fd, data.data, (size_t)data.len);
    if (n < 0) { *out = -1; return false; }
    *out = (int64_t)n;
    return true;
}

// read from child's stdout. reads up to max_bytes.
static inline bool forge_process_read_impl(int64_t handle, int64_t max_bytes, forge_string_t *out) {
    pthread_mutex_lock(&forge_process_mutex);
    if (handle < 0 || handle >= forge_process_count) { 
        pthread_mutex_unlock(&forge_process_mutex);
        *out = forge_string_empty; 
        return false; 
    }
    if (max_bytes <= 0) max_bytes = 4096;
    forge_process_t *p = &forge_process_pool[handle];
    pthread_mutex_unlock(&forge_process_mutex);
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
    pthread_mutex_lock(&forge_process_mutex);
    if (handle < 0 || handle >= forge_process_count) { 
        pthread_mutex_unlock(&forge_process_mutex);
        *out = forge_string_empty; 
        return false; 
    }
    if (max_bytes <= 0) max_bytes = 4096;
    forge_process_t *p = &forge_process_pool[handle];
    pthread_mutex_unlock(&forge_process_mutex);
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
    pthread_mutex_lock(&forge_process_mutex);
    if (handle < 0 || handle >= forge_process_count) {
        pthread_mutex_unlock(&forge_process_mutex);
        return -1;
    }
    forge_process_t *p = &forge_process_pool[handle];
    pthread_mutex_unlock(&forge_process_mutex);
    int status = 0;
    waitpid(p->pid, &status, 0);
    p->alive = false;
    if (WIFEXITED(status)) return WEXITSTATUS(status);
    return -1;
}

// kill the child process. returns true if signal sent successfully.
static inline bool forge_process_kill(int64_t handle) {
    pthread_mutex_lock(&forge_process_mutex);
    if (handle < 0 || handle >= forge_process_count) {
        pthread_mutex_unlock(&forge_process_mutex);
        return false;
    }
    forge_process_t *p = &forge_process_pool[handle];
    if (!p->alive) {
        pthread_mutex_unlock(&forge_process_mutex);
        return false;
    }
    pid_t pid = p->pid;
    pthread_mutex_unlock(&forge_process_mutex);
    return kill(pid, SIGTERM) == 0;
}

// close all pipe file descriptors for this process.
static inline void forge_process_close(int64_t handle) {
    pthread_mutex_lock(&forge_process_mutex);
    if (handle < 0 || handle >= forge_process_count) {
        pthread_mutex_unlock(&forge_process_mutex);
        return;
    }
    forge_process_t *p = &forge_process_pool[handle];
    if (p->stdin_fd >= 0) { close(p->stdin_fd); p->stdin_fd = -1; }
    if (p->stdout_fd >= 0) { close(p->stdout_fd); p->stdout_fd = -1; }
    if (p->stderr_fd >= 0) { close(p->stderr_fd); p->stderr_fd = -1; }
    pthread_mutex_unlock(&forge_process_mutex);
}

#endif // FORGE_RUNTIME_H
