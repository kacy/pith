#!/usr/bin/env bash
# sweep the http servers over wrk connection counts, with the server and wrk
# pinned to disjoint cpu sets, and print one table of median req/s and p99.
#
#   bench/http_sweep.sh
#
# arms, all serving the same /item?id=N route and body:
#   pith      bench/http_server.pith (std.net.http)
#   nethttp   bench/http_server.go (net/http)
#   min       bench/http_server_min.go (hand-rolled keep-alive loop on net and bufio)
#   fasthttp  bench/fasthttp (github.com/valyala/fasthttp, its own module)
#
# within a round every arm runs once, back to back, and the starting arm
# rotates from one trial to the next. running all trials of one arm and then
# all of the next would hand whichever arm went first the host's fresher state
# (see the http server section of bench/README.md), so the order is spread
# across arms instead. each run starts a fresh server, waits for /health,
# runs an unmeasured warmup, then the measured wrk run.
#
# settings, all from the environment:
#   WRK          wrk binary (default: wrk)
#   PITH         pith compiler (default: pith, else ./target/release/pith)
#   ARMS         arms to run (default: "pith nethttp min fasthttp")
#   CONNS        connection counts (default: "32 128 256 1024")
#   TRIALS       trials per arm and connection count; the median is reported (default: 3)
#   DURATION     seconds per measured run (default: 10)
#   WARMUP       seconds of unmeasured load before each run, 0 to skip (default: 1)
#   THREADS      wrk threads (default: 16; with 8, wrk caps the go arms near 1.65M req/s)
#   SERVER_CPUS  taskset cpu list for the server (default: 0-7,16-23)
#   WRK_CPUS     taskset cpu list for wrk (default: 8-15,24-31)
#   WORKERS      PITH_GREEN_WORKERS for pith and GOMAXPROCS for the go arms (default: 16)
#
# the cpu defaults give the server one ccd of a 9950x and wrk the other, with
# each ccd's smt siblings, so the two never share a core or an l3. check
# /sys/devices/system/cpu/cpu*/topology/thread_siblings_list on another machine.
#   PORT         first port; each run takes the next one (default: 18080)
#   PITH_SERVER, NETHTTP_SERVER, MIN_SERVER, FASTHTTP_SERVER
#                prebuilt server binaries; an arm without one is built here
#
# a short smoke run:
#   TRIALS=1 DURATION=2 WARMUP=0 bench/http_sweep.sh
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

wrk=${WRK:-wrk}
arms=(${ARMS:-pith nethttp min fasthttp})
conns=(${CONNS:-32 128 256 1024})
trials=${TRIALS:-3}
duration=${DURATION:-10}
warmup=${WARMUP:-1}
threads=${THREADS:-16}
server_cpus=${SERVER_CPUS:-0-7,16-23}
wrk_cpus=${WRK_CPUS:-8-15,24-31}
workers=${WORKERS:-16}
port=${PORT:-18080}
path='/item?id=12345'

if ! command -v "$wrk" > /dev/null; then
    echo "wrk not found: $wrk (set WRK)" >&2
    exit 1
fi

# 1024 connections need as many descriptors on each side
ulimit -n 65536 2> /dev/null || true

work=$(mktemp -d)
server_pid=
cleanup() {
    if [ -n "$server_pid" ]; then
        kill "$server_pid" 2> /dev/null || true
        wait "$server_pid" 2> /dev/null || true
    fi
    rm -rf "$work"
}
trap cleanup EXIT

# build the arms that were not handed a binary. the go arms build into the
# scratch directory so a sweep never rewrites a binary under bench/.
pith_server=${PITH_SERVER:-}
nethttp_server=${NETHTTP_SERVER:-}
min_server=${MIN_SERVER:-}
fasthttp_server=${FASTHTTP_SERVER:-}
for arm in "${arms[@]}"; do
    case $arm in
    pith)
        if [ -z "$pith_server" ]; then
            pith=${PITH:-pith}
            if ! command -v "$pith" > /dev/null && [ -x ./target/release/pith ]; then
                pith=./target/release/pith
            fi
            echo "building pith arm with $pith" >&2
            "$pith" build bench/http_server.pith > /dev/null
            pith_server=./bench/http_server
        fi
        ;;
    nethttp)
        if [ -z "$nethttp_server" ]; then
            echo "building nethttp arm" >&2
            go build -o "$work/http_server_go" bench/http_server.go
            nethttp_server=$work/http_server_go
        fi
        ;;
    min)
        if [ -z "$min_server" ]; then
            echo "building min arm" >&2
            go build -o "$work/http_server_min" bench/http_server_min.go
            min_server=$work/http_server_min
        fi
        ;;
    fasthttp)
        if [ -z "$fasthttp_server" ]; then
            echo "building fasthttp arm" >&2
            (cd bench/fasthttp && go build -o "$work/http_server_fasthttp" .)
            fasthttp_server=$work/http_server_fasthttp
        fi
        ;;
    *)
        echo "unknown arm: $arm" >&2
        exit 1
        ;;
    esac
