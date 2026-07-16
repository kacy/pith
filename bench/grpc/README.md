# grpc client benchmark

a small, fair comparison of three grpc clients making unary calls against one
server: pith (`std.net.grpc` over its own tls + http/2 + protobuf), go
(`grpc-go`), and rust (`tonic`). zig is left out — it has no grpc.

the server is a local `grpc-go` echo service over tls, using the repo's
`tests/live/fixtures` localhost certificate so every client trusts the same ca
and the whole thing runs on loopback. because all three clients hit the same
server, the numbers reflect the clients, not the server.

## running

from the repo root:

```
bash bench/grpc/build.sh   # builds the go and rust binaries (needs go, cargo, protoc)
bash bench/grpc/run.sh     # starts the server and runs all three clients
```

`run.sh` sweeps a small (16 B) and a larger (1 KiB) payload, sequential and
8-way concurrent over a single connection. `CALLS` and `WARMUP` override the
call counts.

## what it measures

throughput (calls/sec over a batch) is the common metric — all three compute it
the same way, total calls over wall time. pith has only millisecond wall-clock
resolution, so it reports throughput and an average latency; the go and rust
clients also print per-call median and p99 as extra detail.

## the proto

one method, one bytes field, so the payload size is easy to vary:

```proto
service Echo { rpc Unary(EchoRequest) returns (EchoResponse); }
message EchoRequest  { bytes payload = 1; }
message EchoResponse { bytes payload = 1; }
```
