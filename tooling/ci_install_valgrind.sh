#!/usr/bin/env bash
#
# install valgrind on a ci runner, for the memcheck steps.
#
# the runner image lists third-party repositories (microsoft, azure-cli and
# others) beside the ubuntu archive, and a plain `apt-get update` fetches all
# of them. the stalls seen in practice hung inside that update with no output
# until the wall-clock bound killed it, and a retry against the same sources
# usually hung the same way. so the update reads only the ubuntu sources,
# each attempt is bounded, and after the first failure the attempts move from
# the runner's default mirror to archive.ubuntu.com. apt prints its fetches,
# so a stall names the url it hung on.
#
# a failure here must fail the step: `make memcheck` skips itself when
# valgrind is missing, so a swallowed install failure would turn the step into
# a guard that passes without running.
#
# APT_ETC overrides /etc/apt, for testing the source selection off a runner.
set -uo pipefail

etc=${APT_ETC:-/etc/apt}
parts=$(mktemp -d)
trap 'rm -rf "$parts"' EXIT

# older images keep the archive in sources.list itself, so it is copied too
# and read from the same directory.
if grep -qs '^deb ' "$etc/sources.list"; then cp "$etc/sources.list" "$parts/base.list"; fi
for f in "$etc"/sources.list.d/ubuntu.sources "$etc"/sources.list.d/ubuntu*.list; do
  if [ -f "$f" ]; then cp "$f" "$parts/"; fi
done

opts=(-q -o Acquire::Retries=2 -o Acquire::http::Timeout=20 -o Acquire::https::Timeout=20)
if compgen -G "$parts/*" > /dev/null; then
  opts+=(-o Dir::Etc::SourceList=/dev/null -o Dir::Etc::SourceParts="$parts")
else
  echo "no ubuntu source file under $etc/sources.list.d; updating every source"
fi

install_valgrind() {
  timeout 90 sudo apt-get "${opts[@]}" update \
    && timeout 120 sudo apt-get "${opts[@]}" install -y --no-install-recommends valgrind
}

for attempt in 1 2 3; do
  if install_valgrind; then
    exit 0
  fi
  echo "valgrind install attempt $attempt stalled or failed"
  if [ "$attempt" = 1 ]; then
    sed -i -E 's#https?://[a-z0-9.-]*archive\.ubuntu\.com/ubuntu#http://archive.ubuntu.com/ubuntu#g' "$parts"/* 2> /dev/null
  fi
  sleep 10
done
echo "could not install valgrind after 3 attempts" >&2
exit 1
