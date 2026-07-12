# Pith benchmarks

A few Pith vs Go (and sometimes Rust and Zig) benchmarks, measured on the
same machine. Compile times give a sense of the toolchain; the workload
and pipeline benchmarks below isolate runtime and service-logic costs.

## event ledger benchmark (json + collections + crypto)

`bench/event_ledger.*` ingests a stream of newline-delimited JSON events,
indexes them with maps and a set, and signs a canonical summary with
HMAC-SHA256. It exists to exercise a realistic slice of the standard
library — JSON decoding, hash-map and set aggregation, and crypto — in
four languages at once: Pith, Go, Rust, and Zig.

Every implementation generates the same event stream from a shared 31-bit
LCG, so the aggregate `checksum` and the HMAC `digest` come out identical
in all four. That equality is the honesty check: if any version did less
work or decoded a field differently, the digest would diverge.

**Batteries:** Pith, Go, and Zig write this with only their standard
libraries. Rust's std has neither JSON nor crypto, so its version pulls
`serde_json`, `sha2`, and `hmac` — the honest cost of a deliberately
small standard library. (Four independent crypto implementations landing
on the same HMAC digest is also decent evidence the digest is real.)

Run it:

```
bench/event_ledger_bench.sh 200000 5   # events, trials
```

which builds all four, checks the digests match, and prints median phase
times. Or build and run one directly:

```
pith build bench/event_ledger.pith && ./bench/event_ledger 200000
go build -o bench/event_ledger_go bench/event_ledger.go
cargo build --release --manifest-path bench/event_ledger_rust/Cargo.toml
zig build-exe -O ReleaseFast -femit-bin=bench/event_ledger_zig bench/event_ledger.zig
```

Latest measured results on this machine, 200000 events, median of 5:

| lang | gen | parse | analyze | sign | total |
|---|---:|---:|---:|---:|---:|
| pith | 317 | 179 | 55 | 0 | 551 |
| go | 105 | 337 | 15 | 0 | 457 |
| rust | 31 | 57 | 25 | 0 | 114 |
| zig | 21 | 97 | 9 | 0 | 128 |

(ms; `gen` builds the stream, `parse` decodes it into structs, `analyze`
runs the map/set rollup, `sign` is the HMAC.)

Read this honestly. Rust and Zig are fastest; Pith and Go are close, with
Pith about 1.2x Go on the total. The interesting part is `parse`: a flat
struct of required scalars decodes in a single pass, straight into the
struct — Pith's runtime fills it in place, no intermediate map and no
per-field allocation. That makes Pith's `parse` the second-fastest of
the four here, ahead of Go's reflection-based decode. It was not always
so: the first cut of this benchmark had Pith at 1084ms on parse and
1472ms total; the single-pass decoder took parse to ~179ms and the total
to ~551ms.

What's left of the gap is now the `gen` phase — building the event stream
is string-assembly, still Pith's slower area — and the map/set rollup.
And the whole pipeline — JSON, collections, HMAC-SHA256 — is standard
library with no dependencies to add, and compiles in a fraction of the
time the others take to build.

| | Go | Pith |
|---|---|---|
| Cold compile | 27.4s | 0.34s |
| Warm compile | 0.29s | 0.36s |
| Binary size | 7.2 MB | 4.9 MB |

Pith compiles from scratch every time — no incremental build cache yet.
Go's first build pulls and compiles the standard library; subsequent builds
use the cache. The binary-size difference comes from Go embedding its runtime
and GC, while Pith statically links a smaller Rust runtime.

## http server benchmark

`bench/http_server.pith` serves a small JSON API with allocation-churny
request handling — query parsing, catalog lookups, and per-request string
assembly. `bench/http_server_mt.pith` is the threaded variant, and
`bench/http_server.go` is the Go counterpart.

`bench/http_bench.sh` drives a server with `wrk` and samples RSS across the
run, so it doubles as a memory-growth check:

```
pith build bench/http_server.pith
go build -o bench/http_server_go bench/http_server.go

bench/http_bench.sh ./bench/http_server 8080 120
bench/http_bench.sh ./bench/http_server_go 8081 120
```

It prints a per-10s RSS table and a summary line (requests, throughput, and
RSS start / end / peak). The Pith server holds flat RSS across the run.

## catalog service benchmark

there is also a more realistic in-memory microservice benchmark:

- `bench/catalog_server.go`
- `bench/catalog_server.pith`
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
pith build bench/catalog_server.pith && mv bench/catalog_server bench/catalog_server_pith

# start servers
./bench/catalog_server_go &     # default port 9101
./bench/catalog_server_pith &  # default port 9102

