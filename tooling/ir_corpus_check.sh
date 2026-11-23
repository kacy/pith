#!/usr/bin/env bash
# Parallel runner for pure-CPU IR corpus checks (`validate-ir-contract-only` and
# `count-marks-check-only`). Each source file is processed in its own temporary
# directory so concurrent workers never collide on intermediate `.ir` or `.o`
# files while producing identical diagnostics and exit behavior to the serial
# loop.
set -euo pipefail

mode="${1:-}"
if [ "$mode" != "validate" ] && [ "$mode" != "count-marks" ]; then
  echo "usage: $0 <validate|count-marks>" >&2
  exit 2
fi

jobs="${PITH_JOBS:-$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)}"
case "$jobs" in
  ''|*[!0-9]*|0) jobs=4 ;;
esac

if [ ! -x self-host/ir_driver ]; then
  make --no-print-directory self-host-ir-driver
fi

tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/pith-ir-check-XXXXXX")"
trap 'rm -rf "$tmpdir"' EXIT

if [ "$mode" = "count-marks" ]; then
  echo "--- count marks (${jobs} workers) ---"
  PITH_CHECK_COUNTS=1 ./self-host/ir_driver --combined tests/count_marks/marks.pith \
    | awk '$1=="func"{f=$0; next} $1=="mark"{if(f!=""){print f; f=""} print}' > "$tmpdir/marks.out"
  diff -u tests/count_marks/marks.expected "$tmpdir/marks.out" || {
    echo "marks differ from tests/count_marks/marks.expected"
    exit 1
  }
else
  echo "--- IR contract (ir_driver --combined --validate) over the corpus (${jobs} workers) ---"
fi

files=()
for f in tests/cases/test_*.pith examples/*.pith; do
  [ -f "$f" ] || continue
  grep -q "^fn main" "$f" || continue
  files+=("$f")
done

n="${#files[@]}"
status_dir="$tmpdir/status"
mkdir -p "$status_dir"

run_one() {
  local idx="$1"
  local f="$2"
  local work="$tmpdir/work-$idx"
  mkdir -p "$work"

  if [ "$mode" = "validate" ]; then
    if ./self-host/ir_driver --combined --validate "$f" > /dev/null 2> "$work/validate.err"; then
      :
    else
      printf 'VALIDATE FAIL %s: %s\n' "$f" "$(head -1 "$work/validate.err" | cut -c1-160)" > "$status_dir/$idx.fail"
    fi
  else
    ./self-host/ir_driver --combined "$f" > "$work/plain.ir" 2>&1 || true
    PITH_CHECK_COUNTS=1 ./self-host/ir_driver --combined "$f" > "$work/withmarks.ir" 2>&1 || true
    grep -v '^mark ' "$work/withmarks.ir" > "$work/marked.ir" || true
    if ! cmp -s "$work/plain.ir" "$work/marked.ir"; then
      printf 'MARKS CHANGE THE IR: %s\n' "$f" > "$status_dir/$idx.diff"
    fi
    PITH_CHECK_COUNTS=only ./target/release/pith build-ir "$work/withmarks.ir" "$work/counts.bin" 2>&1 \
      | grep -E '^  (unbalanced|borrow): ' > "$work/counts.out" || true
    if [ -s "$work/counts.out" ]; then
      {
        printf 'COUNT CHECK: %s\n' "$f"
        cat "$work/counts.out"
      } > "$status_dir/$idx.unbalanced"
    fi
  fi

  rm -rf "$work"
}

active=0
for idx in "${!files[@]}"; do
  run_one "$idx" "${files[$idx]}" &
  active=$((active + 1))
  if [ "$active" -ge "$jobs" ]; then
    wait -n || true
    active=$((active - 1))
  fi
done
wait || true

if [ "$mode" = "validate" ]; then
  bad=0
  for idx in "${!files[@]}"; do
    if [ -f "$status_dir/$idx.fail" ]; then
      cat "$status_dir/$idx.fail"
      bad=$((bad + 1))
    fi
  done
  echo "validated ${n} programs, ${bad} failed"
  [ "$bad" -eq 0 ]
else
  bad=0
  unbalanced=0
  for idx in "${!files[@]}"; do
    if [ -f "$status_dir/$idx.diff" ]; then
      cat "$status_dir/$idx.diff"
      bad=$((bad + 1))
    fi
    if [ -f "$status_dir/$idx.unbalanced" ]; then
      cat "$status_dir/$idx.unbalanced"
      unbalanced=$((unbalanced + 1))
    fi
  done
  echo "compared ${n} programs with and without marks, ${bad} differ"
  echo "count-checked ${n} programs, ${unbalanced} with findings"
  [ "$bad" -eq 0 ] && [ "$unbalanced" -eq 0 ]
fi
