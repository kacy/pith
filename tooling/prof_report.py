#!/usr/bin/env python3
"""Flat profile from a PITH_PROF sample file.

    PITH_KEEP_SYMBOLS=1 pith build prog.pith
    PITH_PROF=prog.prof ./prog args...
    tooling/prof_report.py prog prog.prof [rows]

Samples in the program resolve against its own symbols (build it with
PITH_KEEP_SYMBOLS=1, or the runtime shows as addresses). Samples in a shared
library resolve against that library's exported symbols and print as
lib:symbol; libc's allocator internals are not exported, so they show under
the nearest exported name, which is a hint rather than an answer.
"""
import bisect
import collections
import os
import subprocess
import sys


def symbols(path, dynamic):
    args = ["nm", "-n", "--defined-only", "-C"] + (["-D"] if dynamic else []) + [path]
    out = subprocess.run(args, capture_output=True, text=True).stdout
    table = []
    for line in out.splitlines():
        parts = line.split(" ", 2)
        if len(parts) == 3 and parts[1] in "tTwWiI":
            table.append((int(parts[0], 16), parts[2]))
    return table


def load_segments(path):
    out = subprocess.run(["readelf", "-lW", path], capture_output=True, text=True).stdout
    segments = []
    for line in out.splitlines():
        fields = line.split()
        if fields and fields[0] == "LOAD":
            segments.append((int(fields[1], 16), int(fields[2], 16), int(fields[4], 16)))
    return segments


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    program, sample_file = sys.argv[1], sys.argv[2]
    rows = int(sys.argv[3]) if len(sys.argv) > 3 else 30
    base = 0
    maps = []
    pcs = []
    for line in open(sample_file):
        if line.startswith("base "):
            base = int(line.split()[1], 16)
        elif line.startswith("map "):
            fields = line[4:].split()
            if len(fields) >= 6 and "x" in fields[1]:
                lo, hi = (int(x, 16) for x in fields[0].split("-"))
                maps.append((lo, hi, int(fields[2], 16), fields[5]))
        elif line.strip():
            pcs.append(int(line, 16))
    own = symbols(program, False)
    own_addrs = [a for a, _ in own]
    program_real = os.path.realpath(program)
    libraries = {}

    def name_of(pc):
        for lo, hi, offset, path in maps:
            if not (lo <= pc < hi):
                continue
            if os.path.realpath(path) == program_real:
                break
            if path not in libraries:
                table = symbols(path, True)
                libraries[path] = (table, [a for a, _ in table], load_segments(path))
            table, addrs, segments = libraries[path]
            file_offset = pc - lo + offset
            vaddr = file_offset
            for seg_offset, seg_vaddr, size in segments:
                if seg_offset <= file_offset < seg_offset + size:
                    vaddr = file_offset - seg_offset + seg_vaddr
            i = bisect.bisect_right(addrs, vaddr) - 1
            return os.path.basename(path) + ":" + (table[i][1] if i >= 0 else "?")
        else:
            if maps:
                return "[unmapped]"
        i = bisect.bisect_right(own_addrs, pc - base) - 1
        return own[i][1] if i >= 0 else "?"

    counts = collections.Counter(name_of(pc)[:100] for pc in pcs)
    total = len(pcs)
    print(f"{total} samples")
    for name, count in counts.most_common(rows):
        print(f"{100.0 * count / max(total, 1):6.2f}%  {name}")


if __name__ == "__main__":
    main()
