#!/usr/bin/env bash
#
# interleaved comparison of the standard library's key-based sorts against the
# selection and insertion sorts they replaced. both arms are the same binary
# (bench/generic_sort), so the compiler, the runtime and the input are fixed
# and only the sort changes.
#
#   pith build bench/generic_sort.pith
#   bench/generic_sort_bench.sh                  # the default sweep
#   TRIALS=9 bench/generic_sort_bench.sh         # more rounds
#   SIZES="100 1000" bench/generic_sort_bench.sh
#
# each round runs legacy then current for one cell before moving to the next
# round, rather than running one arm to completion: ambient load on this box
# drifts over the length of a run and pools on whoever goes last. the reported
# figure is the median of the trials, with the min and max alongside so the
# spread is visible.
#
# the checksums of the two arms must match. they are printed, and a mismatch is
# a failure: it means one arm sorted differently or did less work.
set -euo pipefail

bin=${BIN:-./bench/generic_sort}
sizes=${SIZES:-"100 1000 10000"}
dists=${DISTS:-"sorted reverse random dups"}
keys=${KEYS:-"int string"}
trials=${TRIALS:-5}
warmups=${WARMUPS:-2}

if [ ! -x "$bin" ]; then
    echo "build it first: pith build bench/generic_sort.pith" >&2
    exit 2
fi

# rounds per timed call, so a 100-element cell is not one clock tick.
rounds_for() {
    case "$1" in
        100) echo 200 ;;
        1000) echo 20 ;;
        *) echo 1 ;;
    esac
}

field() { sed -n "s/.*$1=\([^ ]*\).*/\1/p"; }

median() {
    sort -n | awk '{v[NR]=$1} END {if (NR%2) print v[(NR+1)/2]; else print int((v[NR/2]+v[NR/2+1])/2)}'
}

printf '%-8s %-8s %-7s %12s %12s %8s  %s\n' key dist n legacy_us current_us speedup checksum
fail=0
for key in $keys; do
  for dist in $dists; do
    for n in $sizes; do
      rounds=$(rounds_for "$n")
      for _ in $(seq "$warmups"); do
        "$bin" legacy "$dist" "$key" "$n" "$rounds" > /dev/null
        "$bin" current "$dist" "$key" "$n" "$rounds" > /dev/null
      done
      legacy_runs=""
      current_runs=""
      legacy_sum=""
      current_sum=""
      for _ in $(seq "$trials"); do
        a=$("$bin" legacy "$dist" "$key" "$n" "$rounds")
        b=$("$bin" current "$dist" "$key" "$n" "$rounds")
        legacy_runs="$legacy_runs$(printf '%s' "$a" | field per_op_us)"$'\n'
        current_runs="$current_runs$(printf '%s' "$b" | field per_op_us)"$'\n'
        legacy_sum=$(printf '%s' "$a" | field checksum)
        current_sum=$(printf '%s' "$b" | field checksum)
      done
      lo_med=$(printf '%s' "$legacy_runs" | median)
      cu_med=$(printf '%s' "$current_runs" | median)
      lo_min=$(printf '%s' "$legacy_runs" | sort -n | head -1)
      lo_max=$(printf '%s' "$legacy_runs" | sort -n | tail -1)
      cu_min=$(printf '%s' "$current_runs" | sort -n | head -1)
      cu_max=$(printf '%s' "$current_runs" | sort -n | tail -1)
      if [ "$legacy_sum" != "$current_sum" ]; then
        mark="MISMATCH($legacy_sum/$current_sum)"
        fail=1
      else
        mark="$current_sum"
      fi
      speed=$(awk -v a="$lo_med" -v b="$cu_med" 'BEGIN {if (b == 0) print "n/a"; else printf "%.1fx", a/b}')
      printf '%-8s %-8s %-7s %12s %12s %8s  %s\n' "$key" "$dist" "$n" "$lo_med" "$cu_med" "$speed" "$mark"
      printf '%-8s %-8s %-7s %12s %12s\n' "" "" "spread" "$lo_min-$lo_max" "$cu_min-$cu_max"
    done
  done
done
if [ "$fail" -ne 0 ]; then
    echo "checksum mismatch: the two arms did not produce the same order" >&2
    exit 1
fi
