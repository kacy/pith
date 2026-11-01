#!/usr/bin/env bash
#
# file what a pithgen hunt found as github issues, one per signature: a new
# signature opens an issue labelled `pithgen`, and one that already has an open
# issue gets a comment with tonight's seeds instead of a duplicate. the nightly
# workflow runs this after tooling/pithgen_hunt.sh; it needs `gh` and a token
# allowed to write issues.
#
#   tooling/pithgen_issues.sh .pith-build/hunt
#   PITHGEN_ISSUES_DRY_RUN=1 tooling/pithgen_issues.sh .pith-build/hunt
#                                   # say what it would file, write nothing
set -euo pipefail

dry_run="${PITHGEN_ISSUES_DRY_RUN:-0}"

out="${1:-.pith-build/hunt}"
run_url="${GITHUB_SERVER_URL:-https://github.com}/${GITHUB_REPOSITORY:-}/actions/runs/${GITHUB_RUN_ID:-}"
[ -s "$out/signatures.tsv" ] || exit 0

if [ "$dry_run" != 1 ]; then
  gh label create pithgen --color B60205 \
    --description "found by the nightly pithgen hunt" --force >/dev/null
fi

while IFS=$'\t' read -r signature class seed dir count; do
  # the signature is built from sanitizer output and generated names, so it
  # only ever reaches gh as an argument or a file, never through a shell
  title="pithgen: $(printf '%s' "$signature" | tr -cd '[:print:]' | cut -c1-200)"
  body="$(mktemp)"
  {
    echo "found by the nightly pithgen hunt: [run]($run_url)"
    echo
    echo "- class: $class"
    echo "- programs hit tonight: $count"
    echo "- first seed: $seed"
    echo
    echo "reproduce with the asan runtime (see tooling/sanitize.sh setup):"
    echo
    echo '```'
    echo "tooling/pithgen/target/release/pithgen run --sanitize --seeds $seed..$((seed + 1)) --pith ./target/release/pith --out /tmp/repro"
    echo '```'
    echo
    echo "<details><summary>report</summary>"
    echo
    echo '~~~~'
    head -c 20000 "$dir/finding.txt"
    echo
    echo '~~~~'
    echo "</details>"
    if [ -f "$dir/main.pith" ]; then
      echo
      echo "<details><summary>program</summary>"
      echo
      echo '~~~~'
      head -c 40000 "$dir/main.pith"
      echo
      echo '~~~~'
      echo "</details>"
    fi
  } >"$body"

  existing="$(gh issue list --label pithgen --state open --limit 200 --json number,title \
    --jq ".[] | select(.title == $(printf '%s' "$title" | jq -Rs .)) | .number" | head -1)"
  if [ "$dry_run" = 1 ]; then
    echo "would ${existing:+comment on #$existing}${existing:-open}: $title ($(wc -c <"$body") byte body)"
  elif [ -n "$existing" ]; then
    gh issue comment "$existing" --body-file "$body" >/dev/null
    echo "commented on #$existing: $title"
  else
    gh issue create --title "$title" --label pithgen --body-file "$body" >/dev/null
    echo "opened: $title"
  fi
  rm -f "$body"
done <"$out/signatures.tsv"
