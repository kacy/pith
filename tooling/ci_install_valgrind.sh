#!/usr/bin/env bash
#
# install valgrind on a ci runner, for the memcheck steps.
#
# it runs with a hard wall-clock bound and one retry. the Acquire::*::Timeout
# options only bound an HTTP read, but the stalls seen in practice hung before
# that layer (dns, the dpkg lock, sudo itself) and sat until the step timeout
# with zero output. wrapping each apt call in `timeout` fails fast wherever it
# wedges, and a second attempt after a short sleep clears a transiently bad
# mirror. a failure here must fail the step: `make memcheck` skips itself when
# valgrind is missing, so a swallowed install failure would turn the step into
# a guard that passes without running.
set -uo pipefail

install_valgrind() {
  timeout 240 sudo apt-get -o Acquire::Retries=3 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30 update -qq \
    && timeout 240 sudo apt-get -o Acquire::Retries=3 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30 install -y -qq valgrind
}

install_valgrind || { echo "apt install stalled or failed; retrying once after 15s"; sleep 15; install_valgrind; }
