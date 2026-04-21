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
- `bench/catalog_workload.rs`
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

# rust
rustc -O -o bench/catalog_workload_rust bench/catalog_workload.rs
./bench/catalog_workload_rust 4000
```

a helper runner is also available once the workload binaries are built:

```
go build -o bench/catalog_workload_go bench/catalog_workload.go
rustc -O -o bench/catalog_workload_rust bench/catalog_workload.rs
forge build bench/catalog_workload.fg
go run bench/catalog_workload_bench.go 10000 5
```

the second argument is the number of trials. the runner reports median phase
times, which is more reliable than a single run when the timings are short.

the workload benchmark now also uses internal team/region ids and precomputed
candidate index lists for common region/active filters, which is closer to how
an actual in-memory service would avoid rescanning the full catalog on every
request.

latest measured results on this machine, using the median of 7 trials:

| iterations | go total | forge total | ratio | go batch | forge batch |
|---|---:|---:|---:|---:|---:|
| `1000000` | `3128 ms` | `851 ms` | `0.27x` | `2993 ms` | `802 ms` |

with the optional rust workload binary built:

| iterations | rust total | forge/rust | rust batch | forge/rust batch |
|---|---:|---:|---:|---:|
| `1000000` | `689 ms` | `1.24x` | `616 ms` | `1.30x` |

the current forge workload uses derived json struct decoding for the batch
request. six-field flat structs now use a generated one-pass decode helper,
while other wider structs still use one shallow scalar scan before generated
struct construction. the rust workload uses a tiny standalone json field
scanner, so treat it as a lower-bound runtime comparison rather than a
serde-style library comparison.

binary size from the same build:

| binary | file size | text segment |
|---|---:|---:|
| forge workload | `5.2M` | `1.4M` |
| go workload | `2.7M` | `1.7M` |
| rust workload | `3.9M` | `366K` |

the forge workload binary is larger on disk than the go/rust binaries today,
but its executable text segment is smaller than go's in this build. that points
at debug/symbol/linker overhead as a likely size target before reading too much
into the file-size number alone.

this is the better comparison point today if you want to isolate runtime,
language, and service-logic costs from the current long-running HTTP server
behavior.

note: the live HTTP catalog benchmark is still exploratory on the Forge side.
the Forge service currently exits after its first successful request, so the
stable comparison point today is the workload benchmark above.

## std pipeline benchmark

`bench/std_pipeline.*` is a batteries-included data pipeline benchmark. it
generates deterministic records, writes and reads csv, transforms rows with url
and path helpers, writes a json report, gzip round-trips the report, hashes the
result, and touches the temp workspace through fs traversal.

running it:

```
./self-host/forge_main build bench/std_pipeline.fg
env GOCACHE=/tmp/forge-go-cache go build -o bench/std_pipeline_go bench/std_pipeline.go
cargo build --release --manifest-path bench/std_pipeline_rust/Cargo.toml
env GOCACHE=/tmp/forge-go-cache go run bench/std_pipeline_bench.go 50000 5
```

latest measured results on this machine, using the median of 5 trials:

| records | go total | rust total | forge total | forge/go | forge/rust |
|---|---:|---:|---:|---:|---:|
| `50000` | `366 ms` | `212 ms` | `1198 ms` | `3.27x` | `5.65x` |

phase breakdown from the same run:

| phase | go | rust | forge |
|---|---:|---:|---:|
| config | `0 ms` | `0 ms` | `0 ms` |
| csv write | `204 ms` | `96 ms` | `475 ms` |
| csv read | `106 ms` | `72 ms` | `5 ms` |
| transform | `59 ms` | `42 ms` | `711 ms` |
| json | `0 ms` | `0 ms` | `0 ms` |
| gzip + hash | `0 ms` | `0 ms` | `0 ms` |
| fs | `0 ms` | `0 ms` | `0 ms` |

all three implementations report the same checksum:

```
107395835982034
```

binary size from the same build:

| binary | file size | text segment |
|---|---:|---:|
| forge pipeline | `5.3M` | `1.4M` |
| go pipeline | `3.5M` | `2.3M` |
| rust pipeline | `1.4M` | `1.1M` |

the first cut of this benchmark had forge at `12682 ms`. moving csv onto the
bytes path and avoiding per-row maps brought that down to `2023 ms`. the
url/path/hash fast paths brought it down again to about `1400 ms`. lazy csv row
views brought it to about `1230 ms` by avoiding the full `List[List[String]]`
read path. folding csv rows through the public module API keeps the same
zero-copy shape and lands around `1200 ms`; the remaining gap is mostly csv
write overhead and transform work that still turns url and path fields into
strings.

three caveats matter when reading this benchmark:

- rust uses pinned crates for the libraries it does not ship in `std`, which is
  the normal rust way to write this kind of tool.
- the local go toolchain in this environment could not resolve `encoding/csv`
  or `hash/fnv`, so the go workload carries tiny csv and fnv helpers while
  still using go's json, gzip, sha256, url, path, and fs packages.
- the forge version keeps the config setup local for now. importing `std.config`
  with this full module mix currently exposes a checker symbol-collision bug,
  so the benchmark still times the larger csv/url/path/gzip/hash/fs pipeline
  while avoiding that unrelated compile failure.
