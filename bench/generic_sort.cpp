// generic_sort: c++ counterpart of bench/generic_sort.pith.
//
// the same rows as the rust version (a seeded 31-bit lcg, built before the
// clock starts). only the `current` arm is here: copy the vector of row
// pointers and std::stable_sort it on the field, stable like the rust and
// pith sorts.
//
// build: c++ -O2 -std=c++20 -o bench/generic_sort_cpp bench/generic_sort.cpp
// run:   ./bench/generic_sort_cpp current <dist> <key> <n> <rounds>
#include <algorithm>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <string>
#include <vector>

struct Row {
    std::string label;
    long long rank;
};

static long long checksum(const std::vector<const Row *> &rows) {
    long long total = 0;
    for (size_t i = 0; i < rows.size(); i++)
        total = (total + ((long long)i + 1) * (rows[i]->rank + 1) + (long long)rows[i]->label.size()) % 1000000007;
    return total;
}

int main(int argc, char **argv) {
    std::string arm = argc > 1 ? argv[1] : "current";
    std::string dist = argc > 2 ? argv[2] : "random";
    std::string key = argc > 3 ? argv[3] : "int";
    long long n = argc > 4 ? std::atoll(argv[4]) : 1000;
    long long rounds = argc > 5 ? std::atoll(argv[5]) : 5;
    if (arm != "current") {
        std::printf("only the current arm is implemented\n");
        return 2;
    }
    std::vector<Row> rows;
    rows.reserve(n);
    long long state = 2463534242LL;
    for (long long i = 0; i < n; i++) {
        long long rank;
        if (dist == "sorted") {
            rank = i;
        } else if (dist == "reverse") {
            rank = n - i;
        } else {
            state = (state * 1103515245 + 12345) % 2147483648LL;
            rank = dist == "dups" ? state % 8 : state % 1000000;
        }
        std::string padded = "0000000" + std::to_string(rank);
        rows.push_back({"row-" + padded.substr(padded.size() - 7), rank});
    }
    std::vector<const Row *> refs;
    for (const Row &r : rows) refs.push_back(&r);
    auto t0 = std::chrono::steady_clock::now();
    long long sum = 0;
    for (long long r = 0; r < rounds; r++) {
        std::vector<const Row *> out = refs;
        if (key == "string")
            std::stable_sort(out.begin(), out.end(), [](const Row *a, const Row *b) { return a->label < b->label; });
        else
            std::stable_sort(out.begin(), out.end(), [](const Row *a, const Row *b) { return a->rank < b->rank; });
        sum = (sum + checksum(out)) % 1000000007;
    }
    long long us =
        std::chrono::duration_cast<std::chrono::microseconds>(std::chrono::steady_clock::now() - t0).count();
    std::printf("%s %s %s n=%lld rounds=%lld total_us=%lld per_op_us=%lld checksum=%lld\n", arm.c_str(), dist.c_str(),
                key.c_str(), n, rounds, us, rounds > 0 ? us / rounds : us, sum);
}
