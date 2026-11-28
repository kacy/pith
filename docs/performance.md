# Performance

These are live notes on where Pith's time goes and what the sprint is doing
about it. The April 2026 audit of the old C-transpiler era is in
`docs/history/performance_audit_2026_04.md`.

Every section below is dated, and its numbers hold for that date and
machine. Rerun them with the helpers in `bench/` before trusting them on
different hardware.

## Where Pith stands

The current numbers are the 2026-11-28 suite run in
[bench/README.md](../bench/README.md): every benchmark against Go 1.27 and
Rust 1.99 ports on a Ryzen 9 9950X, pinned to one CCD, with every runner on
one build (main at `fb38baae`). A few rows from it, as median wall time:

| Workload | Pith | Go | Rust |
|---|---:|---:|---:|
| JSON decode, 20M small objects | 671 ms | 3697 ms | 1111 ms |
| Event ledger, 200k events | 64 ms | 97 ms | 51 ms |
| Channel fan-out, 1M messages | 40 ms | 50 ms | 466 ms |
| Closure error handling, 2M calls | 280 ms | 182 ms | 161 ms |
| Green fan-out, 2M tasks from main | 645 ms | 241 ms | 392 ms |
| sha512, 256 MB | 243 ms | 188 ms | 208 ms |
| HTTP server at 256 connections | 2.02M req/s, p99 178 µs | 1.04M (net/http), 2.28M (minimal) | 1.82M (hyper), p99 262 µs |
| API gateway, plain forwarding at 64 connections | 432k req/s, p99 356 µs | 289k (net/http), p99 880 µs | No port |

Pith is ahead of Go on JSON decoding, substring search, the event ledger and
catalog workloads, CSV encoding, map updates, channels, spawning inside
tasks, HTTP latency and the API gateway. #1511 moved sha512 onto a native
block kernel, bringing it from 435 ms (2.30x Go) to 243 ms (1.29x Go, 1.17x
Rust). Pith remains behind Go on closures, reference cycles, spawning from
main and concurrent gRPC calls, and behind Rust on most single-threaded
compute. The list of the largest gaps, with their causes, is under "Where
Pith stands" in bench/README.md.

The 2026-11-05 run before this one followed #1315 to #1319, which took the
API gateway's plain forwarding at 64 connections from 118k to 472k req/s,
against Go's 280k. Most of that came from #1316: a mutex unlock no longer
moves the woken task to another worker. The cost is that a task waiting on a
mutex beside CPU-bound tasks on its worker waits longer.
`json_decode_shapes/map` was 15% faster in that run. The details are in
examples/gateway/README.md and bench/README.md.

### The 2-core machine (July to September 2026)

The short version follows, all on the same 2-core machine. It was rerun
2026-08-08, reconfirmed 2026-08-11, rerun 2026-08-22, reconfirmed 2026-08-23
after the module-call leak fix (#901) and the concurrent HTTP server (#902)
landed, and rerun in full on 2026-09-10 at the end of a night of std and
runtime work. (The section "The night of 2026-09-10" below has every program
by instruction count. The rows here are the wall-clock medians of that rerun,
with the comparators reproducing their figures to within a few percent.) The
concurrency rows use the green backend, which is the default on Linux. Rows
marked `PITH_GREEN=0` use the OS-thread opt-out, kept for contrast. The Go,
Rust and Zig columns double as canaries. They are the same programs as in
the 2026-07-29 run, so when one of them moves, the machine moved, not the
language:

| Coordination | Pith | Go | Rust | Zig |
|---|---:|---:|---:|---:|
| chan_fanout, 1M msgs | **65-72 ms** (2026-09-10, 11 and 13, all rounds in the fast mode; earlier reruns bimodal, 96-100 / 59-66) | ~72 ms | 59-69 ms | 203-297 ms |
| chan_fanout, pinned to 1 worker | **~46 ms** | ~71 ms | - | - |
| 20k spawn + join (batches of 64) | **~27 ms / 3.3 MB** | ~8 ms / 3.8 MB | - | - |
| 20k spawn, `PITH_GREEN=0` | ~919 ms / 3.5 MB | - | - | - |

| Services and compute | Pith | Go | Rust | Zig |
|---|---:|---:|---:|---:|
| Catalog workload, 200k requests | **~68 ms** (2026-09-13; ~92 in August) | ~367 ms | ~65 ms | - |
| gRPC unary echo, sequential, 16 B | **4459 calls/s**, p50 219 µs (was 4022) | 4017, p50 233 µs | 2995, p50 325 µs | - |
| gRPC unary echo, conc=8, 16 B | 10598 calls/s, p50 707 µs (was 7560) | 15935, p50 454 µs | 11952, p50 615 µs | - |
| HTTP server under wrk, 60 s | 20.8k req/s, RSS flat (~1.8 B/req) (2026-09-13); was 17.3k, and 14.2-14.4k in August | 20.9k req/s in the same run (regime-dependent, see below) | - | - |
| HTTP sequential latency, 1 connection, p50 | 135µs | 93µs | - | - |
| event_ledger, 200k events | **238 ms (0.61x Go)** (2026-09-13); was 259-263, and 339-344 in August | 393 ms | 106 ms | 125 ms |
| std_pipeline, 50k records | 312 ms (1.29x Go) (2026-09-13); was 344, and 439-448 in August | 242 ms | 137 ms | - |

What moved since July, with the canaries holding: **spawn/await halved**,
from 56 ms to ~27 ms at flat memory, because the argument-ownership fixes
took a per-call allocation out of the spawn path. **event_ledger fell 618 to
570 ms** and **std_pipeline 576 to 480 ms**, closing the gap to Go from 1.28x
to 1.19x and from 1.63x to 1.50x. Channel fan-out and the catalog workload
are unchanged.

On 2026-08-16 **event_ledger fell again, 570 to 351 ms, past Go's 466** (all
four digests matching, medians of 5 interleaved trials). Two changes did it,
one in the compiler and one in the bench. Fixing the typed-decode lowering's
per-call leaks took `parse` from 195 to 175 ms and peak RSS from ~95 to
~67 MB. Rewriting the bench's `gen` phase to build its stream in one
ByteBuffer (the shape the Go version always used with `strings.Builder`,
idiom for idiom) took `gen` from 299 to 120 ms, even with Go's 122. See
bench/README.md for the full phase table.

The HTTP row was wrong for three separate reasons, untangled across
2026-08-22/23, and the row above is the first correct one.

First, the Pith benchmark server was serial by construction. The accept loop
served each connection inline, so the whole benchmark ran on one connection
at a time (throughput equal to the reciprocal of per-request latency, one
core idle) while the Go arm served concurrently. Every earlier Pith figure in
this row measured that flaw rather than the stack. The server now spawns a
task per connection like the Go arm, and reads 14.2k req/s to Go's 21.4k on
the same 60-second spaced runs.

Second, Pith leaked ~64 bytes per request. The cause was a compiler bug in
the ownership of module-qualified call results, not an HTTP bug, and #901
fixed it; RSS is flat again. Third, the Go figure swings between regimes with
box state on this shared host: 21.4k on 2026-08-22 and 31.7k on 2026-08-23 on
the identical 60-second spaced protocol, with Pith at 14.2k and 14.4k on the
same two days. The Pith/Go ratio is therefore whatever regime Go is in
(0.45x-0.66x), and Pith is the stable arm. The sequential-latency row does
not move with that. `bench/http_seq_latency.py` reads one keepalive
connection at a time and is deterministic to a few microseconds. Its
day-to-day band (135-162µs) is the host's latency regime, since the
CPU-bound rows above did not move between the same two days.

The July figures reconcile too: 16.8k was `http_server_mt`, the variant that
already spawned per connection, under `PITH_GREEN=0`, and it reproduced on
2026-08-23 at 16.1k, within 4%. The "throughput halved since July" scare
compared the mt server's number against the serial server's. Per-request
latency, measured with a sequential single-connection probe, has been
~126-136µs from July through today. There was no CPU regression, only the
serial loop and the leak.

A note on launch history for anyone re-measuring: repeated short wrk runs
read progressively lower on this box for either language's server, so use
one long spaced run per arm. The mt-threaded server currently outruns the
green spawning one on this workload (16.1k vs 14.2k). That is a real gap
worth its own measured look, but it is not a regression.

On channel coordination, green beats Zig comfortably but runs ~1.9x behind
both Go and Rust at the default worker count (placement is still decided by
first-resume luck, hence the bimodal spread), and beats Go when pinned to one
worker. Raw spawn/await is still Go's, though by ~3.4x now rather than ~7x.
There, the green backend is ~34x faster than Pith's own OS-thread backend, at
flat memory. The service-shaped rows are the strongest. The catalog workload
(lookups, filtered scans, JSON batch scoring) runs ~3.5x faster than Go's
version, because a flat struct decodes in one pass with no reflection. The
compute rows keep their long-standing shape: modestly behind Go, well behind
Rust and Zig, with string building the remaining gap.

### Why the default worker count stays at the core count

Every channel-shaped benchmark here runs faster pinned to one worker, which
looks like an argument for making one the default. It is not, and the
control settles it: eight spawned tasks doing independent arithmetic, with no
channels and nothing shared:

| 2026-08-17, medians of 3 | 1 worker | 2 workers |
|---|---:|---:|
| chan_fanout, 1M msgs | **46-54 ms** | 64-96 ms |
| green_fanout, 300k spawn+await | **~172 ms** | ~232 ms |
| cpu_parallel, 8 independent tasks | 190 ms | **~107 ms** |

One worker wins the coordination rows by ~1.3-1.8x and loses the parallel
row by **1.77x**, which is near-linear scaling on two cores with an identical
checksum.

The asymmetry settles the question. The channel wins come from the placement
cliff being *avoided*, rather than from one worker being better, because a
pair that cannot land on two workers cannot land split. Defaulting to one
would hard-code that workaround into every program and charge every
genuinely parallel workload 1.77x for it, giving up the point of an M:N
runtime for a bug mitigation. **The default stays `available_parallelism`.**
The channel numbers are a reason to fix placement (see
`concurrency/green/mod.rs` and `wake.rs`), not a reason to change the
default.

`PITH_GREEN_WORKERS=1` remains the right answer for a process that is known to
be handoff-bound and not compute-bound, which is why the flag exists.

### Moving a woken task to its waker (`PITH_GREEN_MIGRATE=1`, 2026-09-06)

The placement cliff has a fix that keeps the worker count: a worker that
wakes a task parked on another worker takes it over, in the same CAS that
claims the task. A pair that talk to each other converge onto one worker
after their first exchange, and a task that never waits is never moved. It
ships behind `PITH_GREEN_MIGRATE=1`, default off. This is what it does on the
handoff benchmarks, in interleaved rounds (off, then on, then the 1-worker
reference within each round), as medians, all on the same 2-core box:

| 2026-09-06, medians of 7 | Flag off | Flag on | 1 worker |
|---|---:|---:|---:|
| chan_fanout, 1M msgs | 93 ms (bimodal: 69, 71, 92-94) | **43 ms** (42-43) | 42 ms |
| task_pingpong, 200k rounds | 48 ms | 48 ms | - |
| green_fanout, 300k spawn+await | 220 ms | 213 ms | - |

A second set of 5 rounds, after the flag-off path was tightened, read the
same way (fan-out 87 ms bimodal against 41 ms flat, fan-out at 1 worker
40 ms, spawn-and-await 220 against 212). It also caught the ping-pong cliff
that the first set had missed: two of the five flag-off runs landed split and
took 413 ms, and the other three took 49-50. The five flag-on runs read 48,
49, 49, 49, 49.

`PITH_PERF_STATS=1` now reports the wake breakdown, and it explains the
whole effect. With the flag off, the fan-out made 105,268 cross-worker wakes
in one run and 8,988 in another (the count is as bimodal as the time). With
the flag on it made 3 and 6, each one a migration, and the rest of the
~8,000 wakes stayed on one worker. The ping-pong's 400,001 wakes were all
same-worker in the runs that were counted, and the flag produces that
arrangement every time rather than some of the time. The spawn-and-await
fan-out has no handoff to fix and moves within noise.

The flag-off path costs nothing. `chan_fanout` under callgrind reads
536.6M instructions against 534.5M for the runtime before the change
(+0.39%), and 536.8M with the flag on, so the program does the same work
either way and the whole difference is where the wakes land.

It is not the default, and the reason is a measurement. Every benchmark
above is handoff-shaped, and the rule that collapses a pipeline onto one
worker is the same rule that pulls parallel work onto one worker when it
synchronizes. A pinned task is never stolen, so once a compute task has been
moved next to the task that woke it, nothing spreads it back out.
`bench/cpu_parallel_sync` has that shape, eight compute tasks that report to
a collector after each chunk, and it shows the effect:

| 2026-09-06, medians of 5 | 1 worker | 2 workers, flag off | 2 workers, flag on |
|---|---:|---:|---:|
| cpu_parallel_sync, 200 chunks of 20k | 448 ms | **259 ms** (247-305) | 337 ms (230-402) |

`PITH_PERF_STATS=1` shows about 280 migrations a run with the flag on, each
one a compute task landing on the collector's worker. So the flag wins the
handoff benchmarks outright and gives back a third of the 2-worker speedup
on this one. The way to have both is to let an idle worker take a `Ready`
task off a peer's pinned queue, which the runtime is now built to allow.
Until that exists, the default stays put.

### Stealing from a busy worker, and migration on by default (2026-10-28)

An idle worker now takes a woken task off a peer's queue when that peer has
been inside one task for 20us, and becomes the task's owner. It never takes
the peer's affinity slot, which holds the other half of a handoff. With that
in place, moving a woken task to its waker is the default
(`PITH_GREEN_MIGRATE=0` turns it off). The table gives medians of 7
interleaved rounds against `b0cccee3`, 16 workers on cores 0-7,16-23 of a
9950X, in ms. The right column also has the cache-line padding of #1232,
which on its own took `lock_sweep/spawn` from 255 to 155 and `green_fanout`
from 1085 to 885:

| Benchmark | b0cccee3 | This change |
|---|---:|---:|
| chan_fanout | 97 (70-130) | 38 |
| cpu_parallel_sync, 200 x 1M | 921 | 658 |
| lock_sweep/spawn | 280 | 63 |
| green_fanout | 1088 | 869 |
| task_pingpong | 397, or 2.2-2.9 s | 399 |

The other concurrency rows and `http_head_read` moved by 3 percent or less.
`task_pingpong` was bimodal before: a run whose two tasks started on
different workers took 2.2-2.9 s, with all four million wakes crossing
workers, and about one run in three did. On one worker the fan-out takes
37 ms and the ping-pong 425 ms, so both now run at their one-worker speed on
sixteen.

Migration alone, without stealing, gave the same fan-out and ping-pong
numbers but took `cpu_parallel_sync` from 911 to 2.6-4.7 s, since every
compute task ended up on the collector's worker.

An idle worker does not count a peer's queue as work it could take when
deciding whether to park. When it did, every idle worker kept spinning beside
a busy pipeline: the fan-out took 0.75 s of CPU for a 49 ms run, against
0.037 s for the same run with them parked. An idle worker still checks its
peers' queues each time it looks for work before parking.

The HTTP server is unchanged within noise: 862k against 863k req/s at 32
connections, 1173k against 1157k at 256 and 1486k against 1479k at 1024,
`PITH_GREEN_MIGRATE=0` against the default, as medians of three 5-second runs
from a Go keep-alive client on the other CCD rather than wrk. The
256-connection row was 1-2 percent lower with migration on in all three
rounds.

That check toggled migration, not stealing, so it missed what stealing cost
the server. Under wrk, the 2026-10-30 suite run read 876k req/s at 32
connections and 1.45M at 256, against 1.22M and 1.83M before this change. A
bisect over the 65 commits since the earlier run stopped here. The thief
spun for up to 20us on every busy peer with a non-empty pinned FIFO, each
time it ran out of work. A loaded server always has such peers, so its idle
workers spent their time spinning on them. With `steal_pinned` turned off,
the server read 1.22M and 1.98M again. The thief now records each peer's
resume count and the time it read it, and takes a task on a later pass if the
count has not moved for 20us. Between passes it parks for 20us instead of
spinning. A worker that waits in epoll rounds that park up to a millisecond.
The server went back to 1.21M and 1.97M req/s, and the concurrency rows above
stayed within noise.

### What preemption safe-points cost, now that they are on (2026-09-07)

Green tasks are preemptible in every build unless it says
`PITH_GREEN_PREEMPT=0`. The backend puts a safe-point before each loop
back-edge (load a process-global byte, test it, branch over a call), so a
compute-only task cannot hold its worker forever. What follows is the cost,
measured off against on with the same source and the same runtime, with the
two arms differing only in that environment variable at build time.

Instruction counts come first, since a compile-time change is exactly the
case where wall time on this box says whatever it likes. Each row is one
`tooling/callgrind_ab.sh` pair, with `PITH_GREEN=0` unless the row says
otherwise:

| 2026-09-07, callgrind | Safe-points off | On | Delta | Safe-points in the binary |
|---|---:|---:|---:|---:|
| Event ledger, 200k events | 3,390,966,577 | 3,393,272,496 | +0.07% | 232 |
| Channel fan-out, 1M messages | 684,679,434 | 685,965,123 | +0.19% | 87 |
| Catalog workload, 200k iterations | 1,381,072,404 | 1,386,079,521 | +0.36% | 139 |
| Compiler compiling itself | 22,493,294,294 | 22,611,240,269 | +0.52% | 824 |
| std pipeline, 200k records | 4,285,492,116 | 4,364,667,321 | +1.85% | 206 |
| Channel fan-out, 1M, under green | 554,038,479 | 604,481,604 | +9.10% | 87 |
| Tight loop, 200M iterations | 1,400,266,874 | 2,600,267,047 | +85.72% | 1 |

The last column is the number of back-edges the build put a check on, counted
from the disassembly. The disassembly is also how the arms were checked for
anything else moving: over the 875 functions the two tight-loop binaries
share, the mnemonic histogram differs by 17 instructions in 2 functions.
Those are the six-instruction safe-point in the loop, and the register moves
and frame that the call it contains forces around it. The safe-point build
links more of the runtime (the park path a safe-point can reach), which is
why the function counts differ at all.

Wall clock on the three shapes where it means something, interleaved with the
order rotated each round, each preceded by `tooling/null_ab.sh` on the off arm to
fix the noise floor:

| 2026-09-07, medians | Off | On | Delta | Null floor |
|---|---:|---:|---:|---:|
| Tight loop, 200M, 9 rounds | 129 ms | 259 ms | **+101%** | ±3.0% |
| Compiler compiling itself, 7 rounds | 2830 ms | 2901 ms | +2.5% | ±1.0% |
| Fan-out 1M, green, 1 worker, 7 rounds | 42 ms | 45 ms | +7.1% | ±2.4% |
| Fan-out 1M, green, 2 workers, 7 rounds | 87 ms | 88 ms | +1.1% | - |

The tight loop is the worst case, and it doubles: six instructions of
safe-point on a seven-instruction body. The wall clock moves further than the
instruction count because the check contains a call. The loop's values have
to survive it, so the allocator puts them in callee-saved registers and gives
the function a frame, visible in the disassembly as a `sub $0x20,%rsp` and
three spills that the off arm does not have.

How bad it gets depends on the body, which is why this benchmark is written
the way it is. The first draft took the sum modulo a prime, and with the
`idiv` in there the same a/b reads +0.4%: the safe-point disappears into the
divider's latency. A second draft left the loop inline in `main` beside a
string format, which had already cost it the registers, and read +33%. So the
loop is a function of its own with nothing in the body but a multiply and two
adds, and what it reports is the ceiling: no body can hide the check better
than nothing at all can. `docs/limitations.md` used to put a degenerate loop
at ~6%, which is presumably one of those softer arrangements; it does not
reproduce on this one.

The compiler moves further on the clock (+2.5%) than on instructions
(+0.52%). The extra work is 824 not-taken branches in a binary that grew
about 1.5%, so this is fetch and layout rather than arithmetic, and it is at
the edge of what the null floor can resolve. Still, the on arm's fastest run
(2836 ms) is slower than the off arm's fastest (2761 ms), which a layout
accident does not usually manage.

The green fan-out is the only row where the safe-point does anything other
than not fire, and it shows the design's weak spot rather than the check's
cost. The request flag is process-global. Once the monitor sets it, every
running task calls the runtime slow path at its next back-edge, where the
check that it is the task which actually overran turns almost all of them
away. `PITH_PERF_STATS=1` now counts both: at one worker that run makes
1,169,107 slow-path calls and preempts 3 times, and at two workers it makes
1,724,997 calls and 16 preemptions. A per-worker request would collapse
that, and is the obvious next step here. Under callgrind the same run reads
+9.1%, which overstates it: the monitor ticks on wall time while the program
runs ~50x slower, so the flag is set for a far larger share of the program's
back-edges than it would be natively.

