#!/usr/bin/env bash
# survey the corpus for faults inside generic bodies.
#
# a generic body was never type-checked before the per-specialization walk
# existed, so the first time the walk runs over a body of code it may find
# real faults and it may find checks that assumed a concrete declaration
# site. this runs the self-hosted checker with the walk in count mode — its
# findings come out as warnings and the exit code stays 0 — and tallies what
# it finds by code and by file, so each finding can be triaged before the
# walk's reports become errors.
#
# the JSON output is what gets read: the text renderer prints diagnostics
# only when an error is among them, so a run whose only findings are
# warnings prints "ok" and nothing else there.
#
# usage: tooling/generic_body_dryrun.sh [file ...]
# with no arguments it covers tests/cases, examples, std and self-host.
set -u
shopt -s globstar nullglob

checker=./self-host/pith_main
if [ ! -x "$checker" ]; then
    echo "no $checker: run make self-host first" >&2
    exit 2
fi

files=("$@")
if [ ${#files[@]} -eq 0 ]; then
    files=(tests/cases/*.pith examples/*.pith std/**/*.pith self-host/*.pith)
fi

findings=$(mktemp "${TMPDIR:-/tmp}/generic-dryrun.XXXXXX")
trap 'rm -f "$findings"' EXIT
for f in "${files[@]}"; do
    PITH_CHECK_GENERIC_BODIES=count "$checker" check --json "$f" 2>/dev/null \
        | python3 -c '
import json, sys
try:
    diags = json.load(sys.stdin)
except Exception:
    sys.exit(0)
for d in diags:
    if d.get("severity") == "warning":
        print("%s\t%s\t%s:%s\t%s" % (sys.argv[1], d.get("code"), d.get("file"), d.get("line"), d.get("message")))
' "$f" >> "$findings"
done

total=$(wc -l < "$findings")
echo "findings: $total"
if [ "$total" -eq 0 ]; then
    exit 0
fi
echo
echo "by code:"
cut -f2 "$findings" | sort | uniq -c | sort -rn
echo
echo "by file:"
cut -f1 "$findings" | sort | uniq -c | sort -rn
echo
echo "each:"
cat "$findings"
