#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GRAMMAR_FILE="$ROOT_DIR/pith.tmLanguage.json"
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
  "keyword.control.pith"
  "keyword.declaration.pith"
  "string.quoted.double.pith"
  "meta.interpolation.pith"
  "support.type.builtin.pith"
  "constant.numeric.float.pith"
  "entity.name.function.pith"
)

for pattern in "${required_patterns[@]}"; do
  if ! grep -q "$pattern" "$GRAMMAR_FILE"; then
    echo "missing expected grammar scope: $pattern" >&2
    exit 1
  fi
done

sample_count=$(find "$SAMPLES_DIR" -type f -name '*.pith' | wc -l | tr -d ' ')
if [[ "$sample_count" -lt 3 ]]; then
  echo "expected at least 3 sample .pith files, found $sample_count" >&2
  exit 1
fi

echo "ok: grammar json and sample coverage checks passed"
