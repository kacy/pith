# Pith benchmarks

A few Pith vs Go (and sometimes Rust and Zig) benchmarks, measured on the
same machine. Compile times give a sense of the toolchain. The workload
and pipeline benchmarks below isolate runtime and service-logic costs.

## Reading these numbers

The cross-language runners interleave: one round runs every language once,
and a round repeats for the trial count. They used to measure each language
to completion in turn, which on a small shared box hands whoever goes first
a systematic advantage, because ambient load drifts over the length of a run
and pools on whoever goes last. The effect was roughly 2x. In one pair of
runs, the same Go binary read 474 ms and then 1005 ms purely from its
position in the order.

Two habits follow from this:

- **Discard the first run after a build.** It competes with whatever the
  build left behind, and it is reliably the worst one.
- **Read the non-Pith columns first.** They are fixed programs. When they
  reproduce their published figures, the run is trustworthy. When they do
  not, nothing in that run is, and you should rerun it instead of believing
  the Pith column.

Absolute times are only comparable within a run. Memory columns are far more
stable than time columns and carry most of the signal here.

## The full suite against Go and Rust (2026-11-28)

### At a glance

Each row groups the benchmarks of one kind and gives the range of Pith's
result against Go and against Rust, as wall time. The tables further down
have every benchmark, with arguments and caveats.

| area | benchmarks | against go | against rust |
|---|---|---|---|
| substring search | `substring_search` | 2.3-5.8x faster | 1.8-5.8x faster |
| JSON decoding | `json_decode_shapes` (6 shapes) | 1.5-5.7x faster | 2.7x faster to 1.16x slower |
| walking decoded JSON | `json_decode_shapes/list_walk` | 2.9x slower | 5.6x slower |
| service logic | `event_ledger`, `catalog_workload`, `std_pipeline` | 1.5x faster to even | 1.26-1.52x slower |
| sorting | `generic_sort` (int, string) | 1.7x faster to even | 2.1-5.0x slower |
| maps | `map_update` (int, bytes, string keys) | 1.19-1.9x faster | 2.0x faster to 1.4x slower |
| strings and text | `string_build`, `string_buffer`, `csv_encode`, `path_clean_count` | 3.0x faster to 1.5x slower | 1.07-2.9x slower |
| HTTP head parsing | `http_head_read` | 1.18x slower | 5.5x slower |
| closures | `closure_calls`, `closure_error` | 1.14-1.5x slower | 1.7-3.2x slower |
| reference cycles | `cyclic_graph`, `cyclic_graph_strong` | 2.1-5.9x slower | 1.8-2.5x slower |
| hashing | sha256, sha512, fnv1a, crc32 | 1.14-1.67x slower | 1.15-2.8x slower |
| channels, spawning, parallel work | `chan_fanout`, `lock_sweep/spawn`, `cpu_parallel_sync` | 1.16-2.0x faster | 1.11-11.6x faster |
| task scheduling | `task_churn`, `task_pingpong`, `green_fanout` | 1.25-2.7x slower | 2.4x faster to 1.65x slower |
| locks and metrics | `lock_sweep`, `metrics_contention` | 1.6x faster to 1.22x slower | 2.6x faster to 1.37x slower |
| HTTP server throughput | `http_sweep.sh`, 32-1024 connections | 1.65-2.4x faster than net/http, 8-17% behind a minimal server | 1.11-1.26x ahead of hyper up to 256 connections, 13% behind at 1024 |
| HTTP server p99 latency | same | lowest at every connection count | lowest at every connection count |
| gRPC unary calls | `bench/grpc/run.sh`, pinned | even at concurrency 1, 1.12-1.24x slower at 8 | 1.07-1.29x faster |
| catalog service p50 | `catalog_server_rust` | 1.07-1.16x slower | 1.02x faster to 1.04x slower |
| zstd, MB/s | `zstd` | decode 5.5-7.2x slower, encode 4.2-8.1x slower | decode 1.4-1.6x slower, encode 2.1-3.0x slower |
| API gateway | `gateway_bench.sh`, 64-1024 connections, forwarding with no token | 1.09-1.5x faster, with 2.5-4.9x lower p99 and 58-70% less memory | no port |

Every row comes from the same build, main at `fb38baae` (after #1511's native
sha512 block kernel, the #1512-#1522 hardening fixes, and the #1523-#1525
runtime, codegen, and IR refactors). The server, gRPC, gateway, catalog
service and zstd rows come from their own runners, described in their
sections below.

Nearly every benchmark in this directory now has a Go and a Rust counterpart.
Four do not. In `tls_head_read` the TLS stack costs more than the head reader it
isolates. `web_hello`, `http_server_mt` and `tls_server` are variants of the
HTTP server, which the net/http and hyper arms below cover. Each port
runs the same algorithm on the same data with the same arguments and prints the
same checksum, so equal output shows the arms did the same work. The ports are
written the way a Go or Rust developer would write them. They use the standard
library, plus a crate where Rust's standard library has no equivalent (serde_json,
tokio, hyper, RustCrypto, crc32fast, ruzstd), or klauspost/compress for zstd in Go.

The single-threaded rows also have C and C++ ports (`bench/<name>.c` and
`.cpp`), built with `-O2` and nothing beyond the standard libraries. They
show how much room is left. `tight_loop`, `path_clean_count` and the
`none`, `fnv1a` and `crc32` hash rows have only a C port, since the C++ code
would be the same. `string_buffer` has only a C++ port, and only its `current`
arm. The sha rows, the task and channel rows and the servers have none: they
would need a library or a hand-written scheduler. Three caveats apply when
reading the numbers. GCC and Clang remove the `malloc` and `free` pair in
`cyclic_graph.c` entirely, so its C++ port with `shared_ptr` is the fair row.
`hash_kernels.c` computes CRC32 with portable slicing-by-8 tables, without the
carry-less multiply `crc32fast` uses. `generic_sort.c` uses `qsort`, which calls
its comparator through a pointer.

`bench/compare.sh` builds every arm, checks its output against Pith's, and runs
interleaved rounds. The benchmark list, with arguments and build commands, is
`bench/compare.list`:

```
bench/compare.sh                    # everything, 5 rounds
TRIALS=3 bench/compare.sh map_update json_decode_shapes
```

Setup: Ryzen 9 9950X, every run pinned to one CCD (CPUs 0-7,16-23), 16 workers
(`PITH_GREEN_WORKERS`, `GOMAXPROCS`, tokio worker threads), Go 1.27.3,
Rust 1.99.1, Pith at main `fb38baae` (Cranelift 0.130). Times are the median
wall time of 5 interleaved rounds, process start included.

### Compute and std

| benchmark | args | pith ms | go ms | rust ms | pith/go | pith/rust |
|---|---|---:|---:|---:|---:|---:|
| event_ledger | `200000` | 64 | 97 | 51 | 0.67x | 1.26x |
| std_pipeline | `50000` | 77 | 78 | 56 | 1.00x | 1.37x |
| catalog_workload | `4000` | 3.5 | 3.5 | 2.3 | 1.00x | 1.52x |
| string_build | `2000000` | 32 | 33 | 21 | 0.96x | 1.50x |
| closure_error | `2000000` | 280 | 182 | 161 | 1.54x | 1.74x |
| tight_loop | `2000000000` | 731 | 353 | 1.8 | 2.07x | (a) |
| closure_calls | `50000000` | 83 | 73 | 26 | 1.14x | 3.16x |
| substring_search | `20000` | 38 | 220 | 221 | 0.17x | 0.17x |
| substring_search/absent | `500 long-absent` | 1.9 | 4.3 | 3.5 | 0.44x | 0.54x |
| string_buffer/build | `current build 100000 200 200` | 565 | 1703 | 262 | 0.33x | 2.15x |
| string_buffer/snapshot | `current snapshot 10000 200 200` | 303 | 197 | 284 | 1.54x | 1.07x |
| cyclic_graph | `10000000` | 262 | 125 | 143 | 2.10x | 1.84x |
| cyclic_graph_strong | `10000000` | 729 | 125 | 289 | 5.85x | 2.52x |
| csv_encode | `5000 400` | 219 | 308 | 129 | 0.71x | 1.70x |
| json_decode_shapes/small | `small 4 20000000` | 671 | 3697 | 1111 | 0.18x | 0.60x |
| json_decode_shapes/wide | `wide 32 3000000` | 561 | 3210 | 1512 | 0.17x | 0.37x |
| json_decode_shapes/nested | `nested 4 8000000` | 898 | 2203 | 791 | 0.41x | 1.14x |
| json_decode_shapes/list | `list 32 400000` | 998 | 2085 | 924 | 0.48x | 1.08x |
| json_decode_shapes/list_walk | `list_walk 32 5000` | 157 | 54 | 28 | 2.93x | 5.61x |
| json_decode_shapes/map | `map 32 300000` | 670 | 1039 | 579 | 0.64x | 1.16x |
| json_decode_shapes/float | `float 4 12000000` | 803 | 3609 | 979 | 0.22x | 0.82x |
| path_clean_count | `2000000` | 523 | 748 | 180 | 0.70x | 2.91x |
| map_update/string | `string 50000000 64` | 442 | 528 | 469 | 0.84x | 0.94x |
| map_update/bytes | `bytes 50000000 64` | 680 | 1228 | 484 | 0.55x | 1.41x |
| map_update/int | `int 100000000 64` | 358 | 673 | 707 | 0.53x | 0.51x |
| generic_sort/int | `current random int 100000 30` | 298 | 501 | 59 | 0.59x | 5.02x |
| generic_sort/string | `current random string 100000 30` | 660 | 650 | 318 | 1.02x | 2.08x |
| http_head_read | `1000000` | 627 | 530 | 114 | 1.18x | 5.49x |

