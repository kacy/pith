// hash_kernels: c counterpart of bench/hash_kernels.pith, for the kernels
// that are short enough to write out: none, fnv1a and crc32.
//
// fills a buffer with xorshift64 output written eight bytes at a time,
// little-endian, then runs one kernel over it and prints the digest, which
// matches the other versions. fnv1a is 32-bit, a byte at a time, as its
// definition reads. crc32 is the ieee polynomial with the slicing-by-8
// tables, the usual portable fast form; it does not use pclmulqdq as
// crc32fast does in the rust version. the sha kernels need a library or
// several hundred lines, so they are left out.
//
// build: cc -O2 -o bench/hash_kernels_c bench/hash_kernels.c
// run:   ./bench/hash_kernels_c crc32 256
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static uint32_t fnv1a(const uint8_t *p, size_t n) {
    uint32_t h = 2166136261u;
    for (size_t i = 0; i < n; i++) h = (h ^ p[i]) * 16777619u;
    return h;
}

static uint32_t crc_table[8][256];

static void crc_init(void) {
    for (uint32_t i = 0; i < 256; i++) {
        uint32_t c = i;
        for (int k = 0; k < 8; k++) c = c & 1 ? 0xedb88320u ^ (c >> 1) : c >> 1;
        crc_table[0][i] = c;
    }
    for (uint32_t i = 0; i < 256; i++)
        for (int t = 1; t < 8; t++) crc_table[t][i] = (crc_table[t - 1][i] >> 8) ^ crc_table[0][crc_table[t - 1][i] & 0xff];
}

static uint32_t crc32(const uint8_t *p, size_t n) {
    uint32_t c = 0xffffffffu;
    while (n >= 8) {
        uint32_t lo, hi;
        memcpy(&lo, p, 4);
        memcpy(&hi, p + 4, 4);
        lo ^= c;
        c = crc_table[7][lo & 0xff] ^ crc_table[6][(lo >> 8) & 0xff] ^ crc_table[5][(lo >> 16) & 0xff] ^
            crc_table[4][lo >> 24] ^ crc_table[3][hi & 0xff] ^ crc_table[2][(hi >> 8) & 0xff] ^
            crc_table[1][(hi >> 16) & 0xff] ^ crc_table[0][hi >> 24];
        p += 8;
        n -= 8;
    }
    while (n--) c = crc_table[0][(c ^ *p++) & 0xff] ^ (c >> 8);
    return c ^ 0xffffffffu;
}

int main(int argc, char **argv) {
    const char *kernel = argc > 1 ? argv[1] : "crc32";
    size_t mb = argc > 2 ? (size_t)atol(argv[2]) : 4;
    size_t total = mb * 1048576;
    uint8_t *data = malloc(total);
    uint64_t state = 88172645463325252ULL;
    for (size_t at = 0; at < total; at += 8) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        memcpy(data + at, &state, 8); // little-endian on x86-64
    }
    printf("len=%zu\n", total);
    if (strcmp(kernel, "none") == 0) {
        printf("digest=none\n");
    } else if (strcmp(kernel, "fnv1a") == 0) {
        printf("digest=%u\n", fnv1a(data, total));
    } else if (strcmp(kernel, "crc32") == 0) {
        crc_init();
        printf("digest=%u\n", crc32(data, total));
    } else {
        printf("unknown kernel %s; one of none fnv1a crc32\n", kernel);
        return 2;
    }
    return 0;
}