What the whole cost buys is that a task looping on a value another task has
to publish can no longer wedge the process. `tests/green/starvation.pith`
finished 40 out of 40 runs with safe-points, at one and at the default worker
count, and 0 out of 40 without them.

### The compiler stopped splitting strings on every generic lookup (2026-09-10)

The profile taken for the std pass (#1097) put `pith_string_split_to_list` at
13.1 percent self and 31.6 percent inclusive of a self-compile, and both
callers were the compiler's own code. `checker_resolve_generic_declaration_key`
is asked, for a base name, which key `generic_declaration_map` holds it under
from the module being checked. Its first two probes are map lookups. When
both miss, which is the common case since most names are not generics, it
walked every `module<TAB>name<TAB>source` entry the whole program's
from-imports had produced and split each one on the tab to compare the first
two fields. That came to 8,838 calls, about 1,000 entries each, and 2.1
million splits: 34.4 percent of the compile inside one function.
`ir_csv_contains` in the emitter answers whether a builtin is in one of
fourteen comma-joined tables (the float-returning builtins, the void methods,
the fallible builtins by result shape), and it did so by splitting the table
into a fresh list per question, 106,406 times.

Both places now build the answer once. The checker keeps the entry lists
as they are (the LSP snapshot carries them, and two `starts_with` scans read
them) and adds an index over them: the named entries under
`module<TAB>name` and the wildcard entries under `module`, each holding its
sources in entry order. A lookup is two map probes, and the loop only runs
over the handful of sources that import that exact name. The index is
appended at the two push sites and rebuilt from the lists on a snapshot
restore. The emitter turns each table into a `Map[String, Bool]` the first
time any table is asked, and the comma-joined strings stay as the record.
What is emitted does not change: `tooling/ir_compare.sh` over the corpus
reads identical raw as well as normalized, and the bootstrap seed reached
its fixed point.

Instruction counts come from `tooling/callgrind_ab.sh`. The whole-run rows
are the trunk's driver against this one, each compiling the same tree. The
per-function rows are a before-and-after pair of the driver built from the
same base with `PITH_KEEP_SYMBOLS=1`, as inclusive figures:

| 2026-09-10, callgrind | Before | After | Delta |
|---|---:|---:|---:|
| Compiler compiling itself, whole run | 22,640,305,204 | 12,495,999,868 | −44.8% |
| `checker_resolve_generic_declaration_key`, inclusive | 7,791,228,580 (34.4%) | 74,819,569 (0.60%) | −99.0% |
| The eleven `ir_metadata` predicates over `ir_csv_contains`, inclusive | 2,483,092,479 (11.0%) | 39,608,789 (0.32%) | −98.4% |
| `pith_string_split_to_list`, self | 2,971,980,003 (13.1%) | 48,562,103 (0.39%) | −98.4% |
| `examples/web_login.pith`, whole run | 19,830,250,211 | 17,111,351,847 | −13.7% |
| `examples/generics.pith`, whole run | 103,235,367 | 88,646,810 | −14.1% |

What is left of `resolve_generic_declaration_key` is its fast path: a
module-qualified key built by `module_symbol_prefix`, which concatenates a
character at a time, and the map probes. The next compiler items in the
profile after this are `ir_runtime_call_name` (1.1 percent, a chain of
string comparisons per call lowered) and `from_import_source_module` (0.5
percent, the remaining `starts_with` scan over the same entry lists).

Wall clock is the cross-check, with the two drivers interleaved on the
self-compile after a discarded warm-up, as medians of seven: 6027 ms before,
3767 ms after (−37.5%). A second pair of three rounds later that afternoon
read 6453 ms against 3619 ms. The absolute figures are about twice the
2830 ms the safe-point section reports for the same compile. The two runs are
not comparable, and the paired delta is the figure this row exists for.
### The substring search (2026-09-10)

`pith_cstring_contains` was 10.5 percent of the std pipeline by self
instruction count, and `pith_string_split_to_list` 10.9 percent of the event
ledger, because the runtime compared a needle-length slice at every position
of the haystack: one `bcmp` call per byte, whether or not that byte could
start a match. `index_of` and `replace` had the same loop. #1099 replaces all
four with one search built on libc's `memmem`. The output is byte-identical.
A golden generated from the old runtime over the awkward inputs (an empty
needle, a needle longer than the haystack, a needle at either end, repeated
first bytes, a needle sharing a prefix with an earlier non-match, bytes above
0x7f) reproduces under the new one, natively and under Valgrind, and a
randomized cross-check of the new search against the naive one over 4000
generated pairs agrees on every pair. One input that is not byte-identical is
`"aaa".split("aa")`: the old loop kept scanning inside the delimiter it had
just consumed and aborted the process on the second, overlapping match. It
now returns `["a"]`.

Two searches were measured before one was chosen, both against the old loop
on the same binaries: `memchr` for the needle's first byte followed by a
compare of the rest, and `memmem`. The table gives instruction counts per
call from `bench/substring_search.pith`, inclusive of the primitive, under
callgrind, for the needles `comma` (one byte, no other occurrence in the
filler), `word` (seven bytes, first byte common in the filler) and `repeat`
(a haystack of one byte against a needle that differs from it only at the
end):

| 2026-09-10, Ir per call | Naive | memchr + compare | memmem |
|---|---:|---:|---:|
| comma, 12 bytes, at start / middle / end / absent | 98 / 248 / 428 / 430 | 153 / 153 / 153 / 129 | 115 / 115 / 115 / 114 |
| comma, 80 bytes | 98 / 1,268 / 2,468 / 2,470 | 153 / 167 / 172 / 147 | 115 / 129 / 134 / 132 |
| comma, 4000 bytes | 98 / 60,068 / 120,068 / 120,070 | 153 / 392 / 585 / 558 | 115 / 354 / 547 / 543 |
| word, 12 bytes | 108 / 192 / 318 / 322 | 164 / 164 / 164 / 129 | 253 / 271 / 271 / 228 |
| word, 80 bytes | 108 / 1,620 / 3,174 / 3,178 | 164 / 359 / 619 / 584 | 253 / 319 / 410 / 367 |
| word, 4000 bytes | 108 / 84,836 / 169,606 / 169,610 | 164 / 12,872 / 25,650 / 25,615 | 253 / 4,347 / 8,434 / 8,389 |
| repeat, 12 bytes | 108 / 192 / 318 / 322 | 164 / 294 / 489 / 493 | 253 / 289 / 343 / 316 |
| repeat, 80 bytes | 108 / 1,768 / 3,356 / 3,402 | 164 / 2,659 / 5,099 / 5,137 | 253 / 909 / 1,575 / 1,540 |
| repeat, 4000 bytes | 108 / 84,836 / 169,606 / 169,610 | 164 / 130,955 / 261,656 / 261,660 | 253 / 36,181 / 72,127 / 72,100 |

The checksum of every cell matches across the three arms. The old loop is
cheapest only when the needle is at position zero, and a needle 80 bytes in
already costs it 2,500 instructions. The two replacements are within 40
instructions of each other on the one-byte shape the workloads have, and
within 100 on a short haystack with a longer needle, where `memchr` wins.
They part on the long haystack and the adversarial one: a seven-byte needle
absent from 4000 bytes costs `memmem` 8.4k instructions and the first-byte
scan 25.6k, and a haystack made of the needle's first byte costs them 72k
and 262k, since the first-byte scan then compares the needle at every
position, exactly as the old loop did. `memmem` has no such cliff, and on the
std pipeline as a whole it reads 0.57 percent fewer instructions than the
first-byte scan (3,843,060,836 against 3,865,161,439), so it is the one that
stays. What it leaves on the table is the fixed cost of a multi-byte needle
in a short haystack, about 100 instructions a call. A first-byte scan for
haystacks under a few dozen bytes would recover that if a profile ever shows
it mattering.

Whole workloads under `tooling/callgrind_ab.sh`, the unmodified runtime
against the new one:

| 2026-09-10, callgrind | Before | After | Delta |
|---|---:|---:|---:|
| std pipeline, 50k records | 4,370,463,019 | 3,846,570,175 | -12.0% |
| Event ledger, 200k events | 3,392,821,275 | 2,919,539,338 | -13.9% |
| Substring search, 36 cells, 2000 rounds | 4,703,069,294 | 855,520,761 | -81.8% |

In the std pipeline, `pith_cstring_contains` goes from 458,901,276
instructions (10.50 percent, the figure in #1099) to 68,151,536 (1.77
percent), with `memmem` itself at 46,401,120. In the event ledger,
`pith_string_split_to_list` goes from 369,804,712 (10.90 percent) to
9,600,063. `__memcmp_avx2_movbe`, which the old loops called once per
position, falls from 182,465,421 to 450,186 in the pipeline and from
176,189,954 to 49,980,659 in the ledger. Every other entry in each profile is
unchanged to the instruction, which is what a change confined to one runtime
function should look like.

Those rows were taken at 25d42d75, the base of the profile in #1099, before
the std profiling pass above it in this file (#1097) merged. That pass took
three `contains` calls per field out of the CSV encoder at the call site, so
on the merged tree the pipeline has far fewer searches left to speed up. The
same pair reads 3,448,402,361 before and 3,412,266,260 after (-1.0%), while
the event ledger, which #1097 did not touch, reads 3,392,817,605 to
2,920,641,142 (-13.9%) as before.

Wall clock, as a cross-check only, with medians of 7 interleaved rounds after
a discarded warm-up on the shared 2-core box:

| 2026-09-10, medians | Before | After |
|---|---:|---:|
| std pipeline, 50k records | 487 ms | 412 ms |
| Event ledger, 200k events | 364 ms | 305 ms |
| Substring search, 36 cells, 2000 rounds | 485 ms | 198 ms |

The compiler searches source text with the same primitives, so its output was
checked too: the IR driver linked against the new runtime emits
byte-identical IR for `bench/std_pipeline.pith` (1,088,812 bytes) and for its
own source (5,119,821 bytes).

### The strict subscripts inlined (2026-09-11)

`bytes[i]` lowers to `bytes_get_strict` and `xs[i]` on a `List[Int]` to
`pith_list_get_value_strict`. Until the first item of #1116 both were runtime
calls: 17 and 28 instructions in the callee plus five at the call site. The
non-strict `bytes_get` and `pith_list_get_value_unchecked` had inline fast
paths, and the std kernels had started calling `bytes_get` by name to reach
one (`std/checksum.pith`). The IR consumer now inlines the strict pair too.
The inline path makes the checks the runtime makes (a non-null, aligned
handle with its magic word intact, an 8-byte element for lists, the index in
range) and calls the runtime function as its slow path, so every failure
still prints the diagnostic it printed before; `tests/aborts` pins those
against goldens generated from the previous compiler. Per element on a 2 MB
probe, subscript loop minus the same loop without the read: `bytes[i]` 22 →
17, `xs[i]` 33 → 18. Per byte over 4 MB, hash arm minus a no-hash arm, old
backend against new on the same std: `checksum.crc32` 60 → 47 (its table
read is a list subscript). On the e80078d0 SHA kernels, which copied the
input into a list and read it back by subscript, `hash.sha1` 762.6 → 654.4
and `hash.sha256` 1164.0 → 1020.0. On the #1119 kernels, which hash straight
out of the input and keep `w[i]` and the round constants as list subscripts,
`hash.sha1` 328.7 → 262.0 and `hash.sha256` 353.3 → 281.6. The by-name
`bytes_get` in crc32 and adler32's tail stays: with the subscript, crc32
measures 51 per byte, the four extra instructions being the handle proof the
strict form makes and the plain builtin skips. Whole programs that hash a
few hundred bytes once do not move:
`bench/event_ledger 200000` is 2,698,639,580 → 2,698,573,602 instructions.
The compiler, the heaviest user of `xs[i]`, was checked on wall clock as the
IR contract asks (an earlier inline attempt had been slower there): 21
interleaved pairs of `pith_main check self-host/pith_main.pith`, medians
0.854 s before and 0.849 s after.

### The night of 2026-09-10, measured end to end

The sections above record each change on its own program. This one is the
whole suite run once, at the end, so the night's gains can be read off one
table and nothing hides behind the program it was measured on. Arm A is the
compiler and runtime at the start of the night (`0fe497c0`, before #1093),
and arm B is the tip after #1107 (`099e7e62`). Every bench program's current
source was compiled by both compilers inside their own trees and run under
callgrind. Stdout and checksums agree between arms on every row.

| 2026-09-10, callgrind | Start of the night | Tip | Delta |
|---|---:|---:|---:|
| Event ledger, 200k events | 3,390,962,001 | 2,695,935,191 | −20.5% |
| std pipeline, 50k records | 4,282,530,140 | 3,414,323,685 | −20.3% |
| Catalog workload, 200k iterations | 1,381,071,924 | 963,273,765 | −30.3% |
| HTTP request head read, 20k requests | 8,803,504,445 | 2,099,721,126 | −76.2% |
| CSV encode, 5000 rows × 10 | 1,437,606,575 | 847,752,217 | −41.0% |
| Path clean count, 20k rounds | 1,942,443,996 | 556,783,020 | −71.3% |
| Substring search, 36 cells × 2000 | 4,700,683,879 | 855,542,295 | −81.8% |
| JSON decode, small struct | 41,769,731 | 32,787,957 | −21.5% |
| JSON decode, wide struct | 560,059,104 | 201,137,570 | −64.1% |
| JSON decode, nested struct | 1,059,907,351 | 8,408,038 | −99.2% |
| JSON decode, list of structs (node pool, #1110) | 4,380,580,768 | 4,383,020,015 | +0.06% |
| Generic sort, int keys, random, n=10000 | 15,500,810,052 | 61,569,224 | −99.6% |
| Generic sort, string keys, random, n=10000 | 6,189,870,344 | 69,972,683 | −98.9% |
| Generic sort, string keys, reverse, n=10000 | 71,970,843 | 58,173,248 | −19.2% |
| Cyclic graph, 200k rings | 238,070,249 | 239,270,441 | +0.5% |
| Closure calls | 1,244,274,560 | 1,278,274,713 | +2.7% |
| Task churn, `PITH_GREEN=0` | 264,189,819 | 272,262,357 | +3.1% |
| zstd codec (time-budgeted, reps vary) | 2,147,354,569 | 2,264,680,854 | +5.5% |
| Tight loop, 200M iterations | 1,400,262,583 | 2,600,262,853 | +85.7% |

| Compile time, `ir_driver --combined` | Start of the night | Tip | Delta |
|---|---:|---:|---:|
| The compiler compiling itself (own sources) | 22,460,437,212 | 11,749,822,655 | −47.7% |
| `examples/web_login.pith` (identical input) | 19,660,438,914 | 16,178,617,675 | −17.7% |
| `examples/generics.pith` (identical input) | 101,987,392 | 85,684,942 | −16.0% |

The rows that went up are one change, and it is not a regression from this
work: arm A predates the preemption safe-point default (#1088, "What
preemption safe-points cost" above). Rebuilding the tip with
`PITH_GREEN_PREEMPT=0` puts closure calls at −0.00%, task churn at +0.02%
and the zstd codec at −0.23% against arm A, so those three rows are the
safe-point check and nothing else. The tight loop is its documented worst
case. The generic sort reverse row is the old insertion sort's best case (a
run it recognized and the merge sort did not, #1096, since fixed by run
detection).

The cuts, by row: the three workloads share the typed decode's field lookup
(#1104), the `memmem` substring search (#1103) and the CSV and path helpers
(#1097). The HTTP head read is one bounded `read_until` in place of a byte
loop (#1097). The nested decode fills the sub-struct in place instead of
parsing it into the node pool (#1107). The sorts are the positions-sorting
merge sort with a key cache (#1094). The compiler stopped splitting strings
on every generic lookup (#1101).

The threaded programs are not comparable under callgrind and were timed
instead, interleaved A then B, with two warmups and seven trials, as medians:
chan fanout green 212 vs 221 ms and OS threads 522 vs 530, green fanout 502
vs 486, task pingpong 55 vs 51, CPU parallel sync 1245 vs 1233, task churn
under green 179 vs 184, metrics contention 477 vs 474. All are within the
box's run-to-run spread, and none moved. (Those absolute figures were taken
under load. The quiet-box figures for the same programs are the ones in the
tables at the top of this document.)

The gRPC sweep (`bench/grpc/run.sh`, 20000 calls per cell, three rounds):
sequential 16 B, Pith 4459 calls/s at p50 219 µs against Go 4017 (p50 233)
and Rust 2995 (p50 325); sequential 1 KiB, Pith 4025 (p50 240), Go 4078,
Rust 2776; 8 concurrent over one connection, 16 B, Pith 10598 (p50 707 µs),
Go 15935 (p50 454), Rust 11952 (p50 615); 1 KiB concurrent, Pith 9315, Go
13784, Rust 10367. The sequential rows reproduce the 2026-08 figures with
the usual few percent of drift on the comparators. The concurrent row moved
in Pith's favor, 7560 to 10598 calls/s (56% to 67% of grpc-go), with the
comparators up 15-19%, so about half of that is the box. The third round
read every client's throughput at half its value while every median
latency held, which is the launch-cadence artefact the HTTP section
describes. The rows above are from the first round, and the medians agree
across all three.

Wall clock on the quiet box afterwards, with the comparators reproducing
their published figures: event ledger 339-344 → 259 ms, catalog workload
~92 → ~70 ms, std pipeline 439-448 → 344 ms, HTTP throughput 14.2-14.4k →
17.3k req/s at a flat RSS, sequential p50 135 µs. The generic sort runner's
per-sort speedups at the tip, same binary legacy vs current, medians of 5:
int keys 8.9× at n=100, 123× at 1000, 1146× at 10000 (random); string keys
2.4×, 38×, 253×; string reverse 0.4×, 0.5×, 2.3×.

### The follow-up pass (2026-09-11): hash kernels, StringBuffer, inlined subscripts

The same suite ran once more the next evening. Arm A is now the tip the table
above was measured against (`099e7e62`), and arm B is the tip after #1112
(StringBuffer on a byte buffer), #1115 (fnv1a and crc32), #1119 (the SHA
kernels and adler32) and #1121 (the strict `bytes[i]` and `list[i]`
subscripts inlined in the backend). The method is the same: every bench
program's current source compiled by both compilers in their own trees, run
under callgrind, with outputs and checksums identical on every row.

| 2026-09-11, callgrind | Before | After | Delta |
|---|---:|---:|---:|
| sha256, Ir per byte over 4 MB (net of generating the input) | 1,164 | 282 | 4.1× |
| sha1 | 763 | 262 | 2.9× |
| sha512 | 797 | 152 | 5.3× |
| crc32 | 260 | 47 | 5.5× |
| fnv1a | 56 | 9.6 | 5.8× |
| adler32 | 71 | 11 | 6.4× |
| StringBuffer build, 100,000 × 8 B, whole program | 2,133,353,569 | 474,705,380 | −77.8% |
| StringBuffer build, 10,000 × 200 B, whole program | 823,452,861 | 583,809,975 | −29.1% |
| Generic sort n=10,000, int keys random | 61,569,201 | 55,316,952 | −10.2% |
| Generic sort, string keys random / reverse | 69,972,578 / 58,173,222 | 63,700,401 / 54,158,047 | −9.0% / −6.9% |
| Catalog workload, 200k iterations | 963,273,676 | 924,090,265 | −4.1% |
| std pipeline, 50k records | 3,413,663,688 | 3,284,210,648 | −3.8% |
| HTTP request head read, 20k requests | 2,092,811,954 | 2,027,154,009 | −3.1% |
| Event ledger, JSON decode shapes, closures, task churn, cyclic graph, tight loop, substring search, CSV, path | | | 0.00% |

The four rows from the generic sort down to the head read are the inlined
subscripts showing up wherever a `list[i]` or `bytes[i]` sits in a hot
loop. Nothing else in std changed on those paths. The hash rows are
`bench/hash_kernels` and the StringBuffer rows are `bench/string_buffer`,
both described in bench/README.md with their per-operation tables.

Compile time needed attributing. The suite read `web_login` at +1.6% and
the compiler compiling itself at +2.8% by instruction count, the shape the
old ir-contract note predicted for inlining `xs[i]`. The call counts said
otherwise: the new compiler made 8% more lexer token reads and 26% more
list pushes on the "identical" input, which inlining cannot do. The input
was not identical. `std/hash.pith` grew from 541 to 1,036 lines and
`std/io.pith` by 119 (the verbatim reference kernels and the StringBuffer
tests kept as oracles), both inside `web_login`'s import closure, and the
combined IR grew from 288,704 to 290,663 lines. The cross experiment, the
new driver on the old std, reads 15,900,516,471 against the old driver's
16,180,809,039 on the same std. The compiler is 1.7% faster, and the source
it compiles is larger. An identical-input compile comparison has to copy
the std as well as the program.

Wall clock on the quiet box, with the comparators reproducing their figures:
event ledger 263 ms (Go 384, Rust 105, Zig 131), channel fan-out green
72 ms (Go 72, Rust 63), catalog workload 66 ms direct (Go 372, Rust 67),
HTTP sequential p50 134 µs. The gRPC sweep's medians did not move (222 µs
sequential against Go's 234; 710 µs at 8 concurrent against 452), as
expected for a steady-state client. Five sweeps now show the Pith client's
8-concurrent throughput bimodal at an unchanged median (#1122, later closed
as specific to that two-core host).

One harness finding matters more than the numbers it spoiled. For about
twenty seconds after the full `bench/generic_sort_bench.sh` sweep, the Go
and Rust comparator binaries read 2× and 2.8× their quiet figures while
the Pith binary in the same probe reads its normal one. Inside a chained
run that inverts the catalog, pipeline and wrk ratios, and it did so on
both passes before it was isolated (#1123, which did not reproduce on a
later host and was closed as host-specific). The rule in bench/README.md
stands: a comparator that does not reproduce its published figure means
the run is discarded, whatever Pith reads.

### The map pass (2026-09-13): borrowed keys, inline values, one probe per update

This pass made three changes to how a map stores and reaches its entries,
measured against `19aceac7`, the tip the section above was written at. #1126
probes with the caller's own key bytes and allocates a key only when a new
entry is created, for sets as well as maps. #1128 stores a word-sized value in
the table instead of boxing it, which is the exemption int-keyed scalar maps
already had, extended to every flavor. #1129 fuses `m.insert(k, m[k] + d)`
into a single probe, after a phase-scoped measurement put the hasher at 12.8%
of the event ledger's analyze phase (#1127). The fused form is taken only when
the map and key are the same side-effect-free read, the delta is
side-effect-free, the values are integers, and the enclosing function does not
return a result.

| 2026-09-13, callgrind | Before | After | Delta |
|---|---:|---:|---:|
| Map update, string keys, 20k updates over 64 keys | 28,373,869 | 11,761,225 | −58.6% |
| Map update, bytes keys | 26,275,158 | 9,874,948 | −62.4% |
| Map update, int keys | 6,148,579 | 4,505,198 | −26.7% |
| Event ledger, 200k events | 2,696,464,280 | 2,349,082,242 | −12.9% |
| The compiler compiling itself | 12,081,548,037 | 11,514,442,954 | −4.7% |
| `examples/web_login.pith` | 16,443,477,973 | 15,672,300,484 | −4.7% |
| JSON decode, list of structs | 4,382,915,671 | 4,314,545,954 | −1.6% |
| HTTP request head read, 20k | 2,030,573,462 | 2,000,571,656 | −1.5% |
| Every other bench program | | | 0.00% |

Allocations per update went from four to zero on string and bytes keys: three
key copies and a value box. At 64 distinct keys the probe allocates 523 times
whether it performs 20,000 updates or 80,000, so allocation no longer scales
with update count.

The compile-time rows were not the target. The compiler is a heavy user of
string-keyed maps and gained 4.7% as a side effect. The input was identical in
both arms of that comparison, since the std sources did not change between
them, so the delta is attributable to the change and not to a larger source.

Wall clock on a settled box, with the comparators reproducing their figures:
the event ledger's analyze phase 56 to 23 ms and its total 259 to 238, the
channel fan-out under green at 68 ms against Go's 72, the catalog workload at
68 against Rust's 65, the std pipeline 344 to 312.

Two runner passes were discarded before those figures were taken. In a
degraded window every arm reads about 2x, and a short canary process does not
detect the state: the Go comparator read its usual 371 ms immediately before a
multi-language run in which every arm read double. The phase-level figures
identified the window. In the degraded runs the event ledger's
allocation-heavy `gen` and `parse` phases doubled, while `analyze`, which this
change made allocation-free, held at 23 ms. A run whose comparators do not
reproduce their published figures is discarded, which is the rule in
bench/README.md. The figures above come from the third pass.

### A note on how these are measured

The cross-language harnesses interleave: one round runs every language once,
and a round repeats for the trial count. They used to measure each language to
completion in turn, which on a small shared box hands whoever goes first a
systematic advantage, because ambient load drifts over the length of a run and
pools on whoever goes last. It was worth roughly 2x. The event_ledger runner
also used to re-run each binary once per metric column, so a row's phase times
did not come from the same work and need not sum to that row's own total.

Two habits follow from that. **Discard the first run after a build**, which
competes with whatever the build left behind. And **read the non-Pith columns
before the Pith one**: they are fixed programs, so when they reproduce their
published figures the run is trustworthy, and when they do not, nothing in that
run is.

A third habit applies to changes inside the runtime rather than across
languages: **prefer instruction counts to wall time for attribution.** A
compile-time change once read +5.6% eight rounds running and was noise (a
byte-identical binary at two paths read −2.8% under the same protocol), while
callgrind put it at +0.06%. `tooling/callgrind_ab.sh` runs two arms under
callgrind and reports totals, the delta, and the hottest functions of each.
The linker strips symbols by default, so build the program under test with
`PITH_KEEP_SYMBOLS=1` first or the runtime reports as anonymous addresses.
`tooling/null_ab.sh` times one binary against a copy of itself to show what
the box adds on its own. A real A/B that lands inside that spread has shown
nothing. An instruction count cannot see a memory-ordering stall, so the two
instruments answer different questions, and a barrier's cost still needs the
timing protocol, with the null floor established first.

`PITH_PERF_STATS=1` also counts the count operations. The `rc ops` line
gives the retains and releases of each counted kind (strings keep their own
line), the struct releases that took the sole-owner path, and the weak
references taken and dropped. A second line counts struct allocations by
field count. An optional shell has two fields and a result box three, so that
line shows how much of a program's struct traffic is wrappers and how much is
its own types.

To see where the time goes, `PITH_PROF=<file>` makes any Pith program sample
itself: every 250 us of CPU time it records the instruction that was running,
and at exit it writes those addresses with the process's memory map.
`tooling/prof_report.py <program> <file>` prints the flat profile by function.
Build the program with `PITH_KEEP_SYMBOLS=1` first. Samples inside libc
resolve to its nearest exported symbol, so the allocator's internals show
under borrowed names such as `_dl_mcount_wrapper`; read those as malloc and
free.

`PITH_ALLOC_SITES=<n>` answers which code allocates. For one heap allocation
in every n it walks the stack to the first compiled function, and at exit it
prints the busiest functions by kind: string, list, map, bytes, closure, and
struct by field count. So `struct/3` in a hot function is usually a result box
and `struct/2` an optional shell. Each sample costs a stack walk, so pick an n
that keeps the run short, and build with `PITH_KEEP_SYMBOLS=1`.

## std profiling pass (2026-09-10)

This was a pass over the standard library where every target came from a
profile rather than from reading the code for a pattern. The instrument is
callgrind through `tooling/callgrind_ab.sh`, with each program built under
`PITH_KEEP_SYMBOLS=1` so that a cost lands on a function name rather than an
address. The acceptance number for every change is an instruction count, not
a wall clock. Compiler and runtime were the trunk at `25d42d75` throughout.
The before and after arms of every comparison were built by the same
`self-host/ir_driver` against the unmodified and the modified `std/`.

Five workloads were chosen to cover the library's busy surfaces: JSON,
collections and crypto (event_ledger); CSV, URL, path, gzip and hashing
(std_pipeline); typed JSON decoding on a hot loop (catalog_workload); the
HTTP/1.1 server on one keepalive connection (http_server under the
sequential probe); and the compiler compiling itself, the largest Pith
program there is. For each, the tables list the fifteen most expensive
functions by self and by inclusive instruction count, each tagged as runtime
(Rust, the `pith_*` primitives, the allocator), std (`std_*`, Pith) or the
program's own. Inclusive rows for the process entry chain are dropped.

### event_ledger, 200000 events

`./bench/event_ledger 200000`. Total 3,393,922,717 Ir.

By self cost:

| Ir | % | Function | Kind |
|---:|---:|---|---|
| 504,822,230 | 14.87 | `pith_json_fill_struct` | runtime |
| 369,804,712 | 10.90 | `pith_string_split_to_list` | runtime |
| 245,611,686 | 7.24 | `free` | runtime |
| 177,293,973 | 5.22 | `__memcmp_avx2_movbe` | runtime |
| 175,016,716 | 5.16 | `pith_runtime::runtime_core::struct_weak_drop` | runtime |
| 165,612,328 | 4.88 | `pith_struct_alloc` | runtime |
| 151,028,535 | 4.45 | `malloc` | runtime |
| 115,600,132 | 3.41 | `pith_byte_buffer_write_string_utf8` | runtime |
| 113,012,226 | 3.33 | `_int_malloc` | runtime |
| 104,207,686 | 3.07 | `pith_struct_release` | runtime |
| 102,648,922 | 3.02 | `pith_cstring_release` | runtime |
| 98,600,000 | 2.91 | `std_bytes_ByteBuffer_write_string_utf8` | std |
| 85,318,471 | 2.51 | `__memcpy_avx_unaligned_erms` | runtime |
| 57,400,365 | 1.69 | `generate_events` | program |
| 56,799,732 | 1.67 | `hashbrown::raw::inner::RawTable<T,A>::find` | runtime |

By inclusive cost:

| Ir | % | Function | Kind |
|---:|---:|---|---|
| 1,520,936,638 | 44.81 | `parse_events` | program |
| 1,080,749,223 | 31.84 | `generate_events` | program |
| 690,542,760 | 20.35 | `pith_json_fill_struct` | runtime |
| 623,944,009 | 18.38 | `analyze` | program |
| 573,932,795 | 16.91 | `pith_string_split_to_list` | runtime |
| 516,556,758 | 15.22 | `map_add_str` | program |
| 418,413,079 | 12.33 | `std_bytes_ByteBuffer_write_string_utf8` | std |
| 377,603,562 | 11.13 | `pith_struct_release` | runtime |
| 360,952,624 | 10.64 | `__rustc::__rust_dealloc` | runtime |
| 356,952,457 | 10.52 | `__rustc::__rdl_dealloc` | runtime |
| 352,952,355 | 10.40 | `free` | runtime |
| 277,370,502 | 8.17 | `__rustc::__rust_alloc` | runtime |
| 273,569,804 | 8.06 | `__rustc::__rdl_alloc` | runtime |
| 269,769,106 | 7.95 | `__rustc::__rdl_alloc` | runtime |
| 245,300,500 | 7.23 | `pith_struct_alloc` | runtime |

### std_pipeline, 50000 records

`./bench/std_pipeline`. Total 4,370,009,153 Ir.

By self cost:

| Ir | % | Function | Kind |
|---:|---:|---|---|
| 458,901,276 | 10.50 | `pith_cstring_contains` | runtime |
| 332,720,838 | 7.61 | `pith_bytes_get_strict` | runtime |
| 294,828,020 | 6.75 | `pith_cstring_release` | runtime |
| 255,397,367 | 5.84 | `free` | runtime |
| 214,472,179 | 4.91 | `std_csv_csv_row_from_bytes` | std |
| 182,465,421 | 4.18 | `__memcmp_avx2_movbe` | runtime |
| 177,077,167 | 4.05 | `std_csv_fold_bytes__PipelineStats` | std |
| 170,182,793 | 3.89 | `malloc` | runtime |
| 123,688,931 | 2.83 | `_int_malloc` | runtime |
| 113,112,521 | 2.59 | `pith_runtime::runtime_core::pith_alloc_cstring` | runtime |
| 91,530,210 | 2.09 | `pith_bytes_len` | runtime |
| 88,301,228 | 2.02 | `pith_runtime::runtime_core::struct_weak_drop` | runtime |
| 84,096,282 | 1.92 | `pith_struct_alloc` | runtime |
| 80,300,040 | 1.84 | `std_os_path_clean_part_count` | std |
| 79,282,896 | 1.81 | `<core::hash::sip::Hasher<S> as core::hash::Hasher>::write` | runtime |

By inclusive cost:

| Ir | % | Function | Kind |
|---:|---:|---|---|
| 2,131,291,108 | 48.77 | `std_csv_fold_bytes__PipelineStats` | std |
| 1,359,146,350 | 31.10 | `std_csv_save_chunked` | std |
| 1,359,146,344 | 31.10 | `std_csv_save_bytes_chunked` | std |
| 1,359,144,798 | 31.10 | `std_csv_encode_bytes` | std |
| 1,335,769,842 | 30.57 | `std_csv_encode_row_bytes_to` | std |
| 916,428,851 | 20.97 | `__lambda_0` | program |
| 914,128,805 | 20.92 | `transform_row` | program |
| 713,480,587 | 16.33 | `make_rows` | program |
| 689,567,843 | 15.78 | `std_csv_csv_row_from_bytes` | std |
| 640,841,598 | 14.66 | `pith_cstring_contains` | runtime |
| 506,750,512 | 11.60 | `pith_cstring_release` | runtime |
| 465,450,489 | 10.65 | `std_os_path_clean_part_count` | std |
| 411,733,350 | 9.42 | `pith_list_push_value` | runtime |
| 370,011,155 | 8.47 | `__rustc::__rust_dealloc` | runtime |
| 365,860,382 | 8.37 | `__rustc::__rdl_dealloc` | runtime |

### catalog_workload, 4000 iterations

`./bench/catalog_workload 4000`. Total 43,282,141 Ir.

By self cost:

| Ir | % | Function | Kind |
|---:|---:|---|---|
| 14,572,000 | 33.67 | `pith_json_fill_struct` | runtime |
| 4,069,006 | 9.40 | `build_limited_id_sum_table` | program |
| 4,037,964 | 9.33 | `pith_list_get_value_strict` | runtime |
| 2,937,379 | 6.79 | `pith_struct_release` | runtime |
| 1,222,184 | 2.82 | `pith_cstring_release` | runtime |
| 968,000 | 2.24 | `pith_runtime::json::read_string_end` | runtime |
| 962,120 | 2.22 | `pith_struct_retain` | runtime |
| 752,272 | 1.74 | `free` | runtime |
| 688,632 | 1.59 | `pith_runtime::collections::list::ListImpl::push_value` | runtime |
| 672,000 | 1.55 | `pith_runtime::json::read_int` | runtime |
| 652,000 | 1.51 | `search_checksum` | program |
| 624,000 | 1.44 | `batch_checksum` | program |
| 596,000 | 1.38 | `pith_cstring_eq` | runtime |
| 578,346 | 1.34 | `pith_struct_alloc` | runtime |
| 563,652 | 1.30 | `pith_list_push_value` | runtime |

By inclusive cost:

| Ir | % | Function | Kind |
|---:|---:|---|---|
| 24,612,447 | 56.87 | `bench_batch` | program |
| 24,532,677 | 56.68 | `batch_checksum` | program |
| 18,068,000 | 41.74 | `pith_json_fill_struct` | runtime |
| 15,366,275 | 35.50 | `init_catalog` | program |
| 11,030,183 | 25.48 | `build_query_id_sums` | program |
| 11,027,996 | 25.48 | `build_limited_id_sum_table` | program |
| 5,182,615 | 11.97 | `pith_struct_release` | runtime |
| 4,037,964 | 9.33 | `pith_list_get_value_strict` | runtime |
| 2,164,000 | 5.00 | `search_checksum` | program |
| 2,032,759 | 4.70 | `pith_cstring_release` | runtime |
| 1,740,000 | 4.02 | `__dtor_BatchRequest` | program |
| 1,448,244 | 3.35 | `pith_list_push_value` | runtime |
| 1,243,124 | 2.87 | `pith_struct_alloc` | runtime |
| 1,232,020 | 2.85 | `bench_search_wide` | program |
| 1,180,020 | 2.73 | `bench_search_hot` | program |

### http_server under the sequential probe, 2000 requests

`./bench/http_server` driven by `bench/http_seq_latency.py <port> 2000`, dumped with `callgrind_control --dump` before the kill. Total 1,026,531,527 Ir.

By self cost:

| Ir | % | Function | Kind |
|---:|---:|---|---|
| 101,494,058 | 9.89 | `free` | runtime |
| 83,161,436 | 8.10 | `core::hash::BuildHasher::hash_one` | runtime |
| 65,864,960 | 6.42 | `malloc` | runtime |
| 59,714,188 | 5.82 | `<core::hash::sip::Hasher<S> as core::hash::Hasher>::write` | runtime |
| 41,347,788 | 4.03 | `pith_green_maybe_yield` | runtime |
| 38,557,873 | 3.76 | `pith_runtime::runtime_core::struct_weak_drop` | runtime |
| 36,721,334 | 3.58 | `pith_struct_alloc` | runtime |
| 36,677,613 | 3.57 | `pith_tls_get_or_init` | runtime |
| 25,307,607 | 2.47 | `pith_cstring_release` | runtime |
| 24,679,065 | 2.40 | `pith_struct_release` | runtime |
| 24,562,305 | 2.39 | `hashbrown::raw::inner::RawTable<T,A>::find` | runtime |
| 24,352,650 | 2.37 | `pith_bytes_release` | runtime |
| 19,040,000 | 1.85 | `pith_bytes_get_strict` | runtime |
| 18,304,544 | 1.78 | `pith_runtime::bytes::pith_bytes_from_vec` | runtime |
| 17,290,390 | 1.68 | `pith_mutex_unlock` | runtime |

By inclusive cost:

| Ir | % | Function | Kind |
|---:|---:|---|---|
| 1,024,732,202 | 99.82 | `serve_client` | program |
| 1,024,730,353 | 99.82 | `std_net_http_serve_connection_fd` | std |
| 1,024,705,424 | 99.82 | `std_net_http_serve_connection` | std |
| 861,801,890 | 83.95 | `std_net_http_read_request_buffered_bytes` | std |
| 861,789,884 | 83.95 | `std_net_http_read_request_from_buffered_bytes` | std |
| 666,099,754 | 64.89 | `std_net_http_read_request_head_buffered_bytes` | std |
| 572,248,418 | 55.75 | `std_io_BufferedBytesReader_read_bytes` | std |
| 571,480,412 | 55.67 | `std_io_buffered_bytes_reader_read` | std |
| 570,712,406 | 55.60 | `std_io_buffered_bytes_reader_read_internal` | std |
| 166,114,516 | 16.18 | `std_io_buffered_bytes_reader_consume` | std |
| 142,875,624 | 13.92 | `core::hash::BuildHasher::hash_one` | runtime |
| 141,523,374 | 13.79 | `std_net_http_build_http_request` | std |
| 140,661,254 | 13.70 | `std_io_buffered_bytes_reader_cache_get` | std |
| 139,108,798 | 13.55 | `pith_tls_get_or_init` | runtime |
| 123,287,229 | 12.01 | `__rustc::__rust_dealloc` | runtime |

### The compiler compiling itself

`ir_driver --combined self-host/pith_main.pith`, the driver relinked from its own IR with `pith build-ir` so it keeps its symbols. Total 22,610,437,376 Ir.

By self cost:

| Ir | % | Function | Kind |
|---:|---:|---|---|
| 2,967,350,151 | 13.12 | `pith_string_split_to_list` | runtime |
| 2,878,827,784 | 12.73 | `pith_cstring_release` | runtime |
| 1,663,344,349 | 7.36 | `free` | runtime |
| 1,409,162,612 | 6.23 | `__memcmp_avx2_movbe` | runtime |
| 1,128,384,402 | 4.99 | `malloc` | runtime |
| 719,417,792 | 3.18 | `pith_runtime::runtime_core::pith_alloc_cstring` | runtime |
| 691,069,792 | 3.06 | `<core::hash::sip::Hasher<S> as core::hash::Hasher>::write` | runtime |
| 635,573,362 | 2.81 | `core::hash::BuildHasher::hash_one` | runtime |
| 519,390,097 | 2.30 | `pith_cstring_contains` | runtime |
| 487,951,160 | 2.16 | `pith_cstring_retain` | runtime |
| 474,888,276 | 2.10 | `_int_malloc` | runtime |
| 466,680,480 | 2.06 | `pith_cstring_eq` | runtime |
| 463,269,342 | 2.05 | `pith_runtime::collections::list::ListImpl::push_value` | runtime |
| 421,048,298 | 1.86 | `pith_list_release` | runtime |
| 348,430,997 | 1.54 | `__memcpy_avx_unaligned_erms` | runtime |

By inclusive cost:

| Ir | % | Function | Kind |
|---:|---:|---|---|
| 22,609,072,635 | 99.99 | `emit_combined_file` | program |
| 19,830,722,260 | 87.71 | `ir_emitter_core_ir_block'2` | program |
| 13,279,565,495 | 58.73 | `parse_and_check` | program |
| 8,876,358,523 | 39.26 | `checker_c_run_module` | program |
| 8,165,360,898 | 36.11 | `checker_check_function_body` | program |
| 8,141,326,870 | 36.01 | `checker_c_check_all` | program |
| 8,139,163,574 | 36.00 | `checker_check_top_level_declarations` | program |
| 8,138,337,593 | 35.99 | `checker_check_declaration` | program |
| 8,067,070,602 | 35.68 | `checker_check_block_statement` | program |
| 8,065,908,000 | 35.67 | `checker_check_statement` | program |
| 7,976,333,770 | 35.28 | `checker_c_check_expr` | program |
| 7,969,614,090 | 35.25 | `checker_check_expression_implementation` | program |
| 7,782,001,771 | 34.42 | `checker_resolve_generic_declaration_key` | program |
| 7,666,039,832 | 33.90 | `driver_resolve_imports'2` | program |
| 7,663,176,776 | 33.89 | `driver_resolve_import_list'2` | program |

### What the profile says

Three std hotspots are large and bounded, and the rest of the picture is the
runtime or the program itself.

**The HTTP request head was read one byte at a time.** The server's
inclusive tree puts 84% of a request under
`read_request_buffered_bytes`, and 56% of the whole process under
`buffered_bytes_reader_read_internal`. The head reader called
`reader.read_bytes(1)` in a loop, and every one of those 64 calls per
request paid the reader's registry lookups (a threadlocal map behind
`pith_tls_get_or_init`, whose hashing is the `hash_one` and sip rows), a
mutex, a fresh `ByteBuffer`, a slice of the cache and a `consume` that
re-slices it. That came to 128,001 `read_bytes` calls for 2,000 requests.
The runtime rows under it are real, but the call shape that produces them is
std's.

**The CSV encoder asked three substring questions per field.**
`encode_row_bytes_to` is 31% of std_pipeline inclusive, and 15 of those
points are `pith_cstring_contains`, called three times per field to
decide whether it needs quoting (`,`, `"`, newline), plus two `chr()`
strings built per field to search for and released again. The runtime's
`contains` walks the haystack comparing a slice per position, so each call
on a sixteen-byte field is ~460 instructions and the three of them ~1,400.
There were 1.4 million calls.

**`path.clean_part_count` built the segment list it only needed the length
of.** It was 11% of std_pipeline inclusive for 50,000 calls: per path, a
`substring` per segment, a stack list that `clean_count_segment` returned
back and forth (nine `pith_list_release_handle` per path), and a fresh
list from `without_last_segment` for every `..`.

What was declined, and why:

- event_ledger has one std row, `ByteBuffer.write_string_utf8` at 2.9%
  self and 12.3% inclusive. The wrapper itself is thin. The cost is the
  `Int!` result the compiler boxes for every fallible call: `pith_struct_alloc`
  and `pith_struct_release` under it are 11.7% of the workload. That is
  the result-type lowering, a compiler matter with the same shape at every
  `T!` call site, and the wrapper's own `text.len()` test carries the
  empty-write semantics. The rest of the workload is `pith_json_fill_struct`,
  `pith_string_split_to_list` and the allocator.
- catalog_workload has no std function in either list: its decode is the
  runtime's `pith_json_fill_struct` (42% inclusive).
- The compiler has no std function in the top fifteen either. Its 32%
  under `pith_string_split_to_list` is the compiler's own `.split` use, in
  two functions: `checker_resolve_generic_declaration_key` (22% of the
  compile, 1.9 million splits) and `ir_metadata_ir_csv_contains` (8.4%).
  That is the next compiler pass, not a std one.
- The hypothesis this pass was asked to test, that `s = s + chunk` loops
  in `std/encoding.pith` and the per-nibble hex build in `std/hash.pith`
  are hot, is not borne out on these workloads: no `std_hash_*` or
  `std_encoding_*` function appears in any list. The HMAC in event_ledger
  and the sha256 in std_pipeline each run once over a summary of a few
  hundred bytes. The byte-compare form of string indexing, `s[i] == "x"`
  and `ord(s[i])`, is lowered by the emitter to a `pith_cstring_byte_at`
  read and does not allocate; only `s[i]` used as a value does. They are
  left alone, because a change nobody can measure has only a downside.
- `csv_row_from_bytes` (16% of std_pipeline inclusive) is the next std
  candidate: three `List[Int]` per row and a `pith_bytes_get_strict` call
  per byte. `Row`'s fields are public, so its layout is an interface, and
  it was left for a change that can take that on.
- The runtime's `pith_cstring_contains` and `pith_cstring_index_of` compare
  a slice at every position. A first-byte scan would make both an order of
  magnitude cheaper for short needles, and the slice comparison shows up in
  every workload here (2.3% self of the compile). That is a runtime change
  and is noted for one.

### What changed, and by how much

There is one commit per hotspot. Instruction counts come from
`tooling/callgrind_ab.sh`, with both arms built by the same compiler and
checksums matching. The micro-benchmarks live in `bench/` next to the
workloads.

| Hotspot | Micro-benchmark | Before | After | Per operation | Whole workload |
|---|---|---:|---:|---|---|
| HTTP head read | `bench/http_head_read 20000` | 8,871,737,831 | 2,099,174,323 (−76.3%) | Head read per request 333,050 → 23,300 Ir | http_server, 2000 sequential requests: 1,030,428,586 → 407,378,733 Ir (−60.5%; 515k → 204k per request) |
| CSV quoting decision | `bench/csv_encode 5000 10` | 1,441,314,484 | 881,419,663 (−38.8%) | Quoting test per field ~1,730 → 666 Ir; `encode_row_bytes_to` per row 26,715 → 15,621 Ir | std_pipeline: 4,369,923,908 → 3,814,760,923 Ir (−12.7%) |
| `path.clean_part_count` | `bench/path_clean_count 20000` | 1,961,084,704 | 556,782,410 (−71.6%) | Per call 9,309 → 1,941 Ir (std_pipeline's paths) | std_pipeline: 3,814,222,835 → 3,451,254,104 Ir (−9.5%; −21.0% with the CSV change) |

HTTP. `read_request_head_buffered_bytes` now asks the buffered reader
for everything up to the blank line with
`read_until_bytes_including_bounded(CRLFCRLF, MAX_HEADER_BYTES)`. The
reader finds the terminator in its own cache and keeps what follows for
the body and the next request, which is what the byte loop was doing by
hand. The reader's cap failure is mapped back to `HEADER_LIMIT_MESSAGE`
through a new `io.is_read_until_limit_error`, so the 431 response and the
size-limit tests are unchanged, and any other failure passes through as
before. The reader's `read_until` gained the `errdefer buf.free()` its cap
path was missing. `tests/cases/test_http_head_read_shapes` pins pipelined
requests, bodies, heads that straddle a chunk boundary every way they can,
a truncated stream, an empty stream and oversize heads against the output
of the byte-at-a-time reader; `tests/leaks/leak_http_head_read` is flat.
The TLS fallback's head reader had the same byte loop over a `tls.Conn`
and was not in that profile, because that workload was plaintext. #1100
measured it on its own with `bench/tls_head_read`, which handshakes once and
then pushes keepalive requests down the one connection: one request costs
43,286,962 Ir and 500 cost 247,597,923, so the head read was 409,441 Ir per
request. Reading the head in record-sized chunks and handing the tail past
the blank line back to the conn (a new `tls.Conn.unread_bytes`, which
prepends to the pending tail the read path already keeps) puts those at
43,348,460 and 180,249,269: 274,350 Ir per request, −33.0%, and −27.2% over
the 500-request program. The one-request run moves +0.14%, which is the
fixed cost of the new path and is all a handshake-dominated run sees. The
byte loop's real cost was not the loop itself: every byte took the
connection's read lock and then re-sliced the record's pending tail, which
is quadratic in the size of the record the head arrived in.

CSV. `csv_field_needs_quoting` reads the field's bytes once in place
(`ord(field[i])`, allocation-free) and replaces the three `contains` calls
and their two `chr()` temporaries. The string encoder `encode_row` uses the
same predicate. The leak case written for it found that `encode_bytes`
returned `out.bytes()`, a copy, and never freed its `ByteBuffer`, so every
call left a buffer registered for the life of the thread (125 bytes per
small table). It now returns `take_bytes()`.
`tests/cases/test_csv_encode_quoting` pins every trigger position and
combination byte for byte against the three-search encoder.

Path. At every step, the segment stack `clean()` walks is some `..` entries
that a relative path could not climb past, followed by the real segments, so
two counters describe it. `clean_part_count` keeps `depth` and `climbs`,
reads the path's bytes in place and copies no segment out.
`clean_count_segment` is gone. `tests/cases/test_path_clean_part_count`
compares it with `clean()` and `parts()` over every four-segment
combination from `{a, b, ., .., ""}`, relative and absolute, 1,273 paths.

The path micro-benchmark has a caveat: its inputs are string literals, and a
literal has no length header, so every `pith_cstring_byte_at` on it runs a
`strlen` first (16% of the after arm). std_pipeline's paths are built at
run time and carry the header. The per-call figure in the table is from
std_pipeline for that reason.

The wall clock agrees in direction and, as always on this box, is the
cross-check rather than the claim. The runs were interleaved, as medians of
5 after a discarded warm-up, with the box busier than on the days the tables
above were taken: std_pipeline 941 ms before (810-953) and 727 ms after
(719-817), -23%; the sequential probe's p50 against the two servers, 3,000
requests per trial, 174 us before (173-174) and 134 us after (133-136), -23%.

## Typed JSON decoding: the flat fill (2026-09-10)

The largest runtime row left in the std profiling pass above was
`pith_json_fill_struct`: 33.7% self of catalog_workload at 4000
iterations and 14.9% self of event_ledger at 200000 events. It is the
single-pass decoder behind `json.decode[T]` and `json.decode_text[T]`
for a flat struct whose fields are all required scalars. The emitter
hands it the object bytes, a pre-allocated struct, and one interned
literal per struct type, the field spec `<type><name>,...` in slot
order. It fills the struct in place and returns a mask of the slots it
wrote. The instrument is the same as in the pass above. Both arms were
built by the same compiler at `f75f26a8`, and the before arm was linked
against the unmodified runtime archive. The after arm's numbers include
`read_key_end`, which is the key scan split out into its own symbol.

Reproduced first: 14,572,000 Ir self (33.66%) of 43,298,007 for
catalog_workload, 3,643 per decode of a six-field struct, and
504,822,230 (14.88%) of 3,392,821,004 for event_ledger, 2,524 per
decode of a four-field struct with one unknown key.

### Where the instructions went

Callgrind at instruction level (`--dump-instr=yes`, with the hot blocks
mapped back through `objdump`) split the 3,643 per catalog decode three
ways.

42% went to walking the spec for every key. The lookup was
`spec.split(',')` per key with a slice compare per field, so a key in
slot k walked the bytes of fields 1..k one at a time, through the
iterator's closure, before it could compare anything. That came to 166
spec bytes per six-key object, at about 8 instructions per byte. This
is the part that is quadratic in the field count. A twenty-field struct
paid 18,007 per decode in the fill alone, 82% of it here.

28% went to scanning the key bytes. `read_string_end` inlined into the
field loop came out as a loop carrying eight induction variables, 22
instructions per key byte, against 4 or 5 for the same function called
out of line on a string value.

The rest is per-key glue, the value parsers and the string copies,
which is the work the function exists to do. The spec's length was not
a factor: `pith_cstring_len` on the literal is already a `strlen` call,
recognized by the optimizer.

### What changed

The spec is not split. A key is compared in place against the
field at a cursor, the field after the one that matched last. Only on a
miss is the spec walked from the start, first byte inline before any
memcmp. An encoder writes keys in declaration order, so the cursor hits
on one compare and the spec is never walked. Keys in any other order
still resolve, with one walk per out-of-order key. The key scan is its
own `#[inline(never)]` function so it keeps the plain loop. The spec
format and the emitter are untouched, and so is the mask contract with
the caller.

The leak case written for this function found two ownership holes in
it. A repeated key overwrote a string slot without releasing the first
occurrence's string, which then had no owner; it is released now. The
error struct `pith_json_decode_missing_error` builds for a missing field
carried no destructor, so its message string was stranded on every
failed decode; it now has one. Neither change affects any output.

### By how much

`bench/json_decode_shapes <shape> <size> <rounds>`: a three-field struct,
a twenty-field struct, a struct with a nested struct, and an array of
small objects decoded one element at a time. `size` is the string value
width for the flat shapes and the element count for the list. Per-call
figures are the fill's self cost (plus the key scan, in the after arm)
divided by the rounds. Inclusive adds the string copies and the parsers
it calls.

| Shape | Size | Fill per call, before → after | Inclusive per call | Whole run |
|---|---:|---|---|---|
| Small (3 fields) | 4 | 1,168 → 542 (−53.6%) | 1,450 → 995 (−31.4%) | 41,891,869 → 32,791,847 (−21.7%) |
| Small | 32 | 1,168 → 542 | 1,755 → 1,272 (−27.5%) | 48,008,666 → 38,348,799 (−20.1%) |
| Small | 256 | 1,168 → 542 | 4,237 → 3,530 (−16.7%) | 97,786,964 → 83,646,801 (−14.5%) |
| Wide (20 fields) | 4 | 18,007 → 3,303 (−81.7%) | 24,215 → 6,459 (−73.3%) | 517,461,868 → 162,341,824 (−68.6%) |
| Wide | 32 | 18,007 → 3,303 | 26,350 → 8,398 (−68.1%) | 560,181,505 → 201,141,719 (−64.1%) |
| Wide | 256 | 18,007 → 3,303 | 43,724 → 24,204 (−44.6%) | 907,814,933 → 517,414,871 (−43.0%) |
| Nested (2000 rounds) | 4 / 32 / 256 | Not this path | - | 1,060,420,752 / 1,455,612,979 / 4,613,063,408, unchanged to ±0.00% |
| List (2000 / 400 / 50 rounds) | 4 / 32 / 256 | Not this path | - | 2,753,795,870 / 4,383,023,336 / 4,470,490,540, unchanged to ±0.00% |

The fill's own cost does not depend on the value width in either arm.
The per-call column is the same at every size, and the inclusive column
grows only by the scan and copy of the value. The nested and list shapes
parse into the node pool and never reach this function. Their rows show
that, and they are a reminder of the gap: a nested three-field decode
costs about 530,000 instructions against the flat shape's 1,000, which
is the next thing to look at on this path.

The workloads:

| Workload | Fill self, before → after | Per decode | Whole workload |
|---|---|---|---|
| catalog_workload 4000 | 14,572,000 → 6,272,000 (−57.0%) | 3,643 → 1,568 | 43,297,608 → 34,825,924 Ir (−19.6%) |
| event_ledger 200000 | 504,822,230 → 296,422,230 (−41.3%) | 2,524 → 1,482 | 3,392,820,888 → 3,169,220,724 Ir (−6.6%) |

Checksums and the HMAC digest match across arms. As always here, the
wall clock is only the cross-check. Interleaved, medians of 7 after a
discarded warm-up, on a box another job was sharing: catalog_workload at
200000 iterations took 231 ms before (104-237) and 203 ms after
(77-215), −12%; event_ledger took 743 ms before (618-956) and 731 ms
after (602-898), −1.6%, with the ranges as wide as the effect.

What is left in the 1,568: 86 per key for the key scan, about 175 per
key for the in-place compare, the value parsers and the spills of a
large frame, and three string allocations. That is linear in the bytes
and the fields now, so the next thing on this path is the node-pool
decode beside it.

## Typed JSON decoding: the nested fill (2026-09-10)

The flat fill above did not cover a struct with a nested struct field.
`json.decode[T]` of such a struct parsed the whole document into
std.json's node pool, read the fields back through the object
accessors, recursed into a `decode_object` specialization for the
sub-struct, and rolled the pool back. `bench/json_decode_shapes
nested 4 2000` put that at 530,232 instructions per round against the
flat shape's 4,400. The pool also stranded about 100 bytes per decode
(tests/leaks/leak_json_fill_nested: 84,128 KB at 200k rounds,
327,872 KB at 800k, on the runtime before this change).

The before arm's profile is all node pool. 23% is `hash_one` and 14%
the sip hasher: the pool's maps, keyed by node ID and by
`"handle:slot"` strings. 13% is `tls_globals_get` and 6%
`pith_tls_get_or_init`, because every one of those maps is a
thread-local looked up on each access. 8% is `pith_cstring_release`
and 7% `free`, the slot keys and copied strings dying. Reading the
three fields is a rounding error.

### What changed

The nested shape fills in place too. The decode lowering
(`ir_emit_json_decode_call`) allocates the whole tree of structs ahead
of the call, each with its destructor attached and each sub-struct
already stored in its parent's slot. It hands the runtime one spec that
carries the sub-struct's layout inside the field's entry:
`sname,oaddr(scity,izip),iscore`. `pith_json_fill_struct_nested` walks
the object once, matches keys at a cursor the way the flat fill does,
and on an `o` field recurses into the struct it reads out of the slot.
It never allocates a struct and never learns a destructor's address. On
any failure it releases the root, and the destructors drop the rest.

The values, the messages and the accepted grammar had to stay what this
shape already had, which is the node parser's. The nested filler
therefore mirrors that parser rather than the flat one. Escapes are
decoded (in keys too), a missing colon or comma is accepted, an object
or array may run off the end of the input, a float is rejected for an
int field, an int past the parser's overflow bound fails the whole
parse, and a repeated key reads last-wins. The messages name the field's
kind ("missing string field: city") in the parser's order: any malformed
byte is "invalid json object" before a field is judged, then the fields
in declaration order, depth first. tests/cases/test_json_fill_nested_shapes
pins fifty-seven lines of that against output generated on the runtime
before the change, under memcheck.

The spec is a string a body references, so it has to be in the string
table before any body is emitted, and the string prepass cannot see
which struct a decode targets. The prepass now notes every
`x.decode[T]` call during the walk it already makes for string literals,
resolves each call's type through the checker, and interns the nested
spec for the targets it finds. A program with a decode whose target is a
type parameter (`web.parse[T]`) gets the spec of every struct that has
one. The specs are appended after everything else the prepass interns,
so a program that does not decode keeps the string table it had. The
decode checks that the spec is in the table before taking the path.

### By how much

The instrument is the same as for the flat fill: `tooling/callgrind_ab.sh`,
with the before arm the compiler and runtime at `a811e520` (what merged
as #1104) and the after arm this branch, both building the same bench
source. Per-round figures are the whole run divided by the rounds. The
fill's own cost is the sum of its symbols (`pith_json_fill_struct_nested`,
`nested_fill_object` at both depths, `nested_read_string`).

| Shape | Size | Per round, before → after | Fill's own cost per decode | Whole run |
|---|---:|---|---|---|
| Nested (3 fields, one nested) | 4 | 530,232 → 4,206 | 2,548 | 1,060,463,011 → 8,412,319 (−99.2%) |
| Nested | 32 | 727,767 → 4,765 | 3,108 | 1,455,533,554 → 9,529,169 (−99.3%) |
| Nested | 256 | 2,306,515 → 9,613 | 7,588 | 4,613,029,890 → 19,225,972 (−99.6%) |
| List (2000 / 400 / 50 rounds) | 4 / 32 / 256 | Not this path | - | 2,753,685,378 / 4,382,866,397 / 4,470,290,609, unchanged to ±0.00% |

The list shape decodes each element out of a parsed array with
`decode_object`, which takes a node handle and cannot fill in place, so
it is unchanged. The flat workloads are unchanged too: their decode
targets have no struct field and never reach the new path.

| Workload | Flat fill self, before → after | Whole workload |
|---|---|---|
| catalog_workload 4000 | 4,216,000 → 4,216,000 | 34,825,418 → 34,825,331 Ir (−0.00%) |
| event_ledger 200000 | 229,422,230 → 229,422,230 | 3,170,321,602 → 3,169,220,680 Ir (−0.03%) |

The flat fill's source is untouched, and its cost is the same to the
instruction. An earlier cut of the nested filler shared the flat one's
string scanner and moved the flat fill by a few thousand instructions
through inlining alone, so the nested filler now scans with its own
loop. Checksums match across arms in every row.

The leak case reads 3,004 KB at 200k rounds and 2,888 KB at 800k on this
branch, against 84,128 and 327,872 before.

What is left is a list of structs. `json.decode[T]` has no list field,
so a list is decoded element by element out of a parsed array, and that
parse strands about 64 bytes a round by itself: `json.parse` followed
by `pool_restore`, with nothing decoded, read 6,000 KB at 50k rounds and
15,456 KB at 200k. That is the pool's defect and the next thing on this
path.

## Typed JSON decoding: float fields (2026-09-15)

A `Float` field in a decode target used to be refused at the checker
(E219), so a document with a non-integer number had no direct decode
target. The packed spec now has an `f` letter. Both fillers take the
number span, convert it with std's `f64::from_str` (the conversion
`parse_float` already uses), and write the bit pattern into the slot.
The scalar-object path (an optional `Float?`) and the node-pool
accessors have float forms too. `bench/json_decode_shapes` has two new
shapes that differ only in two fields being `Float` or `Int`, read
against documents of the same length:

| Shape, 20000 rounds, size 4 | Ir | Per decode |
|---|---:|---:|
| `ints` (Int, String, Int, Int) | 41,014,881 | 2,051 |
| `float` (Int, String, Float, Float) | 52,774,922 | 2,639 |
| `small` (Int, String, Bool) | 33,070,985 | 1,654 |
| `nested` (a struct with a struct field, for scale) | 81,361,184 | 4,068 |

So a float field costs about 290 instructions more than an int field:
the UTF-8 check and the f64 parse against `read_int`'s digit loop. The
checksums of the two arms differ by design (the shapes decode different
values), which is why `tooling/callgrind_ab.sh` reports the comparison
void. The per-arm totals are the measurement. Two runs agreed to within
300 instructions.

The same change released the scalar object that the optional-field path
built per decode, which had been stranded whole. A struct with any
optional or defaulted field leaked about a kilobyte per `json.decode`
(tests/leaks/leak_json_fill_float's `Maybe` rounds read 29,756 KB at
20k rounds and 110,336 KB at 80k before, flat after).

## Typed JSON decoding: list fields (2026-09-15)

A `List` field in a decode target used to be refused at the checker, so
a document with an array in it was parsed into the node pool and walked
by hand: `json.parse`, `array_len`, `array_get`, `decode_object[T]` per
element, `pool_restore`. Every child read cost two int-to-string
conversions, a concatenation and a string-keyed map probe, and
`bench/json_decode_shapes list 32 400` read 4.31 billion instructions,
about 10.8 million per decode of 32 three-field objects.

The nested filler now takes a list entry in its spec, `l<name>(<elem>)`,
where the element is a scalar letter or `o#<k>(<sub-spec>)` for a struct.
The runtime builds the list with the element kind's constructor, the one
the emitter picks for a list literal of that type, so a list of strings
or structs owns its elements. Each element struct is allocated with the
destructor the decode's table names under `k`, pushed before its first
field is read, and filled in place. The list sits in its slot before the
first element is read. At every moment, then, the root struct owns
everything built so far, and the one release on failure drops it all.
A filler that stored the list only after a clean read grew
tests/leaks/leak_json_fill_list_fail by 230 MB over 60k rounds; the
fixed one is flat. A target with an optional or defaulted field beside a
list still takes the node-pool path, which reads a scalar list through
`object_require_int_list` and its siblings, and a struct list through a
loop the lowering emits around `decode_object`.

Callgrind, with the before arm the toolchain at ef96045d reading the
document as a top-level array (the only way it could), and the after arm
the same elements under an `items` key decoded into a `List[Small]`
field:

| Shape, Callgrind | Before | After | Per decode after |
|---|---:|---:|---:|
| `list` 32 × 400 | 4,319,363,939 | 35,399,488 (−99.18%) | 86,211 |
| `list_walk` 32 × 400 (after toolchain, node pool) | | 4,356,333,494 | About 10,890,000 |
| `list_scalar` 32 × 400, a `List[Int]` field | | 5,341,412 | 12,079 |
| event_ledger 200000 | 2,357,723,738 | 2,357,717,155 (−0.00%) | |
| catalog_workload 4000 | 32,784,652 | 32,897,996 (+0.35%) | |

The per-decode figures come from a second run at more rounds (2000 and
20000) with the 400-round total subtracted, so program startup is out of
them. That gives 86,211 instructions for 32 small objects, about 2,700
per element, against the 1,654 a `small` decode of the same object costs
on its own. The difference is the element struct's allocation, its push,
and the per-element key lookups against a spec cursor that resets per
object. For 32 integers the figure is 12,079, about 380 per element. The
same document read through the node pool by the new toolchain
(`list_walk`) costs what the old arm did, so the drop comes from the
path, not from the compiler around it. The ledger and catalog workloads
decode flat structs and do not take the list path. Their rows are the
control, and the catalog's +0.35% is code placement: its own functions
emit the identical IR line for line, and the binary only gained the
eight std.json accessors that nothing in it calls. Checksums match
across arms where the arms decode the same values.

## Typed JSON decoding: nested collections and maps (2026-09-16)

This is the last stage of the collection work. A `List[List[T]]` at any
depth and a `Map[String, T]` field, whose value may be a scalar, a
struct, a list or a map, decode in place through the same filler. The
spec gained `m<name>(<elem>)` and the element form `m(<elem>)`. The
runtime builds a string-keyed map tagged with the value kind (the map a
literal of that type gets), inserts each member before its value's
first byte is read, and lets the map release the earlier value under a
repeated key. The depth bound is the parser's 128 levels, and it now
names itself ("json nested deeper than 128 levels") instead of reading
as a malformed document. A target with an optional or defaulted field
beside such a collection takes the node-pool path, where the lowering
walks the parsed nodes with a general builder (container first, each
value stored as it is built).

Callgrind, the same toolchain, `bench/json_decode_shapes`, with
per-decode figures from a 2000-round run with the 400-round total
subtracted:

| Shape, Callgrind | 400 rounds | Per decode |
|---|---:|---:|
| `nested_list` 32 × 400, a `List[List[Int]]` of 32 inner lists of four | 43,885,513 | 107,842 (about 840 per int, 3,370 per inner list) |
| `map` 32 × 400, a `Map[String, Int]` of 32 members | 32,915,486 | 80,768 (about 2,520 per member) |
| `map_struct` 32 × 400, a `Map[String, Small]` of 32 members | 64,451,761 | 158,531 (about 4,950 per member) |

A map member costs what a list element costs plus the key: a hash, a
probe and the map's own copy of the key bytes on the vacant arm. So a
`Map[String, Small]` member sits at about 4,950 against the 2,700 a
`List[Small]` element read for #1146. The stage 2 shapes are the
control, with the A arm the main tree at e22c5a69 building the stage 2
bench source: `list 32 400` 35,406,953 to 35,371,404 (-0.10%),
`list_scalar 32 400` 5,342,138 to 5,312,686 (-0.55%), `nested 4 2000`
8,889,613 to 8,905,144 (+0.17%), `small 4 20000` 33,115,253 to
32,700,739 (-1.25%). The checksums are identical, and none of those
paths changed, so the movement is placement.

## Typed JSON decoding: the flat decoder in Pith (2026-11-01)

`json.decode[T]` of a struct whose fields are all required scalars used
to call `pith_json_fill_struct`, a Rust walk over the object driven by a
packed field spec. The compiler now generates that decoder in IR for
each target type, at the call site (`self-host/ir_json_fill_emit.pith`),
and the Rust fill and `pith_json_decode_missing_error` are deleted. The
values, the accepted grammar and the error messages are the old fill's.
`tests/cases/test_json_fill_generated` pins 138 lines of output that the
runtime fill produced and the generated decoder reproduces: names around
eight bytes long, quotes and backslashes at every offset of a string
value, escaped and unknown keys, values under unknown keys down to the
depth bound, integer saturation, and the Float and Bool edges.

### What stays native and why

The runtime keeps three primitives. The float conversion
(`pith_json_float_bits`) is std's `f64::from_str`, the conversion
`parse_float` uses, so a Float field agrees with the node parser to the
bit. A string is minted by `pith_cstring_from_raw`, which is an
allocation and a copy. The handle test (`pith_bytes_data_ptr`) runs once
per decode. After it, every byte read is `pith_load_u8`, `_u16`, `_u32`
or `_u64`, which the codegen inlines to one load, and the generated code
compares every index against the length itself. The same loop written
with `bytes[i]` pays a magic check, a bounds check and a reload of the
handle on each byte: a prototype of the small shape written that way
read 2,946 instructions per decode against the runtime fill's 1,525. The
value under an unknown key and the two error structs go to `std.json`
(`fill_skip_value`, `fill_missing_error`, `fill_malformed_error`), since
a well-formed document with known keys never reaches them.

The nested and collection fills (`pith_json_fill_struct_nested`,
`pith_json_fill_struct_lists`) did not move. They are one recursive
filler, `nested_fill_object` with its list and map siblings, written to
the node parser's grammar: escapes decoded, a colon or comma optional,
kind-named messages held until the whole document has parsed, and
element errors named by path. A generated version has to reproduce all
of it, and the Rust can only be deleted once all three shapes have
moved, so this change leaves them alone.

### How the generated code is laid out

The decoder knows the field names, so there is no spec to walk. It keeps
a guess, the field after the last one matched, and compares the next
bytes against that field's quoted name a word at a time: `"id"` is one
4-byte compare and `"active"` one 8-byte compare. A document with its
keys in declaration order is read without scanning a key. Any other key
takes the general path, which scans the key and compares its length and
bytes with each field.

The first version gave every field its own copy of the colon, the
whitespace skips, the value parser and the comma. It ran fewer
instructions than the runtime fill on every shape, but at twenty fields
the function was 41 KB, Cachegrind counted 137 I1 misses per decode
against none, and the wide row ran 38% slower. The layout that shipped
keeps three small blocks per field (the guess, the general compare, and
one that marks the field seen and sets its slot) and one shared parser
per value kind, which stores through `pith_store_u64` at the slot held
in a variable. The wide decode is 12.8 KB.

A string value is scanned eight bytes at a time while none of them is a
quote or a backslash, using the exact zero-byte test on the word XORed
with `0x22..` and with `0x5c..`, and then a byte at a time to the match.
With a byte loop alone, `small 256` ran 17% slower than the runtime fill
and `wide 256` 52% slower.

### By how much

The A arm is the toolchain at `0a0a2959` (origin/main), and the B arm is
this branch, both building the same bench source. Wall time is the
median of 11 interleaved runs of the rows in `bench/compare.list`.
Instructions are Callgrind's per-decode count, from two runs at
different round counts, differenced so startup drops out. Checksums are
identical in every row.

| Row | Main | Branch | Wall | Ir per decode, main → branch |
|---|---:|---:|---:|---|
| small 4 × 20M | 804.5 ms | 708.6 ms | −11.9% | 1,500 → 1,158 (−22.8%) |
| wide 32 × 3M | 774.8 ms | 593.2 ms | −23.4% | 9,285 → 6,209 (−33.1%) |
| float 4 × 12M | 870.5 ms | 796.7 ms | −8.5% | 2,500 → 2,292 (−8.3%) |
| nested 4 × 8M | 845.3 ms | 844.9 ms | −0.1% | 4,023 → 4,023 |
| list 32 × 400k | 936.0 ms | 946.7 ms | +1.1% | 83,523 → 83,574 (+0.06%) |
| list_walk 32 × 5k | 246.3 ms | 248.3 ms | +0.8% | 1,551,506 → 1,551,331 |
| map 32 × 300k | 805.3 ms | 816.7 ms | +1.4% | 67,819 → 67,819 |

The nested, list and map rows do not reach the flat decoder, and their
instruction counts match main to within 0.06%. A rerun of 15
interleaved runs put list at +1.2% and map at +1.5% on those same
counts, which is code placement: the runtime archive lost two functions
and gained eight. Other shapes on the same binaries: `ints 4` −12.7%,
`small 32` −25.0%, `small 256` −55.9%, `wide 4` −6.5%, `wide 256`
−54.8%. The workloads that decode flat structs: event_ledger 200000 went
from 1,689,665,937 to 1,646,553,512 Ir (−2.6%; wall +0.4% on a 70 ms
run), and catalog_workload 4000 from 33,121,484 to 30,058,548 Ir
(−9.2%).

`cranelift/runtime/src/json.rs` went from 1,722 lines to 1,435.

## Result boxes (2026-09-24)

A `T!` crosses a call as a heap struct, `[is_ok, ok, err]`: the callee
allocates it, and the caller frees it after reading the flag and the
payload. The two-register result ABI (`PITH_RESULT_REG=1`, above)
removes that box for Pith functions but is still off by default.
Since #1156 and #1163, the ten builtins that report their own outcome
(`parse_int`, `parse_float` and the eight byte-count writes) built the
same box in the runtime on every call.

### Where the boxes are

Callgrind at 92684dd5, `PITH_KEEP_SYMBOLS=1`, with every `pith_struct_alloc`
caller classified. The runtime's result builders count in full, and so
does a Pith function whose only allocation sites are the three-slot
result tuple the emitter builds (`tooling/callgrind_ab.sh --annotate`
names the callers, and the IR dump names the sites). A released box costs
100 Ir (a dtor-less three-slot `pith_struct_release`, `struct_weak_drop`
included, measured on the same runs), and every box is released exactly
once.

| Workload | Total Ir | Result boxes | Alloc + release Ir | Share | Where they come from |
|---|---:|---:|---:|---:|---|
| `event_ledger 200000` | 2,372,681,848 | 2,400,197 | 410,435,036 | 17.3% | 2,200,000 from the runtime under `ByteBuffer.write_string_utf8`, 200,191 from `ByteBuffer.write_byte`'s sentinel wrap |
| The same, decode boxes | | 200,000 | About 34,000,000 | 1.4% | `json.decode_text[Event](line) catch ...` in `parse_events`, a box built by the inlined decode |
| `std_pipeline 50000` | 3,302,862,259 | 1,301,572 | 222,570,383 | 6.7% | 600,319 `ByteBuffer.write_byte`, 500,012 from the runtime's writes, 100,000 `bytes.substring_utf8`, 100,001 in the CSV writer |
| `catalog_workload 200000` | 926,494,660 | 200,000 | About 35,000,000 | 3.8% | The inlined `json.decode[BatchRequest]` in `batch_checksum`, bound to a local |
| The compiler compiling itself | 12,417,782,888 | 104 | 18,094 | 0.00% | `driver.read_source` and the import walk |

The std profiling pass put `pith_struct_alloc` and `pith_struct_release`
under `ByteBuffer.write_string_utf8` at 11.7% of event_ledger. The boxes
cost the same today in absolute terms: 410 million Ir is 12.1% of the
3.39 billion that run measured, and 17.3% of the 2.37 billion it
measures now, after the map and substring passes took a third of the
rest away. The compiler returns almost nothing through `T!` on its hot
paths, so it has no result boxes to lose.

One place, `ir_maybe_register_result_reg`, decides which functions
return a pair and which build a box. With `PITH_RESULT_REG=1`, a
non-generic, lambda-free, non-`main` free function or plain-impl method
whose ok payload fits one register returns a pair. With the flag off, as
it is by default, every `-> T!` function builds a box.

### The change: builtins return the pair in two registers (#1162)

The ten builtins now return a `#[repr(C)]` struct of two `i64`s, which
every supported target returns in two registers: `rax:rdx` under the
x86-64 System V ABI, and `x0:x1` under AAPCS64 on Linux and on macOS.
Their rows in `runtime_functions.txt` declare `I64,I64` returns, the
consumer's `rcall` resolves runtime functions as well as declared ones,
and the emitter lowers every call to one of them as an `rcall`
(`ir_call_returns_pair`). A `!`, `catch`, `catch:` block, `unwrap_or` or
pair return consumes the pair directly. Every other position folds it
into the box it always had. The error payload is the same owned string,
released on the error arm. The float payload travels as its bits in the
second word, because AAPCS64 returns a mixed `{i64, f64}` struct in
`x0:x1` where a Cranelift `(I64, F64)` signature reads `x0` and `v0`.

Per call, from Callgrind: a probe calls each family 20,000 rounds through
`catch` and through `!` in a fallible helper, all successful, with the A
arm the main tree at 92684dd5.

| Family | Calls | Ir before | Ir after | Per call | `pith_struct_alloc` from the builtin |
|---|---:|---:|---:|---:|---:|
| `parse_int` | 40,000 | 19,853,110 | 12,552,528 | -182.5 | 40,002 to 0 |
| `parse_float` | 40,000 | 27,333,239 | 20,052,630 | -182.0 | 40,000 to 0 |
| `byte_buffer_write`, `byte_buffer_write_string_utf8` | 80,000 | 31,093,757 | 16,733,208 | -179.5 | 80,000 to 0 |
| `file_write`, `file_write_bytes` (`/dev/null`) | 80,000 | 48,875,357 | 34,514,752 | -179.5 | 80,000 to 0 |
| `process_write`, `process_write_bytes` (`cat > /dev/null`) | 80,000 | 79,454,614 | 65,114,033 | -179.3 | 80,000 to 0 |
| `tcp_write`, `tcp_write_bytes` (loopback, a draining reader task) | 80,000 | 481,232,170 | 449,112,976 | Not isolated | 80,000 to 0 |

Three runs of the TCP arm pair went 481 to 449, 526 to 412 and 541 to
402 million Ir. The reader task and the scheduler move the total by more
than the boxes do, so for TCP only the allocation count is a
measurement. The 20,000 allocations left in each after arm are the
probe's own fallible helper returning its box, which is the
Pith-function half of the problem. The runtime's unit tests check the
builtins directly: 2,000 successful parses and 4,000 successful buffer
appends allocate nothing at all.

### The four workloads

| Workload | Before | After | With `PITH_RESULT_REG=1` as well |
|---|---:|---:|---:|
| `event_ledger 200000` | 2,372,681,726 | 2,390,282,114 (+0.74%) | 1,935,834,848 (-18.4%) |
| `std_pipeline 50000` | 3,299,870,014 | 3,303,676,030 (+0.10%) | 3,078,602,971 (-6.7%) |
| `catalog_workload 200000` | 926,494,740 | 926,494,648 (0.00%) | 926,488,148 (0.00%) |
| The compiler compiling itself | 12,417,797,170 | 12,420,884,217 (+0.03%) | 12,429,126,742 (+0.09%) |

On its own, the change moves none of the four, and event_ledger gets
slightly worse. Every hot builtin call in them sits behind a std
wrapper, such as `fn write_string_utf8(text: String) -> Int!: return
byte_buffer_write_string_utf8(self.handle, text)`. A wrapper that returns
a box still has to build one, now in the wrapper from the pair instead
of in the runtime, which costs about 8 Ir more per call. The direct call
sites gain the 180 Ir above. With the wrapper on the register ABI as
well, the box is gone from both ends of that chain, and event_ledger
drops 18.4% with an identical digest: the 2.4 million boxes in the table
are 17.3% of it. The JSON decode boxes in event_ledger and
catalog_workload are built inline by the decode lowering, and neither
change touches them.

Each after figure is compared with the before arm of its own run. The
compiler's +0.03% is one more set lookup per call site the emitter
lowers (`ir_is_pair_builtin`). Its flag column was measured before that
lookup was cut to a single set probe, at about three times the cost.

The flag is still off by default because it has not been under the gate
suite since it landed. The regression corpus with `PITH_RESULT_REG=1`
fails two of 493 cases, on this branch and at 92684dd5 alike, both a
pair-returning function handing back its own result parameter (#1164).
The decode boxes need the decode lowering to hand its (flag, payload) to
a waiting consumer the way the builtins now do, and a `Float!` function
still boxes.

### A forwarded result under the register ABI (#1164)

The two failures were one defect. A pair-returning function that
returned a result box it did not build, such as
`fn forward(x: T!) -> T!: return x`, read the payload out of the box and
handed it out as if it held the box's count on it. The caller's box
still held that same count, so the returned payload and the caller's box
shared one count, and whichever was released first freed what the other
still read. The fix gives the
payload a count of its own whenever the box is not the function's alone
(`ir_emit_return_box_as_pair`, the ownership rule in `docs/ir-contract.md`).
It also teaches the caller's cascade walk that such a callee's `return x`
is a counted extraction, so the caller's local releases its own payload.

`tests/cases/test_result_forward_shapes` covers a parameter, a local bound
from a call, a local bound from the parameter, a match binding, a match
arm and an `if` branch, a field, a method, a list element, two and three
forwarding frames and a generic. Each has an ok and an error result, with
Int, String, List, Map, struct, optional and struct-error payloads.
Before the fix, with the flag on, 11 of its 17 shapes read freed memory
under Valgrind. 10 of those printed another value in place of the
forwarded one, such as `param.temp ok s1` for `ok s5`, and the List
shape crashed (`list index out of bounds`). After the fix, the output
matches the flag-off output line for line and Valgrind is clean.

Callgrind at the branch tip, flag off against flag on, and the flag-on
build against the flag-on build without the fix (c5e569f8):

| Workload | Flag off | Flag on | Change | Fix cost (flag on, with and without the fix) |
|---|---:|---:|---:|---:|
| `event_ledger 200000` | 2,390,245,771 | 1,935,896,836 | -19.0% | +0.00% |
| `std_pipeline 50000` | 3,300,695,904 | 3,075,916,830 | -6.8% | -0.18% |
| `catalog_workload 200000` | 926,495,410 | 926,488,660 | -0.00% | +0.00% |
| The compiler compiling itself | 12,423,402,239 | 12,422,105,790 | -0.01% | +0.01% |

The compiler's output is byte-identical with the flag on and off. With
the flag off, the emitted IR of all 643 corpus programs that build is
identical to c5e569f8. With it on, 183 differ. 181 differ only in two
std functions that return a tracked result local
(`string_buffer_write_line` on its error path and
`http.read_request_bytes`), which now check the box's strong count
before taking its payload. The rest are the forwarding cases themselves.

Three things are left before the flag can become the default. A `Float!`
function needs `result_reg_f`. Interface impls, generic functions and
methods on generic types, functions containing a lambda or a spawn,
`main`, and optional or tuple ok payloads all still return the box. A
result local forwarded through an imported function or a method leaks
its payload with the flag on (#1169), because the caller's walk cannot
read that callee's body. #1167 (a result box held by a struct field or
a list element) leaks with the flag off as well, and a little more with
it on.

### Imported and method callees, and the default (#1169, 2026-10-24)

The caller's cascade walk decides whether a result local releases its own
payload. It read a same-module callee's body to see whether an argument
was only borrowed, but it could not read an imported function's body or
resolve a method call. A result local passed to either one kept the
plain shell release. With the box ABI, that leaked whenever the callee
only read the argument. It balanced when the callee returned the
argument, because the caller's binding of the returned box, which shares
it, released the payload for both. With the register ABI, a returned
result comes back as its payload with a count of its own, and the count
the caller's box holds was never dropped: about 28 MB more peak resident
set between 200k and 800k rounds, one payload per call.

Each module now records, before it emits any body, which result and
optional parameters of its non-generic functions and plain methods pass
the walk (`ir_record_borrow_params`). The walk runs in the declaring
module, where the body's names resolve, and with that function's own
return convention. A caller reads the record for an imported function, a
module-qualified call and a method of a user struct or enum. Modules are
emitted after everything they import, so an imported callee's record is
always there. A callee with no record (a builtin, a generic, a closure
field, an interface default) answers "not safe", as before.

`tests/leaks/leak_result_forward_callee` covers five shapes: an imported
forwarder, an imported forwarder through an alias, a match binding and a
second frame, a method, and methods through an alias and through `self`.
Peak growth between 200k and 800k rounds:

| Shape | Before, register ABI | After, register ABI | After, box ABI |
|---|---:|---:|---:|
| Imported `fwd` | 28,044 KB | -24 KB | -8 KB |
| Imported alias and match | 28,048 KB | 28 KB | -76 KB |
| Imported two frames, plus a read-only callee | 28,044 KB | -20 KB | 84 KB |
| Method `fwd` | 28,256 KB | -24 KB | -48 KB |
| Method alias, `self.fwd`, read-only method | 28,132 KB | -16 KB | -40 KB |

With either ABI, a result local passed only to an imported or method
callee that reads it used to leak its payload. It now releases it.
`tests/cases/test_result_forward_callee` checks values for every callee
kind, including a callee that keeps the box in a struct, which must stay
on the shell release. It is in `MEMCHECK_CASES`.

With that closed, the register ABI is the default. `PITH_RESULT_REG=0`
turns it off, and the `result-box` CI job runs the regression corpus,
the leak gate and the memcheck set that way, so the box path keeps its
coverage. The cases that still return the box are unchanged: `Float!`
functions, generic functions and methods on generic types, interface
impls, functions containing a lambda or a spawn, `main`, and optional or
tuple ok payloads.

Wall time on cores 28-31 with one green worker, interleaved, the median
of 7 rounds for event_ledger and 11 for catalog_workload, with the range
in brackets. Main is 596e0fe5. "Box" is this branch with
`PITH_RESULT_REG=0`:

| Workload | Metric | Main | Box | Register (default) |
|---|---|---:|---:|---:|
| `event_ledger 200000` | gen_ms | 64 [63-66] | 65 [64-65] | 32 [32-33] |
| | parse_ms | 37 [37-38] | 37 [36-38] | 36 [36-37] |
| | total_ms | 112 [111-114] | 113 [111-113] | 80 [79-80] |
| `catalog_workload 2000000` | batch_ms | 287 [284-292] | 287 [285-294] | 295 [292-309] |
| | total_ms | 323 [321-328] | 323 [321-331] | 331 [327-345] |

event_ledger drops 29% with an identical digest, all of it in the
generation phase, where `ByteBuffer.write_string_utf8` stops building a
box per call. catalog_workload's checksum is identical, and its total is
2.5% slower. Callgrind at 200000 iterations puts the two builds 0.35%
apart (901,554,473 against 904,749,472 Ir). Every Pith and runtime
function costs the same, allocation counts included. The whole
difference is glibc's `memcmp` under `pith_cstring_eq`, 22.8 to 26.0
million Ir over the same number of calls. Its hot path returns no Pith
result, so the likely cause is where the compared strings land in
memory, which moves when other allocations change.

## July 2026 hardening, in numbers

Between 2026-07-26 and 2026-07-31 the green backend became the Linux default,
every blocking call got a yield point, an ownership sweep fixed seven
leak/use-after-free defects in the emitter and runtime, and a concurrency
audit of the shared state the flip exposed fixed six more. The table shows
what that changed, each measured before and after on this machine:

| What | Before | After |
|---|---:|---:|
| A co-tenant CPU task while another task waits on DNS | Blocked until the lookup ended | Decoupled (~78 ms) |
| The same, while another task appends to a log file | 257-273 ms | 122-124 ms |
| The same, while another task waits on a child process | 1013-1018 ms | 8-9 ms |
| The same, while two tasks run `process.output` | 2012 ms at 1 worker, 1007 at 2 | 8 ms |
| The same, while another task sleeps a second | 1018 ms | 7-23 ms |
| The same, while a `select` waits on an idle channel | 3015 ms | 20 ms, and the send now arrives in time |
| Twelve concurrent sleepers, 120 ms each | Serialized behind their workers | All wake in ~201 ms |
| One bare TCP connect to a TLS server | Server dead, health checks green | Survives; >512 junk handshakes hold no slots |
| Concurrent HTTPS requests sharing the TLS config registry | Racing handle counter: a handshake could pick up another config's cert and key | Serialized behind one lock |
| `time.delay` with a negative duration | Hung (≈584 million years) | Returns immediately |
| JSON parse per request, 20k docs on one task | 89 MB, degrading | 10 MB flat, ~30% faster |
| Map/list eviction churn, 800k rounds | 38 MB | 10 MB flat |
| `container[expr()]` index keys, 800k | 26-75 MB | Flat |
| `xs.map(f)` result lists, 800k rounds | 269 MB | Flat |
| fn values named in a loop, 800k | 309 MB | Flat |
| Struct values stored in containers, 800k | 318 MB | Flat |

Three of those "leaks" were masking live use-after-free bugs (a lambda stored
in a `List[fn]` segfaulted the moment its leak was fixed; sitegen's tag pages
came out named after the output path when struct stores stopped
over-counting). That is the argument for fixing leaks even when the memory
alone would be tolerable. The sweep is pinned by `make leak-check`, a growth
gate that runs six churn shapes at two round counts in CI and fails on ~2 MB
of drift. It was proven by reverting each fix and watching it go red.

These changes have costs. Single-task tight-loop file reads pay ~2x for the
blocking-pool handoff that keeps file I/O from stalling a green worker, and
event_ledger/std_pipeline drifted a few percent slower as hot paths gained the
releases they had been skipping, so flat memory came at full price.

All numbers come from one 2-core machine, medians of 5 where quick enough to
repeat. The tables below were fully rerun 2026-07-15 (after the ARC
reclamation and weak-reference work), the gRPC table again on 2026-07-19
after the unary-response coalescing and once more on 2026-08-17 with all
three clients re-run together, and std_pipeline again on 2026-07-21 after the
byte-level scanner work. The standout change in the latest rerun was in
std_pipeline: its `transform` phase fell from 347ms to 218ms once the URL and
path scanners stopped minting a one-character string per byte, taking the
workload from 2.0x Go to 1.5x. Earlier work had already put std_pipeline's
peak RSS at parity with Go (239 vs 238 MB, was 1.7x), and the cyclic-graph
benchmark below shows weak references reclaiming reference cycles that
refcounting alone can't. JSON struct decode stays faster than Go's reflection
decode: a flat struct of required scalars decodes in a single pass, filled
straight into the struct.

`bench/chan_fanout`, the coordination benchmark, was the one Pith lost
outright: ~580ms against Go's ~69 for a million messages between eight tasks.
The green wake-path work on 2026-07-26 (details below) brought the green
backend to ~133ms on the 2026-07-29 rerun, ahead of Zig and ~1.8x behind both
Go and Rust, and to ~46ms, faster than Go on this box, when the pipeline is
pinned to one worker. The batch benchmarks measure compute, and Pith is
competitive there. Coordination, long a weak point, is now a strength of the
green backend.

The comparators drift a few percent between days; within a table they are
comparable. Go is 1.24.4 (net/http, encoding/*). Rust is either a pinned crate
set (std_pipeline) or a tiny hand-rolled scanner (catalog), so read those
generously.

`bench/catalog_workload` is service-shaped compute (lookups, filtered
searches, batch JSON) over 200k iterations:

| | Go | Rust | Pith |
|---|---|---|---|
| Total | 386ms | 67ms | **133ms** |

Pith is 2.9x faster than Go and within 2x of Rust (2026-07-15 rerun). The
batch JSON phase (~119ms) dominates. Peak RSS at 200k: Go 10 MB, Rust 10 MB,
Pith 43 MB, because Pith holds the whole catalog resident where the
comparators stream it. The time rose from ~111ms when the old six-field decode
helper was retired for the general single-pass decoder. The specialized helper
was a little quicker but never freed the strings it decoded. The general one
attaches the struct destructor, so it is a touch slower and no longer leaks.

`bench/grpc` runs unary echo calls over TLS on loopback, with the same grpc-go
server for all three clients so the numbers reflect the client. The Pith
client is `std.net.grpc` over its own TLS 1.3, HTTP/2, HPACK, and protobuf,
with no C and no async runtime. All three clients were re-measured together on
2026-08-17, 10,000 calls each, three repetitions with the client order rotated
between them so no client benefits from running first. This bench had an
ordering bias worth ~2x until #678, so the rotation matters:

| Calls/sec | Go | Rust (tonic) | Pith |
|---|---|---|---|
| 16 B, sequential | 3711 | 2716 | **4022** |
| 1 KiB, sequential | 3600 | 2616 | **3638** |
| 16 B, 8 concurrent, one connection | **13434** | 10937 | 7560 |
| 1 KiB, 8 concurrent, one connection | **9643** | 9426 | 6815 |

The latency distribution at 16 B, where Pith looks best:

| 16 B | Go | Rust (tonic) | Pith |
|---|---|---|---|
| Sequential median | 235 µs | 326 µs | **228 µs** |
| Sequential p99 | 633 µs to 1.01 ms | 1.33 to 2.26 ms | **571 to 716 µs** |
| conc=8 median | **479 µs** | 623 µs | 919 µs |

**Sequentially, Pith is now the fastest of the three.** It is ahead of grpc-go
on throughput, on median latency, and by the widest margin on p99, where its
tail is about half Go's and a third of tonic's. That is the whole stack in
Pith: its own TLS 1.3, HTTP/2, HPACK and protobuf, with no C and no async
runtime. That tonic is the slowest of the three sequentially is worth knowing
before treating Rust as the automatic ceiling here.

Concurrency over a single connection is still the weaker spot: eight streams
lift Pith ~1.9x where Go lifts ~3.6x. One number points to the cause. Going
from 16 B to 1 KiB narrows the conc=8 gap from 1.78x to 1.41x while the
sequential columns barely move. A gap that shrinks as the payload grows is a
fixed per-call cost being amortised, not a throughput ceiling. That fixed cost
is the internal pipeline: every frame crosses worker → writer → socket →
reader → worker, and each hop is a thread handoff (a futex wake plus a context
switch). The single reader/writer is correct and required for HPACK ordering.
Coalescing removed the redundant handoffs on that path (below). The ones that
remain are structural, one context switch per pipeline thread per call, so
the way to more parallelism past that is more connections, also below.

Read the conc=8 column with the box in mind. The server runs alongside the
client on a 2-core machine, so eight in-flight calls oversubscribe it by
construction, and that column measures the machine as much as the runtime. It
is a good symptom detector and a bad optimisation target. The sequential
column is the clean measurement, and it is the one Pith wins.

This benchmark paid for itself on the first run. Client sockets had no
`TCP_NODELAY`, so Nagle collided with the peer's delayed acks for a ~40ms
stall per round trip. The Pith client measured 22 calls/sec before the fix and
2269 after. Every request/response path (HTTP, gRPC, the DB drivers) benefits
from that one socket option.

A later perf pass trimmed the client's per-call overhead. The four constant
gRPC request headers are cached, the HTTP/2 writer coalesces queued frames
into one write, and data events share one empty header list. The largest win
was sending a single-frame request body inline instead of on a spawned task,
which drops one OS thread per call. Measured apples-to-apples, best of 7,
sequential 16 B went from ~2550 to ~3120 calls/sec (+23%) and 8-concurrent
from ~4860 to ~6260 (+29%). Over the whole line, that pass moved sequential
Pith from ~2100 to ~2850, roughly ~50% to ~72% of grpc-go. A sweep after the
std thread-safety fixes and the freelist below put it at ~3175 (~77%). The box
is too noisy to resolve the smaller items (cached headers, coalescing, shared
event list) on wall clock, so they stand on counted structural reductions.

A later pass inlined the whole request, not just its body. A client starts
single-threaded: one in-flight call runs synchronously on the caller over the
same lockstep codec the one-shot get() uses, with no reader or writer task and
no channel handoff. It promotes to the multiplexing pipeline once, the first
time a second stream appears (the inline fast-path in
std/net/http2/connection.pith). A controlled before/after put a sequential
unary call at ~340 µs, down from ~368 (~8%). The table above is a fresh
quiet-machine sweep after the change, with 16 B sequential at ~3375 (~83% of
grpc-go).

The first cut of this shipped a flow-control bug worth recording. At promotion
it debited the connection send window by the total body bytes the inline phase
had put on the wire, reasoning that the threaded sender would otherwise
over-count the peer's receive window. But inline is strictly sequential, so by
the time each response was read the peer had already consumed that request and
re-granted the bytes, and the debit double-counted. After a warmup of a few
thousand 1 KiB calls it drove the window ~2 MB negative, and the first
concurrent sends stalled waiting for it to climb back. The 1 KiB/8-concurrent
case fell ~25% (to ~3850), while 16 B, whose debit stayed within the 64 KiB
window, was untouched. The fix is to leave the window alone at promotion, because sends
and grants already net out. That restored 1 KiB/8-concurrent to ~5660, at or
above where it began. The bug hid because the fast-path's own tests used a
20-byte stand-in for the inline total; it took a full gRPC sweep on a quiet
box to surface it.

The newest pass cuts handoffs on the concurrent path. A unary response is
three frames (response headers, the data message, trailer headers), and the
reader used to hand each to the waiting worker as its own event, waking it
three times, where each wake is a kernel context switch. Now the reader marks
a unary stream when it opens, accumulates that stream's frames in a lock-free
per-reader map, and delivers one combined event at end of stream: one wake
instead of three. Streaming RPCs are never marked, so their frames still
arrive one at a time and a server-streaming response stays incremental; it is
never buffered to the end. `perf stat` put context switches per call at 7.8
before and 5.0 after (-35%). The 16 B/8-concurrent case rose ~6025 to ~6832
(+13%) and 1 KiB ~5096 to ~6084 (+19%), at equal or slightly lower CPU, taking
single-connection scaling from ~1.7x to ~2x. That was the last clearly
redundant handoff. The switches left over are one per pipeline thread per call
(the worker blocking for its reply, the reader on the socket, the writer
draining its queue), which is the floor for a reader/writer/worker pipeline
built on real OS threads. Go pays the same handoffs in userspace through its
M:N scheduler, which is most of why it does ~2x the calls at lower CPU.
Closing that gap on one connection would need the same, and the cheaper lever
is more connections.

The 2026-08-16 pass took the syscalls out of the inline path. A traced
sequential call made five socket writes where grpc-go makes ~1.8: HEADERS and
DATA as a TLS record each, then a PING ack (grpc-go probes bandwidth with a
ping alongside each response) and two WINDOW_UPDATEs, each its own record and
syscall. Now the request rides one write, with HEADERS and DATA in a single
record. The read loop queues what it owes the peer (acks and flow-control
grants) and settles it in one write when the response completes, or as soon as
ungranted credit crosses 16 KiB (a quarter of the advertised window) so a
large response never stalls against an exhausted one. A finished stream also
no longer receives a stream-level grant it can never spend. Counted with
strace, the result is exactly 2.0 writes per call, down from 5.0. Interleaved
A/B on the quiet box, 16 B sequential went from ~3.2k to ~4.1k calls/sec
(+24%), median 281 to 228 µs, ahead of grpc-go's ~4.0k on the same box for
the first time. The 8-concurrent case is unchanged, as expected: the threaded
pipeline already batches through the coalescing writer, and its remaining gap
is per-call CPU in the handoff chain, a separate lever.

A follow-up pass indexed the HPACK tables. The encoder used to scan all 61
static entries per header field before consulting its dynamic table (the new
perf-stats counters showed it as ~460 list reads per gRPC call), and the
Huffman decoder built a string key ("length:code") per input bit and hashed it
into a string-keyed map. Now the static table answers from prebuilt maps (the
small dynamic table is scanned first; the two sets are provably disjoint, so
the order is observably identical), and the Huffman map is keyed by a packed
integer on the map's int fast path. Component A/B, 100k iterations each:
encoding a six-field gRPC header list went from 255 to 108 ms (2.4x), and
decoding a block with one Huffman literal from 1.9 s to 250 ms (7.6x). The
gRPC echo moves little (~+4% at conc=8, within noise at conc=1) because its
steady-state fields are all table hits. But any header that changes per
response (date, set-cookie) is a Huffman literal every time from real
servers, and first requests always are.

The deepest cut is an experiment, shipped off by default: a two-register
result ABI behind PITH_RESULT_REG=1. Every `-> T!` function today returns a
heap-allocated three-slot box, and the perf counters put that at ~500 struct
allocations per gRPC call across the byte codecs. Under the flag, a
non-generic, lambda-free function whose ok value is an Int or Bool returns
(is_ok, payload) in two registers instead. A `!` call site consumes the pair
with no allocation at all, and every other context folds the pair into an
ordinary box on the spot, so the ownership rules never change. With the flag
off, not one instruction moves (the full regression suite is byte-for-byte the
box world). With it on, the whole tree passes regressions, the golden
examples, and the leak gate. Measured on the shape it targets (std.binary's
chained fallible reads, a u32+u64 read per round), struct allocations fall
from 14 to 2 per round and wall time drops ~2.3x (54-66 to 23-29 ms per 200k
rounds).

The second phase widened the gate from Int/Bool to every ok payload that fits
one register: String, Bytes, the list/map/set family, and struct handles,
whose reference count transfers through the register exactly as it
transferred through the box's ok slot. `catch` grew the same pair fast path
`!` already had, so a `f(x) catch fallback` loop allocates nothing on either
arm. Float still needs the f64 pair variant, impl methods still box (method
registration never records ok kinds), and the json/config/toml/yaml modules
stay excluded wholesale because their typed-decode emitters call helpers by
raw name. Component A/B at 100k rounds: a String!-returning catch loop falls
from 200,010 to 2 struct allocations, the binary-read catch shape from 400,000
to 0, HTTP/2 frame serialization from 33 to 19 per frame, and HPACK decode
from 59 to 48 per block. End to end, sequential gRPC echo gains ~3% (median
222 to 213 µs) and conc=8 is unchanged, because that path's cost is the futex
handoff chain, not allocation.

The third phase brought in methods and the legacy bridge. A method on a plain
(non-interface, non-generic) impl now registers like a free function. That is
safe because Pith interfaces dispatch statically, so no indirect call can
reach a pair signature. The wrappers around single-register legacy builtins
also stopped costing a box. A `return
byte_buffer_write_byte(...)` used to build a result box just so the pair
return could unpack it, and a caller in a non-pair context built a second
one. The wrap now hands its (flag, payload) registers straight to a waiting
`!`, `catch`, or pair return, and `return f()` forwards an rcall'd pair the
same way. Component A/B at 100k rounds, box world to flag-on: HPACK encode
went from 7 to 1 struct per block, HPACK decode from 59 to 25, frame
serialization from 33 to 16, and protobuf writes from 10 to 5. Sequential gRPC
echo holds at ~+4% (median 226 to 217 µs); conc=8 stays put. Then unwrap_or
and `catch:` blocks joined the same handshake that `!` and inline catch use,
so every result-consuming form now runs box-free on a direct pair-returning
call. A 200k-round unwrap_or/block-catch loop fell from 400,032 struct
allocations to zero. The json/config/toml/yaml family then joined too. Those
modules had been excluded wholesale because their typed-decode emitters call
helpers by resolved name outside the normal call paths, and a plain call into
a pair-returning function reads the flag as the value. The decode call sites
now consult the registry and fold a registered helper's pair back into the box
the decode machinery expects. That put the flag on the event ledger benchmark
for the first time: total 355 to ~320 ms (−10%) with an identical digest.
Still boxed are Float results and interface-impl and generic-impl methods,
the last slices of a default-on decision.

The threaded path then took the write fusion the inline path got above. At
concurrency, a unary request reached the writer task as two messages (the
HEADERS under the encoder mutex, then the DATA behind it) where one would do.
A single-frame body now rides the HEADERS write, and the send credit it needs
is reserved through the mutex the header block already holds, which also saves
a mutex round trip per call. That reservation is all-or-nothing on purpose. A
caller that took a partial grant would have to hand the remainder back on
every error path, and one missed undo silently shrinks the connection window
for the life of the connection, so a connection with no window left simply
falls back to the ordinary flow-controlled send. Bodies past one frame are
untouched; they still stream on their own task while the caller drains the
response. The effect is countable: `PITH_GREEN_STATS=1` over 3300 calls at
conc=8 puts channel wakes at 8174 before and 6625 after. That is about half a
wake per call instead of a whole one, because the coalescing writer was
already absorbing part of the second handoff. Interleaved A/B, 14 alternating
rounds of 20000 16 B calls at conc=8: ~8930 to ~9210 calls/sec (+3%) at 116
to 111 µs of client CPU per call (−4%), with the branch ahead in 10 of the 14
pairs. The run-to-run spread on this box is wider than the gain (8.1k-9.9k
either way), so the counted handoff is the solid half of that result and the
wall clock is the weak half. At conc=1 nothing changes, as that case takes the
inline path.

The green backend (see `docs/concurrency.md`) is that same in-userspace
scheduler, and this benchmark is the case it was built for. Running the
reader, writer, and worker tasks as coroutines on one worker
(`PITH_GREEN_WORKERS=1`) turns every per-call handoff from a futex wake into a
userspace switch. Measured on the 2-core dev box, conc=8, medians of 5 runs,
per-call counts over warmup+calls (2026-07-20 rerun).

**These absolute numbers predate the wake-path work of 2026-07-26** (the
coroutine stack pool, the channel condvar fix, and the slab-free wake), so
they understate green as it stands. Re-running the same shape on 2026-07-27
at a smaller batch put Pith at 5067 calls/sec OS-thread, 8075 at one green
worker (+59%), and 7083 at two (+40%). Those are the same relationships the
table below records (+53% and +41%), so its conclusions hold. The table keeps
its original batch size instead of being replaced with a smaller run that
would not be comparable.

The cross-language shape was rerun in full on 2026-07-29: one local TLS
server (`bench/grpc`), three prebuilt clients each timing 20000 unary calls
over a single connection after 2000 warmup calls, eight concurrent, medians
of three interleaved rounds. Compile time is outside every lane. The Pith
client is built once beforehand, the same as the Go and Rust binaries.

| Payload, conc=8 | Pith | Go | Rust |
|---|---:|---:|---:|
| 16 B, calls/sec | 7476 | **14731** | 12005 |
| 16 B, median / p99 | 954 µs / 2.9 ms | 471 µs / 2.0 ms | 591 µs / 2.3 ms |
| 1 KiB, calls/sec | 7598 | **12366** | 9545 |
| 1 KiB, median / p99 | 957 µs / 2.5 ms | 555 µs / 2.5 ms | 717 µs / 3.3 ms |

All three clients now report the same metrics: per-call median and p99 from
the full sorted latency set, timed with each language's monotonic clock. Pith
sits at ~50-60% of Go and ~60-80% of Rust on throughput, with a per-call
median about 2x Go's. One connection means one reader task, and the two-core
box splits it against the server and seven sibling callers. The earlier
revision of this table printed a Pith "average" computed as wall clock over
total calls, which under eight-way concurrency flattered Pith by nearly an
order of magnitude. The percentile reporting replaced it.

| 16 B, conc=8 | Calls/sec | Ctx-switches/call | CPU |
|---|---|---|---|
| OS-thread | 6887 | 4.9 | 0.98 |
| Green, 1 worker | 10570 | 0.55 | 0.69 |
| Green, 2 workers | 9689 | 0.93 | 0.78 |

| 1 KiB, conc=8 | Calls/sec | Ctx-switches/call | CPU |
|---|---|---|---|
| OS-thread | 6422 | 4.6 | 1.02 |
| Green, 1 worker | 9090 | 0.57 | 0.70 |
| Green, 2 workers | 7215 | 1.72 | 0.93 |

At one worker the mechanism does what it was meant to do. Context switches
per call fall from ~4.9 to ~0.55 (the pipeline stops touching the kernel to
hand a frame between tasks), and that shows up in the wall clock: throughput
rises ~53% at 16 B and ~42% at 1 KiB while CPU drops below a core. That is
close to the CPU grpc-go spends here. On the OS-thread backend Pith reached
about half of Go's throughput at 1.5x the cost; green at one worker gets it to
about three quarters of Go's throughput at Go's efficiency.

The default worker count used to lose this. At two workers green was *slower*
than the OS-thread backend, because a pinned task's wake did a pool-wide notify
that also roused the second worker, which then spun through its (empty) queues
and contended on the shared task lock instead of parking. That was pure
overhead, since a single connection's pipeline pins to one worker and the other
has nothing it can run. Giving each worker its own park spot (a pinned wake now
nudges only the task's owner, and an idle worker no longer counts a peer's
un-stealable pinned work as a reason to stay awake) removes that herd. At two
workers green now beats the OS-thread backend by ~41% at 16 B and ~12% at
1 KiB, at lower CPU (the 16 B margin is the noisier of the two, because the
two-worker path varies run to run).

Two workers still trail one, though, and the reason is locality, not the herd.
The eight request tasks are spawned from the main thread, so they scatter across
both workers while the reader and writer sit on whichever worker promoted the
connection. The roughly half of calls whose task lands away from the
reader/writer pay a cross-worker wake per hop. One worker keeps the whole
pipeline together and pays none, which is why it is fastest for a single
connection. Closing that last gap would need connection-aware placement:
pinning a connection's request tasks to the worker that owns its reader/writer.
The scheduler can't infer that from a plain spawn (a single connection wants its
tasks packed onto one worker; an independent fan-out or a larger connection pool
wants them spread), so it is a task-placement change above the scheduler, not a
scheduler-locality one. On this two-core box a connection pool can't show the
spread paying off anyway, because the client already shares both cores with the
Go server it calls. A pool would need more cores, or a server not fighting the
client for them, to scale. The table at the top of this section was measured on
the OS-thread backend, which was the default when it was taken.

Past a single connection there is a connection pool: `grpc.dial_pool` (and
`http2.open_pool`) opens n independent connections and rotates calls across
them round-robin, the same subchannel trick real gRPC clients use. Each
connection is its own TLS session and reader/writer pipeline, so calls on
different connections run on different cores. The pool needed the std
thread-safety fixes to land first, because a shared per-connection reader was
racing global state and segfaulting under true parallelism. On this 2-core dev
box the pool buys only ~11% at pool=2 and ~15% at pool=4, because eight
concurrent streams already saturate both cores (the client shares them with the
Go server). The pool pays off on hardware where a single connection's ~one-core
pipeline is the real ceiling.

Cutting allocation did *not* help gRPC, which is worth recording. Profiling
flagged ~13% of CPU in malloc/free, and a sequential call does ~800 small struct
allocations (mostly the result box built for every `T!` return). But a
sequential call is ~760 µs, nearly all of it blocked on socket, TLS, and those
thread handoffs, so the allocation is CPU time that barely touches wall clock.
A per-thread struct freelist (recycling small blocks instead of round-tripping
the allocator) moved gRPC by ~0% and struct-alloc-bound compute by up to ~29%.
It is a compute win, not a gRPC one, and it was kept because it is free and
safe. A deeper swing at the same target, returning small results in two
registers instead of a heap box, was prototyped and shelved. It needs boxing
thunks wherever such a function is used as a value (every higher-order call),
which is a large, delicate change for a compute-only gain the freelist already
mostly captures.

The freelist is per thread, and that had a cost the compute numbers did not
show. On the OS-thread backend every spawned task is a fresh thread, so a task
that allocated a batch and returned built a pool, filled it once, and tore it
down without reusing a block. The teardown was the expensive part: callgrind
put the thread-exit destructor's burst of deallocations at 8.1M of 33M
instructions in a task-per-thread benchmark, a quarter of the run, with glibc
consolidating on every one. That made the pool about 35% slower than no pool
for that shape. The pool now releases its slot at thread exit and the next
thread adopts it, blocks and all, so a task's first allocation is a reuse and
nothing is torn down. The retained bytes are also now bounded in bytes
(256 KiB per slot, `STRUCT_POOL_MAX_BYTES`) instead of in blocks per size
bucket, which in practice had meant about 2.1 MiB per thread. Process retention
is that budget times the peak number of live threads. `bench/task_churn` is the
reproducer, and `tooling/callgrind_ab.sh` is how the cost was attributed after
three timing-driven fixes had missed it.

A later thread-safety pass moved `std.binary`'s reader off shared global maps
into its own struct fields (so two HTTP/2 reader threads can't race them). That
also dropped a global-map access per frame parse. A re-measurement put Pith at
~3040 calls/sec sequential 16 B (now matching tonic, ~74% of grpc-go) and ~6300
8-concurrent (~2x scaling, up from ~1.8x); the concurrent path gains more
because it parses more frames. The point of that change was correctness, not
speed. Before it, running several pooled connections in parallel raced the
global maps and crashed. The throughput bump was a free side effect.

`bench/green_fanout` spawns short tasks in batches of 64, awaits each, repeats,
and reads peak RSS from `/proc/self/status`. This is the shape a server takes
when it fans work out per request, and green now bounds its memory. A finished
task hands its slab slot back for the next spawn to reuse and releases the
closure it was spawned with, so the working set is the tasks alive at once, not
the total ever spawned.

| Green, batch 64 | 200k tasks | 500k tasks |
|---|---:|---:|
| Before reclaim | 90 MB | 226 MB |
| After reclaim | 3.1 MB | 3.1 MB |

Before, RSS climbed ~460 bytes per task and never came back: the slab grew one
entry per spawn and each task's closure was never freed. After, it is flat.
With 500k tasks or five million, the peak is the batch. The OS-thread backend
still keeps a record per task it has run, so its RSS still grows with the total
(the closure release lands there too, but the slot does not). Giving that slab
the same reclamation is a tracked follow-up, and until then this bound is a
green-only property.

Spawn *speed* was a separate problem, fixed later. Every green task allocated a
fresh 1 MiB coroutine stack (an mmap plus a guard-page mprotect) and unmapped it
on completion, costing a TLB shootdown across every core. The kernel's
address-space bookkeeping dominated spawn. Finished coroutines now donate their
stacks to a pool and the next spawn reuses one. Spawning 20k tasks and awaiting
them all, medians of 5, interleaved with a Go canary on the 2-core box
(2026-07-26):

| 20k spawn + join | Elapsed | Peak RSS |
|---|---:|---:|
| OS threads (2026-07-26) | ~1450 ms | 174 MB |
| OS threads (2026-07-29) | ~1017 ms | 3.5 MB |
| OS threads (2026-08-08) | ~919 ms | 3.5 MB |
| Green, before the pool | ~580 ms | 10 MB |
| Green (2026-07-29) | ~56 ms | 3.2 MB |
| Green (2026-08-08) | ~27 ms | 3.3 MB |
| Go (batch twin, 2026-07-29) | ~8 ms | 3.8 MB |

The 2026-08-08 halving, 56 ms to ~27 ms, came from the argument-ownership work,
not from anything scheduler-shaped. A container written straight into a call
was stranding its handle, and the spawn path builds one per task. The OS-thread
row moved with the box, not with the fix, which is what says the green number
is real.

Page faults over the run fell 21832 -> ~2500 and context switches 25748 ->
~3000. Shrinking the stack from 1 MiB to 64 KiB changed nothing before the
pool, which is the tell: the cost was the *number* of mappings, not their size.
The 2026-07-29 rerun, against a minimal Go twin of the same batch shape, puts Go
clearly ahead on raw spawn/await (~7x), because Go's scheduler reuses goroutine
stacks with no fd or slab work at all. Green holds an ~18x lead over Pith's own
OS-thread backend at flat memory. The OS-thread RSS collapse from 174 MB to
3.5 MB came free with the July ownership sweep: the task table's records now
release their payloads on eviction like every other container.

`bench/chan_fanout` is the same fan-out shape with cross-language comparators.
Four producer tasks push one million messages through a bounded channel
(capacity 256) and four consumer tasks drain them, folding each into an
order-independent sum. All four languages print the same checksum. Medians of 9
on this 2-core box, 2026-07-26:

| | Pith `PITH_GREEN=0` | Pith green | Go | Rust | Zig |
|---|---:|---:|---:|---:|---:|
| Total (2026-07-26) | 438ms | 171ms | **75ms** | 135ms | 135ms |
| Total (2026-07-29) | - | ~133ms | ~75ms | ~75ms | ~240ms |
| Peak RSS | 3.0 MB | 3.0 MB | 2.0 MB | 2.4 MB | 2.7 MB |

The OS-thread column carried a cost the table above predates. The first channel
reclamation (`#979`, August 2026) let the runtime free a closed and drained
channel by discovering for itself that nothing still named it. It used a
permanent stub, a hazard slot per thread, a limbo list, and a claim taken on
every send and recv. It was measured against the commit before it under the
interleaved protocol: 21 rounds, arm order rotated each round, and a null A/B
of identical binaries first to learn the day's floor. The OS-thread backend was
13% to 23% slower at the median; green, whose workers outlive the work, paid
about 2%. Ablating the pieces one at a time attributed none of it. Removing the
barrier alone, or the entire hazard protocol, moved the benchmark by less than
the null floor, and the per-piece ladder that followed turned out to be
code-layout noise (removing work read *slower* twice). Only the whole was
trustworthy. The fix was to make the runtime not need any of it: channel
handles are now counted by the language like strings and lists (`#1020`), and
the guard, stub, and limbo are gone (`#1021`). The language-level count reads
8.8% faster than the guarded runtime at the median on OS threads and 12% slower
than the pre-reclaim baseline (two earlier runs: 8% and 9%), against a null
floor of 4% to 5% on those days. That recovers about half the regression, and
the rest is real but small. Green is unchanged. The one candidate tried and
refuted for the remainder is padding the count onto its own cache line, which
read worse; the untested one is the park path's two atomics.

Read the rows that oversubscribe OS threads (Pith OS-thread, Rust, Zig) with
the box in mind: eight threads on two cores, so they swing run to run. Across
two suite runs a week apart, Rust moved 135 -> 94 ms and Zig 135 -> 204 with no
code change on either side, and Pith's OS-thread row has been seen anywhere
from ~260 to ~940. The green and Go rows are the stable ones, and the
comparison rests on them.

This table used to read 580 / 782 / 69 / 82 / 201, with Pith last by ~8x and
the green backend slower than OS threads on the shape it was built for. The
story of closing that gap is worth keeping because each step was measured and
two plausible steps failed.

First, the history. Splitting the channel's condvar by role and waking only the
opposite role recovered ~19% in 2026-07. Dropping the global handle-registry
lock measured *2x worse* (it had been accidentally spreading futex contention
across two futexes). The lock-free MPMC ring (2026-07-26) fixed the queueing
but not the elapsed time. The two "obvious" scheduler fixes, spin-before-park
and lock-free run queues, were prototyped or measured out. Spinning was flat at
three budgets on two cores, and per-worker queue contention measured two orders
of magnitude below the slab lock's, so those queues were never the problem.

What closed it was found with counters. At one worker (zero lock contention,
one park, eight futex wakes) the run still made ~2 million channel wake slow
paths. Rust's std condvar is futex-based and pays a `futex(FUTEX_WAKE)` syscall
on every notify even with zero waiters, and the channel notified on every wake.
Green waiters suspend coroutines and never condvar-wait, so every message
bought two syscalls that woke nobody. Counting OS-thread waiters per role under
the channel lock and signalling only when one is parked removed ~70% of the
run. Moving each task's scheduling state into one atomic word in a chunked
side arena then took the slab lock off the wake and resume paths entirely
(contended slab acquires on the wake path: 133 → 0), worth a further ~12% on
the cross-worker mode.

What remains is placement. The green median mixes ~60ms runs (the pipeline
happened to pin to one worker) with ~130-170ms runs (it split), and
`PITH_GREEN_WORKERS=1` gives ~46ms, faster than Go here. Making the scheduler
colocate tasks that talk to each other, instead of leaving it to luck, is the
open lever. Context switches tell the same story: green went from 30.9k to 2.5k
on this run, fewer than Rust's std threads, against Go's 258.

**Result and optional locals.** A `T!` or `T?` bound to a name lowers to a
three-slot heap value holding a flag, the payload, and the error. Releasing one
freed only those three slots, so the payload it owned was never dropped. A loop
that binds a fallible call a million times grew to ~277 MB; the same loop
written `x := call()!` stayed flat, because `!` hands the payload's count to
the caller instead of leaving it in the tuple.

A local now releases that payload when every use of it is provably safe: the
flag reads (`.is_ok`, `.is_err`, `== none`) and the payload reads (`.ok`,
`.err`). A flag read only looks at slot 0. A payload read borrows, and takes a
fresh count where the value escapes the read. Either way the local still holds
the count it was built with, so the cleanup is the only thing that drops it.

| 1M iterations, ~260-byte payload | Before | After |
|---|---:|---:|
| Optional local, probed with `== none` | 277 MB | **2.5 MB** |
| Result local, probed with `.is_err` | 277 MB | **2.5 MB** |
| Result local read through `.ok` | 277 MB | **2.5 MB** |

Anything else keeps the shell-only release and may still leak: a local passed
to a call, returned whole, consumed by `catch` or `unwrap_or`, bound by
`if let`, or mentioned inside a closure. A capture retains the shell and not
the payload, so cascading there would free memory the closure goes on to read.
A result *parameter* never cascades either; it is a borrow, and the caller owns
the payload.

`.ok` was held out when this first landed, because whitelisting it made an
HTTP/2 Valgrind case read freed memory. The cause turned out to be a channel
`try_send` handing its value to another task without taking a count, which the
leak had been covering up. With that fixed, the wider whitelist is clean.

`bench/std_pipeline` runs 50k records through CSV read/write, transform, JSON,
and gzip:

| Phase | Go | Rust | Pith |
|---|---|---|---|
| csv read | 87 | 47 | **5** |
| csv write | 191 | 60 | 257 |
| transform | 48 | 27 | 218 |
| Total | 324 | 136 | 482 |

Overall, Pith is at 1.5x Go (4.1x when this document began), 2026-07-21 rerun.
`transform` (per-row field extraction, URL and path scanning, and a hash) used
to be the whole gap at 347ms. It walked each URL a character at a time with
`s[i] == "/"`, and every one of those reads minted a one-character heap string
just to compare it. Rewriting the URL and path scanners to compare raw bytes,
and fixing the compiler to actually emit the byte compare it had been silently
skipping for character literals, cut the phase to 218ms and dropped the run's
cstring allocations from 7.1M to 2.9M. Pith's CSV read is still the fastest of
the three. Peak RSS at 200k records: Go 238 MB, Rust 266 MB, Pith 239 MB. That
is at parity with Go and below Rust, down from 436 MB (1.7x Go) earlier and
5.3x Go pre-reclaim. What remains of the gap is byte-buffer string assembly in
`csv write`, not per-character allocation.

`bench/event_ledger` is an NDJSON event pipeline in four languages (Pith, Go,
Rust, Zig): decode JSON into structs, aggregate with maps and a set, and sign
an HMAC-SHA256 summary. 200k events:

| Phase | Go | Rust | Zig | Pith |
|---|---|---|---|---|
| gen | 127 | 31 | 19 | 322 |
| parse | 354 | 62 | 104 | **199** |
| analyze | 16 | 24 | 9 | 56 |
| Total | 490 | 122 | 136 | **575** |

Pith is about 1.2x Go on the total (2026-07-18 rerun), and `parse`, which
decodes JSON into a struct, is now faster than Go's reflection decode. A flat
scalar struct is filled in a single pass straight into the struct, with no
intermediate map and no per-field allocation. The remaining gap to Go is `gen`,
which is string assembly. The aggregate checksum and the HMAC digest come out
identical across all four languages, which is how the benchmark proves they do
the same work.

Collection churn, with a list and map built and dropped per iteration, 200k
iterations:

| | Go | Rust | Pith |
|---|---|---|---|
| Peak RSS | 8.0 MB | 2.0 MB | **2.6 MB** |
| Runtime | 140ms | 61ms | 187ms |

Memory stays constant, 3x under Go's GC, matching Rust's shape. The URL/path
churn variant (heavy substring work) runs 712ms at the same constant 2.6 MB.

`bench/zstd_codec.pith` runs the pure-Pith zstd decoder against the
crate-backed kernel, on a corpus built from the repo itself (2:1 to 11:1,
realistic content-encoding shapes; run it with `make zstd-pure-bench`). The
decoder started at 129x the kernel, and three optimization passes brought it to
5.5-8x, with the run-heavy case now beating the kernel outright:

| Corpus | First measure | Now | Vs kernel |
|---|---|---|---|
| Text 8KB | 3.4 MB/s | 56 | 8.0x |
| Prose 281KB | ~5 | 108 | 7.0x |
| Source 585KB | ~7 | 166 | 5.5x |
| JSON 500KB | ~8 | 343 | 6.2x |
| RLE runs 78KB | 29 | 7,475 | **0.63x, faster** |

The story generalized well beyond zstd. The early cost was per-byte runtime
calls (raw blocks and matches copied a byte per call); bulk slice writes,
`copy_within` for overlapping matches, and a cached 64-bit bitstream window
fixed that. The rest was per-operation call overhead that the compiler now
removes for every Pith program, not just this one. `xs[i]` used to call into
the runtime, heap-allocate an optional tuple, unpack it, and release it; the
consumer now collapses that whole pattern to inline loads with the same loud
bounds failure. `bits.band` used to cross two call layers and is now a single
native instruction. A word load from bytes inlines to one 8-byte load and a
mask. The decoder-side share was fusing sequence decode into execution (the
intermediate list of sequence structs cost more than the arithmetic producing
it) and packing every table entry into a plain int, since a struct read in a
hot loop is a handle plus refcount traffic.

Two measurement lessons from the same work are recorded here because they will
bite again. An operation's isolated microbenchmark cost overstates its marginal
cost in a real loop by about 5x (the CPU overlaps independent calls, so only
differential measurement on the real loop justifies a change). A
representation change must price the read side as well as the write (three
parallel int lists beat a struct list on push and lost it all back reading the
fields out).

The encoder side exists too, pure Pith end to end: stored, Huffman, and full
sequence blocks with repeat offsets, verified shape by shape against the system
zstd binary (`make zstd-encode-check`). The system binary must decode every
Pith-compressed frame byte-identical, because a round trip through our own
decoder proved nothing twice on this code. One optimization pass in, it stands
here:

| Corpus | Encode MB/s | Vs kernel | Size vs kernel |
|---|---|---|---|
| Text 8KB | 3 → **8** | 17x | 115% → **101%** |
| Prose 284KB | 4 → **15** | 9x | 139% → **106%** |
| Source 585KB | 6 → **20** | 12x | 140% → **107%** |
| JSON 500KB | 11 → **26** | 22x | 188% → **119%** |
| RLE runs 78KB | 48 → **51** | 69x | 137% |

Both columns moved together; neither was traded for the other. The size win is
mostly per-block FSE tables. All three sequence streams used to emit the
predefined distributions, which cost most on data whose histogram looks nothing
like them, and JSON logs were the worst case at 1.88x. The encoder now
histograms what a block actually uses and sends a table description only when
the estimated bits beat predefined by more than the description costs. The
finder also prefers the previous offset (a repeat code is cheaper than an
offset's magnitude) and holds a match until the next position has been checked
for a longer one.

The speed win is mostly from writing bits forward instead of recording them.
The backward bitstream writer pushed every field as a value and a width onto
two lists and packed them on a second pass; a packer flushing four bytes at a
time into the buffer runs that loop once. That, plus removing per-sequence
scratch allocations, moved the profile's largest cost from handle-registry
hashing (23%) to the match finder (31%), which is where the next pass would
start.

The remaining size gap calls for fewer sequences more than cheaper ones: JSON
spends 51,710 bytes on 19,134 sequences against only 3,986 bytes of literals,
so a hash chain with several candidates per bucket is the untried lever.
Repeat mode for sequence tables is also unimplemented. Later blocks re-send
descriptions nearly identical to the previous block's, worth a few hundred
bytes per multi-block frame at no throughput cost.

`bench/cyclic_graph` wires 2M struct nodes into reference cycles
(parent<->child) and drops them. Refcounting alone cannot reclaim a cycle, so
the strong version leaks; marking one edge of each cycle `weak` breaks it and
the whole graph reclaims:

| | Strong (no weak) | Weak edge |
|---|---|---|
| Peak RSS | 708 MB | **2 MB** |

This is the escape hatch for the one thing reference counting can't do on its
own. The default build has no GC pauses; a `weak` field is a non-owning
reference that reads back as `none` once its target is freed. An experimental
trial-deletion collector exists behind `PITH_CYCLE_GC=1`, measured and
deliberately left off by default. See docs/ownership.md for the numbers and
the recipe that works (the flag plus explicit `gc_collect()` calls).

The weak-edge row was not always flat. A rerun on 2026-08-11 caught it
climbing linearly, to ~120 MB at a million rings, where this table had long
claimed 2. A `weak` field initialized to `none` was storing a freshly
allocated optional tuple and taking a weak reference to it, but nothing
released that tuple's strong count, so every instance of any struct with a
`weak` field leaked one header-sized allocation. The constructor now stores a
bare `0` for a `none` weak field, and the row is flat again: the same 2 MB at
100k, 500k and a million rings. The `leak_weak_field` growth case guards it.
The lesson worth keeping is that the leak-growth gate had no weak-bearing
shape, so a headline claim drifted false for weeks with every other benchmark
still passing.

`bench/closure_error` covers the workload the collection benchmarks don't
reach: closures built, captured, called, and dropped every iteration, and
functions that fail with a heap error, propagate it up with `!`, and get
handled with catch and unwrap_or. 200k iterations, medians of 5 (checksums
match across all three, so the work is equivalent):

| Phase | Go | Rust | Pith (before) | Pith (now) |
|---|---|---|---|---|
| Closures | 3 | 0 | 407 | **45** |
| Errors | 54 | 33 | 137 | 133 |
| Total | 57 | 33 | 543 | **178** |

The closure column is stage A of the plan below, now landed. Closures moved
onto a magic-tagged header and off the global handle registry, and the phase
dropped from 407ms to 45ms, a 9x cut, with the checksum unchanged. That takes
the total from ~9x Go down to ~3x. The residual 45ms (vs Go's 3ms) is the heap
box each closure still allocates, which is separate, harder work; see the
plan.

The error phase is untouched at ~133ms and reasonable: it allocates a
three-slot result tuple and a heap error string per failure, work Go and Rust
do too (2.5x Go).

In the slow version, a closure validated and refcounted through the global
handle registry, a `Mutex<HashSet>` locked on every new, retain, release, and
validity check, while strings and structs had already left that registry for
a magic-tag header (see the sprint below). The ~400k closures this benchmark
builds and drops took that lock several times each. The memory work that
prompted the rerun (reference-counting closures instead of leaking them) had
added the release lock, so removing the registry paid that back too.

`bench/http_server` is a JSON API under `wrk -t2 -c8` on `/item?id=12345`, on
this 2-core machine:

| | Go | Pith threaded |
|---|---|---|
| req/s | ~31,600 | **16,800** |
| RSS | Flat ~13 MB | Flat (~1 B/req) |

2026-07-15 rerun (20s, `wrk -t2 -c8`): the threaded server, with one spawned
OS thread per connection, sustains ~16,800 req/s on this 2-core machine, a bit
over half Go's ~31,600. Go's netpoller stays well ahead.

The per-request growth was long recorded here as ~0.8 KB, then remeasured
2026-07-23 at **~2.8 KB/request** (twice, `wrk -t2 -c8` for 15s), and then sat
**flat**: ~200 KB total over 190k+ requests, about a byte each. That held until
sometime after 2026-07-23. The 2026-08-22 rerun measures ~63 bytes/request
growth on the same harness, on both backends (#899). The history below is the
previous round of this same fight, kept because the first attempt then blamed
the wrong construct and the next one may want the map.

Getting there took two attempts, and the first was a wrong turn worth
recording. The growth was first blamed on a result-typed local:
`serve_connection` reads each request as `req_result := read_...` then
`req := req_result.ok`, and such a `T!` local was being released as a bare
three-slot shell that never dropped its payload. That is a real bug, fixed
(the reclamation entry above), but fixing it barely moved the server.
Remeasured after it, the server still grew ~2.8 KB/request. The request object
was not the leak.

The actual cause was a `for` loop leaking its iterable on an early return.
`serve_connection` and the header helpers it calls do things like this:

```
for part in query.split("&"):
    if ...:
        return part          # skips the loop's end-of-scope release
```

A loop over a fresh iterable (a `split` result, a map's `keys()` list)
released that iterable only at the loop's normal end label. An early
`return`/`fail`/`!` from inside the body left the function without reaching
it, leaking the list and everything it held. Two such sites on the request
path (the query split and a `for key in headers` lookup) accounted for
essentially the whole 2.8 KB. Releasing open loops' iterables at the function
exit edges closes it, and the server holds flat under sustained load.

Build times: Go cold 25.0s / warm 0.1s. Pith compiles the benchmark in 2.1s
every time, and the entire self-hosted compiler in under 7s.

## Why (measured, not guessed)

- ~~Every string derive (`concat`, `substring`, `trim`) copies twice.~~
  Fixed (single allocation now). The runtime perf counters
  (`PITH_PERF_STATS=1`) show std_pipeline makes **zero** `pith_string_*`
  allocations (the copy-on-derive string type); the stdlib builds its string
  handling on byte buffers and list elements instead. The churn that was left
  hid in a different counter: indexing a string with `s[i]` mints a
  one-character heap **cstring**, so the URL and path scanners allocated one
  per byte. Moving them to byte-level scanning cut the run from 7.1M to 2.9M
  cstring allocations (the remaining 2.9M are CSV field materialization),
  alongside 2.4M list pushes and 1.1M byte-buffer writes.
- ~~Every list/map access takes a global mutex plus a hashset lookup to
  validate the handle.~~ Fixed. Validity is now a lock-free magic-tag read
  (`list_magic_ok`, `map_magic_ok`); the registry is touched only when a
  handle is registered or unregistered.
- ~~Lists and maps store non-int elements as one heap allocation per
  element.~~ Fixed. Packed storage is chosen by element *size*, not by element
  type (`uses_value_storage`: `elem_size == 8`), so strings, collections,
  structs, bytes and closures all live unboxed in `values8` alongside ints.

## The big one: memory was never freed (found July 2026, profiling, since fixed)

`perf` on std_pipeline shows ~23% of wall time inside the kernel zeroing fresh
pages (`clear_page_rep` plus fault handling). The reason is that the native
path barely releases anything. The runtime perf counters show zero ARC
allocations and zero ARC releases in the whole run. Bytes objects, C strings,
and most heap allocations are simply never freed, so the heap only grows and
every allocation touches brand-new zeroed pages.

Measured at 200k records, Pith peaks at 1.65 GB RSS and Go at 313 MB. The
benchmarks finish because the process exits before the leak matters; a
long-running server pays this as unbounded growth.

This was the single biggest performance and correctness item in the backend,
and it is now closed: the compiler emits releases for the native path. The
subsections below record how, in the order the pieces landed. Today
std_pipeline's peak RSS at 200k records is 239 MB, level with Go's 238 MB (see
the table above). Everything from here to the end of this section is history,
not an open problem.

### String ARC (landed July 2026)

Strings now reclaim. Heap cstrings carry a refcount header, and the compiler
emits the ownership operations: retain on binding a borrowed value, release on
reassignment and at every return, transfer on returns, and retains at each
escape point (struct fields, containers, tuples, closure captures). Params are
borrows, so an unmodified string parameter costs no RC traffic at all, and
concat/interpolation chains free their intermediates as they fold.

Measured on a 300k-iteration concat/substring/trim loop:

| | Before | After |
|---|---|---|
| Peak RSS | 85.5 MB | 15.2 MB |
| Runtime | 131ms | 81ms |

The header bought a second, unplanned win: `len()` on a heap cstring now reads
the stored length instead of running strlen. Profiling showed the compiler
spent ~80% of its own runtime in strlen, because every
`while i < s.len()` loop over a large string was quadratic. With the header
length, compiling the whole self-hosted frontend went from ~45 seconds to
under 2 seconds. `make self-host` is now a 1.7s operation.

### Collection ARC (landed July 2026)

Lists, maps, and sets are refcounted shared handles now. The emitter retains
on aliasing binds and escapes, releases on rebinding and at every return, and
containers created with a known element type release their elements when the
last count drops. Removed and overwritten elements are deliberately not
released, because a borrow of one may still be live, so those leak until
escape analysis can prove otherwise; only the free path cascades.

The payoff shows on churn-shaped work (the server case): a loop building a
50-element list and a small map per iteration, 200k iterations:

| | Before | After |
|---|---|---|
| Peak RSS | 218 MB | 2.6 MB |
| Runtime | 343ms | 179ms |

Memory stays constant where growth was unbounded. The peak for std_pipeline
drops more modestly (1.65 GB to 1.45 GB at 200k records) because its memory is
dominated by bytes objects and buffers, which still never free.

Peak RSS on the compiler itself barely moves: that memory is the AST and token
structures held in globals. Bytes and structs are the next reclamation
targets, in that order.

### String statement temps (landed July 2026)

The remaining string leak was not in bytes; it was per-character temps.
`s[i]` on a string mints a fresh one-character string, and every loop
like `while input[i] != ":"` leaked one per comparison. Those chars now
classify as owned (a bind takes the count, a comparison releases its operands
in the same block), and empty collection literals in return position pick up
their declared type, so containers like stringbuffer's parts list own their
elements properly.

A URL/path parsing loop (200k iterations over std.net.url and std.os.path
helpers):

| | Before | After |
|---|---|---|
| Peak RSS | 439 MB | 40 MB |
| Runtime | 1076ms | 1411ms |

That is eleven times less memory, at ~30% time cost in RC traffic on
char-heavy paths, a tradeoff that favors long-running processes. The peak for
std_pipeline drops from 1.45 GB to 0.88 GB.

A later pass (2026-07-21) removed that tradeoff for the common case.
`s[i] == "/"` and `ord(s[i])` in a scan now read a raw byte instead of minting
a char at all, so there is no allocation to reclaim and no RC traffic to pay.
That took fixing two long-dead emitter optimizations so they actually fire (a
single-character literal's stored value carries its quotes, and a call
argument is wrapped in an `arg` node; both checks were looking straight past
that). The comparison-heavy URL and path scanners were rewritten onto it,
which is what took std_pipeline's transform from 347ms to 218ms above.

### Argument temps (landed July 2026)

The last string-leak class was owned temps in argument position.
`parts.push(s.substring(start, i))` transferred the substring into the
container (which retains), but the temp's own creation count was never
released. The cstring counters showed container pushes and free-time cascades
perfectly balanced at 1.2M each, with exactly the creation counts leaking.
Owned string arguments now release right after the call they feed: callees
borrow their params, and storing callees (containers, struct fields, channels)
add their own count.

With this, the URL/path churn loop runs at **2.6 MB constant with
alloc == free exactly**, with zero string leaks. The peak for std_pipeline
drops to 0.66 GB (from 1.65 GB pre-ARC). The accumulated RC traffic now costs
std_pipeline ~15% (961→1102ms); eliding provably redundant retain/release
pairs is the next perf item, ahead of bytes reclamation.

Cranelift itself was generating unoptimized code until July 2026
(`opt_level` defaulted to "none"). Turning it to "speed" bought only 2-3% on
these benchmarks, which confirms the hot path is the runtime above, not the
generated code.

## Sprint plan and results

| Change | std_pipeline total | Catalog total (200k) |
|---|---|---|
| Baseline (July 2026) | 1273ms | 102ms |
| opt_level=speed | 1236ms | 100ms |
| Drop per-access handle lock | 888ms | 93ms |
| Single-allocation string derives | No change | No change |
| String ARC + O(1) cstring length | 804ms | 99ms |
| Collection ARC | 904ms* | 103ms |
| String temp reclaim | 961ms | 104ms |
| Argument temp reclaim | 1102ms | 104ms |
| Inline collection elements | | |

*Collection ARC costs std_pipeline ~10% in RC traffic; the churn table
above is what it buys.
| ARC object-list rework | | |

Target: std_pipeline within ~1.5x of Go (about 460ms). Compile time is
tracked too: `pith build bench/std_pipeline.pith` went 4.24s to 4.34s with
opt_level=speed.

## Closure performance plan (stage A landed, July 2026)

`bench/closure_error` put a number on the one workload the collection
benchmarks never reach: closures were ~135x slower than Go (407ms vs 3ms for
200k iterations building and dropping ~400k closures). The error phase in the
same benchmark is fine (2.5x Go), so this section is about closures. Stage A
has since landed and cut the closure phase to 45ms (9x); the write-up below is
kept as the record of what changed and what is left.

**Root cause, confirmed by reading the runtime.** A list validates a handle
with `list_magic_ok`, which reads a magic tag from the object's own header with
no lock (`collections/list.rs`). A closure validates with
`handle_registry::is_valid`, which takes a global `Mutex<HashSet>`
(`runtime_core.rs:354,361`). It takes that lock on *every* closure operation:
`new` registers, `retain`/`release` and every `get_fn` /
`get_env` / `set_env` check validity, and `release` at zero unregisters.
The "Drop per-access handle lock" row in the sprint table above did this for
collections and bought 888ms on std_pipeline. Closures were never converted.
So the fix needs no new design: it finishes a migration that already happened
for every other heap type.

### Stage A: closures onto a magic header (landed, 407ms → 45ms)

`PithClosure` got a magic tag at offset 0 (`CLOSURE_MAGIC`), the same shape
structs use (`STRUCT_MAGIC` / `struct_base`,
`runtime_core.rs:1137,1146`):

- `pith_closure_new` writes the tag instead of calling `register`.
- A `closure_base(handle)` helper null-checks, alignment-checks, and reads the
  tag; every `is_valid(.., Closure)` call site uses it.
- `pith_closure_release` at its last count scrubs the tag (writes 0) before
  `dealloc`, instead of calling `unregister`. A use-after-free then reads a
  dead tag and returns the safe default, exactly as a freed struct does.
- `HandleKind::Closure` and its registry calls come out.

Nothing about the closure lifecycle changes: the ref count, the captured-slot
release from #303, and the indirect-call ABI all stay. Only the *validity
mechanism* moves off the lock.

**Risk.** The magic read dereferences the handle, where the registry check did
not. Closures only ever reach these functions from the checker's closure-typed
path (a real box) or as 0 (null-guarded), the same risk profile lists and
structs already accept. The scrub-on-free turns a stale handle into a safe
miss instead of a wild call.

**Verified.** `bench/closure_error` closure_ms fell from 407 to 45 (median of
5), checksum unchanged. Valgrind stayed clean on the churn (0 errors, 0
definite leaks; the scrub is what makes a double-free or use-after-free a safe
miss, not just unlikely). Full suite, fixed point, and seed all passed, since
the runtime `.a` relinks into every program.

**The residual, as predicted.** The lock was the dominant cost, and removing
it closed most of the gap. What stays is the 45ms vs Go's 3ms: Pith
heap-allocates a box per closure where Go and Rust keep the environment on the
stack or inline it. That is a separate, harder optimization (a closure arena
or escape analysis) and only worth it if a real workload still shows the box
allocation now that the lock is gone.

### Stage B: the same lock on `AtomicInt` and the other primitives (landed for sync primitives and channels)

`AtomicInt`, `Mutex`, `Semaphore`, `WaitGroup`, and `Channel` have since moved
off the global handle registry onto their own magic headers
(`concurrency/sync_handle.rs` and `concurrency/channel.rs`). Only `List` (the
page-crossing fallback), `Process`, `ProcessOutput`, `Task`, and `X25519Key`
remain in `HandleKind` (`handle_registry.rs`).

### Order and stopping rule

Stage A landed first and alone; its measured win and its Valgrind + suite pass
were the gate for touching the others. That took the benchmark total from
543ms to 178ms. The remaining gap to Go's ~57ms total was the error phase and
the per-closure box, not the lock. (Later work also replaced the
`closure_base` / `struct_base` helpers with closure-scoped `with_*` validators
and landed stage B for the sync primitives and channels above.)

## How to rerun

```
./target/release/pith build bench/std_pipeline.pith
for i in 1 2 3 4 5; do ./bench/std_pipeline 50000; done   # take medians
./target/release/pith build bench/catalog_workload.pith
for i in 1 2 3 4 5; do ./bench/catalog_workload 200000; done
./target/release/pith build bench/closure_error.pith      # closures + error paths
for i in 1 2 3 4 5; do ./bench/closure_error 200000; done
./target/release/pith build bench/green_fanout.pith       # per-task memory under fan-out
./bench/green_fanout 200000                               # green (the linux default): flat rss
PITH_GREEN=0 ./bench/green_fanout 200000                  # os threads: watch rss grow
./bench/green_fanout 500000                               # peak rss should barely move
bench/chan_fanout_bench.sh 1000000 9                      # channels, four languages, checksum-checked
```

Go and Rust counterparts build per `bench/README.md`; `closure_error`
builds with `go build -o bench/closure_error_go bench/closure_error.go`
and `rustc -O bench/closure_error.rs -o bench/closure_error_rust`.
`chan_fanout_bench.sh` builds its own four (Pith, Go, rustc, Zig) and
refuses to print a table unless every run agrees on the checksum; it
measures Pith twice, once per backend.
