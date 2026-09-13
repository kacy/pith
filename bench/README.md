# Pith benchmarks

A few Pith vs Go (and sometimes Rust and Zig) benchmarks, measured on the
same machine. Compile times give a sense of the toolchain; the workload
and pipeline benchmarks below isolate runtime and service-logic costs.

## reading these numbers

The cross-language runners interleave: one round runs every language once,
and a round repeats for the trial count. They used to measure each language
to completion in turn, which on a small shared box hands whoever goes first
a systematic advantage, because ambient load drifts over the length of a run
and pools on whoever goes last. It was worth roughly 2x — in one pair of
runs the same Go binary read 474 ms and then 1005 ms purely from its position
in the order.

So, two habits:

- **Discard the first run after a build.** It competes with whatever the
  build left behind, and it is reliably the worst one.
- **Read the non-Pith columns first.** They are fixed programs. When they
  reproduce their published figures the run is trustworthy; when they do
  not, nothing in that run is, and the right move is to rerun rather than
  to believe the Pith column.

Absolute times are only comparable within a run. Memory columns are far more
stable than time columns and carry most of the signal here.

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
`serde_json`, `sha2`, and `hmac` — the real cost of a deliberately
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

Latest measured results on this machine, 200000 events, median of 5
interleaved trials (2026-09-13, after the map work in #1126, #1128 and
#1129; the 2026-09-10 pith total was 259, the 2026-08-16 total 351, the
2026-07-29 total 618):

| lang | gen | parse | analyze | sign | total |
|---|---:|---:|---:|---:|---:|
| pith | 112 | 102 | 23 | 0 | 238 |
| go | 51 | 326 | 15 | 0 | 393 |
| rust | 26 | 57 | 23 | 0 | 106 |
| zig | 18 | 97 | 9 | 0 | 125 |

the `analyze` phase is the one that moved, 56 to 23 ms: it is the map and
set rollup, and a string-keyed update that used to copy the key three
times and box the value now allocates nothing at all. pith's `analyze` is
now level with rust's and its total is 0.61x go's.

(ms; `gen` builds the stream, `parse` decodes it into structs, `analyze`
runs the map/set rollup, `sign` is the HMAC.)

Read this plainly. Rust and Zig are fastest; Pith now leads Go on the
total, and the two changes that got it there are both instructive. The
2026-08-16 `gen` drop (299 → 120, now even with Go) came from writing
the stream into one ByteBuffer instead of concatenating fresh strings
per line — the same builder shape the Go version always used with
`strings.Builder`, so the comparison is idiom for idiom. The `parse`
improvement (195 → 175) came for free from fixing the decode lowering's
per-call leaks: a flat struct of required scalars decodes in a single
pass, straight into the struct, and it no longer strands its input
buffer, so the allocator stays out of fresh pages. That single-pass
decode is what keeps Pith's `parse` the second-fastest of the four,
ahead of Go's reflection-based decode. It was not always so: the first
cut of this benchmark had Pith at 1084ms on parse and 1472ms total.

What's left of the gap to Rust and Zig is spread across all three busy
phases rather than concentrated in one.
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
assembly. it spawns a task per accepted connection, the same serving model as
the Go counterpart, `bench/http_server.go`; `bench/http_server_mt.pith` is the
explicitly threaded variant. it did not always: until 2026-08-23
the accept loop served each connection inline, which serialized the whole
benchmark on one connection — throughput collapsed to the reciprocal of
per-request latency with a core idle, and the pith-vs-go comparison measured a
serial server against a concurrent one. any pith throughput recorded before
that date carries the flaw.

`bench/http_seq_latency.py` is the other half of the http story, and on a
small shared box the more trustworthy half: one keepalive connection, one
request at a time, p50/p90/p99 per round trip. wrk throughput on such a box is
launch-cadence noise — repeated runs read progressively lower as the host's
cpu burst decays — while this probe is deterministic to a few microseconds.
when a throughput number looks like a regression, run this first: if p50 did
not move, the code did not slow down.

```
bench/http_seq_latency.py 8080 3000
```

`bench/http_bench.sh` drives a server with `wrk` and samples RSS across the
run, so it doubles as a memory-growth check:

