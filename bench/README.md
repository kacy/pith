# Forge vs Go — HTTP Server Benchmark

Identical HTTP servers in Forge and Go, measured on the same machine.
Both run sequentially (one request at a time) for a fair comparison.

## Build

| | Go | Forge |
|---|---|---|
| Cold compile | 27.4s | 0.34s |
| Warm compile | 0.29s | 0.36s |
| Binary size | 7.2 MB | 4.9 MB |

Forge compiles from scratch every time — no incremental build cache yet.
Go's first build pulls and compiles the standard library; subsequent builds
use the cache. Binary size difference comes from Go embedding its runtime
and GC, while Forge statically links a smaller Rust runtime.

## Latency (March 2026)

Sequential requests, one connection at a time, measured from a Go benchmark
client on the same machine. Each endpoint hit 50-200 times.

| Endpoint | Go p50 | Forge p50 | Ratio |
|----------|--------|-----------|-------|
| `GET /` (HTML) | 941us | 1031us | 1.1x |
| `GET /json` | 931us | 993us | 1.1x |
| `GET /echo?msg=test` | 899us | 1066us | 1.2x |
| `GET /compute?n=100` | 982us | 1114us | 1.1x |
| `GET /compute?n=1000` | 1082us | 1146us | 1.1x |

At median latency, Forge is within 10-20% of Go across all endpoints.
The gap is almost entirely TCP and syscall overhead — both servers spend
most of their time in the kernel, not in application code.

Tail latency (p95/p99) is noisier on both sides and varies between runs.

## Endpoints

Both servers implement the same four routes:

- `/` — return a small HTML page
- `/json` — return a JSON object
- `/echo?msg=X` — repeat the message 10 times
- `/compute?n=N` — sum of a math series (integer work)

## Running

```
# compile
go build -o bench/server_go bench/server.go
forge build bench/server.fg && mv bench/server bench/server_forge

# start servers
./bench/server_go &     # port 9001
./bench/server_forge &  # port 9002

# run benchmark
go run bench/bench.go
```
