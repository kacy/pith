#!/usr/bin/env bash
# run the cross-language benchmarks in bench/compare.list and print one table
# of median wall times for pith, go and rust.
#
#   bench/compare.sh [name...]
#
# each line of bench/compare.list names a benchmark, its arguments and how to
# build each arm (see the header of that file). a name of the form
# <source>/<label> runs bench/<source>.pith with another set of arguments, and
# each source is built once. with names given, only those lines run; a source
# name selects all of its lines.
#
# every arm is built first, then run once unmeasured, and its output is checked
# against the pith arm's (see work_output), which is what says the arms did the
# same work. a benchmark whose outputs differ is still timed, but its row is
# marked.
#
# within a round every arm runs once, and the starting arm rotates from round
# to round, for the reason given in bench/http_sweep.sh: a fixed order hands
# whichever arm goes first the host's fresher state.
#
# settings, all from the environment:
#   PITH      pith compiler (default: ./target/release/pith)
#   TRIALS    rounds; the median is reported (default: 5)
#   CPUS      taskset cpu list for every run (default: 0-7,16-23, one ccd of a
#             9950x with its smt siblings)
#   WORKERS   PITH_GREEN_WORKERS, GOMAXPROCS, TOKIO_WORKERS and
#             TOKIO_WORKER_THREADS (default: 16)
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

pith=${PITH:-./target/release/pith}
trials=${TRIALS:-5}
cpus=${CPUS:-0-7,16-23}
workers=${WORKERS:-16}
list=bench/compare.list

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export PITH_GREEN_WORKERS=$workers GOMAXPROCS=$workers TOKIO_WORKERS=$workers TOKIO_WORKER_THREADS=$workers

# milliseconds since the epoch, to the microsecond
now_ms() {
    local ns
    ns=$(date +%s%N)
    echo "$((ns / 1000000)).$(((ns / 1000) % 1000))"
}

median() {
    tr ' ' '\n' | awk 'NF' | sort -g | awk '{ v[NR] = $1 } END {
        if (NR == 0) { print "-"; exit }
        if (NR % 2) print v[(NR + 1) / 2]
        else print (v[NR / 2] + v[NR / 2 + 1]) / 2
    }'
}

# the part of a run's output that says what work was done. a line's own
# check field is an awk program that picks it out; without one, it is the
# checksum and digest values when the output has any, else every line that
# does not report a time, a rate or memory.
work_output() {
    local check=$1 out
    out=$(cat)
    if [ -n "$check" ]; then
        echo "$out" | awk "$check"
    elif echo "$out" | grep -qiE 'checksum|digest'; then
        echo "$out" | grep -oiE '(checksum|digest)[=: ]+[^ ]+'
    else
        echo "$out" | grep -viE '[0-9.]+ ?(ms|ns|us|µs|s)\b|_(ms|ns|us)\b|elapsed|time|rate|rss|ns/op|per sec|/s\b' || true
    fi
}

# build one arm and print the command that runs it, or nothing to skip it.
# a build line is one of:
#   pith                     pith build bench/<name>.pith
#   go:<file or dir>         go build of a file, or of a module directory
#   rustc:<file>             rustc -O of one file
#   cargo:<dir>[:<binary>]   cargo build --release of a crate directory
#   -                        no arm
build_arm() {
    local name=$1 spec=$2
    local out=$work/$name.$3
    if [ -n "${built[$out]:-}" ]; then
        echo "$out"
        return 0
    fi
    case $spec in
    -) return 0 ;;
    pith)
        "$pith" build "bench/$name.pith" > /dev/null
        mv "bench/$name" "$out"
        rm -f "bench/$name.o"
        ;;
    go:*)
        local src=${spec#go:}
        if [ -d "$src" ]; then
            (cd "$src" && go build -o "$out" .)
        else
            go build -o "$out" "$src"
        fi
        ;;
    rustc:*) rustc -O -o "$out" "${spec#rustc:}" 2> /dev/null ;;
    cargo:*)
        local rest=${spec#cargo:} dir bin
        dir=${rest%%:*}
        bin=${rest#*:}
        [ "$bin" = "$rest" ] && bin=$(basename "$dir")
        cargo build -q --release --manifest-path "$dir/Cargo.toml"
        cp "$dir/target/release/$bin" "$out"
        ;;
    *)
        echo "unknown build spec for $name: $spec" >&2
        exit 1
        ;;
    esac
    echo "$out"
}

