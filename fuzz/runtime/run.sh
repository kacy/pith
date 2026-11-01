#!/usr/bin/env bash
# build and run the runtime fuzz targets under libFuzzer with AddressSanitizer.
#
#   fuzz/runtime/run.sh <seconds-per-target> [jobs] [out-dir] [targets...]
#
# jobs is how many targets run at once (default 1). out-dir (default
# target/fuzz-runtime) receives, per target, the corpus, crash artifacts and
# the libFuzzer log. corpora start from the small seeds written below and
# grow at run time; nothing under out-dir belongs in the repository.
#
# needs: a stable rustc with rust-src (sanitizer flags via RUSTC_BOOTSTRAP)
# and clang with its libFuzzer archive (see build.rs). CARGO_TARGET_DIR
# overrides the build directory, which defaults to <out-dir>/build.
#
# reproduce one artifact:  <out-dir>/bin/<target> <artifact>
# minimize one artifact:   <out-dir>/bin/<target> -minimize_crash=1 -runs=100000 <artifact>
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
secs="${1:?usage: run.sh <seconds-per-target> [jobs] [out-dir] [targets...]}"
jobs="${2:-1}"
out="${3:-$root/target/fuzz-runtime}"
shift $(( $# < 3 ? $# : 3 ))
targets=("$@")
if [ ${#targets[@]} -eq 0 ]; then
    targets=(json encoding strings bytes zstd)
fi
mkdir -p "$out"
out="$(cd "$out" && pwd)"
triple=x86_64-unknown-linux-gnu
build="${CARGO_TARGET_DIR:-$out/build}"

# sanitizer coverage the way cargo-fuzz sets it up, plus recoverable asan:
# the report hook in src/lib.rs aborts on every report but one known pattern.
# the C side (libzstd, ring) gets the same instrumentation through cc.
export RUSTC_BOOTSTRAP=1
export RUSTFLAGS="-Zsanitizer=address -Zsanitizer-recover=address -Cforce-frame-pointers=yes \
-Cpasses=sancov-module -Cllvm-args=-sanitizer-coverage-level=4 \
-Cllvm-args=-sanitizer-coverage-inline-8bit-counters -Cllvm-args=-sanitizer-coverage-pc-table \
-Cllvm-args=-sanitizer-coverage-trace-compares"
export CC_x86_64_unknown_linux_gnu=clang
export CFLAGS_x86_64_unknown_linux_gnu="-fsanitize=address -fsanitize-recover=address -fsanitize=fuzzer-no-link"
(cd "$here" && cargo build --release --target "$triple" --target-dir "$build" --bins)
mkdir -p "$out/bin"
for t in "${targets[@]}"; do
    cp "$build/$triple/release/$t" "$out/bin/$t"
done

# seeds. each target reads a few control bytes before its payload (see the
# target's source), so a seed is those bytes followed by real input.
seed() { # seed <target> <name> <printf-format> [args...]
    local dir="$out/corpus/$1" name="$2"
    shift 2
    mkdir -p "$dir"
    # shellcheck disable=SC2059
    printf "$@" > "$dir/seed-$name"
}
# json: shape count, per shape a field count, per field a kind and a name
# byte, then the utf-8 check flag and the document. kinds are i=0 s=8 b=16
# f=24 (mod 32 under the scalar arms), 4 a struct field, 6 a list, 7 a map.
seed json flat-int '\x00\x00\x00\x00\x00{"a": 42}'
seed json flat-mixed '\x00\x03\x00\x00\x08\x03\x10\x04\x18\x06\x00{"a":1,"name":"bob","x":true,"v":1.5}'
seed json flat-escape '\x00\x01\x08\x03\x00\x02\x00{"name":"a\\"b\\u00e9","id":-7}'
seed json nested '\x01\x01\x04\x00\x00\x01\x01\x00\x02\x08\x03\x01{"a":{"id":3,"name":"x"},"b":1}'
seed json list-int '\x00\x00\x06\x00\x05\x01{"tags":[1,2,3]}'
seed json list-str '\x00\x00\x06\x08\x05\x01{"tags":["p","q"]}'
seed json list-obj '\x01\x00\x06\x03\x05\x00\x08\x03\x01{"tags":[{"name":"a"},{"name":"b"}]}'
seed json map-int '\x00\x00\x07\x00\x07\x01{"key":{"p":1,"q":2}}'
seed json map-list '\x00\x00\x07\x04\x00\x07\x01{"key":{"p":[1],"q":[]}}'
seed json deep '\x00\x00\x06\x04\x04\x00\x05\x01{"tags":[[[1]],[[2,3]]]}'
seed encoding int '\x00\x00 -9223372036854775808 '
seed encoding float '\x01\x00 1.5e300'
seed encoding b64-round '\x02\x00hello, world'
seed encoding b64-decode '\x03\x00aGVsbG8sIHdvcmxk=='
seed encoding hex-round '\x04\x02\xff\x00\x10abc'
seed encoding hex-decode '\x05\x00DEADbeef00'
seed encoding ints '\x06\x00\x04\x03'
seed encoding sha '\x07\x00abc'
seed encoding utf8 '\x08\x00caf\xc3\xa9'
seed encoding utf8-sub '\x09\x00\x00\x01\x00\x05caf\xc3\xa9!'
seed strings substring '\x00\x00\x0chello, world\x00\x02\x00\x08'
seed strings utf8 '\x00\x00\x09h\xc3\xa9llo\xe2\x82\xac\x00\x02\x00\x04'
seed strings char-at '\x01\x02\x05abcde\x00\x03'
seed strings split '\x02\x00\x07a,b,,c,\x01,\x00\x00\x00\x02'
seed strings replace '\x03\x00\x06aaaaab\x02aaX'
seed strings find '\x04\x00\x0bhello worldo'
seed strings trim '\x05\x00 \t abc \r\n'
seed strings case '\x06\x00Hello World'
seed strings pad '\x07\x00\x02ab\x05\x06-'
seed strings repeat '\x08\x00\x00\x05ab'
seed strings compare '\x09\x00\x03abcabd'
seed strings format '\x0a\x00\x00\x08<*-12'
seed strings pstring '\x0b\x00\x05h\xc3\xa9yo\x00\x01\x00\x04yo'
seed bytes ops '\x00\x00\x08abcdefgh\x01\x00\x00\x03\x02\x00\x00\x01\x00\x05\x05\x00\x00\x02\x04\x08\x08\x00\x0a\x00\x00\x00\x04\x0e\x00\x10\x00\x00\x00'
seed bytes buffer '\x07\x00\x00\x41\x0c\x00\x07\x01\x02\x03\x04\x00\x00\x04\x0a\x00\x00\x00\x00\x06\x0e\x00\x0f\x00\x10\x00\x00\x08'
seed zstd round '\x01\x00\x03\x02the quick brown fox jumps over the lazy dog the quick brown fox'
seed zstd frame '\x00\x00\x40\x28\xb5\x2f\xfd\x04\x68\x6d\x00\x00\x38\x68\x65\x6c\x6c\x6f\x20\x68\x01\x00\x41\x8a\x11\x17\x5e\xae\x0d'
seed zstd raw-block '\x00\x00\x02\x28\xb5\x2f\xfd\x00\x58\x19\x00\x00\x61\x62\x63'

max_len() {
    case "$1" in
        json) echo 4096 ;;
        zstd) echo 8192 ;;
        *) echo 1024 ;;
    esac
}

run_one() {
    local t="$1"
    local dict=()
    [ -f "$here/$t.dict" ] && dict=(-dict="$here/$t.dict")
    mkdir -p "$out/corpus/$t" "$out/artifacts/$t"
    echo "running $t for ${secs}s"
    # a smaller quarantine keeps a long run of the allocation-heavy targets
    # (bytes) under the rss limit; with the default 256MB it crept past 2GB
    # after a few million inputs with under 40MB live.
    ASAN_OPTIONS="halt_on_error=0:detect_leaks=1:allocator_may_return_null=1:detect_stack_use_after_return=0:quarantine_size_mb=64:allocator_release_to_os_interval_ms=1000" \
        "$out/bin/$t" "$out/corpus/$t" "${dict[@]}" \
        -max_total_time="$secs" -max_len="$(max_len "$t")" -rss_limit_mb=2048 -timeout=10 \
        -print_final_stats=1 -artifact_prefix="$out/artifacts/$t/" \
        > "$out/$t.log" 2>&1 || true
    echo "$t: $(grep -E '^#[0-9]+.*(DONE|cov:)' "$out/$t.log" | tail -1)"
    echo "$t: $(ls "$out/artifacts/$t" | wc -l) artifact(s)"
}

running=0
for t in "${targets[@]}"; do
    run_one "$t" &
    running=$((running + 1))
    if [ "$running" -ge "$jobs" ]; then
        wait -n
        running=$((running - 1))
    fi
done
wait
