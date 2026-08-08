#!/usr/bin/env bash
#
# the crash guard. the rust runtime is linked into every pith program, so a
# panic here is a crash — or worse, a silent wedge — in somebody's server. this
# scans the rust sources for the constructs that stop a process (or reinterpret
# memory) and fails on any that is not explicitly justified in place.
#
# --- how a site is justified ---
#
# put a marker comment on the line directly above the site:
#
#     // panic-guard: <why this cannot be reached, or why stopping is correct>
#     std::process::exit(1);
#
# the marker moves with the code, so it cannot rot the way a list of source-line
# regexes in this file did — that list went stale, and every trap added after it
# was written simply never got recorded. a marker whose next line does not match
# the pattern is itself reported, so a marker left behind by a deleted site is
# noticed too.
#
# --- what is scanned ---
#
# test code is skipped: `#[cfg(test)]` items are compiled out of the shipped
# runtime, and an `.unwrap()` in a test is how a test reports failure. the skip
# runs from the attribute line to the closing brace at the attribute's own
# indentation, which is what rustfmt produces; an item whose brace never turns
# up is reported rather than silently swallowing the rest of the file.
#
# `std::process::exit` is scanned only in the runtime and the codegen crate. the
# cli and the build script are programs, and a program exiting non-zero after
# printing a diagnostic is their normal behaviour, not a crash.
#
# --- why awk ---
#
# this used to shell out to `rg`, which is not on the github runner image, with
# a `|| true` after it. the guard therefore passed unconditionally in ci for as
# long as it had been wired up. awk is in posix and is on every image this runs
# on, and nothing below hides a non-zero exit from the search.
set -euo pipefail

if ! command -v awk >/dev/null 2>&1; then
  echo "check_no_panics: awk is not installed, so the crash guard cannot run" >&2
  exit 1
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

# every construct that can stop the process, plus the two that reinterpret
# memory (`transmute`/`forget`) and want the same in-place justification.
export PITH_PANIC_PATTERN='panic!|expect\(|unreachable!|from_utf8_unchecked|\.unwrap\(\)|std::mem::forget|std::mem::transmute'
# scanned for `std::process::exit` on top of the pattern above.
export PITH_EXIT_SCOPE='^cranelift/(runtime|codegen)/src/'
export PITH_EXIT_PATTERN='std::process::exit'

roots=(
  cranelift/cli/src
  cranelift/codegen/src
  cranelift/runtime/src
)
extra_files=(
  cranelift/codegen/build.rs
)

files=()
for root in "${roots[@]}"; do
  if [ ! -d "$root" ]; then
    echo "check_no_panics: $root is not a directory; the scan list is out of date" >&2
    exit 1
  fi
  while IFS= read -r file; do
    files+=("$file")
  done < <(find "$root" -name '*.rs' -type f | sort)
done
for file in "${extra_files[@]}"; do
  if [ ! -f "$file" ]; then
    echo "check_no_panics: $file is missing; the scan list is out of date" >&2
    exit 1
  fi
  files+=("$file")
done

if [ "${#files[@]}" -eq 0 ]; then
  echo "check_no_panics: found no rust sources to scan" >&2
  exit 1
fi

violations="$(awk '
function marker(line) {
  return line ~ /\/\/ panic-guard:[ \t]*[^ \t]/
}
function report(file, line, message) {
  printf "%s:%d: %s\n", file, line, message
}
# whatever the file just finished left dangling.
function finish_file() {
  if (this_file == "") {
    return
  }
  if (skip_brace != "") {
    report(this_file, skip_from, "#[cfg(test)] item never closed at its own indentation; the rest of the file went unscanned")
  }
  if (pending) {
    report(this_file, pending, "stale panic-guard marker: nothing follows it")
  }
}

FNR == 1 {
  finish_file()
  this_file = FILENAME
  pattern = ENVIRON["PITH_PANIC_PATTERN"]
  if (FILENAME ~ ENVIRON["PITH_EXIT_SCOPE"]) {
    pattern = pattern "|" ENVIRON["PITH_EXIT_PATTERN"]
  }
  skip_brace = ""
  skip_from = 0
  pending = 0
}

# inside a #[cfg(test)] item: skip to the closing brace at its indentation.
skip_brace != "" {
  if ($0 == skip_brace) {
    skip_brace = ""
  }
  next
}

/^[ \t]*#\[cfg\(test\)\]$/ {
  indent = $0
  sub(/#.*$/, "", indent)
  skip_brace = indent "}"
  skip_from = FNR
  next
}

{
  # a comment is not code, so it never counts as a site. a marker sitting on the
  # end of a line of code justifies that line where a separate line would read
  # worse.
  is_comment = ($0 ~ /^[ \t]*(\/\/|\*)/)
  standalone_marker = (is_comment && marker($0))
  trailing_marker = (!is_comment && marker($0))
  matched = (!is_comment && !trailing_marker && $0 ~ pattern)

  if (trailing_marker && $0 !~ pattern) {
    report(this_file, FNR, "stale panic-guard marker: this line is not a guarded site")
  }
  if (pending) {
    if (matched) {
      matched = 0
    } else {
      report(this_file, pending, "stale panic-guard marker: the line below it is not a guarded site")
    }
  }
  if (matched) {
    line = $0
    sub(/^[ \t]+/, "", line)
    report(this_file, FNR, "unjustified crash site: " line)
  }
  pending = standalone_marker ? FNR : 0
}

END { finish_file() }
' "${files[@]}")"

if [ -n "$violations" ]; then
  {
    echo "production crash guard failed:"
    echo "$violations"
    echo
    echo "justify a deliberate site with a marker comment on the line above it:"
    echo "    // panic-guard: <why this cannot be reached, or why stopping is correct>"
  } >&2
  exit 1
fi

echo "crash guard clean across ${#files[@]} rust sources"