# run benchmark
go run bench/catalog_bench.go
```

you can also override the ports for ad hoc runs:

```
./bench/catalog_server_go 9201 &
./bench/catalog_server_pith 9202 &
go run bench/catalog_bench.go 9201 9202
```

## catalog workload benchmark

for a stable service-shaped comparison without socket noise, there is also an
in-process catalog workload benchmark:

- `bench/catalog_workload.go`
- `bench/catalog_workload.rs`
- `bench/catalog_workload.pith`

this uses the same synthetic dataset and benchmark shape as the catalog service,
but runs the handler logic directly inside one process:

- profile lookups
- hot filtered searches
- wider aggregate scans
- batch JSON parsing plus score aggregation

running it:

```
# pith
pith build bench/catalog_workload.pith
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
pith build bench/catalog_workload.pith
go run bench/catalog_workload_bench.go 10000 5
```

the second argument is the number of trials. the runner reports median phase
times, which is more reliable than a single run when the timings are short.

the workload benchmark now also uses internal team/region ids and precomputed
candidate index lists for common region/active filters, which is closer to how
an actual in-memory service would avoid rescanning the full catalog on every
request.

latest measured results on this machine, using the median of 5 trials:

| iterations | go total | pith total | ratio | go batch | pith batch |
|---|---:|---:|---:|---:|---:|
| `1000000` | `1977 ms` | `654 ms` | `0.33x` | `1878 ms` | `593 ms` |

with the optional rust workload binary built:

| iterations | rust total | pith/rust | rust batch | pith/rust batch |
|---|---:|---:|---:|---:|
| `1000000` | `369 ms` | `1.77x` | `323 ms` | `1.84x` |

the current pith workload uses derived json struct decoding for the batch
request. a flat struct of required scalars decodes in a single pass, filled
straight into the struct — see the event_ledger benchmark for the details.
the rust workload uses a tiny standalone json field scanner, so treat it as
a lower-bound runtime comparison rather than a serde-style library
comparison.

binary size from the same build:

| binary | file size | text segment |
|---|---:|---:|
| pith workload | `5.2M` | `1.4M` |
| go workload | `2.7M` | `1.7M` |
| rust workload | `3.9M` | `366K` |

the pith workload binary is larger on disk than the go/rust binaries today,
but its executable text segment is smaller than go's in this build. that points
at debug/symbol/linker overhead as a likely size target before reading too much
into the file-size number alone.

this is the better comparison point today if you want to isolate runtime,
language, and service-logic costs from the current long-running HTTP server
behavior.

note: the live HTTP catalog benchmark is still exploratory on the Pith side.
the Pith service currently exits after its first successful request, so the
stable comparison point today is the workload benchmark above.

## std pipeline benchmark

`bench/std_pipeline.*` is a batteries-included data pipeline benchmark. it
generates deterministic records, writes and reads csv, transforms rows with url
and path helpers, writes a json report, gzip round-trips the report, hashes the
result, and touches the temp workspace through fs traversal.

running it:

```
./self-host/pith_main build bench/std_pipeline.pith
env GOCACHE=/tmp/pith-go-cache go build -o bench/std_pipeline_go bench/std_pipeline.go
cargo build --release --manifest-path bench/std_pipeline_rust/Cargo.toml
env GOCACHE=/tmp/pith-go-cache go run bench/std_pipeline_bench.go 50000 5
```

latest measured results on this machine, using the median of 5 trials:

| records | go total | rust total | pith total | pith/go | pith/rust |
|---|---:|---:|---:|---:|---:|
| `50000` | `322 ms` | `135 ms` | `658 ms` | `2.04x` | `4.87x` |

phase breakdown from the same run:

| phase | go | rust | pith |
|---|---:|---:|---:|
| config | `0 ms` | `0 ms` | `0 ms` |
| csv write | `190 ms` | `60 ms` | `265 ms` |
| csv read | `81 ms` | `46 ms` | `5 ms` |
| transform | `44 ms` | `29 ms` | `382 ms` |
| json | `0 ms` | `0 ms` | `0 ms` |
| gzip + hash | `1 ms` | `0 ms` | `1 ms` |
| fs | `0 ms` | `0 ms` | `0 ms` |

all three implementations report the same checksum:

```
107395835982034
```

binary size from the same build:

| binary | file size | text segment |
|---|---:|---:|
| pith pipeline | `5.3M` | `1.4M` |
| go pipeline | `3.5M` | `2.3M` |
| rust pipeline | `1.4M` | `1.1M` |

the first cut of this benchmark had pith at `12682 ms`. moving csv onto the
bytes path and avoiding per-row maps brought that down to `2023 ms`. the
url/path/hash fast paths brought it down again to about `1400 ms`. lazy csv row
views brought it to about `1230 ms` by avoiding the full `List[List[String]]`
read path. folding csv rows through the public module API keeps the same
zero-copy shape and landed around `1200 ms`; string-derive and byte-scanning
work since (single-allocation string derives, a combined bytes-substring
decode) took it to about `658 ms`. the remaining gap is mostly csv write
overhead and transform work that still turns url and path fields into
strings.

three caveats matter when reading this benchmark:

- rust uses pinned crates for the libraries it does not ship in `std`, which is
  the normal rust way to write this kind of tool.
- the local go toolchain in this environment could not resolve `encoding/csv`
  or `hash/fnv`, so the go workload carries tiny csv and fnv helpers while
  still using go's json, gzip, sha256, url, path, and fs packages.
- the pith version keeps the config setup local for now. importing `std.config`
  with this full module mix currently exposes a checker symbol-collision bug,
  so the benchmark still times the larger csv/url/path/gzip/hash/fs pipeline
  while avoiding that unrelated compile failure.
