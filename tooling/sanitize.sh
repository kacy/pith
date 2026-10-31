#!/usr/bin/env bash
#
# the sanitizer gate. builds the runtime with AddressSanitizer or
# ThreadSanitizer, links every regression case and every std test file
# against it, and fails on any report.
#
#   tooling/sanitize.sh asan            # corpus and std tests, both backends
#   tooling/sanitize.sh tsan corpus     # corpus only, os threads
#
# what it can and cannot see: the runtime is instrumented, the code the
# compiler emits is not. a use after free is reported once runtime code
# touches the freed block, which covers every count operation and every
# container access, but not a field load that generated code does inline.
#
# the struct freelist is turned off for every run. with it on, a freed struct
# is handed to the next allocation of its size and nothing ever looks freed:
# that is how a double free in a weak field read went unseen by valgrind.
#
# stable rustc builds sanitized code with RUSTC_BOOTSTRAP=1 and ships the
# matching sanitizer runtimes; tsan also rebuilds std (needs rust-src), since
# an uninstrumented std hides its own locks from the race detector.
set -euo pipefail

kind="${1:-asan}"
scope="${2:-all}"
jobs="${PITH_SANITIZE_JOBS:-$(nproc)}"
pith="./target/release/pith"

case "$kind" in
  asan)
    sanitizer=address
    # green threads switch stacks without telling the sanitizer, so asan
    # still runs there (its checks are per access) but tsan cannot yet
    backends="${PITH_SANITIZE_BACKENDS:-os green}"
    ;;
  tsan)
    sanitizer=thread
    backends="${PITH_SANITIZE_BACKENDS:-os}"
    ;;
  *)
    echo "usage: tooling/sanitize.sh asan|tsan [corpus|std|all]" >&2
    exit 2
    ;;
esac

target_dir="target/sanitize-$kind"
triple="x86_64-unknown-linux-gnu"
runtime_lib="$target_dir/$triple/release/libpith_runtime.a"
wrapper="$target_dir/cc"
rt="$(rustc --print sysroot)/lib/rustlib/$triple/lib/librustc-stable_rt.$kind.a"

# a case whose expected output is a memory measurement. the sanitizer holds
# freed memory in quarantine on purpose, so these grow under it by design;
# they still run, and still fail on a report, but their output is not compared.
measures_memory=(
  test_container_eviction_release
  test_direct_store_ownership
  test_global_rebind_ownership
  test_index_key_ownership
  test_result_arg_borrow_leak
  test_result_unwrap_or_fallback_leak
  test_struct_store_ownership
)

export ASAN_OPTIONS="${ASAN_OPTIONS:-detect_leaks=0:detect_stack_use_after_return=0:allocator_may_return_null=1}"
export TSAN_OPTIONS="${TSAN_OPTIONS:-halt_on_error=0:report_signal_unsafe=0:history_size=4}"
export PITH_STRUCT_FREELIST=0

# --- one case, run by the parallel pass below and again by the retry ---
# prints "ok", "FAIL <why>" or "REPORT" followed by the case and backend.
run_case() {
  local work="$1" name="$2" backend="$3" green=1 out log
  [ "$backend" = os ] && green=0
  out="$work/$name.$backend.out"
  log="$work/$name.$backend.log"
  if ! PITH_GREEN="$green" timeout 600 "$work/cases/$name" >"$out" 2>"$log"; then
    :  # the exit code is judged by the expected output below
  fi
  if grep -q -E '^==[0-9]+==ERROR: (Address|Thread)Sanitizer|^WARNING: ThreadSanitizer' "$log"; then
    echo "REPORT $name $backend"
  elif [[ " ${measures_memory[*]} " == *" $name "* ]]; then
    echo "ok $name $backend"
  elif [ "$(cat "$out")" != "$(cat "tests/expected/$name.txt")" ]; then
    echo "FAIL $name $backend"
  else
    echo "ok $name $backend"
  fi
}

run_std() {
  local work="$1" file="$2" backend="$3" green=1 tag log
  [ "$backend" = os ] && green=0
  tag="$(echo "$file" | tr / _)"
  log="$work/$tag.$backend.log"
  if ! CC="$wrapper" PITH_KEEP_SYMBOLS=1 PITH_RUNTIME_LIB="$runtime_lib" PITH_GREEN="$green" \
      timeout 600 "$pith" test "$file" >"$log" 2>&1; then
    if grep -q -E '^==[0-9]+==ERROR: (Address|Thread)Sanitizer|^WARNING: ThreadSanitizer' "$log"; then
      echo "REPORT $file $backend"
    else
      echo "FAIL $file $backend"
    fi
    return
  fi
  if grep -q -E '^==[0-9]+==ERROR: (Address|Thread)Sanitizer|^WARNING: ThreadSanitizer' "$log"; then
    echo "REPORT $file $backend"
  else
    echo "ok $file $backend"
  fi
}

