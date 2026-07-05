#!/usr/bin/env bash
# drive an http server with wrk and sample its rss over the run.
#
#   bench/http_bench.sh <server-cmd> <port> [duration-seconds]
#
# examples:
#   bench/http_bench.sh ./bench/http_server 8080 120
#   bench/http_bench.sh ./bench/http_server_go 8081 120
#
# prints a per-10s rss table and a summary line:
#   requests, throughput, rss start / end / peak, growth over the run.
set -euo pipefail

cmd=$1
port=$2
duration=${3:-120}

$cmd "$port" > /dev/null 2>&1 &
server_pid=$!
trap 'kill $server_pid 2>/dev/null || true' EXIT
sleep 1

if ! curl -sf "http://127.0.0.1:$port/health" > /dev/null; then
    echo "server failed to come up on port $port" >&2
    exit 1
fi

rss_kb() { awk '/VmRSS/{print $2}' "/proc/$server_pid/status" 2>/dev/null || echo 0; }

start_rss=$(rss_kb)
samples_file=$(mktemp)
(
    while kill -0 $server_pid 2>/dev/null; do
        echo "$(date +%s) $(rss_kb)" >> "$samples_file"
        sleep 2
    done
) &
sampler_pid=$!

wrk_out=$(wrk -t 2 -c 8 -d "${duration}s" "http://127.0.0.1:$port/item?id=12345" 2>&1)

kill $sampler_pid 2>/dev/null || true
end_rss=$(rss_kb)
peak_rss=$(sort -k2 -n "$samples_file" | tail -1 | awk '{print $2}')
requests=$(echo "$wrk_out" | awk '/requests in/{print $1}')
rps=$(echo "$wrk_out" | awk '/Requests\/sec/{print $2}')

echo "rss over the run (kb, every ~20s):"
awk 'NR % 10 == 1 {print "  t+" (NR-1)*2 "s: " $2}' "$samples_file"
echo
echo "requests=$requests throughput=${rps}/s rss_start=${start_rss}kb rss_end=${end_rss}kb rss_peak=${peak_rss}kb growth=$((end_rss - start_rss))kb"
rm -f "$samples_file"
