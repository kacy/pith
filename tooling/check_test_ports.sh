#!/bin/sh
# refuse a regression case that listens on a port inside linux's ephemeral
# range (32768-60999, the default `ip_local_port_range`).
#
# the kernel hands ports from that range to every outgoing connect(), and a
# client socket that closes first leaves its port in TIME_WAIT for 60 s with
# the reuse flag off. a later bind to that number fails with EADDRINUSE even
# with SO_REUSEADDR set, so a corpus program with a fixed port there fails
# whenever any program in the previous minute (or the ci runner itself) dialed
# out from that number. #1092 was exactly this: the tls 1.2 tests had 13 such
# ports and printed `listen-error` for one of them a few times a week in ci,
# never locally, where nothing else was dialing.
#
# only lines that name a port are inspected, so a count or a timeout in
# milliseconds that happens to fall in the range is not flagged.
set -u

status=0
for f in tests/cases/*.pith; do
    awk -v file="$f" '
        /PORT|port|listen\(|run_case\(/ {
            line = $0
            while (match(line, /[0-9]+/)) {
                n = substr(line, RSTART, RLENGTH) + 0
                before = RSTART > 1 ? substr(line, RSTART - 1, 1) : " "
                if (n >= 32768 && n <= 60999 && before !~ /[A-Za-z0-9_.]/) {
                    printf "%s:%d: port %d is inside the ephemeral range 32768-60999\n", file, NR, n
                    bad = 1
                }
                line = substr(line, RSTART + RLENGTH)
            }
        }
        END { exit bad ? 1 : 0 }
    ' "$f" || status=1
done

if [ "$status" -ne 0 ]; then
    echo "FAIL: regression cases must listen outside the ephemeral port range; use 20xxx"
    exit 1
fi
echo "no regression case listens inside the ephemeral port range"
