#!/usr/bin/env bash
# compare the gateway in examples/gateway with its go counterpart, bench/gateway.go,
# and print one table of median req/s, latency and memory. req/s counts successful
# responses only, so a request refused quickly does not read as throughput.
#
#   bench/gateway_bench.sh
#
# scenarios, each with wrk as the client and the same two backends behind it:
#   plain    GET /item?id=7, forwarded to an upstream chosen round robin
#   auth     GET /auth/item?id=7 with an es256 bearer token checked at the edge
#   failure  plain, with one backend killed a third of the way through the run.
#            the rows that matter are the failed requests and the p99.
#
# within a round each arm runs once and the starting arm rotates, for the reason
# bench/http_sweep.sh gives. each run starts fresh processes.
#
# settings, all from the environment:
#   PITH         pith compiler (default: ./target/release/pith)
#   ARMS         arms to run (default: "pith go")
#   SCENARIOS    scenarios to run (default: "plain auth failure")
#   CONNS        wrk connection counts (default: "64 256"). std.web answers 503
#                past 512 open connections, so a higher count measures refusals.
#   TRIALS       trials per arm and scenario; the median is reported (default: 3)
#   DURATION     seconds per measured run (default: 8)
#   WORKERS      PITH_GREEN_WORKERS and GOMAXPROCS for the gateway (default: 16)
#   GATEWAY_CPUS taskset list for the gateway (default: 0-7,16-23)
#   WRK_CPUS     taskset list for wrk (default: 8-11,24-27)
#   BACKEND_CPUS taskset lists for the two backends (default: 12-13,28-29 14-15,30-31)
set -uo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

pith=${PITH:-./target/release/pith}
arms=${ARMS:-pith go}
scenarios=${SCENARIOS:-plain auth failure}
conns_list=${CONNS:-64 256}
trials=${TRIALS:-3}
duration=${DURATION:-8}
workers=${WORKERS:-16}
gateway_cpus=${GATEWAY_CPUS:-0-7,16-23}
wrk_cpus=${WRK_CPUS:-8-11,24-27}
read -r backend_a_cpus backend_b_cpus <<<"${BACKEND_CPUS:-12-13,28-29 14-15,30-31}"

GATEWAY_PORT=20360
BACKEND_A_PORT=20361
BACKEND_B_PORT=20362
IDENTITY_PORT=20363

work=$(mktemp -d)
pids=()
stop_all() {
    for pid in "${pids[@]:-}"; do
        kill "$pid" 2>/dev/null
    done
    wait 2>/dev/null
    pids=()
}
trap 'stop_all; rm -rf "$work"' EXIT

"$pith" build bench/http_server.pith -o "$work/backend" >/dev/null || exit 1
"$pith" build examples/gateway/main.pith -o "$work/gateway_pith" >/dev/null || exit 1
"$pith" build examples/gateway/demo_fixture.pith -o "$work/fixture" >/dev/null || exit 1
go build -o "$work/gateway_go" bench/gateway.go || exit 1

cat >"$work/gateway.toml" <<TOML
[server]
host = "127.0.0.1"
port = $GATEWAY_PORT

[auth]
jwks_url = "http://127.0.0.1:$IDENTITY_PORT/keys"
issuer = "example-idp"
audience = "gateway"

[[route]]
name = "auth"
prefix = "/auth"
strip_prefix = true
auth = true
upstreams = ["127.0.0.1:$BACKEND_A_PORT", "127.0.0.1:$BACKEND_B_PORT"]

[[route]]
name = "item"
prefix = "/item"
upstreams = ["127.0.0.1:$BACKEND_A_PORT", "127.0.0.1:$BACKEND_B_PORT"]
TOML

"$work/fixture" "$IDENTITY_PORT" >"$work/token" 2>/dev/null &
identity_pid=$!
wait_for() {
    local i
    for i in $(seq 1 300); do
        if curl -s -o /dev/null "http://127.0.0.1:$1$2"; then
            return 0
        fi
        sleep 0.02
    done
    echo "nothing answered on port $1" >&2
    return 1
}
wait_for "$IDENTITY_PORT" /keys || exit 1
token=$(head -1 "$work/token")

# wrk's latency text, such as 1.65ms or 870.00us, in microseconds.
to_us() {
    awk -v v="$1" 'BEGIN {
        n = v + 0
        if (v ~ /us$/) print n
        else if (v ~ /ms$/) print n * 1000
        else if (v ~ /s$/) print n * 1000000
        else print "-"
    }'
}

