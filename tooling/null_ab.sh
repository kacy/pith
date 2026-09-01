#!/usr/bin/env bash
# measure the noise floor before believing a timing delta.
#
# copies one binary to two paths and times them against each other exactly
# the way a real a/b would be run: interleaved, order rotated each round, a
# short gap between launches so back-to-back cadence does not favour the
# second arm. the two arms are byte-identical, so any difference is the
# box, not the code. a real a/b that lands inside this spread has shown
# nothing.
#
# usage:
#   tooling/null_ab.sh <binary> [rounds] [-- args...]
#
#   tooling/null_ab.sh ./bench/task_churn 15 -- 2000
#
# historical floor on this box: about ±3% on the minimum, wider on the
# median. environment variables are inherited, so set PITH_GREEN explicitly
# to match the a/b this is calibrating.
set -u

bin=${1:?binary}; shift
rounds=15
if [ $# -gt 0 ] && [ "$1" != "--" ]; then rounds=$1; shift; fi
[ "${1:-}" = "--" ] && shift

work=.pith-build/null_ab
mkdir -p "$work"
cp "$bin" "$work/arm_a"; cp "$bin" "$work/arm_b"; chmod +x "$work/arm_a" "$work/arm_b"

time_ms() {
    local t0 t1
    t0=$(date +%s%N); "$@" > /dev/null 2>&1; t1=$(date +%s%N)
    echo $(( (t1 - t0) / 1000000 ))
}

a_times=(); b_times=()
for ((i = 1; i <= rounds; i++)); do
    if (( i % 2 )); then
        a_times+=("$(time_ms "$work/arm_a" "$@")"); sleep 0.3
        b_times+=("$(time_ms "$work/arm_b" "$@")"); sleep 0.3
    else
        b_times+=("$(time_ms "$work/arm_b" "$@")"); sleep 0.3
        a_times+=("$(time_ms "$work/arm_a" "$@")"); sleep 0.3
    fi
done

python3 - "${a_times[*]}" "${b_times[*]}" <<'PY'
import statistics as s, sys
a = [int(x) for x in sys.argv[1].split()]
b = [int(x) for x in sys.argv[2].split()]
sign = sum(1 for x, y in zip(a, b) if x > y)
print(f"arm a: min={min(a)} median={s.median(a)}")
print(f"arm b: min={min(b)} median={s.median(b)}")
print(f"identical binaries differ by {(min(a)/min(b)-1)*100:+.1f}% min, "
      f"{(s.median(a)/s.median(b)-1)*100:+.1f}% median, a-slower sign {sign}/{len(a)}")
print("a real a/b inside this spread has shown nothing.")
PY
