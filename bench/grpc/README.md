# gRPC client benchmark

This benchmark is a small, fair comparison of three gRPC clients making unary
calls against one server: Pith (`std.net.grpc` over its own TLS, HTTP/2 and
protobuf), Go (`grpc-go`), and Rust (`tonic`). Zig is left out because it has
no gRPC.

The server is a local `grpc-go` echo service over TLS. It uses the repo's
`tests/live/fixtures` localhost certificate, so every client trusts the same CA
and everything runs on loopback. All three clients hit the same server, so the
differences in the numbers come from the clients.

## Running

From the repo root:

```
bash bench/grpc/build.sh   # builds the go and rust binaries (needs go, cargo, protoc)
bash bench/grpc/run.sh     # starts the server and runs all three clients
```

`run.sh` sweeps a small (16 B) and a larger (1 KiB) payload, sequential and
8-way concurrent over a single connection. `CALLS` and `WARMUP` override the
call counts.

## What it measures

Throughput (calls/sec over a batch) is the common metric. All three clients
compute it the same way, as total calls over wall time. They also time each
call with a monotonic clock (`time.mono_nanos()` in Pith) and print the
per-call median and p99 alongside throughput.

## The proto

Each message has one bytes field, so the payload size is easy to vary. The
service declares all four call shapes, and the benchmark exercises `Unary`:

```proto
service Echo {
  rpc Unary(EchoRequest) returns (EchoResponse);
  rpc ServerStream(EchoRequest) returns (stream EchoResponse);
  rpc ClientStream(stream EchoRequest) returns (EchoResponse);
  rpc BidiStream(stream EchoRequest) returns (stream EchoResponse);
}
message EchoRequest  { bytes payload = 1; }
message EchoResponse { bytes payload = 1; }
```
