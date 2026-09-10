#!/usr/bin/env bash
#
# interleaved comparison of std.io's StringBuffer against the chunk-list
# storage it replaced. both arms are the same binary (bench/string_buffer), so
# the compiler, the runtime and the input are fixed and only the buffer's
# storage changes.
#
#   pith build bench/string_buffer.pith
#   bench/string_buffer_bench.sh                    # the default sweep
#   TRIALS=9 bench/string_buffer_bench.sh           # more rounds
#   SIZES="1000 10000" CHUNKS=8 bench/string_buffer_bench.sh
#   WORKLOADS=snapshot EVERY=50 bench/string_buffer_bench.sh
#
# each round runs legacy then current for one cell before moving to the next
# round, rather than running one arm to completion: ambient load on this box
# drifts over the length of a run and pools on whoever goes last. the reported
# figure is the median of the trials, with the min and max alongside so the
# spread is visible, and the peak resident set of the last trial of each arm.
#
# the snapshot workload takes string() every EVERY appends, and every snapshot
# copies the whole prefix in both arms; its cost is not linear in n for either
# arm and the table does not claim it is.
#
# the checksums of the two arms must match. they are printed, and a mismatch is
# a failure: it means one arm produced different text or did less work.
set -euo pipefail

bin=${BIN:-./bench/string_buffer}
sizes=${SIZES:-"1000 10000 100000"}
chunks=${CHUNKS:-"8 200"}
workloads=${WORKLOADS:-"build snapshot"}
every=${EVERY:-100}
trials=${TRIALS:-5}
warmups=${WARMUPS:-2}

if [ ! -x "$bin" ]; then
    echo "build it first: pith build bench/string_buffer.pith" >&2
    exit 2
fi

# rounds per timed call, so a 1000-append cell is not one clock tick.
rounds_for() {
    case "$1" in
        1000) echo 50 ;;
        10000) echo 5 ;;
        *) echo 1 ;;
    esac
}

field() { sed -n "s/.*$1=\([^ ]*\).*/\1/p"; }

median() {
    sort -n | awk '{v[NR]=$1} END {if (NR%2) print v[(NR+1)/2]; else print int((v[NR/2]+v[NR/2+1])/2)}'
}

printf '%-9s %-6s %-7s %12s %12s %8s %10s %10s  %s\n' workload chunk n legacy_us current_us speedup legacy_kb current_kb checksum
fail=0
for workload in $workloads; do
  for chunk in $chunks; do
    for n in $sizes; do
      rounds=$(rounds_for "$n")
      for _ in $(seq "$warmups"); do
        "$bin" legacy "$workload" "$n" "$chunk" "$rounds" "$every" > /dev/null
        "$bin" current "$workload" "$n" "$chunk" "$rounds" "$every" > /dev/null
      done
      legacy_runs=""
      current_runs=""
      legacy_sum=""
      current_sum=""
      legacy_kb=""
      current_kb=""
      for _ in $(seq "$trials"); do
        a=$("$bin" legacy "$workload" "$n" "$chunk" "$rounds" "$every")
        b=$("$bin" current "$workload" "$n" "$chunk" "$rounds" "$every")
        legacy_runs="$legacy_runs$(printf '%s' "$a" | field per_op_us)"$'\n'
        current_runs="$current_runs$(printf '%s' "$b" | field per_op_us)"$'\n'
        legacy_sum=$(printf '%s' "$a" | field checksum)
        current_sum=$(printf '%s' "$b" | field checksum)
        legacy_kb=$(printf '%s' "$a" | field peak_kb)
        current_kb=$(printf '%s' "$b" | field peak_kb)
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
      printf '%-9s %-6s %-7s %12s %12s %8s %10s %10s  %s\n' "$workload" "$chunk" "$n" "$lo_med" "$cu_med" "$speed" "$legacy_kb" "$current_kb" "$mark"
      printf '%-9s %-6s %-7s %12s %12s\n' "" "" "spread" "$lo_min-$lo_max" "$cu_min-$cu_max"
    done
  done
done
if [ "$fail" -ne 0 ]; then
    echo "checksum mismatch: the two arms did not produce the same text" >&2
    exit 1
fi