median() {
    tr ' ' '\n' | awk 'NF' | sort -g | awk '{ v[NR] = $1 } END {
        if (NR == 0) { print "-"; exit }
        if (NR % 2) print v[(NR + 1) / 2]
        else print (v[NR / 2] + v[NR / 2 + 1]) / 2
    }'
}

# run one arm once: prints req/s, p50 us, p99 us, failed requests and peak kb.
run_once() {
    local arm=$1 scenario=$2 conns=$3
    pids=()
    PITH_GREEN_WORKERS=4 taskset -c "$backend_a_cpus" "$work/backend" "$BACKEND_A_PORT" >/dev/null 2>&1 &
    local backend_a=$!
    PITH_GREEN_WORKERS=4 taskset -c "$backend_b_cpus" "$work/backend" "$BACKEND_B_PORT" >/dev/null 2>&1 &
    local backend_b=$!
    pids+=("$backend_a" "$backend_b")
    wait_for "$BACKEND_A_PORT" /health || return 1
    wait_for "$BACKEND_B_PORT" /health || return 1
    if [ "$arm" = pith ]; then
        PITH_GREEN_WORKERS=$workers taskset -c "$gateway_cpus" "$work/gateway_pith" "$work/gateway.toml" >/dev/null 2>&1 &
    else
        GOMAXPROCS=$workers taskset -c "$gateway_cpus" "$work/gateway_go" "$GATEWAY_PORT" "http://127.0.0.1:$IDENTITY_PORT/keys" "127.0.0.1:$BACKEND_A_PORT" "127.0.0.1:$BACKEND_B_PORT" >/dev/null 2>&1 &
    fi
    local gateway=$!
    pids+=("$gateway")
    wait_for "$GATEWAY_PORT" /healthz || return 1

    local path=/item?id=7 header="X-Bench: 1"
    if [ "$scenario" = auth ]; then
        path=/auth/item?id=7
        header="Authorization: Bearer $token"
    fi
    if [ "$scenario" = failure ]; then
        (sleep "$((duration / 3))"; kill -9 "$backend_b" 2>/dev/null) &
    fi
    local out
    out=$(taskset -c "$wrk_cpus" wrk -t8 -c"$conns" -d"${duration}s" --latency -H "$header" "http://127.0.0.1:$GATEWAY_PORT$path")
    local rss
    rss=$(awk '/VmHWM/ { print $2 }' "/proc/$gateway/status" 2>/dev/null)
    local rate p50 p99 failed
    rate=$(echo "$out" | awk '/Requests\/sec/ { print $2 }')
    p50=$(to_us "$(echo "$out" | awk '$1 == "50%" { print $2 }')")
    p99=$(to_us "$(echo "$out" | awk '$1 == "99%" { print $2 }')")
    failed=$(echo "$out" | awk '/Non-2xx/ { n = $NF } /Socket errors/ { gsub(",", ""); s = $4 + $6 + $8 + $10 } END { print n + s + 0 }')
    rate=$(awk -v r="$rate" -v f="$failed" -v d="$duration" 'BEGIN { printf "%.0f", r - f / d }')
    echo "$rate $p50 $p99 $failed $rss"
    stop_all
}

echo "| scenario | conns | arm | ok req/s | p50 us | p99 us | failed | peak rss kb |"
echo "|---|---:|---|---:|---:|---:|---:|---:|"
for scenario in $scenarios; do
    for conns in $conns_list; do
        declare -A rates=() p50s=() p99s=() fails=() rsss=()
        for trial in $(seq 1 "$trials"); do
            ordered=$arms
            if [ $((trial % 2)) = 0 ]; then
                ordered=$(echo "$arms" | tr ' ' '\n' | tac | tr '\n' ' ')
            fi
            for arm in $ordered; do
                read -r rate p50 p99 failed rss <<<"$(run_once "$arm" "$scenario" "$conns")"
                rates[$arm]+=" $rate"
                p50s[$arm]+=" $p50"
                p99s[$arm]+=" $p99"
                fails[$arm]+=" $failed"
                rsss[$arm]+=" $rss"
            done
        done
        for arm in $arms; do
            printf '| %s | %s | %s | %s | %s | %s | %s | %s |\n' "$scenario" "$conns" "$arm" \
                "$(echo "${rates[$arm]}" | median)" "$(echo "${p50s[$arm]}" | median)" "$(echo "${p99s[$arm]}" | median)" \
                "$(echo "${fails[$arm]}" | median)" "$(echo "${rsss[$arm]}" | median)"
        done
    done
done
kill "$identity_pid" 2>/dev/null
