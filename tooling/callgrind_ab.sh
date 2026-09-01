#!/usr/bin/env bash
# compare two arms by instruction count instead of wall time.
#
# timing on a shared two-core box drifts by more than most of the effects
# under study; a compile-time change once read +5.6% eight rounds running and
# callgrind put it at +0.06%. instruction counts are deterministic and do not
# care what else the machine is doing, so this is the acceptance instrument.
#
# usage:
#   tooling/callgrind_ab.sh [--annotate N] <label-a> <cmd-a> <label-b> <cmd-b>
#
# each cmd is one shell string, run under valgrind --tool=callgrind. environment
# assignments inside the string work, so the two arms can be one binary with
# two settings:
#
#   tooling/callgrind_ab.sh \
#     pool-on  'PITH_GREEN=0 ./bench/task_churn 200' \
#     pool-off 'PITH_GREEN=0 PITH_STRUCT_FREELIST=0 ./bench/task_churn 200'
#
# --annotate N prints the N hottest functions of each arm, which is how a
# delta gets attributed to a symbol rather than argued about. that needs a
# binary with its symbol table: build the program under test with
# PITH_KEEP_SYMBOLS=1, or every runtime function reports as an address.
#
# output files go under .pith-build/callgrind in the repo, on disk. /tmp on
# this box is ram.
set -u

annotate=0
if [ "${1:-}" = "--annotate" ]; then
    annotate=${2:?--annotate needs a count}
    shift 2
fi
if [ $# -ne 4 ]; then
    echo "usage: $0 [--annotate N] <label-a> <cmd-a> <label-b> <cmd-b>" >&2
    exit 2
fi
label_a=$1; cmd_a=$2; label_b=$3; cmd_b=$4

out_dir=.pith-build/callgrind
mkdir -p "$out_dir"

run_arm() {
    local label=$1 cmd=$2
    local out="$out_dir/$label.out"
    local log="$out_dir/$label.log"
    rm -f "$out" "$log"
    # leading VAR=value words are the arm's environment and must be exported
    # before valgrind starts, or valgrind takes the first one as the program
    # to run. the arm's own stdout is kept so checksums can be compared.
    # shellcheck disable=SC2086
    bash -c '
        out=$1; shift
        while [ $# -gt 0 ] && [[ $1 == ?*=* ]]; do export "$1"; shift; done
        exec valgrind --tool=callgrind --callgrind-out-file="$out" "$@"
    ' arm "$out" $cmd > "$out_dir/$label.stdout" 2> "$log"
    local ir
    ir=$(grep -E '^summary:' "$out" | awk '{print $2}')
    if [ -z "$ir" ]; then
        echo "$label: no instruction total in $out (see $log)" >&2
        exit 1
    fi
    echo "$ir"
}

ir_a=$(run_arm "$label_a" "$cmd_a") || exit 1
ir_b=$(run_arm "$label_b" "$cmd_b") || exit 1

printf '%-14s %16s Ir\n' "$label_a" "$ir_a"
printf '%-14s %16s Ir\n' "$label_b" "$ir_b"
python3 - "$ir_a" "$ir_b" "$label_a" "$label_b" <<'PY'
import sys
a, b = int(sys.argv[1]), int(sys.argv[2])
la, lb = sys.argv[3], sys.argv[4]
if b:
    print(f"{la} vs {lb}: {(a / b - 1) * 100:+.2f}% instructions")
PY

sum_a=$(grep -E '^checksum=' "$out_dir/$label_a.stdout" || true)
sum_b=$(grep -E '^checksum=' "$out_dir/$label_b.stdout" || true)
if [ -n "$sum_a$sum_b" ] && [ "$sum_a" != "$sum_b" ]; then
    echo "CHECKSUM MISMATCH: $label_a '$sum_a' vs $label_b '$sum_b' — comparison void" >&2
    exit 1
fi

if [ "$annotate" -gt 0 ]; then
    for label in "$label_a" "$label_b"; do
        echo
        echo "== $label: hottest $annotate functions =="
        callgrind_annotate "$out_dir/$label.out" 2>/dev/null \
            | grep -E '^\s*[0-9,]+ ' | head -n "$annotate"
    done
fi