wanted=" $* "
names=()
declare -A cmd args check built
declare -A mismatch=()
while IFS='|' read -r name a p g r c; do
    name=$(echo "$name" | xargs)
    case $name in '' | '#'*) continue ;; esac
    if [ $# -gt 0 ] && [[ $wanted != *" $name "* ]] && [[ $wanted != *" ${name%%/*} "* ]]; then
        continue
    fi
    names+=("$name")
    args[$name]=$(echo "$a" | xargs)
    check[$name]=$(echo "${c:-}" | xargs)
    src=${name%%/*}
    echo "building $name" >&2
    for arm in pith go rust; do
        case $arm in pith) spec=$p ;; go) spec=$g ;; rust) spec=$r ;; esac
        cmd[$name,$arm]=$(build_arm "$src" "$(echo "$spec" | xargs)" "$arm")
        [ -n "${cmd[$name,$arm]}" ] && built[${cmd[$name,$arm]}]=1
    done
done < "$list"

arms=(pith go rust)
declare -A times
for name in "${names[@]}"; do
    # shellcheck disable=SC2086
    base=$(taskset -c "$cpus" ${cmd[$name,pith]} ${args[$name]} 2>&1 | work_output "${check[$name]}")
    for arm in go rust; do
        c=${cmd[$name,$arm]}
        [ -z "$c" ] && continue
        # shellcheck disable=SC2086
        got=$(taskset -c "$cpus" $c ${args[$name]} 2>&1 | work_output "${check[$name]}")
        if [ "$got" != "$base" ]; then
            mismatch[$name,$arm]=1
            echo "  $name: $arm output differs from pith" >&2
        fi
    done
done

for trial in $(seq "$trials"); do
    for name in "${names[@]}"; do
        for i in 0 1 2; do
            arm=${arms[$(((i + trial - 1) % 3))]}
            c=${cmd[$name,$arm]}
            [ -z "$c" ] && continue
            t0=$(now_ms)
            # shellcheck disable=SC2086
            taskset -c "$cpus" $c ${args[$name]} > /dev/null 2>&1
            t1=$(now_ms)
            ms=$(awk -v a="$t0" -v b="$t1" 'BEGIN { printf "%.1f", b - a }')
            times[$name,$arm]="${times[$name,$arm]:-} $ms"
            echo "  trial $trial $name $arm: $ms ms" >&2
        done
    done
done

echo
echo "| benchmark | args | pith ms | go ms | rust ms | pith/go | pith/rust |"
echo "|---|---|---:|---:|---:|---:|---:|"
for name in "${names[@]}"; do
    declare -A med=()
    for arm in "${arms[@]}"; do
        med[$arm]=$(echo "${times[$name,$arm]:-}" | median)
    done
    ratio() {
        awk -v a="${med[pith]}" -v b="$1" 'BEGIN { if (b == "-" || b == 0) print "-"; else printf "%.2fx", a / b }'
    }
    mark() { [ -n "${mismatch[$name,$1]:-}" ] && echo '*' || true; }
    printf '| %s | `%s` | %s | %s%s | %s%s | %s | %s |\n' "$name" "${args[$name]}" \
        "${med[pith]}" "${med[go]}" "$(mark go)" "${med[rust]}" "$(mark rust)" \
        "$(ratio "${med[go]}")" "$(ratio "${med[rust]}")"
done
if [ ${#mismatch[@]} -gt 0 ]; then
    echo
    echo "* this arm's output differed from pith's; see the build log"
fi
