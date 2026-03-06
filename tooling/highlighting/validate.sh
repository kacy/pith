#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GRAMMAR_FILE="$ROOT_DIR/forge.tmLanguage.json"
SAMPLES_DIR="$ROOT_DIR/samples"

if [[ ! -f "$GRAMMAR_FILE" ]]; then
  echo "missing grammar file: $GRAMMAR_FILE" >&2
  exit 1
fi

if command -v jq >/dev/null 2>&1; then
  jq empty "$GRAMMAR_FILE" >/dev/null
elif command -v python3 >/dev/null 2>&1; then
  python3 -m json.tool "$GRAMMAR_FILE" >/dev/null
else
  echo "need jq or python3 to validate grammar json" >&2
  exit 1
fi

required_patterns=(
  "keyword.control.forge"
  "keyword.declaration.forge"
  "string.quoted.double.forge"
  "meta.interpolation.forge"
  "support.type.builtin.forge"
  "constant.numeric.float.forge"
  "entity.name.function.forge"
)

for pattern in "${required_patterns[@]}"; do
  if ! grep -q "$pattern" "$GRAMMAR_FILE"; then
    echo "missing expected grammar scope: $pattern" >&2
    exit 1
  fi
done

sample_count=$(find "$SAMPLES_DIR" -type f -name '*.fg' | wc -l | tr -d ' ')
if [[ "$sample_count" -lt 3 ]]; then
  echo "expected at least 3 sample .fg files, found $sample_count" >&2
  exit 1
fi

echo "ok: grammar json and sample coverage checks passed"
