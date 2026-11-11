// substring_search: c++ counterpart of bench/substring_search.pith.
//
// the same cells as the rust version, searched with std::string::find.
// contains is find != npos and find maps npos to -1, both called each round,
// so every cell costs two searches as in the other versions.
//
// build: c++ -O2 -std=c++20 -o bench/substring_search_cpp bench/substring_search.cpp
// run:   ./bench/substring_search_cpp 20000 [cell|all] [comma|word|repeat|all]
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <string>
#include <vector>

// filler with no comma and no occurrence of "sitting".
static std::string filler(size_t len) {
    const std::string pattern = "lorem ipsum dolor sit amet consectetur s ";
    std::string out;
    while (out.size() < len) out += pattern;
    out.resize(len);
    return out;
}

static size_t size_of(const std::string &size) {
    if (size == "short") return 12;
    if (size == "medium") return 80;
    return 4000;
}

static std::string haystack_for(size_t len, const std::string &pos, const std::string &needle) {
    if (pos == "absent") return filler(len);
    size_t gap = len - needle.size();
    if (pos == "start") return needle + filler(gap);
    if (pos == "end") return filler(gap) + needle;
    size_t left = gap / 2;
    return filler(left) + needle + filler(gap - left);
}

static std::string repeat_haystack(size_t len, const std::string &pos) {
    if (pos == "absent") return std::string(len, 'a');
    size_t gap = len - 7;
    if (pos == "start") return "aaaaaab" + std::string(gap, 'a');
    if (pos == "end") return std::string(gap, 'a') + "aaaaaab";
    size_t left = gap / 2;
    return std::string(left, 'a') + "aaaaaab" + std::string(gap - left, 'a');
}

struct Cell {
    std::string haystack, needle;
};

int main(int argc, char **argv) {
    long rounds = argc > 1 ? std::atol(argv[1]) : 2000;
    std::string only = argc > 2 ? argv[2] : "all";
    std::string kind_only = argc > 3 ? argv[3] : "all";
    std::vector<Cell> cells;
    for (std::string size : {"short", "medium", "long"})
        for (std::string pos : {"start", "middle", "end", "absent"}) {
            std::string name = size + "-" + pos;
            if (only != "all" && only != name) continue;
            for (std::string kind : {"comma", "word", "repeat"}) {
                if (kind_only != "all" && kind_only != kind) continue;
                std::string needle = kind == "comma" ? "," : kind == "repeat" ? "aaaaaab" : "sitting";
                std::string hay = kind == "repeat" ? repeat_haystack(size_of(size), pos)
                                                   : haystack_for(size_of(size), pos, needle);
                cells.push_back({hay, needle});
            }
        }
    long long calls = (long long)cells.size() * rounds * 2;
    auto t0 = std::chrono::steady_clock::now();
    long long sum = 0;
    for (long r = 0; r < rounds; r++)
        for (const Cell &c : cells) {
            if (c.haystack.find(c.needle) != std::string::npos) sum++;
            size_t at = c.haystack.find(c.needle);
            long long index = at == std::string::npos ? -1 : (long long)at;
            sum = (sum * 31 + index + 2) % 1000000007;
        }
    long long ns = std::chrono::duration_cast<std::chrono::nanoseconds>(std::chrono::steady_clock::now() - t0).count();
    std::printf("cells=%zu rounds=%ld calls=%lld total_us=%lld per_call_ns=%lld checksum=%lld\n", cells.size(), rounds,
                calls, ns / 1000, calls > 0 ? ns / calls : ns, sum);
}
