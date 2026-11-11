// string_build: c++ counterpart of bench/string_build.pith.
//
// grows one std::string by many short appends, with std::to_chars for the
// number so no temporary string is made.
//
// build: c++ -O2 -std=c++20 -o bench/string_build_cpp bench/string_build.cpp
// run:   ./bench/string_build_cpp 2000000
#include <charconv>
#include <cstdio>
#include <cstdlib>
#include <string>

int main(int argc, char **argv) {
    long n = argc > 1 ? std::atol(argv[1]) : 0;
    if (n <= 0) n = 1000;
    std::string s;
    char num[24];
    for (long i = 0; i < n; i++) {
        s += "item-";
        auto r = std::to_chars(num, num + sizeof num, i);
        s.append(num, r.ptr);
        s += ',';
    }
    unsigned long long sum = 0;
    for (size_t j = 0; j < s.size(); j += 97) sum += (unsigned char)s[j];
    std::printf("len: %zu checksum: %llu\n", s.size(), sum);
}
