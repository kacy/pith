#!/usr/bin/env bash
# golden harness for diagnostic positions.
#
# each case in tests/diagnostics/cases/ is a file that fails `check`. the
# harness renders its diagnostics with the human formatter — which prints
# the offending source line and a caret under the column — and diffs the
# whole rendering against tests/diagnostics/expected/.
#
# the invalid-example gates compare the set of error CODES a file
# produces, so a caret that moves to the wrong token passes every one of
# them. this is the gate that sees it.
#
# DIAG_CHECK_UPDATE=1 rewrites the expected files from the current output
# instead of diffing; read every line before freezing one.
set -u
export LC_ALL=C

checker="./self-host/pith_main"
update="${DIAG_CHECK_UPDATE:-0}"
fail=0

if [ ! -x "$checker" ]; then
    echo "error: $checker not built; run make self-host first" >&2
    exit 1
fi

# a glob that matched nothing would run no case and report a pass, which
# is the one verdict this gate must never give
count=$(ls tests/diagnostics/cases/*.pith 2>/dev/null | wc -l)
if [ "$count" -eq 0 ]; then
    echo "error: no cases under tests/diagnostics/cases; refusing to report a pass" >&2
    exit 1
fi

for case_file in tests/diagnostics/cases/*.pith; do
    name=$(basename "$case_file" .pith)
    expected="tests/diagnostics/expected/$name.txt"

    got=$("$checker" check "$case_file" 2>&1)
    status=$?

    if [ "$status" -eq 0 ]; then
        echo "FAIL $name (check passed; a case here has to report something)"
        fail=1
        continue
    fi

    if [ "$update" = "1" ]; then
        printf '%s\n' "$got" > "$expected"
        echo "updated $expected"
        continue
    fi

    if [ ! -f "$expected" ]; then
        echo "FAIL $name (missing $expected)"
        fail=1
        continue
    fi

    if printf '%s\n' "$got" | diff -u "$expected" - > /dev/null; then
        echo "ok $name"
    else
        echo "FAIL $name"
        printf '%s\n' "$got" | diff -u "$expected" - || true
        fail=1
    fi
done

if [ "$fail" -eq 0 ]; then
    echo "$count diagnostic renderings match golden files"
fi
exit $fail
