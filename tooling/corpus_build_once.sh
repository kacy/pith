#!/usr/bin/env bash
# Pre-build the regression corpus executables once in parallel into an output
# directory so `verify-green-corpus-only`, `rc-check-only`, and
# `verify-osthread-corpus-only` can execute the compiled binaries across all
# runtime flag combinations (green default, green 1-worker, rc-check, os-thread)
# without recompiling the same ~700 sources five times.
set -euo pipefail

if [ $# -lt 1 ]; then
  echo "usage: $0 <out-dir> [expected-file...]" >&2
  exit 2
fi

out_dir="$1"
shift
mkdir -p "$out_dir"

jobs="${PITH_JOBS:-$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)}"
case "$jobs" in
  ''|*[!0-9]*|0) jobs=4 ;;
esac

# Warm/bootstrap `self-host/ir_driver` once before parallel compilation starts.
./target/release/pith run examples/hello.pith >/dev/null

status_dir="$(mktemp -d "${TMPDIR:-/tmp}/pith-corpus-build-XXXXXX")"
trap 'rm -rf "$status_dir"' EXIT

names=()
for f in "$@"; do
  name="$(basename "$f" .txt)"
  src="tests/cases/${name}.pith"
  [ -f "$src" ] || continue
  names+=("$name")
done

echo "--- building ${#names[@]} regression corpus binaries once (${jobs} workers) ---"

build_one() {
  local idx="$1"
  local name="$2"
  local src="tests/cases/${name}.pith"
  local bin="${out_dir}/${name}"
  local err="${status_dir}/${idx}.err"
  if timeout 120 ./target/release/pith build "$src" -o "$bin" > /dev/null 2> "$err"; then
    rm -f "${bin}.o" "$err"
  else
    printf 'FAIL %s (build)\n' "$name" > "${status_dir}/${idx}.fail"
    head -n 40 "$err" | sed -e 's/^/  /' >> "${status_dir}/${idx}.fail"
    rm -f "$bin" "${bin}.o" "$err"
  fi
}

active=0
for idx in "${!names[@]}"; do
  build_one "$idx" "${names[$idx]}" &
  active=$((active + 1))
  if [ "$active" -ge "$jobs" ]; then
    wait -n || true
    active=$((active - 1))
  fi
done
wait || true

failed=0
for idx in "${!names[@]}"; do
  if [ -f "${status_dir}/${idx}.fail" ]; then
    cat "${status_dir}/${idx}.fail"
    failed=$((failed + 1))
  fi
done

if [ "$failed" -gt 0 ]; then
  echo "corpus build failed: ${failed} of ${#names[@]} cases"
  exit 1
fi
echo "built ${#names[@]} regression corpus binaries in ${out_dir}"
