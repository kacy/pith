// string_buffer: c++ counterpart of bench/string_buffer.pith.
//
// only the `current` arm: a std::string grown with append. a build hands the
// string back by move, with no copy; a snapshot is an owned copy of the
// prefix, as in the rust version. inputs come from the same seeded 31-bit lcg
// and are built before the clock starts.
//
// build: c++ -O2 -std=c++20 -o bench/string_buffer_cpp bench/string_buffer.cpp
// run:   ./bench/string_buffer_cpp current <build|snapshot> <n> <chunk> <rounds> [k]
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>

static const long long MOD = 1000000007;

static long long fold_snapshot(long long sum, const std::string &snap) {
    long long last = snap.empty() ? 0 : (unsigned char)snap.back();
    return (sum * 31 + (long long)snap.size() * 7 + last) % MOD;
}

static long long fnv1a(const std::string &s) {
    uint32_t h = 2166136261u;
    for (unsigned char b : s) h = (h ^ b) * 16777619u;
    return h;
}

static long long peak_kb() {
    FILE *f = std::fopen("/proc/self/status", "r");
    char line[256];
    long long kb = 0;
    while (f && std::fgets(line, sizeof line, f))
        if (std::strncmp(line, "VmHWM:", 6) == 0) kb = std::atoll(line + 6);
    if (f) std::fclose(f);
    return kb;
}

int main(int argc, char **argv) {
    std::string arm = argc > 1 ? argv[1] : "current";
    std::string workload = argc > 2 ? argv[2] : "build";
    long long n = argc > 3 ? std::atoll(argv[3]) : 10000;
    long long chunk = argc > 4 ? std::atoll(argv[4]) : 8;
    long long rounds = argc > 5 ? std::atoll(argv[5]) : 1;
    long long every = argc > 6 ? std::atoll(argv[6]) : 100;
    if (every <= 0) every = 1;
    if (arm != "current") {
        std::printf("only the current arm is implemented\n");
        return 2;
    }
    std::vector<std::string> chunks;
    long long state = 2463534242LL;
    for (long long i = 0; i < n; i++) {
        std::string piece;
        for (long long j = 0; j < chunk; j++) {
            state = (state * 1103515245 + 12345) % 2147483648LL;
            piece += (char)(97 + state % 26);
        }
        chunks.push_back(std::move(piece));
    }
    long long sum = 0;
    std::string last_out;
    auto t0 = std::chrono::steady_clock::now();
    for (long long r = 0; r < rounds; r++) {
        if (workload == "snapshot") {
            std::string buf;
            long long s = 0;
            for (size_t i = 0; i < chunks.size(); i++) {
                buf += chunks[i];
                if ((long long)(i + 1) % every == 0) s = fold_snapshot(s, std::string(buf));
            }
            s = fold_snapshot(s, std::string(buf));
            sum = (sum + s) % MOD;
        } else {
            std::string buf;
            for (const std::string &piece : chunks) buf += piece;
            last_out = std::move(buf);
            sum = fold_snapshot(sum, last_out);
        }
    }
    long long us =
        std::chrono::duration_cast<std::chrono::microseconds>(std::chrono::steady_clock::now() - t0).count();
    sum = (sum + fnv1a(last_out) % MOD) % MOD;
    std::printf("%s %s n=%lld chunk=%lld rounds=%lld every=%lld total_us=%lld per_op_us=%lld peak_kb=%lld checksum=%lld\n",
                arm.c_str(), workload.c_str(), n, chunk, rounds, every, us, rounds > 0 ? us / rounds : us, peak_kb(),
                sum);
}
