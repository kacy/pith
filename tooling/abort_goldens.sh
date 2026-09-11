#!/usr/bin/env bash
#
# goldens for how a program aborts.
#
# the regression corpus compares stdout, so it cannot see the two things an
# abort is made of: the diagnostic on stderr and the exit status. these cases
# pin both. each program in tests/aborts is meant to die, and its golden holds
# stdout and stderr interleaved (the lines printed before the abort prove the
# program ran up to it) followed by `exit: <status>`.
#
# `.pith` cases go through `pith run`. `.ir` cases are combined text ir
# compiled with `pith build-ir`: they exist for the failure modes safe pith
# source cannot express, a null, garbage or already-freed handle handed to an
# indexing operation. both kinds run through the same backend, so a fast path
# the consumer adds for the common case is proved to abort exactly as the
# runtime call it replaced did.
#
# regenerate with PITH_ABORT_GOLDEN_UPDATE=1 and read the diff before
# committing: a golden that changes is a behaviour change.

set -u

PITH=${PITH:-./target/release/pith}
CASES=tests/aborts
EXPECTED_DIR=$CASES/expected
UPDATE=${PITH_ABORT_GOLDEN_UPDATE:-0}

if [ ! -x "$PITH" ]; then
  echo "no pith binary at $PITH; run 'make build' first"
  exit 1
fi

tmpdir=$(mktemp -d "${TMPDIR:-/tmp}/pith-abort-goldens-XXXXXX")
trap 'rm -rf "$tmpdir"' EXIT

pass=0
fail=0

compare() {
  local name=$1 actual=$2
  local expected_file="$EXPECTED_DIR/$name.txt"
  if [ "$UPDATE" = "1" ]; then
    mkdir -p "$EXPECTED_DIR"
    printf '%s\n' "$actual" > "$expected_file"
    echo "wrote $expected_file"
    return 0
  fi
  if [ ! -f "$expected_file" ]; then
    echo "FAIL $name (no golden at $expected_file)"
    fail=$((fail + 1))
    return 0
  fi
  if [ "$actual" = "$(cat "$expected_file")" ]; then
    echo "ok   $name"
    pass=$((pass + 1))
  else
    echo "FAIL $name"
    diff <(printf '%s\n' "$actual") "$expected_file" | sed -e 's/^/  /' | head -n 40
    fail=$((fail + 1))
  fi
}

for f in "$CASES"/*.pith; do
  [ -e "$f" ] || continue
  name=$(basename "$f" .pith)
  output=$(timeout 60 "$PITH" run "$f" 2>&1)
  status=$?
  compare "$name" "$(printf '%s\nexit: %d' "$output" "$status")"
done

for f in "$CASES"/*.ir; do
  [ -e "$f" ] || continue
  name=$(basename "$f" .ir)
  exe="$tmpdir/$name"
  if ! timeout 60 "$PITH" build-ir "$f" "$exe" > "$tmpdir/build.log" 2>&1; then
    echo "FAIL $name (build-ir)"
    sed -e 's/^/  /' "$tmpdir/build.log" | head -n 20
    fail=$((fail + 1))
    continue
  fi
  output=$(timeout 15 "$exe" 2>&1)
  status=$?
  compare "$name" "$(printf '%s\nexit: %d' "$output" "$status")"
done

echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
