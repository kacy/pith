#!/usr/bin/env bash
#
# a time-boxed pithgen hunt under AddressSanitizer: generate programs from
# fresh seeds in batches until the budget runs out, then group what turned up
# by signature. the nightly workflow runs this; it also runs by hand.
#
#   tooling/pithgen_hunt.sh                         # 110 minutes from today's seeds
#   PITHGEN_HUNT_MINUTES=10 tooling/pithgen_hunt.sh
#
# each night starts from a seed base derived from the date, so the hunt walks
# new programs every run instead of repeating the ones pithgen-check pins.
# a finding is reproducible from its seed alone: `pithgen run --sanitize
# --seeds N..N+1`, or `pithgen reduce --sanitize` to shrink it.
#
# output, under $PITHGEN_HUNT_OUT (default .pith-build/hunt):
#   findings/seed_<n>_<class>/   each finding pithgen kept: finding.txt, main.pith
#   signatures.tsv               signature, count, class, first seed, finding dir
#   summary.md                   the same as a markdown table
# the exit status is 1 when anything was found.
set -euo pipefail

budget_min="${PITHGEN_HUNT_MINUTES:-110}"
batch="${PITHGEN_HUNT_BATCH:-1000}"
base="${PITHGEN_HUNT_BASE:-$(( $(date -u +%s) / 86400 * 1000000 ))}"
out="${PITHGEN_HUNT_OUT:-.pith-build/hunt}"
pithgen="tooling/pithgen/target/release/pithgen"

if [ ! -x self-host/pith_main ] || [ ! -x self-host/ir_driver ]; then
  echo "pithgen checks programs with self-host/pith_main; run make self-host self-host-ir-driver first" >&2
  exit 2
fi
echo "--- pithgen hunt: ${budget_min} minutes, batches of ${batch} from seed ${base} ---"
eval "$(bash tooling/sanitize.sh asan setup | grep -E '^(CC|PITH_RUNTIME_LIB)=')"
export CC PITH_RUNTIME_LIB PITH_KEEP_SYMBOLS=1
cargo build --release --quiet --manifest-path tooling/pithgen/Cargo.toml

rm -rf "$out"
mkdir -p "$out/findings"
deadline=$(( $(date +%s) + budget_min * 60 ))
start="$base"
programs=0
while [ "$(date +%s)" -lt "$deadline" ]; do
  end=$(( start + batch ))
  run="$out/batch"
  rm -rf "$run"
  "$pithgen" run --sanitize --seeds "$start..$end" --pith ./target/release/pith \
    --out "$PWD/$run" >"$out/batch.log" 2>&1 || true
  # a batch that ran no programs means the harness is broken, not that the
  # compiler is clean: stop loudly rather than report an empty night
  accepted="$(sed -n 's/^check-accepted: \([0-9]*\).*/\1/p' "$out/batch.log" | tail -1)"
  if [ -z "$accepted" ] || [ "$accepted" -eq 0 ]; then
    echo "batch $start..$end ran no programs:" >&2
    tail -20 "$out/batch.log" >&2
    exit 2
  fi
  grep -E '^(check-accepted|findings):' "$out/batch.log" | sed "s/^/  $start..$end /" || true
  if [ -d "$run/findings" ]; then
    cp -r "$run/findings/." "$out/findings/"
  fi
  # the built programs are tens of megabytes each under asan; only the
  # findings are kept
  rm -rf "$run"
  programs=$(( programs + accepted ))
  start="$end"
done

# one row per signature: how often it hit, its class, and the first seed
: >"$out/signatures.tsv"
for dir in "$out"/findings/*/; do
  [ -f "$dir/finding.txt" ] || continue
  seed="$(sed -n 's/^seed: //p' "$dir/finding.txt" | head -1)"
  class="$(sed -n 's/^class: //p' "$dir/finding.txt" | head -1)"
  # a wrong-output signature names the line it diverged on, which differs
  # from program to program for one bug, so that part is left out
  signature="$(sed -n 's/^signature: //p' "$dir/finding.txt" | head -1 | sed -E 's/ at line [0-9]+//')"
  printf '%s\t%s\t%s\t%s\n' "${signature:-$class}" "$class" "$seed" "$dir" >>"$out/signatures.tsv"
done
sort -t$'\t' -k1,1 -k3,3n "$out/signatures.tsv" | awk -F'\t' '
  $1 != last { if (last != "") print row "\t" n; last = $1; n = 0; row = $1 "\t" $2 "\t" $3 "\t" $4 }
  { n++ }
  END { if (last != "") print row "\t" n }' >"$out/signatures.grouped.tsv"
mv "$out/signatures.grouped.tsv" "$out/signatures.tsv"

distinct="$(wc -l <"$out/signatures.tsv")"
{
  echo "## pithgen hunt"
  echo
  echo "seeds ${base}..${start} (${programs} programs), ${distinct} distinct signature(s)"
  if [ "$distinct" -gt 0 ]; then
    echo
    echo "| signature | class | first seed | programs |"
    echo "|---|---|---|---|"
    while IFS=$'\t' read -r signature class seed _dir count; do
      echo "| \`${signature//|/\\|}\` | $class | $seed | $count |"
    done <"$out/signatures.tsv"
  fi
} >"$out/summary.md"
cat "$out/summary.md"

[ "$distinct" -eq 0 ]
