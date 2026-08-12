#!/usr/bin/env bash
# transcript harness for the language server.
#
# each case in tests/lsp/cases/ holds one raw json-rpc message per line,
# with no framing; the harness adds content-length framing (byte-accurate
# under LC_ALL=C), pipes the stream into `pith_main lsp` with debouncing
# disabled, deframes stdout, and diffs the bodies against the frozen
# expectation in tests/lsp/expected/.
#
# __ROOT__ in a case or expectation stands for the repo root, so cases
# can reference fixture files under tests/lsp/fixtures/ from any checkout.
#
# LSP_CHECK_UPDATE=1 rewrites the expected files from the current output
# instead of diffing; eyeball every line before freezing.
set -u
export LC_ALL=C

root="$PWD"
server="./self-host/pith_main"
update="${LSP_CHECK_UPDATE:-0}"
fail=0

if [ ! -x "$server" ]; then
    echo "error: $server not built; run make self-host first" >&2
    exit 1
fi

for case_file in tests/lsp/cases/*.jsonl; do
    name=$(basename "$case_file" .jsonl)
    expected="tests/lsp/expected/$name.jsonl"
    tmp_in=$(mktemp /tmp/pith-lsp-in-XXXXXX)
    tmp_out=$(mktemp /tmp/pith-lsp-out-XXXXXX)

    # frame each message; __ROOT__ becomes the absolute repo root on the way in
    awk -v root="$root" '{
        gsub(/__ROOT__/, root)
        printf "Content-Length: %d\r\n\r\n%s", length($0), $0
    }' "$case_file" > "$tmp_in"

    env PITH_LSP_NO_DEBOUNCE=1 "$server" lsp < "$tmp_in" > "$tmp_out"
    status=$?

    # deframe: put each body on its own line, drop headers and blank
    # separators, and hide the repo root on the way out
    got=$(sed 's/Content-Length:/\n&/g' "$tmp_out" | grep -v '^Content-Length' | sed '/^\r*$/d' | sed "s|$root|__ROOT__|g")
    rm -f "$tmp_in" "$tmp_out"

    if [ "$status" -ne 0 ]; then
        echo "FAIL $name (server exited $status)"
        fail=1
        continue
    fi

    if [ "$update" = "1" ]; then
        printf '%s\n' "$got" > "$expected"
        echo "updated $expected"
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
    echo "lsp transcripts match golden files"
fi
exit $fail