The hash kernels hash 256 MB. The rows below subtract the `none` arm (59 ms
for Pith, 49 for Go, 48 for Rust), which only builds the input. The sha256,
sha512 and crc32 kernels hand whole blocks to a runtime kernel: sha256 and
crc32 use the CPU's SHA and carry-less multiply instructions (#1242), and
sha512 uses a native Rust block kernel (#1511), which took sha512 from 435 ms
(2.30x Go) to 243 ms (1.29x Go).

| kernel | pith ms | go ms | rust ms | pith/go | pith/rust |
|---|---:|---:|---:|---:|---:|
| sha256 | 109 | 96 | 95 | 1.14x | 1.15x |
| sha512 | 243 | 188 | 208 | 1.29x | 1.17x |
| fnv1a | 235 | 188 | 186 | 1.25x | 1.26x |
| crc32 | 13.2 | 7.9 | 4.8 | 1.67x | 2.75x |

The crc32 row takes about 10 ms once the 50 ms input build is subtracted, so
noise in the subtraction moves it: the previous run measured 2.25x Go on the
same kernel code.

### Against C and C++

These are the single-threaded rows that have a C or C++ port, from the same
run. A dash means there is no port in that language. The two hash rows
include building the input.

| benchmark | pith ms | c ms | c++ ms | pith/c | pith/c++ |
|---|---:|---:|---:|---:|---:|
| string_build | 32 | 49 | 18 | 0.65x | 1.80x |
| tight_loop | 731 | 351 | - | 2.08x | - |
| closure_calls | 83 | 39 | 40 | 2.13x | 2.09x |
| substring_search | 38 | 523 | 858 | 0.07x | 0.04x |
| substring_search/absent | 1.9 | 6.6 | 10.5 | 0.29x | 0.18x |
| hash_kernels/fnv1a | 294 | 235 | - | 1.25x | - |
| hash_kernels/crc32 | 72 | 125 | - | 0.58x | - |
| string_buffer/build | 565 | - | 499 | - | 1.13x |
| string_buffer/snapshot | 303 | - | 367 | - | 0.83x |
| cyclic_graph | 262 | 3.4 | 134 | (b) | 1.95x |
| csv_encode | 219 | 114 | 195 | 1.92x | 1.12x |
| path_clean_count | 523 | 213 | - | 2.46x | - |
| map_update/string | 442 | 485 | 374 | 0.91x | 1.18x |
| map_update/bytes | 680 | 482 | 411 | 1.41x | 1.66x |
| map_update/int | 358 | 234 | 247 | 1.53x | 1.45x |
| generic_sort/int | 298 | 264 | 160 | 1.13x | 1.86x |
| generic_sort/string | 660 | 451 | 309 | 1.46x | 2.14x |

(b) GCC and Clang remove the allocation in `cyclic_graph.c`, so the C time is
not an allocation cost. The C++ port with `shared_ptr` is the comparable row.

### Concurrency

| benchmark | args | pith ms | go ms | rust ms | pith/go | pith/rust |
|---|---|---:|---:|---:|---:|---:|
| chan_fanout | `1000000` | 40 | 50 | 466 | 0.81x | 0.09x |
| task_churn | `200000` | 984 | 677 | 2341 | 1.45x | 0.42x |
| task_pingpong | `2000000` | 398 | 317 | 526 | 1.25x | 0.76x |
| green_fanout | `2000000` | 645 | 241 | 392 | 2.68x | 1.65x |
| cpu_parallel_sync | `200 1000000` | 647 | 750 | 716 | 0.86x | 0.90x |
| lock_sweep/log | `log 16 100000000` | 54 | 44 | 3.9 | 1.21x | (a) |
| lock_sweep/spawn | `spawn 16 1600000` | 38 | 78 | 99 | 0.49x | 0.39x |
| lock_sweep/labels | `labels 16 1600000` | 51 | 80 | 55 | 0.63x | 0.93x |
| lock_sweep/breaker | `breaker 16 50000000` | 63 | 61 | 55 | 1.04x | 1.15x |
| lock_sweep/readfile | `readfile 16 200000` | 89 | 100 | 233 | 0.90x | 0.38x |
| metrics_contention/counter | `3 4000000 16 counter` | 470 | 504 | 342 | 0.93x | 1.37x |
| metrics_contention/histogram | `3 4000000 16 histogram` | 547 | 447 | 531 | 1.22x | 1.03x |
| metrics_contention/labels | `3 1000000 16 labels` | 77 | 106 | 133 | 0.73x | 0.58x |

`lock_sweep/breaker` is sixteen tasks contending on one mutex. Its Pith time
moves between runs: earlier runs landed at about 69, 74, 87 or 113 ms, and this
one read a median of 63. `lock_sweep/spawn` lands at about 37 or 48 ms, and
read 38 ms here. `metrics_contention/histogram` held flat for Pith (549 to
547 ms) while Go landed in its faster ~445 ms mode on four of five trials
(against 677 ms in the previous sweep).

(a) LLVM removes the loop, so the Rust time is not a loop cost: `tight_loop`
becomes a closed form, `lock_sweep/log` hoists the level check out of the loop,
and the `call` and `direct` phases of `closure_calls` fold away.

### What is and is not like for like

- Closures: Go's escape analysis keeps the closures in `closure_calls` and
  `closure_error` on the stack, and Rust closures are unboxed. Pith builds a
  closure in a stack slot when it is called by name or passed as a lambda
  literal to a call-only parameter (`closure_stack`). A closure returned
  from a function (such as `make_adder` in `closure_error`) or otherwise
  escaping is still allocated on the heap. `closure_calls` mainly measures
  stack-closure setup and indirect dispatch. `closure_error` also pays the
  heap allocation of returned closures.
- Hashes and checksums: all three languages use SHA-NI for sha256 and
  pclmulqdq for crc32. In Pith, the padding, the tail and a portable block
  loop are Pith code, and only the loop over whole blocks is in the
  runtime (which Pith code cannot write itself for SHA-NI/pclmulqdq because
  Cranelift has neither instruction, and which #1511 also added for sha512).
  In Pith, fnv1a is portable code. Because fnv1a is a serial chain in every
  language, it is the closest pure-language comparison of the four.
- Cycles: Go has a tracing collector, so both cyclic graph files are the
  same Go program. In the strong variant, Pith and Rust leak every ring.
  Pith leaks about 245 bytes per ring and Rust about 125, which is most of
  the time difference (peak RSS 2.44 GB and 1.25 GB).
- JSON: Go uses encoding/json v1 and Rust uses serde_json with derive.
  `list_walk` does not do the same work in each language. Rust decodes each
  element from a parsed `Value`, and Go decodes each element from a
  `json.RawMessage`, so it scans every element twice.
- path_clean_count: Go calls `path.Clean` and counts the segments of the
  result, which allocates. Rust runs the same single-pass count as Pith.
- map_update/bytes: Go map keys cannot be `[]byte`, so each store converts
  the key with `string(k)`, which allocates.
- generic_sort: Go's `slices.SortStableFunc` is slow on int keys. Rust
  sorts references with `sort_by_key`.
- http_head_read: Go's `http.ReadRequest` does more per request than the
  Pith reader. It builds a canonical header map and parses the URL. The Rust
  arm uses httparse, which does less, so it is close to a lower bound.
- Tasks: Rust uses tokio for task workloads. `chan_fanout` and `task_churn`
  favor Pith over Rust because of tokio's channels and glibc malloc under
  80M allocations. `cpu_parallel_sync` uses async-channel, because the shared
  ack channel has several receivers.
- Metrics and lock_sweep: neither standard library has a metrics registry,
  so each port carries a small one shaped like std.metrics: a map behind an
  RWMutex with compare-and-swap cells. The `labels` modes do one lookup per
  write and keep no per-task cache.

### HTTP server

`bench/http_sweep.sh` now has two Rust arms: `hyper` (hyper 1.x on tokio, the
counterpart to net/http) and `rustmin` (tokio with a hand-written keep-alive
loop, the counterpart to the minimal Go server, byte-identical responses). The
setup is the same as the sweep above (wrk -t16 on the other CCD, 10 s runs,
median of 3):

| conns | pith | net/http | min go | fasthttp | hyper | rustmin |
|---:|---:|---:|---:|---:|---:|---:|
| 32 | 1.29M | 768k | 1.39M | 1.39M | 1.07M | 1.08M |
| 128 | 1.95M | 814k | 2.34M | 2.36M | 1.54M | 1.61M |
| 256 | 2.02M | 1.04M | 2.28M | 2.41M | 1.82M | 1.89M |
| 1024 | 2.04M | 1.23M | 2.45M | 2.43M | 2.34M | 2.34M |

The p99 latency in µs:

| conns | pith | net/http | min go | fasthttp | hyper | rustmin |
|---:|---:|---:|---:|---:|---:|---:|
| 32 | 50 | 299 | 296 | 324 | 74 | 51 |
| 128 | 100 | 1250 | 415 | 418 | 147 | 138 |
| 256 | 178 | 1690 | 524 | 741 | 262 | 248 |
| 1024 | 849 | 3790 | 1480 | 1720 | 970 | 940 |

Pith handles more requests per second than both Rust servers up to 256
connections, and both pass it at 1024. It has the lowest p99 of all six
servers at every connection count.

### Catalog service

`bench/catalog_server_rust` is an axum counterpart to the Go catalog server,
checked endpoint by endpoint against it. The client makes one request per
connection, so connection setup dominates. Each cell is the median p50 over
three rounds of each pairing, with the servers on CCD 0 (16 workers) and the
client on CCD 1:

| endpoint | go | pith | rust |
|---|---:|---:|---:|
| `GET /profile` | 82 µs | 88 µs | 90 µs |
| `GET /search` hot | 81 µs | 94 µs | 90 µs |
| `GET /search` wide | 81 µs | 91 µs | 89 µs |
| `POST /batch-score` | 83 µs | 94 µs | 92 µs |

The client measures its two servers in a fixed order. A Go-against-Go run read
1.0x on every endpoint, so the order does not account for the gap.

### zstd

This runs on a single pinned core. Pith's pure encoder sits between zstd
levels 1 and 3, klauspost runs at `SpeedDefault` (about level 3), and ruzstd
only implements its fastest level, so compare encode speed together with the
output size. The `libzstd` column is libzstd at level 3, which Pith and Rust
both bind. In MB/s:

| corpus | pith decode | klauspost decode | ruzstd decode | pith encode | klauspost encode | ruzstd encode | libzstd decode / encode |
|---|---:|---:|---:|---:|---:|---:|---:|
| prose 292 KB | 244 | 1747 | 394 | 48 | 203 | 112 | 1757 / 340 |
| source 585 KB | 348 | 2217 | 519 | 65 | 293 | 135 | 2350 / 472 |
| json 500 KB | 905 | 4967 | 1260 | 106 | 857 | 317 | 5545 / 1648 |

These are medians of three rotated rounds, each running the three arms in turn,
pinned to one core. Output size as a share of libzstd level 3: Pith 107-119%,
ruzstd 132-150%. Against the 2026-11-05 run, Pith's encoder moved from 47, 59
and 97 MB/s to 48, 65 and 106, while pure decode moved from 255, 312 and 980
to 244, 348 and 905 (the source corpus is gathered from the repository, so it
changes between runs, and #1522 added bounds checks to the pure decoder).

### gRPC

`bench/grpc/run.sh` makes unary calls against one grpc-go server, and the
table gives the median calls per second of five runs. The runner pins nothing
and runs the clients in a fixed order, and two unpinned runs on this box read
Go and Rust at concurrency 8 anywhere from 59k to 101k. So these runs pinned
the server to cores 8-15,24-31 and every client to cores 0-7,16-23, rotated
the client order, and gave each client 16 workers. Every arm stayed within
2 percent of its median across all five rounds.

| payload | concurrency | go (grpc-go) | rust (tonic) | pith |
|---|---:|---:|---:|---:|
| 16 B | 1 | 40.4k | 33.3k | 41.5k |
| 1 KiB | 1 | 39.7k | 30.6k | 39.4k |
| 16 B | 8 | 121.3k | 101.2k | 108.1k |
| 1 KiB | 8 | 111.7k | 83.0k | 90.1k |

At concurrency 1, Pith is even with Go: 2.7 percent ahead at 16 B and 0.7
percent behind at 1 KiB. At concurrency 8, Go is ahead by 1.12-1.24x. Pith is
ahead of tonic on every row.

### Where Pith stands

Pith matches or beats Go on JSON decoding, substring search, the event
ledger, the catalog workload, `std_pipeline`, generic sorting, path cleaning,
CSV encoding, string building, map updates with every key type, file reads,
channel fan-out, parallel compute with
a coordinator, spawn-and-await inside tasks, labeled metrics, gRPC at
concurrency 1, HTTP latency and the API gateway. Its HTTP server also handles
more requests per second than hyper up to 256 connections. It trails Rust on
most compute rows, often by 2x or more. The largest gaps come from these:

- Indirect closure dispatch and escaping closures (`closure_calls`,
  `closure_error`). Non-escaping lambdas passed to call-only parameters use a
  stack slot, but closures returned from a function still allocate on the heap
  and indirect calls are not inlined.
- Checked integer arithmetic. An Int add, subtract or multiply that can
  overflow stops the program (#1401). #1484 dropped the checks the compiler
  can prove never fire. `tight_loop`, three operations per iteration, still
  pays for the two that can, and runs at 2.1x Go against 1.36x before the
  checks existed.
- Per-object allocation and refcount cost (`cyclic_graph`, `task_churn`). A
  some optional built outside a getter still gets a heap shell.
- Spawning from the main thread (`green_fanout`, 2.7x Go). Main is an OS
  thread, so its spawns go through the shared injector and ID pool instead
  of a worker's own cache, and glibc malloc pays for frees on another thread.
- The node pool behind `json_decode_shapes/list_walk`, about 2.9x Go.
- Concurrent gRPC calls, 1.12-1.24x behind grpc-go at concurrency 8.

## Guarded locking against its base (2026-11-06)

The guarded-locking stack (#1324, #1325, #1326) adds `locked` blocks, `guard`
blocks, `holding` functions and the E309 lint, and moves std's lock and unlock
pairs into blocks and its shared state into guard blocks. The tip of the stack
was measured against a1754077, the main commit the stack branched from, on the
setup above.

The cross-language suite ran once on each tree. 41 of its 46 Pith rows are
within 2 percent of each other, and three more are 2.6 to 3.4 percent faster
on the stack (`lock_sweep/labels`, `catalog_workload`, `lock_sweep/readfile`).
The other two moved further and were raced, 21 interleaved rounds each:

| benchmark | base | stack | change |
|---|---:|---:|---:|
| lock_sweep/spawn | 47.3 ms [37.0-48.2] | 45.5 ms [36.0-46.6] | -3.9% |
| substring_search/absent | 0.9 ms | 0.9 ms | +1.1% |

`lock_sweep/spawn` has two modes, near 37 ms and near 47 ms, and the base's
suite run happened to land on the faster one.

The HTTP sweep and the gateway alternated base and stack runs, three each, with
five more gateway runs at 1024 connections because its p99 there swings by a
millisecond from run to run. Medians:

| http server | base req/s | stack req/s | base p99 | stack p99 |
|---:|---:|---:|---:|---:|
| 32 | 1.17M | 1.16M | 53 us | 53 us |
| 128 | 1.85M | 1.80M | 110 us | 109 us |
| 256 | 1.93M | 1.94M | 196 us | 185 us |
| 1024 | 1.93M | 1.94M | 990 us | 870 us |

| gateway | conns | base req/s | stack req/s | base p99 | stack p99 |
|---|---:|---:|---:|---:|---:|
| plain | 64 | 472k | 469k | 276 us | 262 us |
| plain | 256 | 485k | 484k | 950 us | 1060 us |
| plain | 1024 | 465k | 467k | 4.3 ms | 4.5 ms |
| auth | 64 | 150k | 150k | 789 us | 777 us |
| auth | 256 | 152k | 153k | 3.4 ms | 3.0 ms |
| auth | 1024 | 147k | 147k | 10.3 ms | 11.6 ms |
| failure | 1024 | 407k | 406k | 4.2 ms | 4.1 ms |

The gateway's peak memory moved by under 3 percent in every row, and no
request failed in either tree.

The gRPC Pith client ran five rounds against the grpc-go server, alternating
trees, with the server pinned to CPUs 8-15,24-31 and the client to 0-7,16-23:

| payload | concurrency | base | stack |
|---|---:|---:|---:|
| 16 B | 1 | 37.8k | 37.7k |
| 1 KiB | 1 | 35.3k | 37.0k |
| 16 B | 8 | 103.1k | 103.6k |
| 1 KiB | 8 | 83.7k | 84.0k |

Checking costs a little more, since the checker now walks each body with a set
of held mutexes. Both compilers ran on the base tree's sources for seven
interleaved rounds:

| job | base | stack | change |
|---|---:|---:|---:|
| `pith check self-host/ir_emitter_core.pith` | 278 ms | 282 ms | +1.4% |
| `pith check std/net/http.pith` | 273 ms | 277 ms | +1.4% |
| `pith build self-host/pith_main.pith` | 1206 ms | 1208 ms | +0.1% |

The regression suite passed on the stack, memcheck and AddressSanitizer
included. ThreadSanitizer reports the same findings on both trees: a race on a
test's own global in `test_tls_server_config_release`, thread leaks in
`test_task_handle_scope` and the HTTP/2 connection tests, and the intermittent
HTTP/2 client race.

## Event ledger benchmark (JSON + collections + crypto)

`bench/event_ledger.*` ingests a stream of newline-delimited JSON events,
indexes them with maps and a set, and signs a canonical summary with
HMAC-SHA256. It exercises a realistic slice of the standard library (JSON
decoding, hash-map and set aggregation, and crypto) in four languages at
once: Pith, Go, Rust, and Zig.

Every implementation generates the same event stream from a shared 31-bit
LCG, so the aggregate `checksum` and the HMAC `digest` come out identical
in all four. That equality checks the work: if any version did less work
or decoded a field differently, the digest would diverge.

**Batteries:** Pith, Go, and Zig write this with only their standard
libraries. Rust's std has neither JSON nor crypto, so its version pulls
`serde_json`, `sha2`, and `hmac`, which is the real cost of a deliberately
small standard library. (Four independent crypto implementations landing
on the same HMAC digest is also decent evidence the digest is real.)

Run it:

```
bench/event_ledger_bench.sh 200000 5   # events, trials
```

The script builds all four, checks that the digests match, and prints median
phase times. Or build and run one directly:

```
pith build bench/event_ledger.pith && ./bench/event_ledger 200000
go build -o bench/event_ledger_go bench/event_ledger.go
cargo build --release --manifest-path bench/event_ledger_rust/Cargo.toml
zig build-exe -O ReleaseFast -femit-bin=bench/event_ledger_zig bench/event_ledger.zig
```

On 2026-10-25, on the 32-thread Ryzen 9 9950X pinned to CPUs 28-31, main at
c935006c reads, for 200000 events and the median of 5 interleaved trials:

| lang | gen | parse | analyze | sign | total |
|---|---:|---:|---:|---:|---:|
| pith | 27 | 34 | 9 | 0 | 70 |
| go | 17 | 73 | 5 | 0 | 95 |
| rust | 11 | 24 | 10 | 0 | 45 |
| zig | 7 | 34 | 4 | 0 | 46 |

Pith's total is 0.74x Go's and about 1.5x Rust's and Zig's. Its `parse` is
under half of Go's and level with Zig's. `gen` is the largest gap: 27 ms
against 17 for Go and 11 for Rust.

The results below come from an older, slower machine, so compare their rows
with each other and not with the table above. Earlier results, 200000 events,
median of 5 interleaved trials (2026-09-13, after the map work in #1126, #1128 and
#1129; the 2026-09-10 Pith total was 259, the 2026-08-16 total 351, the
2026-07-29 total 618):

| lang | gen | parse | analyze | sign | total |
|---|---:|---:|---:|---:|---:|
| pith | 112 | 102 | 23 | 0 | 238 |
| go | 51 | 326 | 15 | 0 | 393 |
| rust | 26 | 57 | 23 | 0 | 106 |
| zig | 18 | 97 | 9 | 0 | 125 |

The `analyze` phase is the one that moved, 56 to 23 ms. It is the map and
set rollup, and a string-keyed update that used to copy the key three
times and box the value now allocates nothing at all. Pith's `analyze` is
now level with Rust's, and its total is 0.61x Go's.

(Times in ms. `gen` builds the stream, `parse` decodes it into structs,
`analyze` runs the map/set rollup, and `sign` is the HMAC.)

Rust and Zig are fastest. Pith now leads Go on the total, and both changes
that got it there are instructive. The 2026-08-16 `gen` drop (299 → 120, now
even with Go) came from writing the stream into one ByteBuffer instead of
concatenating fresh strings per line. That is the same builder shape the Go
version always used with `strings.Builder`, so the comparison is idiom for
idiom. The `parse` improvement (195 → 175) came for free from fixing the
decode lowering's per-call leaks: a flat struct of required scalars decodes
in a single pass, straight into the struct, and it no longer strands its
input buffer, so the allocator stays out of fresh pages. That single-pass
decode keeps Pith's `parse` the second-fastest of the four, ahead of Go's
reflection-based decode. The first cut of this benchmark had Pith at 1084ms
on parse and 1472ms total.

What is left of the gap to Rust and Zig is spread across all three busy
phases. The whole pipeline (JSON, collections, HMAC-SHA256) uses only the
standard library, with no dependencies to add, and compiles in a fraction of
the time the others take to build.

| | Go | Pith |
|---|---|---|
| Cold compile | 27.4s | 0.34s |
| Warm compile | 0.29s | 0.36s |
| Binary size | 7.2 MB | 4.9 MB |

Pith compiles from scratch every time, since it has no incremental build
cache yet. Go's first build pulls and compiles the standard library, and
later builds use the cache. The binary-size difference comes from Go
embedding its runtime and GC, while Pith statically links a smaller Rust
runtime.

## HTTP server benchmark

`bench/http_server.pith` serves a small JSON API with allocation-churny
request handling: query parsing, catalog lookups, and per-request string
assembly. It spawns a task per accepted connection, the same serving model as
the Go counterpart, `bench/http_server.go`. `bench/http_server_mt.pith` is the
explicitly threaded variant. Until 2026-08-23 the accept loop served each
connection inline, which serialized the whole benchmark on one connection.
Throughput collapsed to the reciprocal of per-request latency with a core
idle, and the Pith-vs-Go comparison measured a serial server against a
concurrent one. Any Pith throughput recorded before that date carries the
flaw.

`bench/http_seq_latency.py` is the other half of the HTTP story, and on a
small shared box the more trustworthy half: one keepalive connection, one
request at a time, p50/p90/p99 per round trip. On such a box, wrk throughput
is launch-cadence noise (repeated runs read progressively lower as the host's
CPU burst decays), while this probe is deterministic to a few microseconds.
When a throughput number looks like a regression, run this first. If p50 did
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
identical protocol, with ~6 MB of growth that reproduces. On 2026-09-10, after the
request head is read with one bounded `read_until` instead of a byte at a
time (#1097), the same 60-second run reads 17.3k req/s for Pith (1.04M
requests, 2.6 MB of growth, ~2.5 bytes/request) against 27.1k for Go, and
the one-connection probe below holds p50 135 µs. The earlier figures this
section carried (4648 req/s, "zero growth") were both wrong: the server was
serial, and it was leaking ~64 bytes/request until #901, which a 30-second
window could not see.

Take the throughput figures as same-run comparisons only, never across runs.
`wrk` competes with the server for the same two cores, and the Go arm in
particular swings between regimes with the host's state, so the Pith/Go
ratio is whatever regime Go is in that day (0.45x-0.66x across the two runs
above). Pith is the stable arm. For a number that does not move with the
host, use `bench/http_seq_latency.py`. The memory column is trustworthy only
over a long window: run at least 60 seconds before calling an RSS line flat.

On 2026-10-23 the same benchmarks ran on a 32-thread Ryzen 9 9950X, and the
green server got slower as it got more cores. At 32 workers, the default on
that machine, `wrk -t 8 -c 256` read 36k req/s for Pith against 140k at 4
workers and 607k for Go. Three global locks and the scheduler's wake path
caused it. "Scaling to 32 hardware threads" below has the details. After the
fixes the same run reads 884k req/s for Pith (p99 1.7 ms) against 615k for Go.
The 60-second `http_bench.sh` run above reads 275k for Pith against 387k for
Go, with the same ~2.6 bytes/request of RSS growth. `wrk` shares the machine's
cores with the server in both runs, so neither figure is a ceiling.

### Against a minimal Go server (2026-10-25)

`bench/http_sweep.sh` runs four servers on the same `/item?id=N` route and
body: Pith's std.net.http (`bench/http_server.pith`), Go's net/http
(`bench/http_server.go`), a minimal Go server on net and bufio
(`bench/http_server_min.go`), and fasthttp (`bench/fasthttp`). The server runs
on one CCD of the 9950X (CPUs 0-7,16-23, with 16 workers or GOMAXPROCS=16) and
wrk runs on the other with 16 threads. The table has the median req/s and p99
of 3 interleaved 10-second runs on main at 2c531be4:

| conns | pith | net/http | minimal go | fasthttp |
|---:|---:|---:|---:|---:|
| 32 | 1.19M, 51 µs | 746k, 302 µs | 1.33M, 314 µs | 1.33M, 315 µs |
| 128 | 1.71M, 140 µs | 779k, 1.36 ms | 2.18M, 509 µs | 2.15M, 512 µs |
| 256 | 1.77M, 235 µs | 987k, 1.92 ms | 2.20M, 823 µs | 2.18M, 880 µs |
| 1024 | 1.75M, 1.14 ms | 1.16M, 4.11 ms | 2.26M, 1.67 ms | 2.18M, 2.11 ms |

Pith serves about 80% as many requests as the minimal Go server from 128
connections up, and 90% at 32. It serves 1.5 to 2.2 times as many as
net/http, and its p99 is the lowest of the four at every connection count.

The sweep needs all 16 wrk threads. With 8, its old default, wrk held the
minimal Go server and fasthttp near 1.65M req/s, with the minimal Go server's
CPUs about 24% idle, and Pith read 5% ahead of them at 1024 connections. With
16, the minimal Go server is about 6.5% idle at 1024 connections and Pith
about 0.5%, so both are close to CPU bound. The difference is user-space CPU:
#1188 measured 3.2 µs per request for Pith against 1.5 µs for the minimal Go
server, with about the same kernel time for both.

```
WRK=wrk PITH=./target/release/pith bench/http_sweep.sh
```

## API gateway benchmark

`bench/gateway_bench.sh` compares `examples/gateway` with `bench/gateway.go`, a
Go `net/http` gateway with the same behavior. It covers plain forwarding, an
ES256 bearer token checked at the edge, and a backend killed partway through a
run. The results from the 2026-11-05 run, the changes behind them and what
they cost are in [examples/gateway/README.md](../examples/gateway/README.md).

The 2026-11-28 sweep (main at `fb38baae`, the setup above, three trials per
row) measured these for plain forwarding:

| conns | pith req/s | go req/s | pith p99 | go p99 | pith peak rss | go peak rss |
|---:|---:|---:|---:|---:|---:|---:|
| 64 | 432k | 289k | 356 µs | 880 µs | 11.5 mb | 27.6 mb |
| 256 | 452k | 413k | 1.32 ms | 4.33 ms | 16.3 mb | 42.2 mb |
| 1024 | 429k | 361k | 4.21 ms | 20.4 ms | 30.6 mb | 102 mb |

With an ES256 token checked at the edge, the two gateways are within 4 percent
of each other at 256 and 1024 connections (154k vs 160k and 149k vs 153k),
Pith handles 1.60x Go's rate at 64 (150k vs 94k), and Pith has the lower p99
(0.81, 3.00 and 14.65 ms vs 3.25, 6.43 and 19.40 ms) and 42-65% less peak
memory at every count. With a backend killed a third of the way through,
neither gateway failed a request.

## Catalog service benchmark

There is also a more realistic in-memory microservice benchmark:

- `bench/catalog_server.go`
- `bench/catalog_server.pith`
- `bench/catalog_bench.go`

This pair serves the same synthetic catalog dataset and exposes:

- `/health`: simple readiness check
- `/profile?id=123`: single-record lookup
- `/search?...`: filtered scans and aggregate summaries
- `POST /batch-score`: JSON body parsing plus aggregate scoring

It aims to benchmark something closer to a normal Go service: request
parsing, dataset scans, query filtering, and JSON responses.

To run it:

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

You can also override the ports for ad hoc runs:

```
./bench/catalog_server_go 9201 &
./bench/catalog_server_pith 9202 &
go run bench/catalog_bench.go 9201 9202
```

The first measured results, from 2026-09-13 at `0429a5e0`, ran sequentially
with one connection at a time and both servers on the same dataset:

| endpoint | go p50 | pith p50 | go p99 | pith p99 | pith/go p50 |
|---|---:|---:|---:|---:|---:|
| `GET /profile` | 509us | 578us | 821us | 1292us | 1.1x |
| `GET /search` hot | 494us | 570us | 972us | 992us | 1.2x |
| `GET /search` wide | 497us | 575us | 1055us | 2423us | 1.2x |
| `POST /batch-score` | 501us | 584us | 749us | 1710us | 1.2x |

Neither side had errors. The medians sit within 20% of Go across all four
shapes. The tails do not, and they spread on the two scan-and-aggregate
endpoints (2.3x at p99). This benchmark is sequential by construction, so it
says nothing about throughput under concurrency. The HTTP server section
above covers that.

## Catalog workload benchmark

For a stable service-shaped comparison without socket noise, there is also an
in-process catalog workload benchmark:

- `bench/catalog_workload.go`
- `bench/catalog_workload.rs`
- `bench/catalog_workload.pith`

This uses the same synthetic dataset and benchmark shape as the catalog service,
but runs the handler logic directly inside one process:

- Profile lookups
- Hot filtered searches
- Wider aggregate scans
- Batch JSON parsing plus score aggregation

To run it:

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

A helper runner is also available once the workload binaries are built:

```
go build -o bench/catalog_workload_go bench/catalog_workload.go
rustc -O -o bench/catalog_workload_rust bench/catalog_workload.rs
pith build bench/catalog_workload.pith
go run bench/catalog_workload_bench.go 10000 5
```

The second argument is the number of trials. The runner reports median phase
times, which are more reliable than a single run when the timings are short.

The workload benchmark now also uses internal team/region IDs and precomputed
candidate index lists for common region/active filters, which is closer to how
an actual in-memory service would avoid rescanning the full catalog on every
request.

The latest measured results on this machine use the median of 5 trials
(2026-09-10, direct 200k-iteration runs interleaved, two warmups):

| iterations | go total | pith total | ratio | rust total |
|---|---:|---:|---:|---:|
| `200000` | `~386 ms` | `~70 ms` | `0.18x` | `~68 ms` |

The 2026-08-08 run read Go `~408`, Pith `~118`, Rust `~70`, and the 2026-07-29
run read Go `~436`, Pith `~114`, Rust `~80`. The comparators held, so the drop
from 118 to 70 is Pith's own. By instruction count the workload is 30% cheaper
than at the start of 2026-09-10 (1.38 G to 0.96 G Ir), most of it from the
typed decode's field lookup (#1104). Pith and Rust are now within noise of
each other on this workload.

The earlier 1M-iteration medians, for the trend:

| iterations | go total | pith total | ratio | go batch | pith batch |
|---|---:|---:|---:|---:|---:|
| `1000000` | `2009 ms` | `691 ms` | `0.34x` | `1910 ms` | `630 ms` |

With the optional Rust workload binary built:

| iterations | rust total | pith/rust | rust batch | pith/rust batch |
|---|---:|---:|---:|---:|
| `1000000` | `369 ms` | `1.87x` | `320 ms` | `1.97x` |

The current Pith workload uses derived JSON struct decoding for the batch
request. A flat struct of required scalars decodes in a single pass, filled
straight into the struct. The event_ledger benchmark has the details.
The Rust workload uses a tiny standalone JSON field scanner, so treat it as
a lower-bound runtime comparison, not a comparison of serde-style libraries.

Binary size from the same build:

| binary | file size | text segment |
|---|---:|---:|
| pith workload | `5.2M` | `1.4M` |
| go workload | `2.7M` | `1.7M` |
| rust workload | `3.9M` | `366K` |

The Pith workload binary is larger on disk than the Go and Rust binaries
today, but its executable text segment is smaller than Go's in this build.
That points at debug, symbol and linker overhead as a likely size target
before reading too much into the file-size number alone.

This is the better comparison point today if you want to isolate runtime,
language, and service-logic costs from HTTP and socket handling.

## Cyclic graph benchmark (weak references)

`bench/cyclic_graph.pith` and `bench/cyclic_graph_strong.pith` build many
parent<->child rings and drop them. Each ring has a strong forward edge
(`next`) and a back edge (`parent`). The two programs differ in one word:
the back edge is `weak` in one and a plain optional in the other.

A `weak` back edge holds the parent without owning it, so the ring closes
no strong cycle and every ring reclaims as the loop moves on. A strong
back edge closes a real cycle (`next` owns forward, `parent` owns back),
so neither node's refcount reaches zero and every ring leaks. The two
binaries print the same checksum, and only their memory behavior differs.

Run them:

```
pith build bench/cyclic_graph.pith
pith build bench/cyclic_graph_strong.pith
./bench/cyclic_graph 2000000
./bench/cyclic_graph_strong 2000000
```

Peak resident memory on this machine, two million rings:

| back edge | peak RSS | structs freed |
|---|---:|---|
| `weak` | ~11 MB | all (bounded) |
| strong | ~752 MB | none (leaks every ring) |

(2026-09-13, peak RSS from the child's rusage. The strong figure reproduces
the ~730 MB this table carried before. The weak figure is larger than the
~2 MB recorded earlier and has not been attributed. The pair still shows
what it is for: one back edge is bounded and the other is not.)

The `weak` run holds flat because the rings free as fast as they are
built, and the strong run grows without bound. `PITH_PERF_STATS=1` prints the
underlying struct alloc/free counts: balanced for the weak variant, and
alloc-heavy with almost no frees for the strong one.

## Channel fan-out benchmark (concurrency)

`bench/chan_fanout.*` is the concurrency counterpart to the batch
benchmarks here. Four producer tasks push messages into one bounded
channel (capacity 256) and four consumer tasks drain it. The work per
message is two LCG rounds, kept small on purpose so the handoff, which is
what the benchmark measures, dominates the arithmetic.

Each consumer folds its messages into a partial sum modulo a prime and
the partials are added at the end, so the total does not depend on which
consumer saw which message. All four implementations print the same
`checksum=90815792` at one million messages. That equality is the same
check `event_ledger` uses. Each also reports how
many messages it sent and received, and its peak RSS read from
`/proc/self/status`.

Each language does this its own way: Pith `spawn` and `Channel[Int]`, Go
goroutines and a buffered channel, Rust std threads and
`mpsc::sync_channel`, and Zig `std.Thread` over a hand-written mutex/condvar
ring buffer (Zig's std has no channel). Rust's mpsc is single-consumer,
so its receiver sits behind an `Arc<Mutex<..>>`. That is the std-only way to
fan one channel out to several consumers, and its number includes that
cost.

Run it:

```
bench/chan_fanout_bench.sh 1000000 9   # messages, trials
```

The script builds all four, checks that every run of every implementation
agrees on the checksum, and prints the medians. Or build and run one
directly:

```
pith build bench/chan_fanout.pith && ./bench/chan_fanout 1000000
PITH_GREEN=0 ./bench/chan_fanout 1000000
go build -o bench/chan_fanout_go bench/chan_fanout.go
rustc -O -o bench/chan_fanout_rust bench/chan_fanout.rs
zig build-exe -O ReleaseFast -femit-bin=bench/chan_fanout_zig bench/chan_fanout.zig
```

The table shows one million messages, the median of 9 trials on this 2-core
machine with nothing else running on it. Eight tasks on two cores is
oversubscribed on purpose, and equally so for all four (measured 2026-09-13;
the 2026-07-26 table it replaces read 438 / 171 / 75 / 135 / 135):

| lang | ms | messages/sec | peak rss |
|---|---:|---:|---:|
| pith (`PITH_GREEN=0`, os threads) | 250 | 4.0 m | 3.1 mb |
| pith (green, the linux default) | 68 | 14.7 m | 3.1 mb |
| go | 72 | 13.9 m | 3.7 mb |
| rust | 62 | 16.1 m | 2.4 mb |
| zig | 272 | 3.7 m | 4.5 mb |

The green row has been bimodal on every rerun since July (fast rounds near
60-70 ms, slow rounds near 95-100, see docs/performance.md). All nine
rounds of the 2026-09-10 run landed in the fast mode, so read 65 as the
fast mode's figure, not as the end of the bimodality.

The 2026-07-29 rerun (medians of 7, interleaved) held the same shape with
the usual comparator drift: green ~133 (bimodal, best runs at ~69), green
pinned to one worker ~46, Go ~75, Rust ~75, Zig ~240. The 2026-08-08 rerun
reproduced it almost exactly (green ~133 with best runs at ~61, Go ~71, Rust
~73, Zig ~205). That is the clearest evidence the harness fix landed
correctly: the same three comparator programs came back to the same figures.

Read the rows that oversubscribe OS threads (Pith OS-thread, Rust, Zig)
with the box in mind: eight threads on two cores, so they swing run to run.
Across two suite runs a week apart, Rust moved 135 -> 94 ms and Zig 135 -> 204
with no code change on either side, and Pith's OS-thread row has been seen
anywhere from ~260 to ~940. The green and Go rows are the stable ones, and
the comparison rests on them.

For most of this benchmark's life Pith lost it outright: 580ms OS-thread
and 782ms green against Go's ~70, roughly 8x behind. Two fixes on
2026-07-26 changed that. The first was found with `perf`: Rust's standard
condvar is futex-based and pays a `futex(FUTEX_WAKE)` syscall on every
notify *even when nobody is waiting*, and the channel notified its
condvar on every wake. Green waiters never condvar-wait (they suspend
their coroutine instead), so on an all-green channel that was two
pointless syscalls per message, about 70% of the run. The channel now
counts its OS-thread waiters per role, under the same lock the parker and
waker already hold, and only signals when one is actually parked. The
second moved each task's scheduling state (run state, wake flags, owner)
into one atomic word in a chunked side arena, so a wake and a resume no
longer touch the scheduler's slab lock at all.

With those in, the green backend finally does what it is for: 171ms is
2.6x faster than Pith's own OS threads and ahead of Rust and Zig, at
2.3x Go. The remaining gap to Go is placement. Eight tasks that all
block on one channel land on both workers, and a cross-worker handoff
still wakes the peer. `PITH_GREEN_WORKERS=1` pins the pipeline to one
worker and gives ~46ms, faster than Go on this box. Locality accounts for
all of the difference, and the knob is green-only, so it is not
in the table. The green median above mixes both
placement modes (~60ms when the pinning falls same-worker, ~130-170 when
it splits), which is also why green is the noisier row.

The OS-thread improvement (580→438) is older and separate. The channel
core moved to a lock-free MPMC ring earlier the same day, after two
prior rounds: splitting the condvar by role (~19%), and one failed
attempt (dropping the handle-registry lock measured 2x worse by
concentrating futex contention). OS threads still condvar-wait, so the
notify-skip buys them little. The ring is what moved their number.

Context switches over the same run (`perf stat -e context-switches`):

| | pith | pith green | go | rust | zig |
|---|---:|---:|---:|---:|---:|
| context switches | 5.0k | 2.5k | 258 | 4.8k | 60k |

Green used to take 30.9k switches here. It now takes fewer than Rust's
std threads, which is the userspace-handoff behavior it was built for.
Go's 258 remains the mark: its scheduler almost never touches the
kernel on this shape.

Memory is the one column where Pith was always fine. Everything holds
flat: 4x the messages moves peak RSS by under 100 KB in every
implementation, so nothing is retained per message on any of them.
Pith's 3.0 MB against Go's 2.0 MB is runtime baseline, not growth.

Zig's timing is by far the noisiest of the four, probably because of the
plain `signal` on a shared condvar, which wakes whichever consumer the
kernel feels like. The median is stable enough to compare, but read any
single Zig run with suspicion.

## Parallel compute with a coordinator (the control for task migration)

`bench/cpu_parallel_sync.pith` is the shape every other concurrency bench
here lacks. Eight tasks do genuinely parallel arithmetic in chunks and,
between chunks, report to one collector task and wait for its
acknowledgement, so the work spreads across cores while every chunk still
ends in a real handoff. It was written to measure moving a woken task to the
worker that woke it. On the fan-out and ping-pong benches that is a pure win,
and on this one it pulled the compute tasks onto the collector's worker and
gave part of the speedup back, until idle workers could take queued tasks off
a busy peer. The checksum must match across arms.

```
pith build bench/cpu_parallel_sync.pith
PITH_GREEN_WORKERS=1 ./bench/cpu_parallel_sync 200 20000
PITH_GREEN_MIGRATE=0 ./bench/cpu_parallel_sync 200 20000
./bench/cpu_parallel_sync 200 20000
```

On 2026-09-06, medians of 5 interleaved rounds on a 2-core box read 448 ms
at 1 worker, 259 ms at 2 workers with migration off (247-305), and 337 ms
with it on (230-402). That is why migration was not made the default then.
On 2026-10-28, with idle workers taking queued tasks off a busy peer, 16
workers on a 9950X ran `200 1000000` in 911 ms with migration off and 655 ms
with it on (5042 ms on one worker).

On 2026-09-13 the ordering had reversed: 404 ms at 1 worker (403-405), 327
with the flag off (228-364), **241 with it on** (223-370), nine rounds
interleaved arm by arm after two warmups. The flag won 5 of the 7 paired
rounds (the paired comparison is within a round, so it survives a drifting
box in a way the medians alone do not). The arm that used to cost a third of
the two-worker speedup is now the fastest of the three. The 2026-09-06 run
predates preemption safe points becoming the default (#1088), which changes
when a task yields and so how placement settles. That is a hypothesis,
not an attribution. #1131 tracked it until #1233 turned migration on by
default. The 2026-10-28 section of `docs/performance.md` has the numbers that
decided it.

## Task churn benchmark (per-thread pools)

`bench/task_churn.pith` runs one short task after another, each allocating
a batch of structs into a list and releasing them at return. The per-thread
struct pool loses on this shape: under `PITH_GREEN=0` every task
is a fresh thread, so the pool is built, filled once, and torn down without
a block being reused. Under green the worker outlives the task and the pool
pays for itself. Run both arms and compare. The checksum must match.

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
to trust. See "Measuring" below.

## Closure calls benchmark (dispatch cost)

`bench/closure_calls.pith` splits a closure's cost into three phases so a
change to one is visible on its own: two million closures each built,
called once, and dropped (`build_ms`); one closure called two million times
(`call_ms`); and a plain function called two million times as the floor
(`direct_ms`). All three fold into one checksum. A typical split on the
two-core box is build 111 ms, call 14 ms, direct 4 ms, so a closure call is
about 7 ns against a 2 ns direct call, and construction dominates.

```
pith build bench/closure_calls.pith && ./bench/closure_calls
```

## Tight loop (the preemption safe-point's worst case)

`bench/tight_loop.pith` sums `i * 3` over 200 million iterations and prints
a checksum, with no allocation, call or channel. It prices the green
preemption safe-point, which the backend puts before every loop back-edge,
because this is the body the check is largest against: six instructions of
safe-point on a seven-instruction body.

Two details of how it is written each move the number several fold. There is
no division: an `idiv` is slow enough to hide the check behind its latency,
and the same loop with one reads +0.4%. And the loop is a function of its own
instead of the body of `main`, so that whatever main does around it cannot
decide which registers the loop gets. Inline beside a string format, it reads
+33% instead.

```
PITH_GREEN_PREEMPT=0 pith build bench/tight_loop.pith && ./bench/tight_loop
PITH_GREEN_PREEMPT=1 pith build bench/tight_loop.pith && ./bench/tight_loop
```

On the two-core box, medians of 9 interleaved rounds read 129 ms without
safe-points and 259 ms with them, against a ±3% null floor, and 1.400G
against 2.600G instructions under callgrind. The clock moves further than the
instruction count because the check contains a call, so the loop's values move
into callee-saved registers and the function grows a frame. Every other
program measured here pays between +0.07% and +1.85% instructions, and the
table is in `docs/performance.md`.

Re-measured 2026-09-13 (9 rounds interleaved, 2 warmups, canary checked
either side): 132 ms with safe points off (130-140), 265 with them on
(262-278), +100.5%. That reproduces the +101% this section has carried since
2026-09-07, on a binary two dozen changes newer.

## Substring search (the runtime's `contains` and `index_of`)

`bench/substring_search.pith` calls `contains` and `index_of` over haystacks
of 12, 80 and 4000 bytes with the needle at the start, in the middle, at the
end, or absent, and folds every answer into one checksum. Three needles run
over each cell: `comma`, one byte with no other occurrence in the filler,
which is the CSV and path shape; `word`, seven bytes whose first byte is
common in the filler; and `repeat`, a haystack of one repeated byte against a
needle that differs from it only in its last byte, the adversarial case for a
search that jumps between occurrences of the first byte. The benchmark exists
because the runtime used to compare the needle at every position of the
haystack, and that made `pith_cstring_contains` a tenth of the std pipeline
(#1099).

```
pith build bench/substring_search.pith
./bench/substring_search                  # every cell, 2000 rounds
./bench/substring_search 500 long-absent  # one cell
./bench/substring_search 2000 all comma   # one needle over every cell
```

The per-cell instruction counts of the old search, the libc `memmem` search that
replaced it (since joined by an SSE2 prefilter), and the `memchr` alternative it
was measured against are in `docs/performance.md`.

## Measuring: instruction counts over wall time

Wall time on a shared two-core box drifts by more than most effects under
study: a compile-time change once read +5.6% eight rounds running, and
callgrind put it at +0.06%. Two scripts make the reliable measurement the
easy one.

`tooling/callgrind_ab.sh` runs two arms under `valgrind --tool=callgrind`
and prints their instruction totals, the delta, and (with `--annotate N`)
the hottest functions of each. Attribution needs a symbol table, which the
linker strips by default, so build the program under test with
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
Run it before believing a timing delta. A real A/B inside that spread has
shown nothing. The floor here has been about ±3% on the minimum.

```
PITH_GREEN=0 tooling/null_ab.sh ./bench/task_churn 15 -- 2000
```

`tooling/ir_hash.sh` hashes the emitted IR of every corpus source into one
file. A compiler change that should be behavior-neutral reproduces every
hash, and one that should touch a single shape changes exactly the files that
have it. Capture before and after, then `diff`.

## Generic sort benchmark (std.collections and std.algo)

`bench/generic_sort.pith` measures the standard library's key-based sorts
against the selection and insertion sorts they replaced. Both arms live in the
one program: `legacy` is a verbatim copy of the old bodies and `current` calls
the shipped functions, so a single build measures both and nothing but the
sort differs. Inputs come from a seeded 31-bit LCG and are built before the
clock starts. The timed section covers what a caller pays for one call, which
is the copy, the key extraction and the sort. Each arm folds its result into a
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

Two details matter when reading the table. The old
`collections.sort_by_key` called the key function once per element per pass,
so key calls, not comparisons, dominate its cost, and the speedup there
keeps growing with n. And the old `algo.sort_by_key` was an
insertion sort that inserted before the first strictly greater element, which
makes a descending input its best case: it probes exactly one element per
insertion. That was the one cell the merge sort lost, at 100 and 1000 elements.
Run detection (#1096) recovered it: the sort now finds the ascending and
strictly descending runs the input already has, so a sorted or exactly reversed
input is one run and one linear scan. The reverse string cells went from 0.4x
and 0.5x against the insertion sort to 0.8x at 100 and 1.1x at 1000, and from
2.8x to 6.9x at 10000. The scan gives up within about eight comparisons on an
input with no runs in it, so no other cell pays for it.

Wall clock on the two-core box moves around, so instruction counts through
`tooling/callgrind_ab.sh` are the number to quote for a per-operation claim.
Running an arm with a round count of 0 gives the input generation on its own,
which is identical in both arms and can be subtracted out.

```
tooling/callgrind_ab.sh \
  legacy  './bench/generic_sort legacy random int 1000 20' \
  current './bench/generic_sort current random int 1000 20'
```

## String buffer benchmark (std.io)

`bench/string_buffer.pith` measures `std.io`'s `StringBuffer` against the
chunk-list storage it replaced. Both arms live in the one program: `legacy` is
a verbatim copy of the old bodies (a `List[String]` per buffer, joined back
into one chunk every 32 writes) and `current` calls the shipped buffer, so a
single build measures both and nothing but the storage differs. The chunks
come from a seeded 31-bit LCG and are built before the clock starts.

```
pith build bench/string_buffer.pith
bench/string_buffer_bench.sh
SIZES="1000 10000" CHUNKS=8 bench/string_buffer_bench.sh
WORKLOADS=snapshot EVERY=50 bench/string_buffer_bench.sh
```

There are two workloads. `build` appends n chunks, takes `string()` once,
closes the buffer, and hashes the whole result. That is the shape a builder or
an encoder has, and the one the storage change is about. `snapshot` takes
`string()` every k appends (default 100) as well as at the end. Every snapshot
is a fresh copy of the whole prefix in either arm, so its total is not linear
in n for either arm, and the table does not report it as a buffer cost. Each
run prints its peak resident set alongside the time.

The old storage copied the whole prefix at every compaction, about
s·n²/62 bytes over n appends of s bytes. Instruction counts for one arm at
1,000, 10,000 and 100,000 appends show whether that term is present. With a
round count of 0 the program does the input generation alone, which is
identical in both arms and can be subtracted out.

```
tooling/callgrind_ab.sh \
  legacy  './bench/string_buffer legacy build 10000 8 1' \
  current './bench/string_buffer current build 10000 8 1'
```

## std pipeline benchmark

`bench/std_pipeline.*` is a batteries-included data pipeline benchmark. It
generates deterministic records, writes and reads CSV, transforms rows with URL
and path helpers, writes a JSON report, gzip round-trips the report, hashes the
result, and touches the temp workspace through fs traversal.

To run it:

```
./self-host/pith_main build bench/std_pipeline.pith
go build -o bench/std_pipeline_go bench/std_pipeline.go
cargo build --release --manifest-path bench/std_pipeline_rust/Cargo.toml
go run bench/std_pipeline_bench.go 50000 5
```

These used to pin `GOCACHE=/tmp/pith-go-cache`. Don't: `/tmp` is a tmpfs
on the machine these numbers come from, so that puts a build cache in RAM
and takes it away from the thing being measured. With enough of it, the
OOM killer starts taking builds out mid-run. Go's default cache is on
disk, which is what you want.

The latest measured results on this machine use the median of 5 trials
(2026-09-10, direct runs interleaved, two warmups):

| records | go total | rust total | pith total | pith/go | pith/rust |
|---|---:|---:|---:|---:|---:|
| `50000` | `~242 ms` | `~137 ms` | `~312 ms` | `1.29x` | `2.28x` |

(2026-09-13; Pith read `~344 ms` on 2026-09-11.) Pith read `~480 ms` (1.50x
Go) on 2026-08-08 and `~576 ms` (1.63x) on 2026-07-29. The comparators
reproduce their 2026-08-22 figures (Go 249, Rust 134). By instruction count
the pipeline is 20% cheaper than at the start of 2026-09-10 (4.28 G to
3.41 G Ir): the CSV quoting decision became one byte pass and
`path.clean_part_count` two counters (#1097), and the runtime's substring
search runs on `memmem` (#1103). The 2026-09-10 Pith phases, run directly:
CSV write 171, CSV read 2, transform 170, gzip + hash 1, total 344.
`transform` is now the whole gap to Go (38 ms there). This is the noisiest
suite on this box. The medians are taken from rounds where the comparators
held their shape.

Phase breakdown from the 2026-07-21 run, whose shape still holds for Go and
Rust:

| phase | go | rust | pith |
|---|---:|---:|---:|
| config | `0 ms` | `0 ms` | `0 ms` |
| csv write | `191 ms` | `60 ms` | `257 ms` |
| csv read | `87 ms` | `47 ms` | `5 ms` |
| transform | `48 ms` | `27 ms` | `218 ms` |
| json | `0 ms` | `0 ms` | `0 ms` |
| gzip + hash | `1 ms` | `0 ms` | `1 ms` |
| fs | `0 ms` | `0 ms` | `0 ms` |

All three implementations report the same checksum:

```
107395835982034
```

Binary size from the same build:

| binary | file size | text segment |
|---|---:|---:|
| pith pipeline | `5.3M` | `1.4M` |
| go pipeline | `3.5M` | `2.3M` |
| rust pipeline | `1.4M` | `1.1M` |

The first cut of this benchmark had Pith at `12682 ms`. Moving CSV onto the
bytes path and avoiding per-row maps brought that down to `2023 ms`. The
URL, path and hash fast paths brought it down again to about `1400 ms`. Lazy
CSV row views brought it to about `1230 ms` by avoiding the full
`List[List[String]]` read path. Folding CSV rows through the public module API
keeps the same zero-copy shape and landed around `1200 ms`. String-derive and
byte-scanning work since then (single-allocation string derives, a combined
bytes-substring decode) took it to about `634 ms`. Finally, rewriting the URL
and path scanners to compare raw bytes, instead of minting a one-character
string per position, cut the `transform` phase from `347 ms` to `218 ms` and
the total to about `482 ms`, dropping the run's cstring allocations from 7.1M
to 2.9M. What is left is mostly CSV write overhead.

Three caveats matter when reading this benchmark:

- Rust uses pinned crates for the libraries it does not ship in `std`, which is
  the normal Rust way to write this kind of tool.
- The local Go toolchain in this environment could not resolve `encoding/csv`
  or `hash/fnv`, so the Go workload carries tiny CSV and FNV helpers while
  still using Go's json, gzip, sha256, url, path, and fs packages.
- The Pith version keeps the config setup local, so the benchmark times the
  csv/url/path/gzip/hash/fs pipeline and not config parsing.

## Map update (the event ledger's rollup in isolation)

`bench/map_update.pith` is the shape the event ledger's analyze phase is
made of: `if m.contains_key(k): m.insert(k, m[k] + d)` over a small set of
distinct keys, so nearly every update is a hit on a key that is already
there. It exists because that shape used to cost four allocations per
update: three key copies and a value box.

```
pith build bench/map_update.pith
./bench/map_update string 20000 64    # flavor, updates, distinct keys
./bench/map_update bytes 20000 64
./bench/map_update int 20000 64
```

On 2026-09-13, with 20,000 updates over 64 distinct keys, instruction counts
come from `tooling/callgrind_ab.sh` and allocation counts from Valgrind:

| key flavor | Ir before | Ir after | allocations per update |
|---|---:|---:|---:|
| string | 28,373,869 | 11,761,225 | 4.00 to 0 |
| bytes | 26,275,158 | 9,874,948 | 4.00 to 0 |
| int | 6,148,579 | 4,505,198 | 0 to 0 |

"Before" is `19aceac7` and "after" is `0429a5e0`: borrowed key probes
(#1126), word-sized values stored in the table (#1128), and the
read-add-write fused into one probe (#1129). The allocation count no longer
scales with the number of updates at all: at 64 distinct keys the whole run
allocates 523 times whether it performs 20,000 updates or 80,000.

## std hotspot micro-benchmarks (2026-09-10)

Three small programs isolate the std functions that a callgrind profile of
the workloads above put at the top (docs/performance.md, "std profiling
pass"), so a change to one of them can be measured on its own before it is
read off the whole workload:

```
pith build bench/http_head_read.pith && ./bench/http_head_read 20000
pith build bench/csv_encode.pith && ./bench/csv_encode 5000 10
pith build bench/path_clean_count.pith && ./bench/path_clean_count 20000
```

`http_head_read` feeds a stream of keepalive requests through
`std.net.http`'s request reader from a bytes cursor, with no socket.
`csv_encode` encodes std_pipeline's ten-column rows. `path_clean_count`
counts a fixed set of messy paths. Each prints a checksum, and the
instruction-count comparison is `tooling/callgrind_ab.sh` over a binary
built before the change and one built after it, by the same compiler.

A fourth isolates the runtime's typed JSON decode, the largest runtime
row of that profile:

```
pith build bench/json_decode_shapes.pith
./bench/json_decode_shapes small 4 20000    # three-field struct, 4-byte strings
./bench/json_decode_shapes wide 32 20000    # twenty-field struct, 32-byte strings
./bench/json_decode_shapes nested 4 2000    # a struct with a nested struct (nested fill)
./bench/json_decode_shapes list 32 400      # a struct with a List of 32 small objects (list fill)
./bench/json_decode_shapes list_walk 32 400 # the same document through json.parse and decode_object per element
./bench/json_decode_shapes list_scalar 32 400  # a struct with a List[Int] of 32 elements
./bench/json_decode_shapes nested_list 32 400  # a struct with a List[List[Int]] of 32 inner lists of four
./bench/json_decode_shapes map 32 400          # a struct with a Map[String, Int] of 32 members
./bench/json_decode_shapes map_struct 32 400   # a struct with a Map[String, Small] of 32 members
./bench/json_decode_shapes float 4 20000    # four fields, two of them Float
./bench/json_decode_shapes ints 4 20000     # the same layout with Int in their place
```

`json_decode_shapes` decodes one struct shape per run. `size` is the
string value width for the flat shapes and the element count for the
collections. The flat shapes take the runtime's single-pass fill. The
nested shape takes its nested twin, which fills the sub-struct in place,
and the list and map shapes take the collection-aware form of that filler,
which builds the lists, maps and element structs as it reads. `list_walk`
parses the same document into the node pool and decodes each element out
of it, the way a list had to be read before a `List` field was a decode
target, so the pair reads one document two ways.

The night's totals for these programs, each compiled by the compiler at the
start of 2026-09-10 (`0fe497c0`) and by the tip (`099e7e62`, after #1107)
from the same source and run under callgrind, with identical outputs and
checksums:

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

The list shape did not move that night because a list of structs still
went through the node pool by hand (#1110). The list fill that took it
off the pool is measured under "Typed JSON decoding: list fields" in
docs/performance.md.

### The same head read over TLS (2026-09-14)

`bench/tls_head_read` is the TLS twin of `http_head_read`. It cannot skip the
socket the way the plaintext one does, because the head arrives as decrypted
record data, so it handshakes once and then sends `requests` keepalive GETs
down that one connection while a second task drains the answers.

```
pith build bench/tls_head_read.pith
./bench/tls_head_read 1
./bench/tls_head_read 500
```

Run it at 1 and at 500. The one-request run is almost all handshake, so the
difference between the two divided by the request count is what the head read
costs per request. Before #1100 that was 409,441 Ir per request and after it
274,350 (−33.0%), with the whole 500-request program at −27.2% and the
one-request run unchanged within 0.14%.

## Hash kernels (std.hash and std.checksum, per byte)

`bench/hash_kernels.pith` runs one kernel over a few megabytes of fixed
xorshift content, so a change to a kernel can be measured on its own:

```
pith build bench/hash_kernels.pith
./bench/hash_kernels sha256 4       # one of sha1 sha224 sha256 sha384 sha512 fnv1a crc32 adler32
./bench/hash_kernels none 4         # generation only
```

Run the kernel arm and the `none` arm under callgrind. The difference
divided by the byte count is the kernel's cost per byte. The digest line
must agree between a binary built before a change and one built after it.

The table gives instructions per byte over 4 MB, hash arm minus the `none`
arm, with digests identical across the two columns. The first column is
`e80078d0`, after #1115 made fnv1a and crc32 word- and table-driven. The
second is after the sha kernels stopped copying the input into a list and
adler32 stopped reducing per byte (#1116):

| kernel | before | after |
|---|---:|---:|
| `hash.sha1` | 762.6 | 328.7 |
| `hash.sha224` / `hash.sha256` | 1164.0 | 353.3 |
| `hash.sha384` / `hash.sha512` | 796.5 | 201.5 |
| `hash.fnv1a` | 9.6 | 9.6 |
| `checksum.crc32` | 60.0 | 60.0 |
| `checksum.adler32` | 47.0 | 11.0 |

What remained in the sha kernels after that was mostly the strict `w[i]`
and round-constant `List[Int]` reads, about 140 of the 353 in SHA-256,
which were runtime calls. The first section of #1116 gave `xs[i]` and
`bytes[i]` an inline fast path in the IR consumer (the runtime call stays
as the slow path, so a bad index aborts as before). On the same kernels,
with the same std, old backend against new: `hash.sha1` 328.7 → 262.0,
`hash.sha256` 353.3 → 281.6, `checksum.crc32` 60.0 → 47.0 (its table read
is a list subscript), `checksum.adler32` 11.0 → 11.0 (it reads through
`bytes_read_word` and `bytes_get`, both already inline). The digests were
identical.

## zstd codec benchmark (pure-Pith encoder and decoder)

`bench/zstd_codec.pith` times the pure-Pith zstd codec
(`std.compress.zstd_pure_*`) against the crate-backed kernel
(`std.compress.zstd`), in both directions.

Decode: both read the same frames and the outputs are compared byte for
byte before either is timed, so the numbers can't come from doing
different work. Encode: the pure encoder's frame is handed to the
*kernel's* decoder and checked against the input before timing. That tests
interop, since the frame does not round-trip through our own code. The
reported size is a percentage of the kernel's own output, so the throughput
number can't be bought by compressing worse.

The corpus is built at runtime from the repo itself (docs, std sources,
and generated log lines, compressing between 2:1 and 11:1), plus one
deliberately degenerate run-heavy case, reported last and labelled. A
frame that packs 80KB into 22 bytes decodes almost entirely through the
match copy, so it measures memory bandwidth, not entropy decoding.
Treating it as the headline would flatter the pure decoder by a factor
of five.

Each side runs to a wall-clock budget instead of a fixed rep count,
because the kernel decodes some of these frames in tens of microseconds
and a fixed count lands inside the millisecond clock's granularity.

Run it:

```
make zstd-pure-bench
```

Current standing and the optimization history live in
`docs/performance.md`. The decoder has been through three optimization
passes and the encoder one, so the encode column still has the more
obvious headroom. The match finder is now its largest cost.

## Scaling to 32 hardware threads (2026-10-23)

Every figure above this section came from a two-core box. On a 32-thread
Ryzen 9 9950X the green backend starts 32 workers by default, and several
benchmarks ran slower than they did with 4. These are the medians from one
script, run the same way against `main` before and after the scaling fixes:

| benchmark (32 workers unless noted) | before | after |
|---|---:|---:|
| `http_server`, `wrk -t 8 -c 256` | 36k req/s, p99 326 ms | 884k req/s, p99 1.7 ms |
| `http_server_mt`, same load | 36k req/s | 876k req/s |
| `web_hello`, same load | 30k req/s | 716k req/s |
| `http_server_go`, same load | 607k req/s | 615k req/s |
| `green_fanout 200000` | 9,484 ms | 146 ms |
| `green_fanout 200000`, 1 worker | 86-319 ms | 94-361 ms |
| `task_pingpong` | 538 ms | 57 ms |
| `task_pingpong_trials 200 20000`, pairs split | 96% | 2% |
| `chan_fanout 1000000` | 107-129 ms | 93-109 ms |
| `cpu_parallel_sync 200 200000` | 269-380 ms | 351-380 ms |

The single-threaded benchmarks (event ledger, std pipeline, catalog
workload, generic sort, string buffer, zstd) did not move. `green_fanout`
and `chan_fanout` swing by 2x or more between runs on this machine, so their
rows are ranges or medians of interleaved runs, not single readings.

Four problems caused the slowdown, and each fix was measured on its own:

- A fresh spawn woke every worker. One took the task and the other 31
  scanned all 32 queues under their locks and parked again, which cost about
  28 park/wake round trips per spawned task. The scheduler now wakes a
  single searching worker, which hands the search on when more work is
  queued (Go's spinning-worker rule), and idle workers read queue lengths
  from atomics instead of locking every queue.
- Every list and map allocation and free took one global lock in the
  runtime's handle registry. Sharding it 64 ways raised HTTP at 4 workers
  from 141k to 350k req/s.
- Every buffered read and write in `std.io` took one process-wide mutex,
  although the state it guarded is per task. With it removed and the old
  scheduler kept, HTTP at 32 workers reached 272k req/s on 6 cores, capped
  by the next item.
- One reactor thread served every socket, behind one registry lock that
  every wait took. Each worker now has its own epoll poller and parks in it,
  so a socket that becomes ready wakes the worker that owns its task.

With only the two lock fixes, HTTP at 32 workers reads 272k req/s, and with only
the scheduler and poller changes, 57k.

To check a change for this kind of regression, sweep the worker count. A
benchmark that gets slower as workers are added has a shared bottleneck:

```
for w in 1 4 8 16 32; do PITH_GREEN_WORKERS=$w ./bench/green_fanout 200000; done
PITH_GREEN_STATS=1 ./bench/green_fanout 50000   # worker-parks should be near the task count
```

One known gap remains. A task still pins to the worker that first runs it,
so connections land on whichever workers are awake when they arrive: with 32
connections on 32 workers, 13 workers did all the work. Letting idle
workers take part of a busy worker's pinned backlog evened the spread but
did not raise throughput here, because `wrk` ran on the same machine and
used the cores the idle workers would have had. It needs a separate client
machine to measure.

## Global lock sweep (2026-10-24)

After the scaling work above, `bench/lock_sweep.pith` measures shared state
that the HTTP benchmarks do not exercise. Every mode does the same total work
split across `tasks` tasks, so state that scales prints the same time or less
as tasks are added, and state behind one lock prints more:

```
pith build bench/lock_sweep.pith
./bench/lock_sweep spawn 1 1600000
./bench/lock_sweep spawn 32 1600000
```

`bench/tls_server.pith` is `http_server`'s shape over TLS, with the
live-test fixture certificate. Load it with `wrk https://127.0.0.1:<port>/`.

On the 32-thread Ryzen 9 9950X, at 32 workers, before and after the sweep:

| benchmark | before | after |
|---|---:|---:|
| `http_server`, `wrk -t 8 -c 256` | 870k req/s | 1.19M req/s |
| `web_hello`, same load | 696k req/s | 1.04M req/s |
| `tls_server`, same load, keep-alive | 138k req/s | 1.11M req/s |
| `lock_sweep log`: filtered `log.debug`, 1 / 32 tasks | 172 / 431 ms | 32 / 17 ms |
| `lock_sweep spawn`: spawn and await, 1 / 32 tasks | 885 / 2,741 ms | 725 / 157 ms |
| `lock_sweep breaker`: a breaker per task, 1 / 32 tasks | 71 / 1,711 ms | 55 / 23 ms |
| `lock_sweep trace`: span start and end, 1 / 32 tasks | 7,596 / 4,531 ms | 5,870 / 968 ms |
| `lock_sweep labels`: labeled counter write, 1 / 32 tasks | 4,090 / 1,816 ms | 4,080 / 477 ms |
| `metrics_contention 5 4000000 32 labels` | 40,135 ms | 1,198 ms |

Each row was a single global lock on a per-operation path:

- The handle registry still had a lock per shard on every list allocation and
  free, and maps and sets registered there although nothing ever read their
  entries. Maps and sets no longer register, and the list registry uses a
  cheap hash and 256 shards.
- Every `AtomicInt` operation validated its handle through that registry, so
  the lock-free integer took a lock. Cells are now checked by magic word.
- The green task slab had one lock for every spawn, finish and reclaim, and
  the stack pool one for every stack taken and returned. The slab is sharded
  by blocks of task IDs, and each worker keeps its own stack cache.
- `log.debug` took the settings lock before checking the level. The level is
  now mirrored in an atomic and checked first.
- Every rate limiter and circuit breaker shared one module lock. Each now
  has its own.
- `uuid.v7` took a mutex for its counter, twice per root trace span. It is now
  one atomic. Secure random bytes opened `/dev/urandom` per call, and the
  open and close contended on the process's fd table. On Linux it now uses
  `getrandom`. The finished-span buffer is sharded by trace ID.
- Every labeled metric write resolved its series under the shard lock of
  that series, so tasks writing one hot series queued on one lock. Each task
  now caches the series it has resolved.
- Every TLS read and write looked its connection up in one map under one
  lock. The registry is sharded by handle.

Two limits the sweep found are not locks. TLS handshakes are CPU-bound in
RSA-PSS signing and SHA-2, and reach about 14k per second at 32 workers.
`lock_sweep readfile` stops scaling at 4 tasks, the file pool's thread cap,
and a larger cap made it slower, because jobs then landed on sleeping pool
threads instead of spinning ones. The `readfile` mode also exposed a lost wake
in the blocking pool that could park a task forever. That fix is separate.

The remaining module-wide locks are in `std.web.session` (every session
access), the Postgres and MySQL pool registries (every acquire and release),
`std.crypto.jwks`, gRPC stream tracking and `std.shutdown`. None appeared in
the profiles above, but none of these benchmarks exercises them.

## Markdown rendering (2026-11-28)

`bench/markdown_render.pith` renders `docs/*.md`, concatenated and repeated to
11.2 MB, three times with `std.markdown`. At main `fb38baae`, pinned to one
CCD, it renders 62 MB/s with the GitHub extensions on (511 ms median of 5) and
70 MB/s with them off (453 ms). It has no Go or Rust port yet. docs/markdown.md
compares GFM on and off, from the change that added the extensions.
