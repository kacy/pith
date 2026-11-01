#!/usr/bin/env bash
# run.sh - drive fuzz_surfaces in batches and keep what it finds.
#
#   tools/fuzz-surfaces/run.sh [options] [target ...]
#
#   --minutes N    total time per target (default 1)
#   --batch N      seconds per batch process (default 60)
#   --jobs N       targets run in parallel (default 4)
#   --seed N       first batch seed (default 1); batch k uses seed + k
#   --out DIR      findings, corpora and logs (default /tmp/fuzz-surfaces)
#   --asan         build against the asan runtime (see README.md)
#   --count N      deterministic mode for ci: one batch of N execs per target
#                  on small inputs, failing only on a crash or a hang
#   --known DIR    replay the inputs in DIR (named <target>-*.bin) and report
#                  which oracle flags each one
#
# each batch runs under a wall-clock limit of its own length plus --hang
# seconds (default 20). a batch that dies (signal, asan report, non-zero
# exit) or overruns that limit is a finding: the input it was running is
# saved as <out>/findings/<target>-crash-* or -hang-*.

set -u
cd "$(dirname "$0")/../.."

minutes=1
batch=60
jobs=4
seed=1
out=/tmp/fuzz-surfaces
asan=0
count=0
hang=20
known=""
targets=()
while [ $# -gt 0 ]; do
    case "$1" in
        --minutes) minutes=$2; shift 2 ;;
        --batch) batch=$2; shift 2 ;;
        --jobs) jobs=$2; shift 2 ;;
        --seed) seed=$2; shift 2 ;;
        --out) out=$2; shift 2 ;;
        --asan) asan=1; shift ;;
        --count) count=$2; shift 2 ;;
        --hang) hang=$2; shift 2 ;;
        --known) known=$2; shift 2 ;;
        *) targets+=("$1"); shift ;;
    esac
done
if [ ${#targets[@]} -eq 0 ]; then
    targets=(http1 http2 hpack h2server websocket yaml toml json protobuf inflate gzip zstd tar zip regex glob)
fi
mkdir -p "$out/findings" "$out/logs"

extra=()
if [ $asan -eq 1 ]; then
    : "${PITH_ASAN_RUNTIME:?set PITH_ASAN_RUNTIME to an asan build of libpith_runtime.a}"
    : "${PITH_ASAN_CC:?set PITH_ASAN_CC to a linker wrapper that adds the asan runtime}"
    CC=$PITH_ASAN_CC PITH_KEEP_SYMBOLS=1 PITH_RUNTIME_LIB=$PITH_ASAN_RUNTIME \
        ./target/release/pith build tools/fuzz-surfaces/fuzz_surfaces.pith > /dev/null 2>&1 || exit 1
    export PITH_STRUCT_FREELIST=0
    # report_globals=0: the runtime reads a string header just before its
    # headerless static strings, which asan reports as a global overflow on
    # nearly every input; heap and stack errors are still caught
    export ASAN_OPTIONS=detect_leaks=0:detect_stack_use_after_return=0:allocator_may_return_null=1:halt_on_error=1:report_globals=0
    # asan slows the parsers down and inflates rss with its shadow and
    # quarantine, so the time oracle gets room and the rss oracle is off:
    # run the resource oracles in a plain build
    extra=(--time-ms 4000 --rss-floor-mb 1000000)
else
    ./target/release/pith build tools/fuzz-surfaces/fuzz_surfaces.pith > /dev/null 2>&1 || exit 1
fi
# run a private copy, so a plain run and an asan run can share the machine
cp tools/fuzz-surfaces/fuzz_surfaces "$out/fuzz_surfaces" || exit 1
bin=$out/fuzz_surfaces

# one batch; prints a summary line, saves the running input on a crash or hang
run_batch() {
    local target=$1 s=$2 limit=$3
    shift 3
    local log="$out/logs/$target-$s.log"
    timeout --signal=KILL "$limit" "$bin" --target "$target" --seed "$s" --out "$out" "${extra[@]}" "$@" > "$log" 2>&1
    local status=$?
    grep -h '^FINDING' "$log"
    if [ $status -ne 0 ]; then
        local kind=crash
        [ $status -eq 137 ] && kind=hang
        local saved="$out/findings/$target-$kind-seed$s.bin"
        cp "$out/cur-$target" "$saved" 2>/dev/null
        # replay the input alone: a crash that does not come back (another
        # process ran the machine out of memory, say) is marked as such
        timeout --signal=KILL $((hang * 3)) "$bin" --target "$target" --replay "$saved" --out "$out/replay" "${extra[@]}" >> "$log" 2>&1
        local again=$?
        if [ $again -eq 0 ]; then
            mv "$saved" "${saved%.bin}-unconfirmed.bin"
            saved="${saved%.bin}-unconfirmed.bin"
        fi
        echo "FINDING $target $kind exit=$status replay-exit=$again seed=$s -> $saved (log $log)"
    fi
    grep -h '^fuzz-surfaces' "$log"
    return 0
}

replay_known() {
    local status=0
    for file in "$known"/*.bin; do
        local name target mode
        name=$(basename "$file")
        target=${name%%-*}
        mode=0
        case "$name" in http1-*) mode=1 ;; h2server-*|websocket-*) mode=2 ;; esac
        timeout --signal=KILL "$hang" "$bin" --target "$target" --replay "$file" --tail "$mode" --out "$out" "${extra[@]}" > "$out/logs/known-$name.log" 2>&1
        local rc=$?
        if [ $rc -eq 137 ]; then
            echo "$name: hang (killed after ${hang}s)"
        elif [ $rc -ne 0 ]; then
            echo "$name: crash (exit $rc)"
        elif grep -q '^FINDING' "$out/logs/known-$name.log"; then
            echo "$name: $(grep -h '^FINDING' "$out/logs/known-$name.log" | awk '{print $3, $5, $6}' | tr '\n' ' ')"
        else
            echo "$name: not flagged ($(grep -h "^replay" "$out/logs/known-$name.log"))"
            status=1
        fi
    done
    return $status
}

run_target() {
    local target=$1
    if [ "$count" -gt 0 ]; then
        # the quick mode: small inputs only, and only crashes and hangs fail
        # it, so a slow shared machine cannot turn it red
        run_batch "$target" "$seed" $((hang * 10)) --count "$count" --deep 0 --time-ms 1000000 --rss-floor-mb 1000000
        return
    fi
    local stop=$(( $(date +%s) + minutes * 60 ))
    local k=0
    while [ "$(date +%s)" -lt "$stop" ]; do
        run_batch "$target" $((seed + k)) $((batch + hang)) --secs "$batch"
        k=$((k + 1))
    done
}

if [ -n "$known" ]; then
    replay_known
    exit $?
fi

for target in "${targets[@]}"; do
    while [ "$(jobs -rp | wc -l)" -ge "$jobs" ]; do
        wait -n
    done
    run_target "$target" &
done
wait
found=$(ls "$out/findings" | wc -l)
echo "fuzz-surfaces: $found finding file(s) in $out/findings"
if [ "$count" -gt 0 ] && [ "$found" -gt 0 ]; then
    exit 1
fi
exit 0