```
pith build bench/http_server.pith
go build -o bench/http_server_go bench/http_server.go

bench/http_bench.sh ./bench/http_server 8080 120
bench/http_bench.sh ./bench/http_server_go 8081 120
```

It prints a per-10s RSS table and a summary line (requests, throughput, and
RSS start / end / peak). On 60-second spaced runs (2026-08-22 and 23) the
Pith server held 14.2-14.4k req/s with ~3 bytes/request of RSS growth, which
is flat, while the Go server read 21k one day and 32k the next on the
identical protocol, with ~6 mb of growth that reproduces. On 2026-09-10, after the
request head is read with one bounded `read_until` instead of a byte at a
time (#1097), the same 60-second run reads 17.3k req/s for Pith (1.04m
requests, 2.6 mb of growth, ~2.5 bytes/request) against 27.1k for Go, and
the one-connection probe below holds p50 135 µs. The earlier figures this
section carried (4648 req/s, "zero growth") were both wrong: the server was
serial, and it was leaking ~64 bytes/request until #901, which a 30-second
window could not see.

Take the throughput figures as same-run comparisons only, never across runs.
`wrk` competes with the server for the same two cores, and the Go arm in
particular swings between regimes with the host's state, so the Pith/Go
ratio is whatever regime Go is in that day (0.45x-0.66x across the two runs
above); Pith is the stable arm. For a number that does not move with the
host, use `bench/http_seq_latency.py`. The memory column is trustworthy only
over a long window: run at least 60 seconds before calling an RSS line flat.

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

first measured results, 2026-09-13 at `0429a5e0`, sequential with one
connection at a time, both servers on the same dataset:

| endpoint | go p50 | pith p50 | go p99 | pith p99 | pith/go p50 |
|---|---:|---:|---:|---:|---:|
| `GET /profile` | 509us | 578us | 821us | 1292us | 1.1x |
| `GET /search` hot | 494us | 570us | 972us | 992us | 1.2x |
| `GET /search` wide | 497us | 575us | 1055us | 2423us | 1.2x |
| `POST /batch-score` | 501us | 584us | 749us | 1710us | 1.2x |

no errors on either side. the medians sit within 20% of go across all four
shapes; the tails do not, and the two scan-and-aggregate endpoints are where
they spread (2.3x at p99). this benchmark is sequential by construction, so
it says nothing about throughput under concurrency; the http server section
above is the one that does.

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

latest measured results on this machine, using the median of 5 trials
(2026-09-10, direct 200k-iteration runs interleaved, two warmups):

| iterations | go total | pith total | ratio | rust total |
|---|---:|---:|---:|---:|
| `200000` | `~386 ms` | `~70 ms` | `0.18x` | `~68 ms` |

