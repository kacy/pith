#!/usr/bin/env bash
# event_ledger_bench.sh — build the four event_ledger implementations,
# check they all produce the same checksum and digest (proof they do the
# same work), and report the median phase timings.
#
#   bench/event_ledger_bench.sh [events] [trials]
#
# defaults: 200000 events, 5 trials. skips any language whose toolchain
# is not installed.
set -euo pipefail
cd "$(dirname "$0")/.."

EVENTS=${1:-200000}
TRIALS=${2:-5}

PITH=./bench/event_ledger
GO=./bench/event_ledger_go
RUST=./bench/event_ledger_rust/target/release/event_ledger_rust
ZIG=./bench/event_ledger_zig

echo "building..."
./self-host/pith_main build bench/event_ledger.pith >/dev/null
command -v go >/dev/null && go build -o "$GO" bench/event_ledger.go
command -v cargo >/dev/null && cargo build --release --quiet --manifest-path bench/event_ledger_rust/Cargo.toml
command -v zig >/dev/null && zig build-exe -O ReleaseFast -femit-bin="$ZIG" bench/event_ledger.zig

# median of a metric across TRIALS runs of one binary
median() {
    local bin=$1 metric=$2
    local vals=()
    for _ in $(seq "$TRIALS"); do
        vals+=("$("$bin" "$EVENTS" | grep "^$metric=" | cut -d= -f2)")
    done
    printf '%s\n' "${vals[@]}" | sort -n | awk '{a[NR]=$1} END {print a[int((NR+1)/2)]}'
}

digest_of() { "$1" "$EVENTS" | grep '^digest=' | cut -d= -f2; }

printf '\nevent ledger — %s events, median of %s trials\n\n' "$EVENTS" "$TRIALS"
printf '%-8s %8s %8s %9s %8s %9s\n' lang gen_ms parse_ms analyze_ms sign_ms total_ms

ref_digest=""
for entry in "pith:$PITH" "go:$GO" "rust:$RUST" "zig:$ZIG"; do
    name=${entry%%:*}
    bin=${entry#*:}
    [ -x "$bin" ] || continue
    printf '%-8s %8s %8s %9s %8s %9s\n' "$name" \
        "$(median "$bin" gen_ms)" \
        "$(median "$bin" parse_ms)" \
        "$(median "$bin" analyze_ms)" \
        "$(median "$bin" sign_ms)" \
        "$(median "$bin" total_ms)"
    d=$(digest_of "$bin")
    if [ -z "$ref_digest" ]; then
        ref_digest=$d
    elif [ "$d" != "$ref_digest" ]; then
        echo "MISMATCH: $name digest $d != $ref_digest" >&2
        exit 1
    fi
done

echo
echo "all digests match: $ref_digest"
