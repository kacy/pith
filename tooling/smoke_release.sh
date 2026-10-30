#!/bin/sh
# unpack a release tarball into a scratch directory and use it from outside
# the checkout, the way an installed toolchain is used: through a symlink on
# PATH, with no repo std/ above the program.
set -eu

tarball=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/home" "$work/bin" "$work/proj"
tar -C "$work/home" -xzf "$tarball" --strip-components=1
ln -s "$work/home/bin/pith" "$work/bin/pith"
PATH="$work/bin:$PATH"
unset PITH_HOME PITH_SELF_HOST PITH_IR_DRIVER PITH_RUNTIME_LIB
cd "$work/proj"

cat > hello.pith <<'PITH'
import std.json as json

fn main() -> Int!:
    doc := json.parse("{{\"name\": \"pith\"}}")
    print("hello, " + json.object_get_string(doc, "name").to_upper())
    return 0

test "arithmetic":
    assert 1 + 2 == 3
PITH

pith version
pith check hello.pith
test "$(pith run hello.pith)" = "hello, PITH"
pith build hello.pith
test "$(./hello)" = "hello, PITH"
pith test hello.pith
pith fmt hello.pith > /dev/null
echo "smoke ok"