the 2026-08-08 run read go `~408`, pith `~118`, rust `~70`, and 2026-07-29
go `~436`, pith `~114`, rust `~80`. the comparators held, so the 118 to 70
is pith's own: by instruction count the workload is 30% cheaper than at the
start of 2026-09-10 (1.38 G to 0.96 G Ir), most of it the typed decode's
field lookup (#1104). pith and rust are now within noise of each other on
this workload.

the earlier 1m-iteration medians, for the trend:

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
language, and service-logic costs from HTTP and socket handling.

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
| `weak` | ~11 MB | all (bounded) |
| strong | ~752 MB | none (leaks every ring) |

(2026-09-13, peak rss from the child's rusage. the strong figure reproduces
the ~730 MB this table carried before; the weak figure is larger than the
~2 MB recorded earlier and is worth attributing, but the point of the pair
stands, one is bounded and the other is not.)

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
PITH_GREEN=0 ./bench/chan_fanout 1000000
go build -o bench/chan_fanout_go bench/chan_fanout.go
rustc -O -o bench/chan_fanout_rust bench/chan_fanout.rs
zig build-exe -O ReleaseFast -femit-bin=bench/chan_fanout_zig bench/chan_fanout.zig
```

one million messages, median of 9 trials on this 2-core machine, run
with nothing else on it. eight tasks on two cores is oversubscribed on
purpose, and equally so for all four (measured 2026-09-13; the 2026-07-26
table it replaces read 438 / 171 / 75 / 135 / 135):

| lang | ms | messages/sec | peak rss |
|---|---:|---:|---:|
| pith (`PITH_GREEN=0`, os threads) | 250 | 4.0 m | 3.1 mb |
| pith (green, the linux default) | 68 | 14.7 m | 3.1 mb |
| go | 72 | 13.9 m | 3.7 mb |
| rust | 62 | 16.1 m | 2.4 mb |
| zig | 272 | 3.7 m | 4.5 mb |

the green row has been bimodal on every rerun since july (fast rounds near
60-70 ms, slow rounds near 95-100, see docs/performance.md); all nine
rounds of the 2026-09-10 run landed in the fast mode, so read 65 as the
fast mode's figure, not as the end of the bimodality.

the 2026-07-29 rerun (medians of 7, interleaved) held the same shape with
the usual comparator drift: green ~133 (bimodal, best runs at ~69), green
pinned to one worker ~46, go ~75, rust ~75, zig ~240. the 2026-08-08 rerun
reproduced it almost exactly — green ~133 (best runs at ~61), go ~71, rust
~73, zig ~205 — which is the clearest evidence the harness fix landed
correctly: the same three comparator programs came back to the same figures.

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

## parallel compute with a coordinator (the control for task migration)

`bench/cpu_parallel_sync.pith` is the shape every other concurrency bench
here lacks: eight tasks do genuinely parallel arithmetic in chunks and,
between chunks, report to one collector task and wait for its
acknowledgement, so the work spreads across cores while every chunk still
ends in a real handoff. it exists to measure `PITH_GREEN_MIGRATE=1`, which
moves a woken task to the worker that woke it: on the fan-out and ping-pong
benches that is a pure win, and on this one it pulls the compute tasks onto
the collector's worker and gives part of the 2-worker speedup back. the
checksum must match across arms.

```
pith build bench/cpu_parallel_sync.pith
PITH_GREEN_WORKERS=1 ./bench/cpu_parallel_sync 200 20000
PITH_GREEN_MIGRATE=0 ./bench/cpu_parallel_sync 200 20000
PITH_GREEN_MIGRATE=1 ./bench/cpu_parallel_sync 200 20000
```

on 2026-09-06, medians of 5 interleaved rounds: 448 ms at 1 worker, 259 ms
at 2 workers with the flag off (247-305), 337 ms with it on (230-402), with
`PITH_PERF_STATS=1` showing ~280 migrations a run. that is why the flag was
not made the default.

on 2026-09-13 the ordering had reversed: 404 ms at 1 worker (403-405), 327
with the flag off (228-364), **241 with it on** (223-370), nine rounds
interleaved arm by arm after two warmups, and the flag won 5 of the 7 paired
rounds (the paired comparison is within a round, so it survives a drifting
box in a way the medians alone do not). the arm that used to cost a third of
the two-worker speedup is now the fastest of the three. the 2026-09-06 run
predates preemption safe points becoming the default (#1088), which changes
when a task yields and so how placement settles, but that is a hypothesis
and not an attribution. tracked in #1131; until it is settled, the paragraph
in `docs/performance.md` explaining why the flag is off rests on the old
numbers.

## task churn benchmark (per-thread pools)

`bench/task_churn.pith` runs one short task after another, each allocating
a batch of structs into a list and releasing them at return. It is the
shape the per-thread struct pool loses on: under `PITH_GREEN=0` every task
is a fresh thread, so the pool is built, filled once, and torn down without
a block being reused. Under green the worker outlives the task and the pool
pays for itself. Run both arms and compare; the checksum must match.

```
pith build bench/task_churn.pith
PITH_GREEN=0 ./bench/task_churn
PITH_GREEN=0 PITH_STRUCT_FREELIST=0 ./bench/task_churn
```

This shape used to lose: the pool-on arm was about 35% slower by wall time
and 25% more instructions under callgrind, because the thread-exit destructor
deallocated every retained block in a burst. Pool slots are now released at
exit and adopted by the next thread, and the same arm is about 48% fewer
instructions and 19% faster than no pool. The instruction count is the number
to trust; see "measuring" below.

## closure calls benchmark (dispatch cost)

`bench/closure_calls.pith` splits a closure's cost into three phases so a
change to one is visible on its own: two million closures each built,
called once, and dropped (`build_ms`); one closure called two million times
(`call_ms`); and a plain function called two million times as the floor
(`direct_ms`). All three fold into one checksum. Typical split on the
two-core box: build 111 ms, call 14 ms, direct 4 ms — so a closure call is
about 7 ns against a 2 ns direct call, and construction dominates.

```
pith build bench/closure_calls.pith && ./bench/closure_calls
```

## tight loop (the preemption safe-point's worst case)

`bench/tight_loop.pith` sums `i * 3` over 200 million iterations and prints
a checksum. Nothing else: no allocation, no call, no channel. It exists to
price the green preemption safe-point, which the backend puts before every
loop back-edge, because this is the body the check is largest against — six
instructions of safe-point on a seven-instruction body.

Two things about how it is written, both of which move the number several
fold. There is no division: an `idiv` is slow enough to hide the check behind
its latency, and the same loop with one reads +0.4%. And the loop is a
function of its own rather than the body of `main`, so that whatever main does
around it cannot decide which registers the loop gets — inline beside a string
format it reads +33% instead.

```
PITH_GREEN_PREEMPT=0 pith build bench/tight_loop.pith && ./bench/tight_loop
PITH_GREEN_PREEMPT=1 pith build bench/tight_loop.pith && ./bench/tight_loop
```

On the two-core box, medians of 9 interleaved rounds: 129 ms without
safe-points and 259 ms with them, against a ±3% null floor, and 1.400G
against 2.600G instructions under callgrind. The clock moves further than the
instruction count because the check contains a call, so the loop's values move
into callee-saved registers and the function grows a frame. Every other
program measured here pays between +0.07% and +1.85% instructions; the table
is in `docs/performance.md`.

re-measured 2026-09-13 (9 rounds interleaved, 2 warmups, canary checked
either side): 132 ms with safe points off (130-140), 265 with them on
(262-278), +100.5%. that reproduces the +101% this section has carried since
2026-09-07, on a binary two dozen changes newer.

## substring search (the runtime's `contains` and `index_of`)

`bench/substring_search.pith` calls `contains` and `index_of` over haystacks
of 12, 80 and 4000 bytes with the needle at the start, in the middle, at the
end, or absent, and folds every answer into one checksum. Three needles run
over each cell: `comma`, one byte with no other occurrence in the filler,
which is the csv and path shape; `word`, seven bytes whose first byte is
common in the filler; and `repeat`, a haystack of one repeated byte against a
needle that differs from it only in its last byte, the adversarial case for a
search that jumps between occurrences of the first byte. It exists because
the runtime used to compare the needle at every position of the haystack, and
that made `pith_cstring_contains` a tenth of the std pipeline (#1099).

```
pith build bench/substring_search.pith
./bench/substring_search                  # every cell, 2000 rounds
./bench/substring_search 500 long-absent  # one cell
./bench/substring_search 2000 all comma   # one needle over every cell
```

The per-cell instruction counts of the old search, the `memchr` search that
replaced it, and the libc `memmem` alternative it was measured against are in
`docs/performance.md`.

## measuring: instruction counts over wall time

Wall time on a shared two-core box drifts by more than most effects under
study: a compile-time change once read +5.6% eight rounds running, and
callgrind put it at +0.06%. Two scripts make the reliable measurement the
easy one.

`tooling/callgrind_ab.sh` runs two arms under `valgrind --tool=callgrind`
and prints their instruction totals, the delta, and (with `--annotate N`)
the hottest functions of each. Attribution needs a symbol table, which the
linker strips by default; build the program under test with
`PITH_KEEP_SYMBOLS=1` first.

```
PITH_KEEP_SYMBOLS=1 pith build bench/task_churn.pith
tooling/callgrind_ab.sh --annotate 10 \
  pool-on  'PITH_GREEN=0 ./bench/task_churn 100' \
  pool-off 'PITH_GREEN=0 PITH_STRUCT_FREELIST=0 ./bench/task_churn 100'
```

`tooling/null_ab.sh` copies one binary to two paths and times them against
each other with the same interleaved, order-rotated, spaced protocol a real
A/B uses. The two arms are identical, so the spread it reports is the box.
Run it before believing a timing delta; a real A/B inside that spread has
shown nothing. The floor here has been about ±3% on the minimum.

```
PITH_GREEN=0 tooling/null_ab.sh ./bench/task_churn 15 -- 2000
```

`tooling/ir_hash.sh` hashes the emitted IR of every corpus source into one
file. A compiler change that should be behaviour-neutral reproduces every
hash; one that should touch a single shape changes exactly the files that
have it. Capture before and after, then `diff`.

## generic sort benchmark (std.collections and std.algo)

`bench/generic_sort.pith` measures the standard library's key-based sorts
against the selection and insertion sorts they replaced. Both arms live in the
one program: `legacy` is a verbatim copy of the old bodies, `current` calls the
shipped functions, so a single build measures both and nothing but the sort
differs. Inputs come from a seeded 31-bit LCG and are built before the clock
starts; the timed section covers what a caller pays for one call, which is the
copy, the key extraction and the sort. Each arm folds its result into a
checksum, and the runner fails if the two arms disagree.

```
pith build bench/generic_sort.pith
bench/generic_sort_bench.sh
TRIALS=9 SIZES="100 1000" bench/generic_sort_bench.sh
```

The sweep runs sizes 100, 1000 and 10000 over four distributions (sorted,
reverse, random, many duplicates) and two key kinds (an integer field and a
zero-padded string), interleaving the arms within each round and reporting the
median with the observed spread.

Two things are worth knowing before reading the table. The old
`collections.sort_by_key` called the key function once per element per pass, so
its cost is dominated by key calls rather than by comparisons, and the speedup
there grows with n rather than settling. And the old `algo.sort_by_key` was an
insertion sort that inserted before the first strictly greater element, which
makes a descending input its best case: it probes exactly one element per
insertion. That is the one cell the merge sort loses, at 100 and 1000 elements,
before the insertion cost turns it around again at 10000.

Wall clock on the two-core box moves around; instruction counts through
`tooling/callgrind_ab.sh` are the number to quote for a per-operation claim.
Running an arm with a round count of 0 gives the input generation on its own,
which is identical in both arms and can be subtracted out.

```
tooling/callgrind_ab.sh \
  legacy  './bench/generic_sort legacy random int 1000 20' \
  current './bench/generic_sort current random int 1000 20'
```

## string buffer benchmark (std.io)

`bench/string_buffer.pith` measures `std.io`'s `StringBuffer` against the
chunk-list storage it replaced. Both arms live in the one program: `legacy` is
a verbatim copy of the old bodies (a `List[String]` per buffer, joined back
into one chunk every 32 writes), `current` calls the shipped buffer, so a
single build measures both and nothing but the storage differs. The chunks
come from a seeded 31-bit LCG and are built before the clock starts.

```
pith build bench/string_buffer.pith
bench/string_buffer_bench.sh
SIZES="1000 10000" CHUNKS=8 bench/string_buffer_bench.sh
WORKLOADS=snapshot EVERY=50 bench/string_buffer_bench.sh
```

Two workloads. `build` appends n chunks, takes `string()` once and closes the
buffer, and hashes the whole result; that is the shape a builder or an encoder
has, and the one the storage change is about. `snapshot` takes `string()`
every k appends (default 100) as well as at the end; every snapshot is a
fresh copy of the whole prefix in either arm, so its total is not linear in n
for either arm, and the table reports it as what it is rather than as a
buffer cost. Each run prints its peak resident set alongside the time.

The old storage copied the whole prefix at every compaction, about
s·n²/62 bytes over n appends of s bytes; instruction counts for one arm at
1,000, 10,000 and 100,000 appends show whether that term is present. With a
round count of 0 the program does the input generation alone, which is
identical in both arms and can be subtracted out.

```
tooling/callgrind_ab.sh \
  legacy  './bench/string_buffer legacy build 10000 8 1' \
  current './bench/string_buffer current build 10000 8 1'
```

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

latest measured results on this machine, using the median of 5 trials
(2026-09-10, direct runs interleaved, two warmups):

| records | go total | rust total | pith total | pith/go | pith/rust |
|---|---:|---:|---:|---:|---:|
| `50000` | `~242 ms` | `~137 ms` | `~312 ms` | `1.29x` | `2.28x` |

(2026-09-13; pith read `~344 ms` on 2026-09-11.) pith read `~480 ms` (1.50x go) on 2026-08-08 and `~576 ms` (1.63x) on
2026-07-29; the comparators reproduce their 2026-08-22 figures (go 249,
rust 134). by instruction count the pipeline is 20% cheaper than at the
start of 2026-09-10 (4.28 G to 3.41 G Ir): the csv quoting decision became
one byte pass and `path.clean_part_count` two counters (#1097), and the
runtime's substring search runs on `memmem` (#1103). the 2026-09-10 pith
phases, run directly: csv write 171, csv read 2, transform 170, gzip + hash
1, total 344. `transform` is now the whole gap to go (38 ms there). this is
the noisiest suite on this box; the medians are taken from rounds where the
comparators held their shape.

phase breakdown from the 2026-07-21 run, whose shape still holds for go and
rust:

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
- the pith version keeps the config setup local, so the benchmark times the
  csv/url/path/gzip/hash/fs pipeline rather than config parsing.

## map update (the event ledger's rollup in isolation)

`bench/map_update.pith` is the shape the event ledger's analyze phase is
made of: `if m.contains_key(k): m.insert(k, m[k] + d)` over a small set of
distinct keys, so nearly every update is a hit on a key that is already
there. it exists because that shape used to cost four allocations per
update, three key copies and a value box, none of which the program asked
for.

```
pith build bench/map_update.pith
./bench/map_update string 20000 64    # flavor, updates, distinct keys
./bench/map_update bytes 20000 64
./bench/map_update int 20000 64
```

2026-09-13, 20,000 updates over 64 distinct keys, instruction counts from
`tooling/callgrind_ab.sh`, allocation counts from valgrind:

| key flavor | Ir before | Ir after | allocations per update |
|---|---:|---:|---:|
| string | 28,373,869 | 11,761,225 | 4.00 to 0 |
| bytes | 26,275,158 | 9,874,948 | 4.00 to 0 |
| int | 6,148,579 | 4,505,198 | 0 to 0 |

"before" is `19aceac7`, "after" `0429a5e0`: borrowed key probes (#1126),
word-sized values stored in the table (#1128), and the read-add-write fused
into one probe (#1129). the allocation count no longer scales with the
number of updates at all: at 64 distinct keys the whole run allocates 523
times whether it performs 20,000 updates or 80,000.

## std hotspot micro-benchmarks (2026-09-10)

three small programs isolate the std functions a callgrind profile of the
workloads above put at the top (docs/performance.md, "std profiling pass"),
so a change to one of them can be measured on its own before it is read off
the whole workload:

```
pith build bench/http_head_read.pith && ./bench/http_head_read 20000
pith build bench/csv_encode.pith && ./bench/csv_encode 5000 10
pith build bench/path_clean_count.pith && ./bench/path_clean_count 20000
```

`http_head_read` feeds a stream of keepalive requests through
`std.net.http`'s request reader from a bytes cursor, with no socket;
`csv_encode` encodes std_pipeline's ten-column rows; `path_clean_count`
counts a fixed set of messy paths. each prints a checksum, and the
instruction-count comparison is `tooling/callgrind_ab.sh` over a binary
built before the change and one built after it, by the same compiler.

a fourth isolates the runtime's typed json decode, the largest runtime
row of that profile:

```
pith build bench/json_decode_shapes.pith
./bench/json_decode_shapes small 4 20000    # three-field struct, 4-byte strings
./bench/json_decode_shapes wide 32 20000    # twenty-field struct, 32-byte strings
./bench/json_decode_shapes nested 4 2000    # a struct with a nested struct (nested fill)
./bench/json_decode_shapes list 32 400      # an array of 32 small objects (node pool)
```

`json_decode_shapes` decodes one struct shape per run; `size` is the
string value width for the flat shapes and the element count for the
list. the flat shapes take the runtime's single-pass fill and the
nested shape its nested twin, which fills the sub-struct in place; the
list shape parses into the node pool and decodes each element out of
it, so it answers a different question.

the night's totals for these programs, each compiled by the compiler at the
start of 2026-09-10 (`0fe497c0`) and by the tip (`099e7e62`, after #1107)
from the same source, run under callgrind, outputs and checksums identical:

| 2026-09-10, callgrind | start of the night | tip | delta |
|---|---:|---:|---:|
| http_head_read, 20000 requests | 8,803,504,445 | 2,099,721,126 | −76.2% |
| csv_encode, 5000 rows × 10 | 1,437,606,575 | 847,752,217 | −41.0% |
| path_clean_count, 20000 rounds | 1,942,443,996 | 556,783,020 | −71.3% |
| substring_search, 36 cells × 2000 | 4,700,683,879 | 855,542,295 | −81.8% |
| json_decode_shapes small 4 × 20000 | 41,769,731 | 32,787,957 | −21.5% |
| json_decode_shapes wide 32 × 20000 | 560,059,104 | 201,137,570 | −64.1% |
| json_decode_shapes nested 4 × 2000 | 1,059,907,351 | 8,408,038 | −99.2% |
| json_decode_shapes list 32 × 400 | 4,380,580,768 | 4,383,020,015 | +0.06% |

the list shape did not move because a list of structs still goes through
the node pool by hand (#1110).

## hash kernels (std.hash and std.checksum, per byte)

`bench/hash_kernels.pith` runs one kernel over a few megabytes of fixed
xorshift content, so a change to a kernel can be measured on its own:

```
pith build bench/hash_kernels.pith
./bench/hash_kernels sha256 4       # one of sha1 sha224 sha256 sha384 sha512 fnv1a crc32 adler32
./bench/hash_kernels none 4         # generation only
```

run the kernel arm and the `none` arm under callgrind; the difference
divided by the byte count is the kernel's cost per byte. the digest line
must agree between a binary built before a change and one built after it.

instructions per byte over 4 MB, hash arm minus the `none` arm, digests
identical across the two columns. the first column is `e80078d0`, after
#1115 made fnv1a and crc32 word- and table-driven; the second is after the
sha kernels stopped copying the input into a list and adler32 stopped
reducing per byte (#1116):

| kernel | before | after |
|---|---:|---:|
| `hash.sha1` | 762.6 | 328.7 |
| `hash.sha224` / `hash.sha256` | 1164.0 | 353.3 |
| `hash.sha384` / `hash.sha512` | 796.5 | 201.5 |
| `hash.fnv1a` | 9.6 | 9.6 |
| `checksum.crc32` | 60.0 | 60.0 |
| `checksum.adler32` | 47.0 | 11.0 |

what remained in the sha kernels after that was mostly the strict `w[i]`
and round-constant `List[Int]` reads, about 140 of the 353 in sha-256,
which were runtime calls. the first section of #1116 gave `xs[i]` and
`bytes[i]` an inline fast path in the ir consumer (the runtime call stays
as the slow path, so a bad index aborts as before); on the same kernels,
same std, old backend against new: `hash.sha1` 328.7 → 262.0,
`hash.sha256` 353.3 → 281.6, `checksum.crc32` 60.0 → 47.0 (its table read
is a list subscript), `checksum.adler32` 11.0 → 11.0 (it reads through
`bytes_read_word` and `bytes_get`, both already inline). digests identical.

## zstd codec benchmark (pure-pith encoder and decoder)

`bench/zstd_codec.pith` times the pure-pith zstd codec
(`std.compress.zstd_pure_*`) against the crate-backed kernel
(`std.compress.zstd`), in both directions.

Decode: both read the same frames and the outputs are compared byte for
byte before either is timed, so the numbers can't come from doing
different work. Encode: the pure encoder's frame is handed to the
*kernel's* decoder and checked against the input before timing — interop,
not a round trip through our own code — and the reported size is a
percentage of the kernel's own output, so the throughput number can't be
bought by compressing worse.

The corpus is built at runtime from the repo itself — docs, std sources,
and generated log lines, compressing between 2:1 and 11:1 — plus one
deliberately degenerate run-heavy case, reported last and labelled. A
frame that packs 80KB into 22 bytes decodes almost entirely through the
match copy, so it measures memory bandwidth rather than entropy decoding;
treating it as the headline would flatter the pure decoder by a factor
of five.

Each side runs to a wall-clock budget rather than a fixed rep count,
because the kernel decodes some of these frames in tens of microseconds
and a fixed count lands inside the millisecond clock's granularity.

Run it:

```
make zstd-pure-bench
```

Current standing and the optimization history live in
`docs/performance.md`. The decoder has been through three optimization
passes and the encoder one, so the encode column still has the more
obvious headroom — the match finder is now its largest cost.
