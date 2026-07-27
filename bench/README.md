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
| pith | 322 | 199 | 56 | 0 | 575 |
| go | 127 | 354 | 16 | 0 | 490 |
| rust | 31 | 62 | 24 | 0 | 122 |
| zig | 19 | 104 | 9 | 0 | 136 |

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
| `1000000` | `2009 ms` | `691 ms` | `0.34x` | `1910 ms` | `630 ms` |

with the optional rust workload binary built:

| iterations | rust total | pith/rust | rust batch | pith/rust batch |
|---|---:|---:|---:|---:|
| `1000000` | `369 ms` | `1.87x` | `320 ms` | `1.97x` |

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

## cyclic graph benchmark (weak references)

`bench/cyclic_graph.pith` and `bench/cyclic_graph_strong.pith` build many
parent<->child rings and drop them. each ring has a strong forward edge
(`next`) and a back edge (`parent`). the two programs differ in one word:
the back edge is `weak` in one and a plain optional in the other.

a `weak` back edge holds the parent without owning it, so the ring closes
no strong cycle and every ring reclaims as the loop moves on. a strong
back edge closes a real cycle — `next` owns forward, `parent` owns back —
so neither node's refcount reaches zero and every ring leaks. the two
binaries print the same checksum; only their memory behavior differs.

run them:

```
pith build bench/cyclic_graph.pith
pith build bench/cyclic_graph_strong.pith
./bench/cyclic_graph 2000000
./bench/cyclic_graph_strong 2000000
```

peak resident memory on this machine, two million rings:

| back edge | peak RSS | structs freed |
|---|---:|---|
| `weak` | ~2 MB | all (flat) |
| strong | ~730 MB | none (leaks every ring) |

the `weak` run holds flat because the rings free as fast as they are
built; the strong run grows without bound. `PITH_PERF_STATS=1` prints the
underlying struct alloc/free counts — balanced for the weak variant,
alloc-heavy with almost no frees for the strong one.

## channel fan-out benchmark (concurrency)

`bench/chan_fanout.*` is the concurrency counterpart to the batch
benchmarks here. four producer tasks push messages into one bounded
channel (capacity 256) and four consumer tasks drain it. the work per
message is two lcg rounds, kept small on purpose so the handoff
dominates the arithmetic — the handoff is the thing being measured.

each consumer folds its messages into a partial sum modulo a prime and
the partials are added at the end, so the total does not depend on which
consumer saw which message. all four implementations print the same
`checksum=90815792` at one million messages — that equality is the
honesty check, the same one `event_ledger` uses. each also reports how
many messages it sent and received, and its peak rss read from
`/proc/self/status`.

