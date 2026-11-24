#!/usr/bin/env bash
# checks how the compiler handles pathological input.
#
# the inputs are too large to keep in the repo, so this generates them.
# each case runs under an 8 MiB stack, the usual default, with a time
# limit. an input past one of the limits in SECURITY.md must fail with
# its diagnostic and print a bounded amount of output. an input inside
# the limits must still compile and run. a stack overflow, a timeout or
# a crash fails the check.
set -u
export LC_ALL=C

checker="./self-host/pith_main"
pith="./target/release/pith"
for tool in "$checker" "$pith" ./self-host/ir_driver; do
    if [ ! -x "$tool" ]; then
        echo "error: $tool not built; run make build self-host self-host-ir-driver first" >&2
        exit 1
    fi
done

ulimit -s 8192
tmp=$(mktemp -d /tmp/pith-limits-XXXXXX)
trap 'rm -rf "$tmp"' EXIT
fail=0

# repeat <text> <count>: the text written count times. awk reads \n in
# the text as a newline.
repeat() {
    awk -v text="$1" -v count="$2" 'BEGIN { for (i = 0; i < count; i++) printf "%s", text }'
}

# a main that binds x to the expression in $1 and prints it
main_with() {
    printf 'fn main() -> Int:\n    x := %s\n    print("{x}")\n    return 0\n' "$1"
}

# expect_error <name> <code> <text>: check fails with the code, at most
# 3 diagnostics, and output that names the text
expect_error() {
    local name=$1 code=$2 text=$3 file="$tmp/$1.pith" out status count
    out=$(timeout 20 "$checker" check "$file" 2>&1)
    status=$?
    count=$(printf '%s\n' "$out" | grep -c 'error\[')
    if [ $status -ne 1 ] || ! printf '%s' "$out" | grep -q "error\[$code\]" \
        || ! printf '%s' "$out" | grep -qF "$text" || [ "$count" -gt 3 ]; then
        echo "FAIL $name (exit $status, $count errors)"
        printf '%s\n' "$out" | head -c 600
        echo
        fail=1
    else
        echo "ok   $name"
    fi
}

# expect_output <name> <expected>: run prints exactly the expected text
expect_output() {
    local name=$1 want=$2 out status
    out=$(timeout 60 "$pith" run "$tmp/$1.pith" 2>&1)
    status=$?
    if [ $status -ne 0 ] || [ "$out" != "$want" ]; then
        echo "FAIL $name (exit $status)"
        printf '%s\n' "$out" | head -c 600
        echo
        fail=1
    else
        echo "ok   $name"
    fi
}

# --- inside the limits: these compile and run ---

main_with "$(repeat '1 + ' 19999)1" > "$tmp/chain_20000.pith"
expect_output chain_20000 20000

main_with "$(repeat '(' 250)1$(repeat ')' 250)" > "$tmp/parens_250.pith"
expect_output parens_250 1

main_with "$(repeat '- ' 250)1" > "$tmp/unary_250.pith"
expect_output unary_250 1

printf 'fn main() -> Int:\n    s := "a"\n    x := s%s\n    print(x)\n    return 0\n' "$(repeat '.len().to_string()' 1000)" > "$tmp/calls_2000.pith"
expect_output calls_2000 1

# a string join of 256 parts or more once lost its first part: the part
# count overflowed into the bits that mark a part as an Int
printf 'fn main() -> Int:\n    x := %s"a"\n    print("{x.len()}")\n    mut s := ""\n    s = s + %s"a"\n    print("{s.len()}")\n    return 0\n' \
    "$(repeat '"a" + ' 299)" "$(repeat '"a" + ' 299)" > "$tmp/join_300.pith"
expect_output join_300 "300
300"

# 500 nested blocks, one more space of indentation each
{
    printf 'fn main() -> Int:\n'
    for i in $(seq 0 499); do
        printf '%*sif true:\n' $((i + 4)) ""
    done
    printf '%*sprint("deep")\n    return 0\n' 504 ""
} > "$tmp/blocks_500.pith"
expect_output blocks_500 deep

# 150,000 comment lines once overflowed the lexer's stack
{
    printf 'fn main() -> Int:\n'
    repeat '    # comment line\n' 150000
    printf '    print("ok")\n    return 0\n'
} > "$tmp/comments_150000.pith"
expect_output comments_150000 ok

# --- past the limits: one diagnostic, quickly ---

main_with "$(repeat '-' 100000)1" > "$tmp/unary_100000.pith"
expect_error unary_100000 E242 "nesting too deep"

main_with "$(repeat '1 + ' 49999)1" > "$tmp/chain_50000.pith"
expect_error chain_50000 E242 "expression too deep"

printf 'fn main() -> Int:\n    s := "a"\n    x := s%s\n    print(x)\n    return 0\n' "$(repeat '.len().to_string()' 10000)" > "$tmp/calls_20000.pith"
expect_error calls_20000 E242 "expression too deep"

main_with "$(repeat '[' 5000)1$(repeat ']' 5000)" > "$tmp/brackets_5000.pith"
expect_error brackets_5000 E242 "nesting too deep"

main_with "$(repeat '(' 100000)" > "$tmp/unclosed_100000.pith"
expect_error unclosed_100000 E242 "nesting too deep"

printf 'fn main() -> Int:\n    x: %sInt%s := []\n    return 0\n' "$(repeat 'List[' 100000)" "$(repeat ']' 100000)" > "$tmp/types_100000.pith"
expect_error types_100000 E242 "nesting too deep"

{
    printf 'fn main() -> Int:\n'
    for i in $(seq 0 1099); do
        printf '%*sif true:\n' $((i + 4)) ""
    done
    printf '%*sprint("deep")\n    return 0\n' 1104 ""
} > "$tmp/blocks_1100.pith"
expect_error blocks_1100 E242 "blocks nested too deep"

printf 'fn main() -> Int:\n    s := "ok \377 no"\n    return 0\n' > "$tmp/not_utf8.pith"
expect_error not_utf8 E243 "not_utf8.pith:2:14: error[E243]: the file is not valid utf-8 (invalid byte at offset 31)"

{
    printf 'fn main() -> Int:\n'
    head -c 17000000 /dev/zero | tr '\0' '#'
    printf '\n    return 0\n'
} > "$tmp/too_large.pith"
expect_error too_large E243 "source file too large"

# 500 bad lines print 100 errors and a note with the count left out
{
    printf 'fn main() -> Int:\n'
    repeat '    x := )\n' 500
    printf '    return 0\n'
} > "$tmp/errors_500.pith"
out=$(timeout 20 "$checker" check "$tmp/errors_500.pith" 2>&1)
count=$(printf '%s\n' "$out" | grep -c 'error\[')
if [ "$count" -eq 100 ] && printf '%s' "$out" | grep -qE "^note: too many errors; [0-9]+ more not shown"; then
    echo "ok   errors_500"
else
    echo "FAIL errors_500 ($count errors printed)"
    printf '%s\n' "$out" | tail -3
    fail=1
fi

if [ $fail -ne 0 ]; then
    exit 1
fi
echo "the compiler handles every pathological input"
