#!/usr/bin/env bash
set -euo pipefail

paths=(
  cranelift/cli/src
  cranelift/codegen/src
  cranelift/codegen/build.rs
  cranelift/runtime/src
)

pattern='panic!|expect\(|unreachable!|from_utf8_unchecked|\.unwrap\(\)|Layout::(from_size_align|array).*\.unwrap\(\)|lock\(\)\.unwrap\(\)|wait\(.*\)\.unwrap\(\)'

matches="$(rg -n "$pattern" "${paths[@]}" || true)"
allowed='cranelift/codegen/src/ir_consumer.rs:[0-9]+:        let mut codegen = crate::create_codegen\(\).expect\("create codegen"\);|cranelift/codegen/src/ir_consumer.rs:[0-9]+:        result.err\(\).expect\("compile error"\).to_string\(\)|cranelift/runtime/src/encoding.rs:[0-9]+:        let c_input = CString::new\(input\).unwrap\(\);'
violations="$(printf '%s\n' "$matches" | grep -Ev "$allowed" || true)"

if [ -n "$violations" ]; then
  printf 'production panic guard failed:\n%s\n' "$violations" >&2
  exit 1
fi
