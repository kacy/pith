#!/usr/bin/env bash
#
# compare the ir two built trees emit for every corpus program, after
# normalizing what moves without meaning (tooling/ir_normalize.py: register
# numbers, function order, specialization suffix spelling).
#
# this is the proof step for a compiler change that is supposed to be
# ir-neutral, and the inventory step for one that is not: every program that
# still differs after normalization is listed with its raw and normalized
# line counts and its struct retain and release counts, so the change can be
# explained program by program. it refuses to say "identical" for reasons it
# cannot see: a normalizer that produced no output is an error, not a match,
# and with --canary the program named must differ or the run fails, so a
# comparison that silently compares nothing cannot pass.
#
#   tooling/ir_compare.sh <tree A> <tree B> [--canary <program>] [--out <dir>]
#                                          [--only <glob>]
#
# --only restricts the corpus to the programs whose name matches the glob
# (`--only 'test_generic_*'`), for a quick look at one shape or for checking
# the harness itself: a canary run over its own program alone takes seconds.
# a verdict over a subset is not a corpus verdict and the summary says so.
#
# both trees must already be built (target/release/pith, self-host/ir_driver,
# self-host/pith_main). the corpus is tree A's tests/cases/test_*.pith and
# examples/*.pith; a program that fails to build in either tree is counted,
# not compared. exit 0 when every program is identical, 3 when some differ,
# 1 on a harness failure (missing binary, normalizer produced nothing, canary
# did not differ). one heavy job at a time on this box: run it alone.
set -uo pipefail

a=""; b=""; canary=""; out=""; only=""
while [ $# -gt 0 ]; do
  case "$1" in
    --canary) canary="$2"; shift 2 ;;
    --only) only="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    *) if [ -z "$a" ]; then a="$1"; elif [ -z "$b" ]; then b="$1"; else echo "unexpected argument: $1" >&2; exit 1; fi; shift ;;
  esac
done
[ -n "$a" ] && [ -n "$b" ] || { echo "usage: tooling/ir_compare.sh <tree A> <tree B> [--canary <program>] [--out <dir>] [--only <glob>]" >&2; exit 1; }
a="$(cd "$a" && pwd)"; b="$(cd "$b" && pwd)"
for t in "$a" "$b"; do
  for f in target/release/pith self-host/ir_driver self-host/pith_main; do
    [ -x "$t/$f" ] || { echo "not built: $t/$f" >&2; exit 1; }
  done
done
here="$(cd "$(dirname "$0")" && pwd)"
normalize="$here/ir_normalize.py"
[ -n "$out" ] || out="$(mktemp -d "${TMPDIR:-/tmp}/ir_compare.XXXXXX")"
mkdir -p "$out"
: > "$out/trace.txt"; : > "$out/differ.txt"

echo "A: $a ($(git -C "$a" rev-parse --short HEAD 2>/dev/null))"
echo "B: $b ($(git -C "$b" rev-parse --short HEAD 2>/dev/null))"
total=0; same=0; nobuild=0; canary_seen=""
cd "$a"
for f in tests/cases/test_*.pith examples/*.pith; do
  name="$(basename "$f" .pith)"
  # shellcheck disable=SC2254
  if [ -n "$only" ]; then case "$name" in $only) ;; *) continue ;; esac; fi
  rm -f "$out/a.ir" "$out/b.ir"
  (cd "$a" && PITH_DUMP_IR="$out/a.ir" ./target/release/pith build "$f" > /dev/null 2>&1)
  (cd "$b" && PITH_DUMP_IR="$out/b.ir" ./target/release/pith build "$f" > /dev/null 2>&1)
  if [ ! -s "$out/a.ir" ] || [ ! -s "$out/b.ir" ]; then
    nobuild=$((nobuild + 1)); echo "$name nobuild" >> "$out/trace.txt"; continue
  fi
  total=$((total + 1))
  python3 "$normalize" "$out/a.ir" > "$out/a.n" 2> /dev/null
  python3 "$normalize" "$out/b.ir" > "$out/b.n" 2> /dev/null
  if [ ! -s "$out/a.n" ] || [ ! -s "$out/b.n" ]; then
    echo "normalizer produced nothing for $name; refusing to compare" >&2; exit 1
  fi
  raw=$(diff "$out/a.ir" "$out/b.ir" | grep -c '^[<>]')
  norm=$(diff "$out/a.n" "$out/b.n" | grep -c '^[<>]')
  ra=$(grep -c "pith_struct_retain" "$out/a.ir"); rb=$(grep -c "pith_struct_retain" "$out/b.ir")
  la=$(grep -c "pith_struct_release" "$out/a.ir"); lb=$(grep -c "pith_struct_release" "$out/b.ir")
  echo "$name raw=$raw norm=$norm retain=$ra->$rb release=$la->$lb" >> "$out/trace.txt"
  if [ "$norm" = "0" ]; then
    same=$((same + 1))
  else
    echo "$name norm=$norm retain=$ra->$rb release=$la->$lb" >> "$out/differ.txt"
  fi
  [ "$name" = "$canary" ] && canary_seen="$norm"
done

[ -n "$only" ] && echo "subset only ($only): not a corpus verdict"
echo "compared: $total; identical after normalization: $same; differing: $(wc -l < "$out/differ.txt"); no build: $nobuild"
echo "trace: $out/trace.txt; differing programs: $out/differ.txt"
if [ -n "$canary" ]; then
  if [ -z "$canary_seen" ]; then echo "canary $canary was not compared (no build?); refusing the result" >&2; exit 1; fi
  if [ "$canary_seen" = "0" ]; then echo "canary $canary did not differ; the comparison is not measuring what it should" >&2; exit 1; fi
  echo "canary $canary differs ($canary_seen normalized lines), as required"
fi
[ "$(wc -l < "$out/differ.txt")" = "0" ] && exit 0
cat "$out/differ.txt" | head -40
exit 3
