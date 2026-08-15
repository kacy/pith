#!/usr/bin/env bash
#
# the leak growth gate. every case under tests/leaks/ runs one churn pattern
# `PITH_LEAK_ROUNDS` times and prints its peak resident set in kilobytes. this
# runs each case twice — once at a low round count and once at a multiple of
# it — and fails when the peak grew between the two.
#
# growth rather than an absolute number because a program that leaks k bytes
# per round moves its peak by k times the extra rounds, which is a signal that
# scales with the round count and does not care what the program's working set
# happens to be. an absolute ceiling would drift with every unrelated allocator
# or runtime change and would need retuning to stay useful.
#
# valgrind's leak check is deliberately not what this uses: the runtime keeps a
# struct freelist, a coroutine stack pool, arenas and thread pools alive for the
# life of the process, so "still reachable" bytes swamp anything real.
set -euo pipefail

low_rounds="${PITH_LEAK_ROUNDS_LOW:-200000}"
high_rounds="${PITH_LEAK_ROUNDS_HIGH:-800000}"
# the leaks this gate was built from ran 20 to 90 bytes per round, so 600k
# extra rounds move the peak by 12 mb at the very quietest. measured run to run
# noise on the flat cases is under 200 kb. 2 mb sits an order of magnitude above
# the noise and well below the smallest real signal.
limit_kb="${PITH_LEAK_GROWTH_LIMIT_KB:-2048}"

pith="./target/release/pith"
cases=(
  tests/leaks/leak_container_eviction
  tests/leaks/leak_index_key
  tests/leaks/leak_direct_store
  tests/leaks/leak_struct_store
  tests/leaks/leak_loop_var_slot
  tests/leaks/leak_list_transform
  tests/leaks/leak_container_arg
  tests/leaks/leak_handed_back_arg
  tests/leaks/leak_empty_literal_arg
  tests/leaks/leak_map_value_store
  tests/leaks/leak_list_element_store
  tests/leaks/leak_fn_value
  tests/leaks/leak_task_result_payload
  tests/leaks/leak_result_extraction
  tests/leaks/leak_result_arg_borrow
  tests/leaks/leak_yaml_parse
  tests/leaks/leak_sql_numeric
  tests/leaks/leak_none_lowering
  tests/leaks/leak_optional_arg
  tests/leaks/leak_discarded_spawn
  tests/leaks/leak_weak_field
  tests/leaks/leak_weak_local
  tests/leaks/leak_weak_capture_cycle
  tests/leaks/leak_closure_captured_optional
  tests/leaks/leak_return_widened_optional
  tests/leaks/leak_tls_client_config
  tests/leaks/leak_http_string_head_flood
)

echo "--- leak growth (${low_rounds} vs ${high_rounds} rounds, limit ${limit_kb}kb) ---"

# a case prints one number and nothing else. no output, or a zero because
# /proc/self/status could not be read, would make the comparison below
# trivially pass, so neither counts as a measurement.
is_peak() {
  case "$1" in
    "" | 0 | *[!0-9]*) return 1 ;;
  esac
}

# one measurement: peak at the high round count minus peak at the low one.
# prints the growth in kilobytes, or nothing at all if the case did not run.
measure() {
  local exe="$1" low high
  low="$(PITH_LEAK_ROUNDS="$low_rounds" "$exe" 2>/dev/null)" || return 1
  high="$(PITH_LEAK_ROUNDS="$high_rounds" "$exe" 2>/dev/null)" || return 1
  is_peak "$low" && is_peak "$high" || return 1
  echo $((high - low))
}

failed=0
for base in "${cases[@]}"; do
  if ! "$pith" build "$base.pith" > /dev/null 2>&1; then
    echo "FAIL $base (build)"
    failed=1
    continue
  fi

  # a passing case is measured once. a failing one is measured again before it
  # is called a failure, so a single spike on a busy machine cannot turn the
  # gate red — the retry only costs anything when something is already wrong.
  growth=""
  for _ in 1 2; do
    if ! growth="$(measure "./$base")"; then
      growth=""
      break
    fi
    if [ "$growth" -le "$limit_kb" ]; then
      break
    fi
  done

  if [ -z "$growth" ]; then
    echo "FAIL $base (did not run to completion)"
    failed=1
  elif [ "$growth" -le "$limit_kb" ]; then
    echo "ok   $base (grew ${growth}kb)"
  else
    echo "FAIL $base (grew ${growth}kb, limit ${limit_kb}kb)"
    failed=1
  fi
done

if [ "$failed" -ne 0 ]; then
  echo "leak growth gate failed"
  exit 1
fi
echo "all leak cases flat across ${low_rounds} and ${high_rounds} rounds"
