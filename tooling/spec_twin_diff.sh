#!/usr/bin/env bash
# compare each generic specialization's emitted body against its annotated
# concrete twin.
#
# a golden that pairs `f[T]` with `f_con` (the same body, concrete types
# written out) is the control the generic-body work measures itself against:
# once a specialization is typed and tracked like a concrete body, the two
# should lower to the same opcode sequence. this dumps a program's IR, pairs
# every `func <name>__<kinds>` with `func <name>_con`, strips register and
# label numbers, and diffs the opcode sequences. a pair that differs is a
# finding to explain, not a case to relax. a twin is written at one type, so
# a specialization at another type (`probe__string` against an Int twin) is
# expected to differ where the kinds lower differently; the same-typed pair
# is the one that must match.
#
# usage: tooling/spec_twin_diff.sh <file.pith> [...]
# exit 1 when any pair differs. the IR dump goes to a disk-backed temp under
# the repo (never /tmp, which is RAM on the development box).
set -u
pith=./target/release/pith
tmp=$PWD/.twindiff.tmp
status=0
normalize() {
    # drop the func header and the parameter lines (a twin may spell its
    # parameters differently; the body is what is compared), then blank every
    # register, label and temp-name number so two bodies compare by shape
    sed -e '1d' -e '/^param /d' -e 's/\bL[0-9]\+\b/L/g' -e 's/_[0-9]\+\([_ ]\|$\)/_N\1/g' -e 's/\b[0-9]\+\b/N/g'
}
for f in "$@"; do
    rm -f "$tmp"
    PITH_DUMP_IR=$tmp "$pith" build "$f" > /dev/null 2>&1 || { echo "$f: build failed"; status=1; continue; }
    for spec in $(grep -oE '^func [A-Za-z0-9_]+__[a-z][A-Za-z0-9_]*' "$tmp" | awk '{print $2}'); do
        base=${spec%%__*}
        twin="${base}_con"
        grep -qE "^func $twin " "$tmp" || continue
        a=$(awk -v s="func $spec " 'index($0,s)==1{p=1} p{print} p&&/^endfunc/{exit}' "$tmp" | normalize)
        b=$(awk -v s="func $twin " 'index($0,s)==1{p=1} p{print} p&&/^endfunc/{exit}' "$tmp" | normalize)
        if [ "$a" = "$b" ]; then
            echo "same   $f: $spec vs $twin"
        else
            echo "DIFFER $f: $spec vs $twin"
            diff <(echo "$a") <(echo "$b") | head -12 | sed 's/^/    /'
            status=1
        fi
    done
done
rm -f "$tmp"
exit $status
