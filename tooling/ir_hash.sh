#!/usr/bin/env bash
# hash the emitted ir for the whole corpus, one line per source.
#
# a refactor that is supposed to change nothing reproduces every hash; a
# change that is supposed to touch one shape shows up in exactly the files
# that have it. capture once before, once after, and diff the two files.
#
#   tooling/ir_hash.sh before.txt
#   ... make the change, rebuild the compiler AND the ir driver ...
#   tooling/ir_hash.sh after.txt
#   diff before.txt after.txt
#
# the dump goes to a disk-backed temp inside the repo and is overwritten per
# source. a full corpus dump is around half a gigabyte, and /tmp on this box
# is ram: writing it there once came within a few hundred megabytes of the
# out-of-memory killer. sources that do not compile (the negative cases)
# report NOIR, and about forty of them are expected to.
#
# compare only completed captures. a diff against a file still being written
# once reported 129 changes that were not there.
set -u

out=${1:?output file}
tmp=$PWD/.irhash.tmp
: > "$out"
for f in tests/cases/*.pith examples/*.pith; do
    b=$(basename "$f" .pith)
    rm -f "$tmp"
    PITH_DUMP_IR=$tmp ./target/release/pith build "$f" > /dev/null 2>&1
    if [ -f "$tmp" ]; then
        echo "$b $(sha256sum < "$tmp" | cut -c1-16)"
    else
        echo "$b NOIR"
    fi >> "$out"
done
rm -f "$tmp"
echo "$(wc -l < "$out") sources hashed into $out ($(grep -c NOIR "$out") NOIR)"