each language does this its own way: pith `spawn` and `Channel[Int]`, go
goroutines and a buffered channel, rust std threads and
`mpsc::sync_channel`, zig `std.Thread` over a hand-written mutex/condvar
ring buffer (zig's std has no channel). rust's mpsc is single-consumer,
so its receiver sits behind an `Arc<Mutex<..>>` — the std-only way to
fan one channel out to several consumers, and part of what its number
includes.

run it:

```
bench/chan_fanout_bench.sh 1000000 9   # messages, trials
```

which builds all four, checks that every run of every implementation
agrees on the checksum, and prints the medians. or build and run one
directly:

```
pith build bench/chan_fanout.pith && ./bench/chan_fanout 1000000
PITH_GREEN=1 ./bench/chan_fanout 1000000
go build -o bench/chan_fanout_go bench/chan_fanout.go
rustc -O -o bench/chan_fanout_rust bench/chan_fanout.rs
zig build-exe -O ReleaseFast -femit-bin=bench/chan_fanout_zig bench/chan_fanout.zig
```

one million messages, median of 9 trials on this 2-core machine, run
with nothing else on it. eight tasks on two cores is oversubscribed on
purpose, and equally so for all four (measured 2026-07-26, after the
green wake-path work described below):

| lang | ms | messages/sec | peak rss |
|---|---:|---:|---:|
| pith (os threads) | 438 | 2.3 m | 3.0 mb |
| pith (`PITH_GREEN=1`) | 171 | 5.8 m | 3.0 mb |
| go | 75 | 13.3 m | 2.0 mb |
| rust | 135 | 7.4 m | 2.4 mb |
| zig | 135 | 7.4 m | 2.7 mb |

read the rows that oversubscribe os threads (pith os-thread, rust, zig)
with the box in mind: eight threads on two cores, so they swing run to run.
across two suite runs a week apart rust moved 135 -> 94 ms and zig 135 -> 204
with no code change on either side, and pith's os-thread row has been seen
anywhere from ~260 to ~940. the green and go rows are the stable ones and are
what the comparison rests on.

for most of this benchmark's life pith lost it outright — 580ms os-thread
and 782ms green against go's ~70, roughly 8x behind. two fixes on
2026-07-26 changed that. the first was found with `perf`: rust's standard
condvar is futex-based and pays a `futex(FUTEX_WAKE)` syscall on every
notify *even when nobody is waiting*, and the channel notified its
condvar on every wake. green waiters never condvar-wait (they suspend
their coroutine instead), so on an all-green channel that was two
pointless syscalls per message — about 70% of the run. the channel now
counts its os-thread waiters per role, under the same lock the parker and
waker already hold, and only signals when one is actually parked. the
second moved each task's scheduling state (run state, wake flags, owner)
into one atomic word in a chunked side arena, so a wake and a resume no
longer touch the scheduler's slab lock at all.

with those in, the green backend finally does what it is for: 171ms is
2.6x faster than pith's own os threads and ahead of rust and zig, at
2.3x go. the remaining gap to go is placement — eight tasks that all
block on one channel land on both workers, and a cross-worker handoff
still wakes the peer. `PITH_GREEN_WORKERS=1` pins the pipeline to one
worker and gives ~46ms, faster than go on this box. locality is the
whole story of the difference, and the knob is green-only, so it is not
in the table. the green median above mixes both
placement modes (~60ms when the pinning falls same-worker, ~130-170 when
it splits), which is also why green is the noisier row.

the os-thread improvement (580→438) is older and separate: the channel
core moved to a lock-free mpmc ring earlier the same day, after two
prior rounds — splitting the condvar by role (~19%) and one failed
attempt (dropping the handle-registry lock measured 2x worse by
concentrating futex contention). os threads still condvar-wait, so the
notify-skip buys them little; the ring is what moved their number.

context switches over the same run (`perf stat -e context-switches`):

| | pith | pith green | go | rust | zig |
|---|---:|---:|---:|---:|---:|
| context switches | 5.0k | 2.5k | 258 | 4.8k | 60k |

green used to take 30.9k switches here; it now takes fewer than rust's
std threads, which is the userspace-handoff behavior it was built for.
go's 258 remains the mark: its scheduler almost never touches the
kernel on this shape.

memory is the one column where pith was always fine. everything holds
flat: 4x the messages moves peak rss by under 100 kb in every
implementation, so nothing is retained per message on any of them.
pith's 3.0 mb against go's 2.0 mb is runtime baseline, not growth.

zig's timing is by far the noisiest of the four — probably the plain
`signal` on a shared condvar, which wakes whichever consumer the kernel
feels like. the median is stable enough to compare, but read any single
zig run with suspicion.

## std pipeline benchmark

`bench/std_pipeline.*` is a batteries-included data pipeline benchmark. it
generates deterministic records, writes and reads csv, transforms rows with url
and path helpers, writes a json report, gzip round-trips the report, hashes the
result, and touches the temp workspace through fs traversal.

running it:

```
./self-host/pith_main build bench/std_pipeline.pith
go build -o bench/std_pipeline_go bench/std_pipeline.go
cargo build --release --manifest-path bench/std_pipeline_rust/Cargo.toml
go run bench/std_pipeline_bench.go 50000 5
```

these used to pin `GOCACHE=/tmp/pith-go-cache`. don't: `/tmp` is a tmpfs
on the machine these numbers come from, so that puts a build cache in ram
and takes it away from the thing being measured — enough of it, and the
oom killer starts taking builds out mid-run. go's default cache is on
disk, which is what you want.

latest measured results on this machine, using the median of 5 trials:

| records | go total | rust total | pith total | pith/go | pith/rust |
|---|---:|---:|---:|---:|---:|
| `50000` | `324 ms` | `136 ms` | `482 ms` | `1.49x` | `3.54x` |

phase breakdown from the same run:

| phase | go | rust | pith |
|---|---:|---:|---:|
| config | `0 ms` | `0 ms` | `0 ms` |
| csv write | `191 ms` | `60 ms` | `257 ms` |
| csv read | `87 ms` | `47 ms` | `5 ms` |
| transform | `48 ms` | `27 ms` | `218 ms` |
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
decode) took it to about `634 ms`. finally, rewriting the url and path
scanners to compare raw bytes — instead of minting a one-character string
per position — cut the `transform` phase from `347 ms` to `218 ms` and the
total to about `482 ms`, dropping the run's cstring allocations from 7.1m to
2.9m. what's left is mostly csv write overhead.

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