done

# start one arm's server on $port, pinned, and wait for /health
start_server() {
    local arm=$1
    case $arm in
    pith) env PITH_GREEN_WORKERS="$workers" taskset -c "$server_cpus" "$pith_server" "$port" > /dev/null 2>&1 & ;;
    nethttp) env GOMAXPROCS="$workers" taskset -c "$server_cpus" "$nethttp_server" "$port" > /dev/null 2>&1 & ;;
    min) env GOMAXPROCS="$workers" taskset -c "$server_cpus" "$min_server" "$port" > /dev/null 2>&1 & ;;
    fasthttp) env GOMAXPROCS="$workers" taskset -c "$server_cpus" "$fasthttp_server" "$port" > /dev/null 2>&1 & ;;
    esac
    server_pid=$!
    for _ in $(seq 50); do
        if curl -sf "http://127.0.0.1:$port/health" > /dev/null; then
            return 0
        fi
        sleep 0.1
    done
    echo "$arm failed to come up on port $port" >&2
    exit 1
}

stop_server() {
    kill "$server_pid" 2> /dev/null || true
    wait "$server_pid" 2> /dev/null || true
    server_pid=
}

run_wrk() {
    taskset -c "$wrk_cpus" "$wrk" -t "$threads" -c "$1" -d "${2}s" --latency "http://127.0.0.1:$port$path" 2>&1
}

# wrk prints latencies with a unit; normalize to microseconds
to_us() {
    awk -v v="$1" 'BEGIN {
        n = v + 0
        if (v ~ /us$/) print n
        else if (v ~ /ms$/) print n * 1000
        else if (v ~ /s$/) print n * 1000000
        else print n
    }'
}

median() {
    tr ' ' '\n' | awk 'NF' | sort -g | awk '{ v[NR] = $1 } END {
        if (NR == 0) { print "-"; exit }
        if (NR % 2) print v[(NR + 1) / 2]
        else print (v[NR / 2] + v[NR / 2 + 1]) / 2
    }'
}

declare -A rps p99 errs
any_errs=0
narms=${#arms[@]}
echo "server cpus $server_cpus, wrk cpus $wrk_cpus, wrk -t $threads, ${duration}s x $trials trials, workers $workers" >&2
for c in "${conns[@]}"; do
    for trial in $(seq "$trials"); do
        for i in $(seq 0 $((narms - 1))); do
            arm=${arms[$(((i + trial - 1) % narms))]}
            start_server "$arm"
            if [ "$warmup" != 0 ]; then
                run_wrk "$c" "$warmup" > /dev/null
            fi
            out=$(run_wrk "$c" "$duration")
            stop_server
            port=$((port + 1))
            r=$(echo "$out" | awk '/^Requests\/sec:/ { print $2 }')
            l=$(echo "$out" | awk '$1 == "99%" { print $2 }')
            e=$(echo "$out" | awk '/Socket errors|Non-2xx/ { print }' | tr '\n' ' ')
            rps[$arm,$c]="${rps[$arm,$c]:-} ${r:-}"
            p99[$arm,$c]="${p99[$arm,$c]:-} $(to_us "${l:-0}")"
            if [ -n "$e" ]; then
                errs[$arm,$c]=1
                any_errs=1
                echo "  $arm c=$c trial $trial: $e" >&2
            fi
            echo "  c=$c trial $trial $arm: ${r:-?} req/s, p99 ${l:-?}" >&2
        done
    done
done

echo
printf '| conns |'
for arm in "${arms[@]}"; do printf ' %s req/s | %s p99 us |' "$arm" "$arm"; done
echo
printf '|---:|'
for _ in "${arms[@]}"; do printf -- '---:|---:|'; done
echo
for c in "${conns[@]}"; do
    printf '| %s |' "$c"
    for arm in "${arms[@]}"; do
        r=$(echo "${rps[$arm,$c]}" | median)
        l=$(echo "${p99[$arm,$c]}" | median)
        mark=
        if [ -n "${errs[$arm,$c]:-}" ]; then mark='*'; fi
        printf ' %.0f%s | %.0f |' "$r" "$mark" "$l"
    done
    echo
done
if [ "$any_errs" = 1 ]; then
    echo
    echo "* wrk reported socket errors or non-2xx responses in at least one trial"
fi