# the parallel pass calls back into this script for each item
if [ "$scope" = "--case" ] || [ "$scope" = "--std" ]; then
  work="$3" item="$4" backend="$5"
  if [ "$scope" = "--case" ]; then run_case "$work" "$item" "$backend"; else run_std "$work" "$item" "$backend"; fi
  exit 0
fi

if [ ! -f "$rt" ]; then
  echo "no $kind runtime at $rt; this needs a stable x86_64 linux toolchain" >&2
  exit 1
fi

echo "--- building the runtime with -Zsanitizer=$sanitizer ---"
flags="-Zsanitizer=$sanitizer -Cforce-frame-pointers=yes"
build_std=()
[ "$kind" = tsan ] && build_std=(-Zbuild-std)
RUSTC_BOOTSTRAP=1 RUSTFLAGS="$flags" cargo build --release -p pith-runtime \
  "${build_std[@]}" --target "$triple" --target-dir "$target_dir" --quiet
cat >"$wrapper" <<EOF
#!/bin/sh
# link with the sanitizer runtime that matches the rustc that built the archive
exec \${PITH_SANITIZE_CC:-cc} "\$@" -Wl,--whole-archive "$rt" -Wl,--no-whole-archive -lpthread -ldl -lm -lrt -lstdc++
EOF
chmod +x "$wrapper"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
results="$work/results"
: >"$results"

if [ "$scope" = all ] || [ "$scope" = corpus ]; then
  echo "--- regression corpus ($backends) ---"
  # a copy of the whole tree, so cases that import sibling modules find them
  cp -r tests/cases "$work/cases"
  names=()
  for expected in tests/expected/*.txt; do
    name="$(basename "$expected" .txt)"
    [ -f "tests/cases/$name.pith" ] && names+=("$name")
  done
  printf '%s\n' "${names[@]}" | xargs -P "$jobs" -I{} sh -c \
    'CC="$1" PITH_KEEP_SYMBOLS=1 PITH_RUNTIME_LIB="$2" timeout 300 "$3" build "$4/cases/$5.pith" >/dev/null 2>&1 || echo "FAIL $5 build"' \
    _ "$wrapper" "$runtime_lib" "$pith" "$work" {} >>"$results"
  for backend in $backends; do
    printf '%s\n' "${names[@]}" | while read -r name; do
      [ -x "$work/cases/$name" ] && echo "$name"
    done | xargs -P "$jobs" -I{} "$0" "$kind" --case "$work" {} "$backend" >>"$results"
  done
fi

if [ "$scope" = all ] || [ "$scope" = std ]; then
  echo "--- std tests ($backends) ---"
  files="$(find std -name '*.pith' -print | sort | while read -r f; do
    grep -q '^[[:space:]]*test "' "$f" && echo "$f"
  done)"
  for backend in $backends; do
    echo "$files" | xargs -P "$jobs" -I{} "$0" "$kind" --std "$work" {} "$backend" >>"$results"
  done
fi

# cases bind fixed ports, so two of them can collide in the parallel pass. a
# failure is run again on its own before it counts; a report always counts.
failed=0
while read -r status item backend; do
  case "$status" in
    ok) ;;
    REPORT)
      echo "REPORT $item ($backend)"
      sed -n '/^==[0-9]*==ERROR\|^WARNING: ThreadSanitizer/,/^SUMMARY/p' \
        "$work/$(echo "$item" | tr / _).$backend.log" 2>/dev/null | head -60
      failed=1
      ;;
    FAIL)
      if [ "$backend" = build ]; then
        echo "FAIL $item (build)"
        failed=1
        continue
      fi
      if [[ "$item" == std/* ]]; then again="$(run_std "$work" "$item" "$backend")"; else again="$(run_case "$work" "$item" "$backend")"; fi
      if [[ "$again" != ok* ]]; then
        echo "$again"
        failed=1
      fi
      ;;
  esac
done <"$results"

total="$(wc -l <"$results")"
if [ "$failed" -ne 0 ]; then
  echo "$kind gate failed ($total runs)"
  exit 1
fi
echo "$kind clean across $total runs"
