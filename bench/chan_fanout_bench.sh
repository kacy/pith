#!/usr/bin/env bash
# chan_fanout_bench.sh — build the four chan_fanout implementations,
# check they all produce the same checksum (proof they do the same
# work), and report median throughput and peak rss.
#
#   bench/chan_fanout_bench.sh [messages] [trials]
#
# defaults: 1000000 messages, 5 trials. pith is measured twice, once with
# PITH_GREEN=0 (os threads) and once with PITH_GREEN=1 (green). both are set
# explicitly, so the table means the same thing whichever backend the host
# defaults to. skips any language whose toolchain is not installed.
set -euo pipefail
cd "$(dirname "$0")/.."

MESSAGES=${1:-1000000}
TRIALS=${2:-5}

PITH=./bench/chan_fanout
GO=./bench/chan_fanout_go
RUST=./bench/chan_fanout_rust
ZIG=./bench/chan_fanout_zig

echo "building..."
./self-host/pith_main build bench/chan_fanout.pith >/dev/null
command -v go >/dev/null && go build -o "$GO" bench/chan_fanout.go
command -v rustc >/dev/null && rustc -O -o "$RUST" bench/chan_fanout.rs
command -v zig >/dev/null && zig build-exe -O ReleaseFast -femit-bin="$ZIG" bench/chan_fanout.zig

# run one binary TRIALS times, keeping every metric line from every run
trials_output=""
run_trials() {
    local bin=$1 green=$2
    trials_output=""
    for _ in $(seq "$TRIALS"); do
        trials_output+="$(env PITH_GREEN="$green" "$bin" "$MESSAGES")"$'\n'
    done
}

median() {
    printf '%s' "$trials_output" | grep "^$1=" | cut -d= -f2 |
        sort -n | awk '{a[NR]=$1} END {print a[int((NR+1)/2)]}'
}

# every run of every implementation has to agree, not just one sample
checksums() { printf '%s' "$trials_output" | grep '^checksum=' | cut -d= -f2 | sort -u; }

printf '\nchan fanout — %s messages, median of %s trials\n\n' "$MESSAGES" "$TRIALS"
printf '%-12s %8s %14s %12s\n' lang ms msgs_per_sec peak_rss_kb

ref_checksum=""
for entry in "pith-threads:$PITH:0" "pith-green:$PITH:1" "go:$GO:0" "rust:$RUST:0" "zig:$ZIG:0"; do
    name=${entry%%:*}
    rest=${entry#*:}
    bin=${rest%%:*}
    green=${rest#*:}
    [ -x "$bin" ] || continue

    run_trials "$bin" "$green"
    printf '%-12s %8s %14s %12s\n' "$name" \
        "$(median elapsed_ms)" "$(median rate_per_sec)" "$(median peak_rss_kb)"

    seen=$(checksums)
    if [ "$(printf '%s\n' "$seen" | wc -l)" -ne 1 ]; then
        echo "UNSTABLE: $name produced more than one checksum:" $seen >&2
        exit 1
    fi
    if [ -z "$ref_checksum" ]; then
        ref_checksum=$seen
    elif [ "$seen" != "$ref_checksum" ]; then
        echo "MISMATCH: $name checksum $seen != $ref_checksum" >&2
        exit 1
    fi
done

echo
echo "all checksums match: $ref_checksum"
