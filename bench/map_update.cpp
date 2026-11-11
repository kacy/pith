// map_update: c++ counterpart of bench/map_update.pith.
//
// bump the count of a key the map already holds, or insert it with 1, over a
// small pool of keys repeated many times, with std::unordered_map and its
// default std::hash, keyed by std::string, std::vector<unsigned char> or
// int64_t for the three flavors. std::hash has no specialization for a
// vector of bytes, so that flavor hashes the bytes through a string_view.
// the update is find then insert, so a hit allocates nothing and only a miss
// copies the key, as in the rust version.
//
// build: c++ -O2 -std=c++20 -o bench/map_update_cpp bench/map_update.cpp
// run:   ./bench/map_update_cpp <flavor: string|int|bytes> <updates> <distinct keys>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <string>
#include <string_view>
#include <unordered_map>
#include <vector>

using Bytes = std::vector<unsigned char>;

struct BytesHash {
    size_t operator()(const Bytes &b) const {
        return std::hash<std::string_view>{}(std::string_view(reinterpret_cast<const char *>(b.data()), b.size()));
    }
};

template <class K, class H = std::hash<K>>
static int64_t count(const std::vector<K> &keys, long updates) {
    std::unordered_map<K, int64_t, H> m;
    for (long i = 0; i < updates; i++) {
        const K &k = keys[i % keys.size()];
        auto it = m.find(k);
        if (it != m.end())
            it->second++;
        else
            m.emplace(k, 1);
    }
    int64_t total = 0;
    for (auto &kv : m) total += kv.second;
    return total;
}

static long arg_int(int argc, char **argv, int i, long fallback) {
    long v = argc > i ? std::atol(argv[i]) : 0;
    return v > 0 ? v : fallback;
}

int main(int argc, char **argv) {
    std::string flavor = argc > 1 ? argv[1] : "string";
    long updates = arg_int(argc, argv, 2, 200000);
    long distinct = arg_int(argc, argv, 3, 64);
    auto key_text = [](long i) { return "user-" + std::to_string(i) + "-region"; };
    int64_t total;
    if (flavor == "int") {
        std::vector<int64_t> keys;
        for (long i = 0; i < distinct; i++) keys.push_back(i);
        total = count(keys, updates);
    } else if (flavor == "bytes") {
        std::vector<Bytes> keys;
        for (long i = 0; i < distinct; i++) {
            std::string s = key_text(i);
            keys.emplace_back(s.begin(), s.end());
        }
        total = count<Bytes, BytesHash>(keys, updates);
    } else {
        std::vector<std::string> keys;
        for (long i = 0; i < distinct; i++) keys.push_back(key_text(i));
        total = count(keys, updates);
    }
    std::printf("map_update flavor=%s updates=%ld distinct=%ld\n", flavor.c_str(), updates, distinct);
    std::printf("total=%lld\n", (long long)total);
}
