#!/usr/bin/env bash
# no frame in the runtime archive may hold a thread-local's address.
#
# a green task can park on one worker and resume on another. compiled as an
# ordinary library, a function that touches a thread-local fetches the
# thread's tls base once (`__tls_get_addr`, relaxed by the linker to a
# `%fs:0` load) and adds offsets to it for every access in the frame, so a
# read after a park goes through the base of the thread that ran the read
# before it. two things close that, and this script checks both on the
# built archive, because the object code is what caches the base, not the
# source:
#
# 1. the workspace is compiled with `-C relocation-model=pie`
#    (`.cargo/config.toml`), so a plain cell access is one `%fs:`-relative
#    instruction with no base register. a dynamic-model relocation
#    (`TLSGD`, `TLSLD`, `DTPOFF32`) against one of the runtime's own cells
#    means that setting was lost — a `RUSTFLAGS` in the environment replaces
#    it — and the whole archive is back to caching bases.
#
# 2. a cell whose address has to exist as a value (a lazily initialized
#    handle, a `RefCell`) is touched only inside a function the optimizer may
#    not inline, listed below. an address materialization — a `TPOFF32`
#    relocation on an instruction that is not itself `%fs:`-relative, or a
#    `GOTTPOFF` one folded into an `add` — anywhere else is a frame that
#    holds a thread-local address, and fails the check.
#
# usage: tooling/check_tls_barriers.sh [path/to/libpith_runtime.a]
set -u

archive=${1:-target/release/libpith_runtime.a}
if [ ! -f "$archive" ]; then
    echo "no archive at $archive (run cargo build --release first)" >&2
    exit 2
fi

# functions allowed to hold a thread-local address, as `::` paths. the
# runtime's own accessors first; then the collection pass, which runs on a
# plain thread that never resumes a coroutine; then std's and parking_lot's
# own frames, which block the os thread and return without ever suspending
# a coroutine. a path is matched against the mangled symbol as its sequence
# of length-prefixed segments (`12pith_runtime12runtime_core14pool_slot_slow`),
# which is how both the legacy and the v0 mangling spell a path, so this
# works whichever the toolchain emits. the length prefix makes each segment
# exact: `7collect` cannot match `collect_from`.
allowed='pith_runtime::runtime_core::pool_slot_slow
pith_runtime::runtime_core::tls_globals_get
pith_runtime::runtime_core::tls_globals_set
pith_runtime::cycle::mutator_slot
pith_runtime::cycle::existing_mutator_slot
pith_runtime::cycle::adopt_mutator_slot
pith_runtime::cycle::collector_main
pith_runtime::cycle::collector_visit
pith_runtime::cycle::seed_node
pith_runtime::cycle::children_of
pith_runtime::cycle::collect
std::sys::thread_local
std::sync::once
std::thread
std::rt
std::panicking::panic_count
parking_lot
lock_api'

# the regex for one allowed path: each segment as `<len><ident>`, in order,
# anything between. the legacy mangling spells a trait-impl path inside one
# `$LT$..$GT$` segment with `..` between its parts
# (`$LT$parking_lot..remutex..RawThreadId$u20$as$u20$...`), so a segment is
# also accepted in that `ident..` form.
path_regex() {
    local out="" seg
    IFS=':' read -ra segs <<<"$1"
    for seg in "${segs[@]}"; do
        [ -z "$seg" ] && continue
        out="${out}${out:+.*}(${#seg}${seg}|${seg}\\.\\.)"
    done
    printf '%s\n' "$out"
}
allowed_regex=$(while IFS= read -r path; do [ -n "$path" ] && path_regex "$path"; done <<<"$allowed")

# `12pith_runtime12runtime_core14pool_slot_slow` becomes
# `pith_runtime::runtime_core::pool_slot_slow`, for the messages only. hash
# and tag characters between segments are dropped; readable, not exact.
pretty() {
    awk '{
        s = $0; i = 1; out = ""
        while (i <= length(s)) {
            c = substr(s, i, 1)
            if (c ~ /[0-9]/) {
                j = i
                while (substr(s, j, 1) ~ /[0-9]/) j++
                n = substr(s, i, j - i) + 0
                seg = substr(s, j, n)
                if (n > 0 && seg ~ /^[A-Za-z_$]/) { if (seg !~ /^h[0-9a-f]{16}$/) out = out (out == "" ? "" : "::") seg; i = j + n; continue }
            }
            i++
        }
        print out
    }' <<<"$1"
}

listing=$(objdump -dr --no-show-raw-insn "$archive" 2>/dev/null)

# check 1: no dynamic-model relocation against a runtime cell.
dynamic=$(awk '
    /^[0-9a-f]+ <.*>:$/ { fn = $2; sub(/^</, "", fn); sub(/>:$/, "", fn) }
    /R_X86_64_(TLSLD|TLSGD|DTPOFF32)/ && /pith_runtime/ { print fn "\t" $NF }
' <<<"$listing" | sort -u)
if [ -n "$dynamic" ]; then
    echo "FAIL the archive addresses runtime thread-locals with the dynamic tls model:"
    while IFS=$'\t' read -r fn sym; do
        echo "     $(pretty "$fn")  ->  $sym"
    done <<<"$dynamic"
    echo "     the workspace must be built with -C relocation-model=pie (.cargo/config.toml); a RUSTFLAGS in the environment replaces it" >&2
    exit 1
fi

# check 2: every address materialization sits in an allowed function.
materialized=$(awk '
    /^[0-9a-f]+ <.*>:$/ { fn = $2; sub(/^</, "", fn); sub(/>:$/, "", fn) }
    /R_X86_64_TPOFF32/ && prev !~ /%fs:/ { print fn "\t" $NF "\t" prev }
    /R_X86_64_GOTTPOFF/ && prev !~ /%fs:/ && prev !~ /\tmov / { print fn "\t" $NF "\t" prev }
    { prev = $0 }
' <<<"$listing" | sort -u)

fail=0
seen=0
while IFS=$'\t' read -r fn sym insn; do
    [ -z "$fn" ] && continue
    seen=$((seen + 1))
    if ! grep -Eq -f <(printf '%s\n' "$allowed_regex") <<<"$fn"; then
        cell=$(grep -oE '[0-9]+[A-Z][A-Z_]{2,}' <<<"$sym" | head -1 | sed -E 's/^[0-9]+//')
        echo "FAIL $(pretty "$fn") holds the address of $cell:  $(sed -E 's/^ +//' <<<"$insn")"
        fail=1
    fi
done <<<"$materialized"

if [ $fail -ne 0 ]; then
    echo "a frame outside the accessors holds a thread-local address; see the accessor block in green.rs" >&2
    exit 1
fi
if [ "$seen" -eq 0 ]; then
    echo "no thread-local address materialization found at all; the archive is not what this script expects" >&2
    exit 2
fi
echo "tls barriers hold: $seen address materializations, all inside the accessors; no dynamic-model tls access to a runtime cell"
