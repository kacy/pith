// cyclic_graph: c counterpart of bench/cyclic_graph.pith.
//
// builds many two-node parent<->child rings and frees them. c has no
// reference counting, so the owning edge and the back edge are both plain
// pointers and the ring is freed by hand when build_ring returns, which is
// what the weak back edge buys in the pith and rust versions. the compiler
// may remove the malloc and free pair altogether.
//
// build: cc -O2 -o bench/cyclic_graph_c bench/cyclic_graph.c
// run:   ./bench/cyclic_graph_c 10000000
#include <stdio.h>
#include <stdlib.h>

struct node {
    long long value;
    struct node *next, *parent;
};

static struct node *new_node(long long value) {
    struct node *n = malloc(sizeof *n);
    if (!n) abort();
    n->value = value;
    n->next = n->parent = NULL;
    return n;
}

static long long build_ring(long long seed) {
    struct node *a = new_node(seed), *b = new_node(seed + 1);
    a->next = b;
    b->parent = a;
    long long sum = a->value + b->value;
    free(b);
    free(a);
    return sum;
}

int main(int argc, char **argv) {
    long long rings = argc > 1 ? atoll(argv[1]) : 0;
    if (rings <= 0) rings = 1000000;
    long long checksum = 0;
    for (long long i = 0; i < rings; i++) checksum += build_ring(i);
    printf("rings: %lld checksum: %lld\n", rings, checksum);
    return 0;
}
