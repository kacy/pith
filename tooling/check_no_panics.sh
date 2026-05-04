#!/usr/bin/env bash
set -euo pipefail

paths=(
  cranelift/cli/src
  cranelift/codegen/src
  cranelift/codegen/build.rs
  cranelift/runtime/src
)

pattern='panic!|expect\(|unreachable!|from_utf8_unchecked|\.unwrap\(\)|Layout::(from_size_align|array).*\.unwrap\(\)|lock\(\)\.unwrap\(\)|wait\(.*\)\.unwrap\(\)|std::mem::forget|std::mem::transmute|std::process::exit'

matches="$(rg -n "$pattern" "${paths[@]}" || true)"
allowed_patterns=(
  'cranelift/codegen/src/ir_consumer.rs:[0-9]+:        let mut codegen = crate::create_codegen\(\).expect\("create codegen"\);'
  'cranelift/codegen/src/ir_consumer.rs:[0-9]+:        result.err\(\).expect\("compile error"\).to_string\(\)'
  'cranelift/runtime/src/encoding.rs:[0-9]+:        let c_input = CString::new\(input\).unwrap\(\);'
  'cranelift/runtime/src/concurrency/task.rs:[0-9]+:        let func: extern "C" fn\(i64\) -> i64 = std::mem::transmute\(func_ptr as \*const \(\)\);'
  'cranelift/runtime/src/collections/list.rs:[0-9]+:    let func: extern "C" fn\(i64, i64\) -> i64 = std::mem::transmute\(func_ptr as \*const \(\)\);'
  'cranelift/runtime/src/collections/list.rs:[0-9]+:    let func: extern "C" fn\(i64, i64, i64\) -> i64 = std::mem::transmute\(func_ptr as \*const \(\)\);'
  'cranelift/cli/src/main.rs:[0-9]+:.*std::process::exit\('
  'cranelift/codegen/build.rs:[0-9]+:        std::process::exit\(1\);'
  'cranelift/runtime/src/platform.rs:[0-9]+:    std::process::exit\(code as i32\);'
  'cranelift/runtime/src/runtime_core.rs:[0-9]+:.*std::process::exit\(1\);'
)

violations="$matches"
for allowed in "${allowed_patterns[@]}"; do
  violations="$(printf '%s\n' "$violations" | grep -Ev "$allowed" || true)"
done

if [ -n "$violations" ]; then
  printf 'production panic guard failed:\n%s\n' "$violations" >&2
  exit 1
fi
