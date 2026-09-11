#!/bin/sh
# print why a regression case failed, bounded so a runaway program cannot
# flood the log.
#
# usage: explain_case_failure.sh <expected-file> <stdout-file> <stderr-file> <exit-status>
#
# the corpus gates in the Makefile used to report only `FAIL <name>`. a
# failure that never reproduces locally (#1092: a tls handshake test failing
# in ci and nowhere else) then leaves nothing to work from: not whether the
# program timed out, crashed, or printed a different line. this prints the
# exit status (naming the `timeout` sentinel), the first lines of a unified
# diff of expected against actual stdout, and the first lines of stderr.
set -u

expected="$1"
out="$2"
err="$3"
status="$4"
limit="${CASE_FAILURE_LINES:-60}"

case "$status" in
    124) echo "  exit status: 124 (killed by timeout)" ;;
    137) echo "  exit status: 137 (killed by SIGKILL)" ;;
    *) echo "  exit status: $status" ;;
esac

echo "  --- diff expected actual (first $limit lines) ---"
diff -u "$expected" "$out" | sed -e '1,2d' | head -n "$limit" | sed -e 's/^/  /'
if [ -s "$err" ]; then
    echo "  --- stderr (first $limit lines) ---"
    head -n "$limit" "$err" | sed -e 's/^/  /'
else
    echo "  --- stderr: empty ---"
fi
