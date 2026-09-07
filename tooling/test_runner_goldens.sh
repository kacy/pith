#!/usr/bin/env bash
#
# goldens for what `pith test` reports.
#
# the machine-readable output is a contract with whatever ci consumes it, so it
# is pinned line for line rather than grepped: a record that quietly loses a
# field, renames an outcome or reorders its keys still passes a grep and breaks
# every reader. the prose runs are here for the same reason on the tag side —
# they pin which tests a selection runs and what the runner says when a tag
# names nothing.
#
# each case runs tests/testrunner/fixture.pith one way and compares stdout plus
# the exit status against tests/testrunner/expected/<case>.txt. stderr is left
# out: a failing built-in assertion prints there from the child, interleaved
# with nothing to order it against, and its text is already on the record.
#
# durations vary run to run, so `"duration_ms":<n>` is rewritten to
# `"duration_ms":<ms>` before the comparison. the field stays in the golden,
# which keeps it proved present and well-formed; only its value is dropped.
#
# regenerate with PITH_TEST_GOLDEN_UPDATE=1 and read the diff before committing.

set -u

PITH=${PITH:-./target/release/pith}
FIXTURE=tests/testrunner/fixture.pith
EXPECTED_DIR=tests/testrunner/expected
UPDATE=${PITH_TEST_GOLDEN_UPDATE:-0}

if [ ! -x "$PITH" ]; then
  echo "no pith binary at $PITH — run 'make build' first"
  exit 1
fi

pass=0
fail=0

run_case() {
  local name=$1
  shift
  local output status normalized expected_file
  output=$("$PITH" test "$FIXTURE" "$@" 2>/dev/null)
  status=$?
  normalized=$(printf '%s\n' "$output" | sed 's/"duration_ms":[0-9][0-9]*/"duration_ms":<ms>/g')
  normalized=$(printf '%s\nexit: %d\n' "$normalized" "$status")
  expected_file="$EXPECTED_DIR/$name.txt"
  if [ "$UPDATE" = "1" ]; then
    printf '%s\n' "$normalized" > "$expected_file"
    echo "wrote $expected_file"
    return 0
  fi
  if [ ! -f "$expected_file" ]; then
    echo "FAIL $name (no golden at $expected_file)"
    fail=$((fail + 1))
    return 0
  fi
  if [ "$normalized" = "$(cat "$expected_file")" ]; then
    echo "ok   $name"
    pass=$((pass + 1))
  else
    echo "FAIL $name"
    diff -u "$expected_file" - <<< "$normalized"
    fail=$((fail + 1))
  fi
}

echo "--- test runner goldens ---"

# machine-readable output: every outcome the runner has, in one run
run_case json_full --json
# a filter aimed at one table row: the rows that were not selected leave no
# record, the tests that were not selected leave a filtered one
run_case json_row --json --filter "rows report one by one / beta"
# a tag no test carries: the summary names it and the run fails
run_case json_unmatched_tag --json --tag nope

# prose: the shape a person reads, unchanged by any of this
run_case prose_full
# one tag selected
run_case tag_select --tag fast
# one tag excluded
run_case tag_exclude --exclude-tag fast
# a tag and a filter compose as a conjunction
run_case tag_and_filter --tag fast --filter arithmetic
# a tag on a parameterized test selects the whole table
run_case tag_parameterized --tag table
# a test carrying two tags is selected by either one
run_case tag_one_of_several --tag slow
# an unmatched tag says so rather than passing an empty run
run_case tag_unmatched --tag nope
# the row identity `--filter` has always accepted still selects one row
run_case row_filter --filter "rows report one by one / beta"

if [ "$UPDATE" = "1" ]; then
  exit 0
fi

echo "$pass passed, $fail failed"
if [ "$fail" -gt 0 ]; then
  exit 1
fi
echo "all test runner goldens passed"
