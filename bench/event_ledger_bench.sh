#!/usr/bin/env bash
# event_ledger_bench.sh — build the four event_ledger implementations,
# check they all produce the same checksum and digest (proof they do the
# same work), and report the median phase timings.
#
#   bench/event_ledger_bench.sh [events] [trials]
#
# defaults: 200000 events, 5 trials. skips any language whose toolchain
# is not installed.
#
# --- why the trials interleave ---
#
# this used to measure one language to completion before starting the next,
# and to re-run the binary once per metric column. both were wrong on a
# small shared box. ambient load drifts over the length of a run, so a fixed
# order hands the language measured first a systematic advantage — in one
# observed pair of runs pith held 571 and 577 ms while go, measured after it,
# read 474 then 1005 for the same binary. and taking each column from its own
# set of runs meant the phase numbers did not come from the same work, so a
# row's phases need not sum to its own total.
#
# so: one round runs every language once, in order, and a round is repeated
# TRIALS times. drift then lands on all of them alike instead of pooling on
# whoever went last, and every metric in a row comes from the same run.
#
# a slow first round is normal — the first run after a build competes with
# whatever the build left behind. run the suite twice and keep the second if
# the numbers disagree, and read the non-pith rows as the canary: they should
# reproduce their published figures, and when they do not it is the box that
# moved, not the language.
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

METRICS=(gen_ms parse_ms analyze_ms sign_ms total_ms)

# the languages that actually have a binary to run, in a stable order.
names=()
bins=()
for entry in "pith:$PITH" "go:$GO" "rust:$RUST" "zig:$ZIG"; do
    bin=${entry#*:}
    [ -x "$bin" ] || continue
    names+=("${entry%%:*}")
    bins+=("$bin")
done
if [ "${#names[@]}" -eq 0 ]; then
    echo "event_ledger_bench: no implementations built" >&2
    exit 1
fi

declare -A samples=()
declare -A digests=()

for _ in $(seq "$TRIALS"); do
    for i in "${!names[@]}"; do
        out="$("${bins[$i]}" "$EVENTS")"
        for metric in "${METRICS[@]}"; do
            value=$(printf '%s\n' "$out" | grep "^$metric=" | cut -d= -f2)
            if [ -z "$value" ]; then
                echo "event_ledger_bench: ${names[$i]} reported no $metric" >&2
                exit 1
            fi
            samples[${names[$i]},$metric]+="$value"$'\n'
        done
        digests[${names[$i]}]+="$(printf '%s\n' "$out" | grep '^digest=' | cut -d= -f2)"$'\n'
    done
done

median() {
    printf '%s' "${samples[$1,$2]}" | sort -n |
        awk 'NF {a[++n]=$1} END {print a[int((n+1)/2)]}'
}

printf '\nevent ledger — %s events, median of %s interleaved trials\n\n' "$EVENTS" "$TRIALS"
printf '%-8s %8s %8s %9s %8s %9s\n' lang "${METRICS[@]}"

for name in "${names[@]}"; do
    printf '%-8s %8s %8s %9s %8s %9s\n' "$name" \
        "$(median "$name" gen_ms)" \
        "$(median "$name" parse_ms)" \
        "$(median "$name" analyze_ms)" \
        "$(median "$name" sign_ms)" \
        "$(median "$name" total_ms)"
done

# every run of every implementation has to agree, not just one sample.
ref_digest=""
for name in "${names[@]}"; do
    seen=$(printf '%s' "${digests[$name]}" | sort -u)
    if [ "$(printf '%s\n' "$seen" | wc -l)" -ne 1 ]; then
        echo "UNSTABLE: $name produced more than one digest:" $seen >&2
        exit 1
    fi
    if [ -z "$ref_digest" ]; then
        ref_digest=$seen
    elif [ "$seen" != "$ref_digest" ]; then
        echo "MISMATCH: $name digest $seen != $ref_digest" >&2
        exit 1
    fi
done

echo
echo "all digests match: $ref_digest"
