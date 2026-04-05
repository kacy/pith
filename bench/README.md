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

## catalog service benchmark

there is also a more realistic in-memory microservice benchmark:

- `bench/catalog_server.go`
- `bench/catalog_server.fg`
- `bench/catalog_bench.go`

this pair serves the same synthetic catalog dataset and exposes:

- `/health` — simple readiness check
- `/profile?id=123` — single-record lookup
- `/search?...` — filtered scans and aggregate summaries
- `POST /batch-score` — JSON body parsing plus aggregate scoring

the goal is to benchmark something closer to a normal Go service:
request parsing, dataset scans, query filtering, and JSON responses.

running it:

```
# compile
go build -o bench/catalog_server_go bench/catalog_server.go
forge build bench/catalog_server.fg && mv bench/catalog_server bench/catalog_server_forge

# start servers
./bench/catalog_server_go &     # default port 9101
./bench/catalog_server_forge &  # default port 9102

# run benchmark
go run bench/catalog_bench.go
```

you can also override the ports for ad hoc runs:

```
./bench/catalog_server_go 9201 &
./bench/catalog_server_forge 9202 &
go run bench/catalog_bench.go 9201 9202
```

## catalog workload benchmark

for a stable service-shaped comparison without socket noise, there is also an
in-process catalog workload benchmark:

- `bench/catalog_workload.go`
- `bench/catalog_workload.fg`

this uses the same synthetic dataset and benchmark shape as the catalog service,
but runs the handler logic directly inside one process:

- profile lookups
- hot filtered searches
- wider aggregate scans
- batch JSON parsing plus score aggregation

running it:

```
# forge
forge build bench/catalog_workload.fg
./bench/catalog_workload 4000

# go
go run bench/catalog_workload.go 4000
```

this is the better comparison point today if you want to isolate runtime and
language costs from the current long-running HTTP server behavior.

a helper runner is also available once both workload binaries are built:

```
go build -o bench/catalog_workload_go bench/catalog_workload.go
forge build bench/catalog_workload.fg
go run bench/catalog_workload_bench.go 10000
```

note: the live HTTP catalog benchmark is still exploratory on the Forge side.
the Forge service currently exits after its first successful request, so the
stable comparison point today is the workload benchmark above.
