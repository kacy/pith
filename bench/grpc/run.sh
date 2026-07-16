#!/usr/bin/env bash
# run the grpc unary echo benchmark: one local tls server, three clients
# (go, rust, pith) each timing many calls over one connection. all three trust
# the localhost fixture ca and hit the same server, so it is a fair comparison
# of the clients. zig is left out — it has no grpc.
#
# run from the repo root so the pith client can resolve std/ and ir_driver:
#   bash bench/grpc/run.sh
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$ROOT"
B=bench/grpc

SERVER=$B/bin/server
GOCLIENT=$B/bin/goclient
RUSTCLIENT=$B/rust/target/release/rustclient
PITH=./target/release/pith
PITHSRC=$B/pith/client.pith
CA=tests/live/fixtures/localhost-ca.crt

CALLS=${CALLS:-20000}
WARMUP=${WARMUP:-2000}
ADDR=127.0.0.1:50051

"$SERVER" -addr "$ADDR" -cert "tests/live/fixtures/localhost.crt" -key "tests/live/fixtures/localhost.key" >/tmp/grpc_bench_server.log 2>&1 &
SRV=$!
trap 'kill $SRV 2>/dev/null || true' EXIT
sleep 1

row() {
  local size=$1 conc=$2
  "$GOCLIENT"   -addr "$ADDR"         -ca "$CA" -size "$size" -calls "$CALLS" -warmup "$WARMUP" -concurrency "$conc"
  "$RUSTCLIENT" -addr "https://$ADDR" -ca "$CA" -size "$size" -calls "$CALLS" -warmup "$WARMUP" -concurrency "$conc"
  PITH_GRPC_SIZE=$size PITH_GRPC_CALLS=$CALLS PITH_GRPC_WARMUP=$WARMUP PITH_GRPC_CONC=$conc PITH_GRPC_CA=$CA "$PITH" run "$PITHSRC"
  echo
}

echo "# grpc unary echo — tls over localhost, one connection, $CALLS calls each"
echo "# small payload (16 B), sequential"
row 16 1
echo "# larger payload (1 KiB), sequential"
row 1024 1
echo "# small payload (16 B), 8 concurrent over one connection"
row 16 8
echo "# larger payload (1 KiB), 8 concurrent"
row 1024 8
