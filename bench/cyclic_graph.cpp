// cyclic_graph: c++ counterpart of bench/cyclic_graph.pith.
//
// builds many two-node parent<->child rings and drops them. `next` is an
// owning std::shared_ptr and `parent` a std::weak_ptr back edge, the same
// shape as the pith `weak parent` field and the rust Rc/Weak version, so
// both nodes are freed when build_ring returns. shared_ptr counts with
// atomics once the program links threads; this one does not, so libstdc++
// uses plain increments, as Rc does.
//
// build: c++ -O2 -std=c++20 -o bench/cyclic_graph_cpp bench/cyclic_graph.cpp
// run:   ./bench/cyclic_graph_cpp 10000000
#include <cstdio>
#include <cstdlib>
#include <memory>

struct Node {
    long long value;
    std::shared_ptr<Node> next;
    std::weak_ptr<Node> parent;
    explicit Node(long long v) : value(v) {}
};

static long long build_ring(long long seed) {
    auto a = std::make_shared<Node>(seed);
    auto b = std::make_shared<Node>(seed + 1);
    a->next = b;
    b->parent = a;
    return a->value + b->value;
}

int main(int argc, char **argv) {
    long long rings = argc > 1 ? std::atoll(argv[1]) : 0;
    if (rings <= 0) rings = 1000000;
    long long checksum = 0;
    for (long long i = 0; i < rings; i++) checksum += build_ring(i);
    std::printf("rings: %lld checksum: %lld\n", rings, checksum);
}
