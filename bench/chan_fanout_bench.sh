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

# --- why the trials interleave ---
#
# this used to measure one variant to completion before starting the next.
# ambient load drifts over the length of a run, so a fixed order hands the
# variant measured first a systematic advantage; the sibling event_ledger
# suite showed the same binary reading 474 ms and then 1005 ms across two runs
# purely from where it sat in the order. one round now runs every variant
# once, and a round repeats TRIALS times, so drift lands on all of them alike.
#
# a slow first round is normal — the first run after a build competes with
# whatever the build left behind. read the non-pith rows as the canary: they
# should reproduce their published figures, and when they do not it is the box
# that moved rather than the language.
METRICS=(elapsed_ms rate_per_sec peak_rss_kb)

names=()
bins=()
greens=()
for entry in "pith-threads:$PITH:0" "pith-green:$PITH:1" "go:$GO:0" "rust:$RUST:0" "zig:$ZIG:0"; do
    rest=${entry#*:}
    bin=${rest%%:*}
    [ -x "$bin" ] || continue
    names+=("${entry%%:*}")
    bins+=("$bin")
    greens+=("${rest#*:}")
done
if [ "${#names[@]}" -eq 0 ]; then
    echo "chan_fanout_bench: no implementations built" >&2
    exit 1
fi

declare -A samples=()
declare -A checksums=()

for _ in $(seq "$TRIALS"); do
    for i in "${!names[@]}"; do
        out="$(env PITH_GREEN="${greens[$i]}" "${bins[$i]}" "$MESSAGES")"
        for metric in "${METRICS[@]}"; do
            value=$(printf '%s\n' "$out" | grep "^$metric=" | cut -d= -f2)
            if [ -z "$value" ]; then
                echo "chan_fanout_bench: ${names[$i]} reported no $metric" >&2
                exit 1
            fi
            samples[${names[$i]},$metric]+="$value"$'\n'
        done
        checksums[${names[$i]}]+="$(printf '%s\n' "$out" | grep '^checksum=' | cut -d= -f2)"$'\n'
    done
done

median() {
    printf '%s' "${samples[$1,$2]}" | sort -n |
        awk 'NF {a[++n]=$1} END {print a[int((n+1)/2)]}'
}

printf '\nchan fanout — %s messages, median of %s interleaved trials\n\n' "$MESSAGES" "$TRIALS"
printf '%-12s %8s %14s %12s\n' lang ms msgs_per_sec peak_rss_kb

for name in "${names[@]}"; do
    printf '%-12s %8s %14s %12s\n' "$name" \
        "$(median "$name" elapsed_ms)" \
        "$(median "$name" rate_per_sec)" \
        "$(median "$name" peak_rss_kb)"
done

# every run of every implementation has to agree, not just one sample
ref_checksum=""
for name in "${names[@]}"; do
    seen=$(printf '%s' "${checksums[$name]}" | sort -u)
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
