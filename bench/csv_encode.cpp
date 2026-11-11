// csv_encode: c++ counterpart of bench/csv_encode.pith.
//
// the encoder of the rust version over a vector of rows of std::string,
// appending to a fresh std::string each round. the checksum is the encoded
// byte count folded with every 97th byte.
//
// build: c++ -O2 -std=c++20 -o bench/csv_encode_cpp bench/csv_encode.cpp
// run:   ./bench/csv_encode_cpp <rows> <rounds>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <string>
#include <vector>

using std::string, std::to_string;

static string encode(const std::vector<std::vector<string>> &rows) {
    string out;
    for (size_t r = 0; r < rows.size(); r++) {
        if (r > 0) out += '\n';
        for (size_t i = 0; i < rows[r].size(); i++) {
            if (i > 0) out += ',';
            const string &f = rows[r][i];
            if (f.find_first_of(",\"\n") != string::npos) {
                out += '"';
                for (char c : f) {
                    if (c == '"') out += '"';
                    out += c;
                }
                out += '"';
            } else {
                out += f;
            }
        }
    }
    return out;
}

int main(int argc, char **argv) {
    long records = argc > 1 ? std::atol(argv[1]) : 0;
    long rounds = argc > 2 ? std::atol(argv[2]) : 0;
    if (records <= 0) records = 5000;
    if (rounds <= 0) rounds = 10;
    const char *regions[] = {"north", "south", "east", "west"};
    std::vector<std::vector<string>> rows;
    rows.push_back({"id", "name", "region", "active", "score", "quota", "url", "joined", "note", "path"});
    for (long i = 0; i < records; i++) {
        string region = regions[i % 4], id = to_string(i);
        long score = (i * 17 + 11) % 100, quota = (i * 13 + 7) % 50;
        rows.push_back({id, "user-" + id, region, i % 3 != 0 ? "true" : "false", to_string(score), to_string(quota),
                        "https://example.com/api/" + region + "/users/" + id + "?score=" + to_string(score),
                        "2026-" + to_string(i % 12 + 1) + "-" + to_string(i % 28 + 1),
                        "user " + id + ", region " + region, "data//" + region + "/./users/../users/" + id + ".json"});
    }
    auto t0 = std::chrono::steady_clock::now();
    unsigned long long checksum = 0;
    for (long round = 0; round < rounds; round++) {
        string encoded = encode(rows);
        checksum += encoded.size();
        for (size_t i = (size_t)(round % 7); i < encoded.size(); i += 97) checksum += (unsigned char)encoded[i];
    }
    long long ms =
        std::chrono::duration_cast<std::chrono::milliseconds>(std::chrono::steady_clock::now() - t0).count();
    std::printf("csv encode benchmark\nrows=%ld\nrounds=%ld\nelapsed_ms=%lld\nchecksum=%llu\n", records, rounds, ms,
                checksum);
}
