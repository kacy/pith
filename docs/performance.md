# performance

live notes on where pith's time goes and what the sprint is doing about it.
the april 2026 audit of the old c-transpiler era lives in
`docs/history/performance_audit_2026_04.md`.

numbers below are medians (5-7 trials) on one machine, july 2026. rerun with
the helpers in `bench/` before trusting them on different hardware.

## where pith stands

the short version, all on the same 2-core machine, rerun 2026-08-08,
reconfirmed 2026-08-11, rerun 2026-08-22, and reconfirmed 2026-08-23 after the
module-call leak fix (#901) and the concurrent http server (#902) landed —
every row within run-to-run noise of the day before, comparators reproducing
their figures (on 2026-08-08 the one mover was a weak-reference leak, found
and fixed on the rerun, in cyclic_graph below; 2026-08-22 moved catalog and
chan_fanout in pith's favor and corrected the http row, below). the
concurrency rows are the green backend, which is the default on linux; rows
marked `PITH_GREEN=0` are the os-thread opt-out, kept for contrast. the go,
rust and zig columns double as canaries — they are the same programs as in the
2026-07-29 run, so when one of them moves it is the machine that moved and not
the language:

| coordination | pith | go | rust | zig |
|---|---:|---:|---:|---:|
| chan_fanout, 1m msgs | **90-95 ms** (bimodal: 13/15 runs 96-100, 2/15 runs 59-66) | ~71 ms | 66-69 ms | 203-245 ms |
| chan_fanout, pinned to 1 worker | **~46 ms** | ~71 ms | — | — |
| 20k spawn + join (batches of 64) | **~27 ms / 3.3 mb** | ~8 ms / 3.8 mb | — | — |
| 20k spawn, `PITH_GREEN=0` | ~919 ms / 3.5 mb | — | — | — |

| services and compute | pith | go | rust | zig |
|---|---:|---:|---:|---:|
| catalog workload, 200k requests | **~92 ms** | ~376 ms | ~68 ms | — |
| grpc unary echo, sequential, 16 B | **4022 calls/s** | 3711 | 2716 | — |
| grpc unary echo, conc=8, 16 B | 7560 calls/s | 13434 | 10937 | — |
| http server under wrk, 60 s | 14.2-14.4k req/s, rss flat (~3 b/req) | 21-32k req/s (regime-dependent, see below) | — | — |
| http sequential latency, 1 connection, p50 | 135-162µs | 93µs | — | — |
| event_ledger, 200k events | **339-344 ms (0.85x go)** | 404-406 ms | 105 ms | 127-131 ms |
| std_pipeline, 50k records | 439-448 ms (1.78x go) | 249 ms | 134 ms | — |

what moved since july, with the canaries holding: **spawn/await halved**, 56 ms
to ~27 ms at flat memory — the argument-ownership fixes took a per-call
allocation out of the spawn path. **event_ledger fell 618 to 570 ms** and
**std_pipeline 576 to 480 ms**, closing the gap to go from 1.28x to 1.19x and
from 1.63x to 1.50x. channel fan-out and the catalog workload are unchanged.

and on 2026-08-16 **event_ledger fell again, 570 to 351 ms — past go's
466** (all four digests matching, medians of 5 interleaved trials). two
changes, one each in the compiler and the bench: fixing the typed-decode
lowering's per-call leaks took `parse` from 195 to 175 ms and peak rss
from ~95 to ~67 mb, and rewriting the bench's `gen` phase to build its
stream in one ByteBuffer — the shape the go version always used with
`strings.Builder`, idiom for idiom — took `gen` from 299 to 120 ms, even
with go's 122. see bench/README.md for the full phase table.

the http row was wrong for three separate reasons, untangled across
2026-08-22/23, and the row above is the first correct one.

first, the pith benchmark server was serial by construction: the accept loop
served each connection inline, so the whole benchmark ran on one connection at
a time — throughput equal to the reciprocal of per-request latency, one core
idle — while the go arm served concurrently. every earlier pith figure in this
row measured that flaw, not the stack. the server now spawns a task per
connection like the go arm, and reads 14.2k req/s to go's 21.4k on the same
60-second spaced runs.

second, pith leaked ~64 bytes per request — a compiler bug in the ownership of
module-qualified call results, not an http bug — fixed in #901; rss is flat
again. third, the go figure swings between regimes with box state on this
shared host — 21.4k on 2026-08-22 and 31.7k on 2026-08-23 on the identical
60-second spaced protocol, with pith at 14.2k and 14.4k on the same two days
— so the pith/go ratio is whatever regime go is in (0.45x-0.66x) and pith is
the stable arm. the sequential-latency row is the number that does not move
with that: `bench/http_seq_latency.py` reads one keepalive connection at a
time and is deterministic to a few microseconds; its day-to-day band
(135-162µs) is the host's latency regime, since the cpu-bound rows above did
not move between the same two days.

the july figures reconcile too: 16.8k was `http_server_mt`, the variant that
already spawned per connection, under `PITH_GREEN=0` — reproduced 2026-08-23
at 16.1k, within 4%. the "throughput halved since july" scare was the mt
server's number compared against the serial server's. per-request latency,
measured with a sequential single-connection probe, has been ~126-136µs from
july through today: no cpu regression, only the serial loop and the leak.

a launch-history note for anyone re-measuring: repeated short wrk runs read
progressively lower on this box for either language's server, so use one long
spaced run per arm. the mt-threaded server currently outruns the green
spawning one on this workload (16.1k vs 14.2k) — a real gap worth its own
measured look, not a regression.

reading the rest plainly: on channel coordination green beats zig comfortably
but runs ~1.9x behind both go and rust at the default worker count (placement is still
decided by first-resume luck, hence the bimodal spread), and beats go pinned to
one worker. raw spawn/await is still go's, though by ~3.4x now rather than ~7x;
what the green backend buys over pith's own os-thread backend there is ~34x the
speed at flat memory. the service-shaped rows are the strongest: the catalog
workload — lookups, filtered scans, json batch scoring — runs ~3.5x faster than
go's version, because a flat struct decodes in one pass with no reflection. the
compute rows keep their long-standing shape: modestly behind go, well behind
rust and zig, with string building the remaining gap.

### why the default worker count stays at the core count

every channel-shaped benchmark here runs faster pinned to one worker, which
looks like an argument for making one the default. it is not, and the control
is what settles it — eight spawned tasks doing independent arithmetic, no
channels, nothing shared:

| 2026-08-17, medians of 3 | 1 worker | 2 workers |
|---|---:|---:|
| chan_fanout, 1m msgs | **46-54 ms** | 64-96 ms |
| green_fanout, 300k spawn+await | **~172 ms** | ~232 ms |
| cpu_parallel, 8 independent tasks | 190 ms | **~107 ms** |

so one worker wins the coordination rows by ~1.3-1.8x and loses the parallel
row by **1.77x** — near-linear scaling on two cores, identical checksum.

that asymmetry is the whole answer. the channel wins are not one worker being
better; they are the placement cliff being *avoided*, because a pair that
cannot land on two workers cannot land split. defaulting to one would hard-code
that workaround into every program, and would charge every genuinely parallel
workload 1.77x to do it — trading the point of an M:N runtime for a bug
mitigation. **the default stays `available_parallelism`.** the channel numbers
are a reason to fix placement (see the migration note in `green.rs`), not a
reason to change the default.

`PITH_GREEN_WORKERS=1` remains the right answer for a process that is known to
be handoff-bound and not compute-bound, which is why the flag exists.

### moving a woken task to its waker (`PITH_GREEN_MIGRATE=1`, 2026-09-06)

the placement cliff has a fix that keeps the worker count: a worker that
wakes a task parked on another worker takes it over, in the same CAS that
claims the task. a pair that talk to each other converge onto one worker
after their first exchange, and a task that never waits is never moved. it
ships behind `PITH_GREEN_MIGRATE=1`, default off, and this is what it does on
the handoff benchmarks. interleaved rounds, off then on then the 1-worker
reference within each round, medians, all on the same 2-core box:

| 2026-09-06, medians of 7 | flag off | flag on | 1 worker |
|---|---:|---:|---:|
| chan_fanout, 1m msgs | 93 ms (bimodal: 69, 71, 92-94) | **43 ms** (42-43) | 42 ms |
| task_pingpong, 200k rounds | 48 ms | 48 ms | — |
| green_fanout, 300k spawn+await | 220 ms | 213 ms | — |

a second set of 5 rounds after the flag-off path was tightened read the same
way (fan-out 87 ms bimodal against 41 ms flat, fan-out at 1 worker 40 ms,
spawn-and-await 220 against 212) and caught the ping-pong cliff that the
first set had missed: two of the five flag-off runs landed split and took
413 ms, the other three 49-50; the five flag-on runs read 48, 49, 49, 49, 49.

`PITH_PERF_STATS=1` now reports the wake breakdown, and it is the whole
explanation. with the flag off the fan-out made 105,268 cross-worker wakes
in one run and 8,988 in another (the count is as bimodal as the time); with
the flag on it made 3 and 6, each one a migration, and the rest of the
~8,000 wakes stayed on one worker. the ping-pong's 400,001 wakes were all
same-worker in the runs that were counted, which is the arrangement the flag
produces every time rather than some of the time. the spawn-and-await
fan-out has no handoff to fix and moves within noise.

the flag-off path costs nothing: `chan_fanout` under callgrind reads
536.6M instructions against 534.5M for the runtime before the change
(+0.39%), and 536.8M with the flag on, so the program does the same work
either way and the whole difference is where the wakes land.

it is not the default, and the reason is a measurement rather than a
worry. every benchmark above is handoff-shaped, and the rule that collapses
a pipeline onto one worker is the same rule that pulls parallel work onto
one worker when it synchronizes: a pinned task is never stolen, so once a
compute task has been moved next to the task that woke it, nothing spreads
it back out. `bench/cpu_parallel_sync` is that shape, eight compute tasks
that report to a collector after each chunk, and it says so:

| 2026-09-06, medians of 5 | 1 worker | 2 workers, flag off | 2 workers, flag on |
|---|---:|---:|---:|
| cpu_parallel_sync, 200 chunks of 20k | 448 ms | **259 ms** (247-305) | 337 ms (230-402) |

`PITH_PERF_STATS=1` shows about 280 migrations a run with the flag on, each
one a compute task landing on the collector's worker. so the flag wins the
handoff benchmarks outright and gives back a third of the 2-worker speedup
on this one. the way to have both is to let an idle worker take a `Ready`
task off a peer's pinned queue, which the runtime is now built to allow;
until that exists the default stays put.

### what preemption safe-points cost, now that they are on (2026-09-07)

green tasks are preemptible in every build unless it says
`PITH_GREEN_PREEMPT=0`. the backend puts a safe-point before each loop
back-edge — load a process-global byte, test it, branch over a call — so a
compute-only task cannot hold its worker forever. what follows is the bill,
measured off against on with the same source and the same runtime, the two arms
differing only in that environment variable at build time.

instruction counts first, since a compile-time change is exactly the case where
wall time on this box says whatever it likes. each row is one
`tooling/callgrind_ab.sh` pair, `PITH_GREEN=0` unless the row says otherwise:

| 2026-09-07, callgrind | safe-points off | on | delta | safe-points in the binary |
|---|---:|---:|---:|---:|
| event ledger, 200k events | 3,390,966,577 | 3,393,272,496 | +0.07% | 232 |
| channel fan-out, 1m messages | 684,679,434 | 685,965,123 | +0.19% | 87 |
| catalog workload, 200k iterations | 1,381,072,404 | 1,386,079,521 | +0.36% | 139 |
| compiler compiling itself | 22,493,294,294 | 22,611,240,269 | +0.52% | 824 |
| std pipeline, 200k records | 4,285,492,116 | 4,364,667,321 | +1.85% | 206 |
| channel fan-out, 1m, under green | 554,038,479 | 604,481,604 | +9.10% | 87 |
| tight loop, 200m iterations | 1,400,266,874 | 2,600,267,047 | +85.72% | 1 |

the last column is the number of back-edges the build put a check on, counted
out of the disassembly. it is also how the arms were checked for anything else
moving: over the 875 functions the two tight-loop binaries share, the mnemonic
histogram differs by 17 instructions in 2 functions — the six-instruction
safe-point in the loop, and the register moves and frame the call it contains
forces around it. the safe-point
build links more of the runtime (the park path a safe-point can reach), which is
why the function counts differ at all.

wall clock on the three shapes where it means something, interleaved with the
order rotated each round, each preceded by `tooling/null_ab.sh` on the off arm to
fix the noise floor:

| 2026-09-07, medians | off | on | delta | null floor |
|---|---:|---:|---:|---:|
| tight loop, 200m, 9 rounds | 129 ms | 259 ms | **+101%** | ±3.0% |
| compiler compiling itself, 7 rounds | 2830 ms | 2901 ms | +2.5% | ±1.0% |
| fan-out 1m, green, 1 worker, 7 rounds | 42 ms | 45 ms | +7.1% | ±2.4% |
| fan-out 1m, green, 2 workers, 7 rounds | 87 ms | 88 ms | +1.1% | — |

three of those rows need a word of explanation.

the tight loop is the worst case and it doubles: six instructions of safe-point
on a seven-instruction body, and the wall clock moves further than the
instruction count because the check contains a call. the loop's values have to
survive it, so the allocator puts them in callee-saved registers and gives the
function a frame — visible in the disassembly as a `sub $0x20,%rsp` and three
spills that the off arm does not have.

how bad it gets depends on the body, which is why this benchmark is written the
way it is. the first draft took the sum modulo a prime, and with the `idiv` in
there the same a/b reads +0.4%: the safe-point disappears into the divider's
latency. a second draft left the loop inline in `main` beside a string format,
which had already cost it the registers, and read +33%. so the loop is a
function of its own with nothing in the body but a multiply and two adds, and
what it reports is the ceiling: no body can hide the check better than nothing
at all can. `docs/limitations.md` used to put a degenerate
loop at ~6%, which is presumably one of those softer arrangements; it does not
reproduce on this one.

the compiler moves further on the clock (+2.5%) than on instructions (+0.52%).
the extra work is 824 not-taken branches in a binary that grew about 1.5%, so
this is fetch and layout rather than arithmetic, and it is at the edge of what
the null floor can resolve — but the on arm's fastest run (2836 ms) is still
slower than the off arm's fastest (2761 ms), which a layout accident does not
usually manage.

the green fan-out is the only row where the safe-point does anything other than
not fire, and it is the design's weak spot rather than the check's cost. the
request flag is process-global: once the monitor sets it, every running task
calls the runtime slow path at its next back-edge, where the check that it is the
task which actually overran turns almost all of them away. `PITH_PERF_STATS=1`
now counts both: at one worker that run makes 1,169,107 slow-path calls and
preempts 3 times, and at two workers it makes 1,724,997 calls
and 16 preemptions. a per-worker request would collapse that, and is the obvious
next thing to do here. under callgrind the same run reads +9.1%, which overstates
it: the monitor ticks on wall time while the program runs ~50x slower, so the
flag is set for a far larger share of the program's back-edges than it would be
natively.

what the whole bill buys is that a task looping on a value another task has to
publish can no longer wedge the process. `tests/green/starvation.pith` finished
40 out of 40 runs with safe-points, at one and at the default worker count, and 0
out of 40 without them.

### the compiler stopped splitting strings on every generic lookup (2026-09-10)

the profile taken for the std pass (#1097) put `pith_string_split_to_list` at 13.1
percent self and 31.6 percent inclusive of a self-compile, and both callers
were the compiler's own code. `checker_resolve_generic_declaration_key` is
asked, for a base name, which key `generic_declaration_map` holds it under
from the module being checked. its first two probes are map lookups; when
both miss, which is the common case since most names are not generics, it
walked every `module<TAB>name<TAB>source` entry the whole program's
from-imports had produced and split each one on the tab to compare the first
two fields. 8,838 calls, about 1,000 entries each, 2.1 million splits: 34.4
percent of the compile inside one function. `ir_csv_contains` in the
emitter answers whether a builtin is in one of fourteen comma-joined tables
(the float-returning builtins, the void methods, the fallible builtins by
result shape), and did it by splitting the table into a fresh list per
question, 106,406 times.

both places now build the answer once. the checker keeps the entry lists
as they are (the lsp snapshot carries them, and two `starts_with` scans read
them) and adds an index over them: the named entries under
`module<TAB>name`, the wildcard entries under `module`, each holding its
sources in entry order, so a lookup is two map probes and the loop only runs
over the handful of sources that import that exact name. the index is
appended at the two push sites and rebuilt from the lists on a snapshot
restore. the emitter turns each table into a `Map[String, Bool]` the first
time any table is asked, and the comma-joined strings stay as the record.
what is emitted does not change: `tooling/ir_compare.sh` over the corpus
reads identical raw as well as normalized, and the bootstrap seed reached
its fixed point.

instruction counts through `tooling/callgrind_ab.sh`. the whole-run rows are
the trunk's driver against this one, each compiling the same tree; the
per-function rows are a before-and-after pair of the driver built from the
same base with `PITH_KEEP_SYMBOLS=1`, inclusive figures:

| 2026-09-10, callgrind | before | after | delta |
|---|---:|---:|---:|
| compiler compiling itself, whole run | 22,640,305,204 | 12,495,999,868 | −44.8% |
| `checker_resolve_generic_declaration_key`, inclusive | 7,791,228,580 (34.4%) | 74,819,569 (0.60%) | −99.0% |
| the eleven `ir_metadata` predicates over `ir_csv_contains`, inclusive | 2,483,092,479 (11.0%) | 39,608,789 (0.32%) | −98.4% |
| `pith_string_split_to_list`, self | 2,971,980,003 (13.1%) | 48,562,103 (0.39%) | −98.4% |
| `examples/web_login.pith`, whole run | 19,830,250,211 | 17,111,351,847 | −13.7% |
| `examples/generics.pith`, whole run | 103,235,367 | 88,646,810 | −14.1% |

what is left of `resolve_generic_declaration_key` is its fast path: a
module-qualified key built by `module_symbol_prefix`, which concatenates a
character at a time, and the map probes. the next compiler items in the
profile after this are `ir_runtime_call_name` (1.1 percent, a chain of
string comparisons per call lowered) and `from_import_source_module` (0.5
percent, the remaining `starts_with` scan over the same entry lists).

wall clock as the cross-check, the two drivers interleaved on the
self-compile after a discarded warm-up, medians of seven: 6027 ms before,
3767 ms after (−37.5%); a second pair of three rounds later that
afternoon read 6453 ms against 3619 ms. the absolute figures are about
twice the 2830 ms the safe-point section reports for the same compile;
the two runs are not comparable, and the paired delta is the figure this
row exists for.
### the substring search (2026-09-10)

`pith_cstring_contains` was 10.5 percent of the std pipeline by self
instruction count, and `pith_string_split_to_list` 10.9 percent of the event
ledger, because the runtime compared a needle-length slice at every position
of the haystack: one `bcmp` call per byte, whether or not that byte could
start a match. `index_of` and `replace` had the same loop. #1099 replaces all four with one search built on
libc's `memmem`. the output is byte-identical: a golden generated from the old
runtime over the awkward inputs (an empty needle, a needle longer than the
haystack, a needle at either end, repeated first bytes, a needle sharing a
prefix with an earlier non-match, bytes above 0x7f) reproduces under the new
one, natively and under valgrind, and a randomized cross-check of the new
search against the naive one over 4000 generated pairs agrees on every pair.
one input that is not byte-identical is `"aaa".split("aa")`: the old loop kept
scanning inside the delimiter it had just consumed and aborted the process on
the second, overlapping match; it now returns `["a"]`.

two searches were measured before one was chosen, both against the old loop on
the same binaries: `memchr` for the needle's first byte followed by a compare
of the rest, and `memmem`. instruction counts per call from
`bench/substring_search.pith`, inclusive of the primitive, callgrind, needle
`comma` (one byte, no other occurrence in the filler), `word` (seven bytes,
first byte common in the filler) and `repeat` (a haystack of one byte against
a needle that differs from it only at the end):

| 2026-09-10, Ir per call | naive | memchr + compare | memmem |
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

the checksum of every cell matches across the three arms. reading it: the old
loop is cheapest only when the needle is at position zero, and a needle 80
bytes in already costs it 2,500 instructions. the two replacements are within
40 instructions of each other on the one-byte shape the workloads have, and
within 100 on a short haystack with a longer needle, where `memchr` wins.
where they part is the long haystack and the adversarial one: a seven-byte
needle absent from 4000 bytes costs `memmem` 8.4k instructions and the
first-byte scan 25.6k, and a haystack made of the needle's first byte costs
them 72k and 262k, since the first-byte scan then compares the needle at every
position, exactly as the old loop did. `memmem` has no such cliff, and on the
std pipeline as a whole it reads 0.57 percent fewer instructions than the
first-byte scan (3,843,060,836 against 3,865,161,439), so it is the one that
stays. what it leaves on the table is the fixed cost of a multi-byte needle
in a short haystack, about 100 instructions a call; a first-byte scan for
haystacks under a few dozen bytes would recover that if a profile ever shows
it mattering.

whole workloads, `tooling/callgrind_ab.sh`, the unmodified runtime against
the new one:

| 2026-09-10, callgrind | before | after | delta |
|---|---:|---:|---:|
| std pipeline, 50k records | 4,370,463,019 | 3,846,570,175 | -12.0% |
| event ledger, 200k events | 3,392,821,275 | 2,919,539,338 | -13.9% |
| substring search, 36 cells, 2000 rounds | 4,703,069,294 | 855,520,761 | -81.8% |

in the std pipeline `pith_cstring_contains` goes from 458,901,276 instructions
(10.50 percent, the figure in #1099) to 68,151,536 (1.77 percent), with
`memmem` itself at 46,401,120; in the event ledger `pith_string_split_to_list`
goes from 369,804,712 (10.90 percent) to 9,600,063. `__memcmp_avx2_movbe`,
which the old loops called once per position, falls from 182,465,421 to
450,186 in the pipeline and from 176,189,954 to 49,980,659 in the ledger. every
other entry in each profile is unchanged to the instruction, which is what a
change confined to one runtime function should look like.

those rows were taken at 25d42d75, the base of the profile in #1099, before
the std profiling pass above it in this file (#1097) merged. that pass took
three `contains` calls per field out of the csv encoder at the call site, so
on the merged tree the pipeline has far fewer searches left to speed up: the
same pair reads 3,448,402,361 before and 3,412,266,260 after (-1.0%), while
the event ledger, which #1097 did not touch, reads 3,392,817,605 to
2,920,641,142 (-13.9%) as before.

wall clock as a cross-check only, medians of 7 interleaved rounds after a
discarded warm-up, on the shared 2-core box:

| 2026-09-10, medians | before | after |
|---|---:|---:|
| std pipeline, 50k records | 487 ms | 412 ms |
| event ledger, 200k events | 364 ms | 305 ms |
| substring search, 36 cells, 2000 rounds | 485 ms | 198 ms |

the compiler searches source text with the same primitives, so its output was
checked too: the ir driver linked against the new runtime emits byte-identical
ir for `bench/std_pipeline.pith` (1,088,812 bytes) and for its own source
(5,119,821 bytes).

### a note on how these are measured

the cross-language harnesses interleave: one round runs every language once,
and a round repeats for the trial count. they used to measure each language to
completion in turn, which on a small shared box hands whoever goes first a
systematic advantage — ambient load drifts over the length of a run and pools
on whoever goes last. it was worth roughly 2x. the event_ledger runner also
used to re-run each binary once per metric column, so a row's phase times did
not come from the same work and need not sum to that row's own total.

two habits follow from that. **discard the first run after a build**, which
competes with whatever the build left behind. and **read the non-pith columns
before the pith one**: they are fixed programs, so when they reproduce their
published figures the run is trustworthy, and when they do not, nothing in that
run is.

a third habit, for changes inside the runtime rather than across languages:
**prefer instruction counts to wall time for attribution.** a compile-time
change once read +5.6% eight rounds running and was noise — a byte-identical
binary at two paths read −2.8% under the same protocol — while callgrind put
it at +0.06%. `tooling/callgrind_ab.sh` runs two arms under callgrind and
reports totals, the delta, and the hottest functions of each; the linker
strips symbols by default, so build the program under test with
`PITH_KEEP_SYMBOLS=1` first or the runtime reports as anonymous addresses.
`tooling/null_ab.sh` times one binary against a copy of itself to show what
the box adds on its own; a real a/b that lands inside that spread has shown
nothing. an instruction count cannot see a memory-ordering stall, so the two
instruments answer different questions and a barrier's cost still needs the
timing protocol — but with the null floor established first.

## std profiling pass (2026-09-10)

a pass over the standard library where every target came from a profile
rather than from reading the code for a pattern. the instrument is
callgrind through `tooling/callgrind_ab.sh` with each program built under
`PITH_KEEP_SYMBOLS=1`, so a cost lands on a function name and not an
address, and the acceptance number for every change is an instruction
count, not a wall clock. compiler and runtime were the trunk at
`25d42d75` throughout; the before and after arms of every comparison
were built by the same `self-host/ir_driver` against the unmodified and
the modified `std/`.

five workloads, chosen to cover the library's busy surfaces: json,
collections and crypto (event_ledger); csv, url, path, gzip and hashing
(std_pipeline); typed json decoding on a hot loop (catalog_workload); the
http/1.1 server on one keepalive connection (http_server under the
sequential probe); and the compiler compiling itself, the largest pith
program there is. for each, the fifteen most expensive functions by self
and by inclusive instruction count, each tagged as runtime (rust, the
`pith_*` primitives, the allocator), std (`std_*`, pith) or the program's
own. inclusive rows for the process entry chain are dropped.

### event_ledger, 200000 events

`./bench/event_ledger 200000`. total 3,393,922,717 Ir.

by self cost:

| Ir | % | function | kind |
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

by inclusive cost:

| Ir | % | function | kind |
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

`./bench/std_pipeline`. total 4,370,009,153 Ir.

by self cost:

| Ir | % | function | kind |
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

by inclusive cost:

| Ir | % | function | kind |
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

`./bench/catalog_workload 4000`. total 43,282,141 Ir.

by self cost:

| Ir | % | function | kind |
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

by inclusive cost:

| Ir | % | function | kind |
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

`./bench/http_server` driven by `bench/http_seq_latency.py <port> 2000`, dumped with `callgrind_control --dump` before the kill. total 1,026,531,527 Ir.

by self cost:

| Ir | % | function | kind |
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

by inclusive cost:

| Ir | % | function | kind |
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

### the compiler compiling itself

`ir_driver --combined self-host/pith_main.pith`, the driver relinked from its own IR with `pith build-ir` so it keeps its symbols. total 22,610,437,376 Ir.

by self cost:

| Ir | % | function | kind |
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

by inclusive cost:

| Ir | % | function | kind |
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

### what the profile says

three std hotspots are large and bounded, and the rest of the picture is
runtime or the program itself.

**the http request head was read one byte at a time.** the server's
inclusive tree puts 84% of a request under
`read_request_buffered_bytes`, and 56% of the whole process under
`buffered_bytes_reader_read_internal`: the head reader called
`reader.read_bytes(1)` in a loop, and every one of those 64 calls per
request paid the reader's registry lookups (a threadlocal map behind
`pith_tls_get_or_init`, whose hashing is the `hash_one` and sip rows), a
mutex, a fresh `ByteBuffer`, a slice of the cache and a `consume` that
re-slices it. 128,001 `read_bytes` calls for 2,000 requests. the runtime
rows under it are real, but the call shape that produces them is std's.

**the csv encoder asked three substring questions per field.**
`encode_row_bytes_to` is 31% of std_pipeline inclusive, and 15 of those
points are `pith_cstring_contains`, called three times per field to
decide whether it needs quoting (`,`, `"`, newline), plus two `chr()`
strings built per field to search for and released again. the runtime's
`contains` walks the haystack comparing a slice per position, so each call
on a sixteen-byte field is ~460 instructions and the three of them ~1,400.
1.4 million calls.

**`path.clean_part_count` built the segment list it only needed the length
of.** 11% of std_pipeline inclusive for 50,000 calls: per path a
`substring` per segment, a stack list that `clean_count_segment` returned
back and forth (nine `pith_list_release_handle` per path), and a fresh
list from `without_last_segment` for every `..`.

what was declined, and why:

- event_ledger has one std row, `ByteBuffer.write_string_utf8` at 2.9%
  self and 12.3% inclusive. the wrapper itself is thin; the cost is the
  `Int!` result the compiler boxes for every fallible call: `pith_struct_alloc`
  and `pith_struct_release` under it are 11.7% of the workload. that is
  the result-type lowering, a compiler matter with the same shape at every
  `T!` call site, and the wrapper's own `text.len()` test carries the
  empty-write semantics. the rest of the workload is `pith_json_fill_struct`,
  `pith_string_split_to_list` and the allocator.
- catalog_workload has no std function in either list: its decode is the
  runtime's `pith_json_fill_struct` (42% inclusive).
- the compiler has no std function in the top fifteen either. its 32%
  under `pith_string_split_to_list` is the compiler's own `.split` use, in
  two functions: `checker_resolve_generic_declaration_key` (22% of the
  compile, 1.9 million splits) and `ir_metadata_ir_csv_contains` (8.4%).
  that is the next compiler pass, not a std one.
- the hypothesis this pass was asked to test, that `s = s + chunk` loops
  in `std/encoding.pith` and the per-nibble hex build in `std/hash.pith`
  are hot, is not borne out on these workloads: no `std_hash_*` or
  `std_encoding_*` function appears in any list. the hmac in event_ledger
  and the sha256 in std_pipeline each run once over a summary of a few
  hundred bytes. the byte-compare form of string indexing, `s[i] == "x"`
  and `ord(s[i])`, is lowered by the emitter to a `pith_cstring_byte_at`
  read and does not allocate; only `s[i]` used as a value does. they are
  left alone, because a change nobody can measure has only a downside.
- `csv_row_from_bytes` (16% of std_pipeline inclusive) is the next std
  candidate: three `List[Int]` per row and a `pith_bytes_get_strict` call
  per byte. `Row`'s fields are public, so its layout is an interface, and
  it was left for a change that can take that on.
- the runtime's `pith_cstring_contains` and `pith_cstring_index_of` compare
  a slice at every position; a first-byte scan would make both an order of
  magnitude cheaper for short needles and shows up in every workload here
  (2.3% self of the compile). that is a runtime change and is noted for
  one.

### what changed, and by how much

one commit per hotspot; instruction counts from `tooling/callgrind_ab.sh`,
both arms built by the same compiler, checksums matching. the
micro-benchmarks live in `bench/` next to the workloads.

| hotspot | micro-benchmark | before | after | per operation | whole workload |
|---|---|---:|---:|---|---|
| http head read | `bench/http_head_read 20000` | 8,871,737,831 | 2,099,174,323 (−76.3%) | head read per request 333,050 → 23,300 Ir | http_server, 2000 sequential requests: 1,030,428,586 → 407,378,733 Ir (−60.5%; 515k → 204k per request) |
| csv quoting decision | `bench/csv_encode 5000 10` | 1,441,314,484 | 881,419,663 (−38.8%) | quoting test per field ~1,730 → 666 Ir; `encode_row_bytes_to` per row 26,715 → 15,621 Ir | std_pipeline: 4,369,923,908 → 3,814,760,923 Ir (−12.7%) |
| `path.clean_part_count` | `bench/path_clean_count 20000` | 1,961,084,704 | 556,782,410 (−71.6%) | per call 9,309 → 1,941 Ir (std_pipeline's paths) | std_pipeline: 3,814,222,835 → 3,451,254,104 Ir (−9.5%; −21.0% with the csv change) |

http. `read_request_head_buffered_bytes` now asks the buffered reader
for everything up to the blank line with
`read_until_bytes_including_bounded(CRLFCRLF, MAX_HEADER_BYTES)`. the
reader finds the terminator in its own cache and keeps what follows for
the body and the next request, which is what the byte loop was doing by
hand. the reader's cap failure is mapped back to `HEADER_LIMIT_MESSAGE`
through a new `io.is_read_until_limit_error`, so the 431 response and the
size-limit tests are unchanged, and any other failure passes through as
before. the reader's `read_until` gained the `errdefer buf.free()` its cap
path was missing. `tests/cases/test_http_head_read_shapes` pins pipelined
requests, bodies, heads that straddle a chunk boundary every way they can,
a truncated stream, an empty stream and oversize heads against the output
of the byte-at-a-time reader; `tests/leaks/leak_http_head_read` is flat.
the tls fallback's head reader has the same byte loop over a `tls.Conn`
and was not in the profile; it is left for its own measurement.

csv. `csv_field_needs_quoting` reads the field's bytes once in place
(`ord(field[i])`, allocation-free) and replaces the three `contains` calls
and their two `chr()` temporaries; the string encoder `encode_row` uses the
same predicate. the leak case written for it found that `encode_bytes`
returned `out.bytes()`, a copy, and never freed its `ByteBuffer`, so every
call left a buffer registered for the life of the thread (125 bytes per
small table); it now returns `take_bytes()`. `tests/cases/test_csv_encode_quoting`
pins every trigger position and combination byte for byte against the
three-search encoder.

path. the segment stack `clean()` walks is, at every step, some `..`
entries a relative path could not climb past followed by the real
segments, so two counters describe it. `clean_part_count` keeps `depth`
and `climbs`, reads the path's bytes in place and copies no segment out;
`clean_count_segment` is gone. `tests/cases/test_path_clean_part_count`
compares it with `clean()` and `parts()` over every four-segment
combination from `{a, b, ., .., ""}`, relative and absolute, 1,273 paths.

a note on the path micro-benchmark: its inputs are string literals, and a
literal has no length header, so every `pith_cstring_byte_at` on it runs a
`strlen` first (16% of the after arm). std_pipeline's paths are built at
run time and carry the header. the per-call figure in the table is from
std_pipeline for that reason.

the wall clock agrees in direction and, as always on this box, is the
cross-check and not the claim. interleaved, medians of 5 after a
discarded warm-up, the box busier than on the days the tables above were
taken: std_pipeline 941 ms before (810-953) and 727 ms after (719-817),
-23%; the sequential probe's p50 against the two servers, 3,000 requests
per trial, 174 us before (173-174) and 134 us after (133-136), -23%.

## typed json decoding: the flat fill (2026-09-10)

the largest runtime row left in the std profiling pass above was
`pith_json_fill_struct`: 33.7% self of catalog_workload at 4000
iterations and 14.9% self of event_ledger at 200000 events. it is the
single-pass decoder behind `json.decode[T]` and `json.decode_text[T]`
for a flat struct whose fields are all required scalars: the emitter
hands it the object bytes, a pre-allocated struct, and one interned
literal per struct type, the field spec `<type><name>,...` in slot
order, and it fills the struct in place and returns a mask of the slots
it wrote. same instrument as the pass above, both arms built by the
same compiler at `f75f26a8`, the before arm linked against the
unmodified runtime archive; the after arm's numbers include
`read_key_end`, which is the key scan split out into its own symbol.

reproduced first: 14,572,000 Ir self (33.66%) of 43,298,007 for
catalog_workload, 3,643 per decode of a six-field struct;
504,822,230 (14.88%) of 3,392,821,004 for event_ledger, 2,524 per
decode of a four-field struct with one unknown key.

### where the instructions went

callgrind at instruction level (`--dump-instr=yes`, the hot blocks mapped
back through `objdump`) split the 3,643 per catalog decode three ways.

42% went to walking the spec for every key. the lookup was
`spec.split(',')` per key with a slice compare per field, so a key in
slot k walked the bytes of fields 1..k one at a time, through the
iterator's closure, before it could compare anything: 166 spec bytes per
six-key object, at about 8 instructions per byte. this is the part that
is quadratic in the field count. a twenty-field struct paid 18,007 per
decode in the fill alone, 82% of it here.

28% went to scanning the key bytes. `read_string_end` inlined into the
field loop came out as a loop carrying eight induction variables, 22
instructions per key byte, against 4 or 5 for the same function called
out of line on a string value.

the rest is per-key glue, the value parsers and the string copies, which
is the work the function exists to do. the spec's length was not a
factor: `pith_cstring_len` on the literal is already a `strlen` call,
recognized by the optimizer.

### what changed

the spec is not split. a key is compared in place against the field at
a cursor, the field after the one that matched last, and only on a miss
is the spec walked from the start, first byte inline before any memcmp.
an encoder writes keys in declaration order, so the cursor hits on one
compare and the spec is never walked; keys in any other order still
resolve, one walk per out-of-order key. the key scan is its own
`#[inline(never)]` function so it keeps the plain loop. the spec format
and the emitter are untouched, and so is the mask contract with the
caller.

two ownership holes in the same function came out of the leak case
written for it. a repeated key overwrote a string slot without releasing
the first occurrence's string, which was then nobody's; it is released
now. and the error struct `pith_json_decode_missing_error` builds for a
missing field carried no destructor, so its message string was stranded
on every failed decode; it now has one. neither changes any output.

### by how much

`bench/json_decode_shapes <shape> <size> <rounds>`: a three-field struct,
a twenty-field struct, a struct with a nested struct, and an array of
small objects decoded one element at a time. `size` is the string value
width for the flat shapes and the element count for the list. per-call
figures are the fill's self cost (plus the key scan, in the after arm)
divided by the rounds; inclusive adds the string copies and the parsers
it calls.

| shape | size | fill per call, before → after | inclusive per call | whole run |
|---|---:|---|---|---|
| small (3 fields) | 4 | 1,168 → 542 (−53.6%) | 1,450 → 995 (−31.4%) | 41,891,869 → 32,791,847 (−21.7%) |
| small | 32 | 1,168 → 542 | 1,755 → 1,272 (−27.5%) | 48,008,666 → 38,348,799 (−20.1%) |
| small | 256 | 1,168 → 542 | 4,237 → 3,530 (−16.7%) | 97,786,964 → 83,646,801 (−14.5%) |
| wide (20 fields) | 4 | 18,007 → 3,303 (−81.7%) | 24,215 → 6,459 (−73.3%) | 517,461,868 → 162,341,824 (−68.6%) |
| wide | 32 | 18,007 → 3,303 | 26,350 → 8,398 (−68.1%) | 560,181,505 → 201,141,719 (−64.1%) |
| wide | 256 | 18,007 → 3,303 | 43,724 → 24,204 (−44.6%) | 907,814,933 → 517,414,871 (−43.0%) |
| nested (2000 rounds) | 4 / 32 / 256 | not this path | — | 1,060,420,752 / 1,455,612,979 / 4,613,063,408, unchanged to ±0.00% |
| list (2000 / 400 / 50 rounds) | 4 / 32 / 256 | not this path | — | 2,753,795,870 / 4,383,023,336 / 4,470,490,540, unchanged to ±0.00% |

the fill's own cost does not depend on the value width in either arm:
the per-call column is the same at every size, and the inclusive column
grows only by the scan and copy of the value. the nested and list shapes parse into the
node pool and never reach this function; their rows are there to show
that, and as a reminder of the gap: a nested three-field decode costs
about 530,000 instructions against the flat shape's 1,000, which is the
next thing to look at on this path.

the workloads:

| workload | fill self, before → after | per decode | whole workload |
|---|---|---|---|
| catalog_workload 4000 | 14,572,000 → 6,272,000 (−57.0%) | 3,643 → 1,568 | 43,297,608 → 34,825,924 Ir (−19.6%) |
| event_ledger 200000 | 504,822,230 → 296,422,230 (−41.3%) | 2,524 → 1,482 | 3,392,820,888 → 3,169,220,724 Ir (−6.6%) |

checksums and the hmac digest match across arms. the wall clock, as
always here, is the cross-check and not the claim: interleaved, medians
of 7 after a discarded warm-up, on a box another job was sharing,
catalog_workload at 200000 iterations 231 ms before (104-237) and 203 ms
after (77-215), −12%; event_ledger 743 ms before (618-956) and 731 ms
after (602-898), −1.6%, with the ranges as wide as the effect.

what is left in the 1,568: 86 per key for the key scan, about 175 per
key for the in-place compare, the value parsers and the spills of a
large frame, and three string allocations. that is linear in the bytes
and the fields now; the next thing on this path is the node-pool decode
beside it, not this function.

## july 2026 hardening, in numbers

between 2026-07-26 and 2026-07-31 the green backend became the linux default,
every blocking call got a yield point, an ownership sweep fixed seven
leak/use-after-free defects in the emitter and runtime, and a concurrency
audit of the shared state the flip exposed fixed six more. what that changed,
each measured before and after on this machine:

| what | before | after |
|---|---:|---:|
| a co-tenant cpu task while another task waits on dns | blocked until the lookup ended | decoupled (~78 ms) |
| the same, while another task appends to a log file | 257-273 ms | 122-124 ms |
| the same, while another task waits on a child process | 1013-1018 ms | 8-9 ms |
| the same, while two tasks run `process.output` | 2012 ms at 1 worker, 1007 at 2 | 8 ms |
| the same, while another task sleeps a second | 1018 ms | 7-23 ms |
| the same, while a `select` waits on an idle channel | 3015 ms | 20 ms, and the send now arrives in time |
| twelve concurrent sleepers, 120 ms each | serialized behind their workers | all wake in ~201 ms |
| one bare tcp connect to a tls server | server dead, health checks green | survives; >512 junk handshakes hold no slots |
| concurrent https requests sharing the tls config registry | racing handle counter — a handshake could pick up another config's cert and key | serialized behind one lock |
| `time.delay` with a negative duration | hung (≈584 million years) | returns immediately |
| json parse per request, 20k docs on one task | 89 mb, degrading | 10 mb flat, ~30% faster |
| map/list eviction churn, 800k rounds | 38 mb | 10 mb flat |
| `container[expr()]` index keys, 800k | 26-75 mb | flat |
| `xs.map(f)` result lists, 800k rounds | 269 mb | flat |
| fn values named in a loop, 800k | 309 mb | flat |
| struct values stored in containers, 800k | 318 mb | flat |

three of those "leaks" were masking live use-after-free bugs (a lambda stored
in a `List[fn]` segfaulted the moment its leak was fixed; sitegen's tag pages
came out named after the output path when struct stores stopped over-counting),
which is the argument for fixing leaks even when the memory alone would be
tolerable. the sweep is pinned by `make leak-check`, a growth gate that runs
six churn shapes at two round counts in ci and fails on ~2 mb of drift — it
was proven by reverting each fix and watching it go red.

the cost side, stated plainly: single-task tight-loop file reads pay ~2x for
the blocking-pool handoff that keeps file i/o from stalling a green worker,
and event_ledger/std_pipeline drifted a few percent slower as hot paths gained
the releases they had been skipping. flat memory was bought at list price.

all numbers from one 2-core machine, medians of 5 where quick enough to
repeat; the tables below were fully rerun 2026-07-15 (after the arc
reclamation and weak-reference work), the grpc table again on 2026-07-19
after the unary-response coalescing and once more on 2026-08-17 with all
three clients re-run together, and std_pipeline again on 2026-07-21
after the byte-level scanner work. the standout change in the latest
rerun: std_pipeline's `transform` phase fell from 347ms to 218ms once the
url and path scanners stopped minting a one-character string per byte,
taking the workload from 2.0x go to 1.5x. earlier work had already put
std_pipeline's peak rss at parity with go (239 vs 238 mb, was 1.7x), and
the cyclic-graph benchmark below shows weak references reclaiming
reference cycles refcounting alone can't. json struct decode stays faster
than go's reflection decode — a flat struct of required scalars decodes
in a single pass, filled straight into the struct.

`bench/chan_fanout`, the coordination benchmark, was the one pith lost
outright — ~580ms against go's ~69 for a million messages between eight
tasks. the green wake-path work on 2026-07-26 (details below) brought
the green backend to ~133ms on the 2026-07-29 rerun — ahead of zig, ~1.8x
behind both go and rust — with ~46ms, faster than go on this box, when the
pipeline is pinned to one worker. the batch benchmarks measure compute and pith is competitive
there; coordination is now a genuine strength of the green backend
rather than the standing embarrassment it was.

the comparators drift a few percent between days; within a table they
are comparable. go 1.24.4 (net/http, encoding/*); rust either a pinned
crate set (std_pipeline) or a tiny hand-rolled scanner (catalog), read
those generously.

`bench/catalog_workload` — service-shaped compute: lookups, filtered
searches, batch json; 200k iterations:

| | go | rust | pith |
|---|---|---|---|
| total | 386ms | 67ms | **133ms** |

2.9x faster than go, within 2x of rust (2026-07-15 rerun). the batch
json phase (~119ms) dominates. peak rss at 200k: go 10 mb, rust 10 mb,
pith 43 mb — pith holds the whole catalog resident where the comparators
stream it. this rose from ~111ms when the old six-field decode helper
was retired for the general single-pass decoder — the specialized
helper was a little quicker but never freed the strings it decoded;
the general one attaches the struct destructor, so it is a touch slower
and no longer leaks.

`bench/grpc` — unary echo calls over tls on loopback, the same grpc-go
server for all three clients so the numbers reflect the client. pith is
`std.net.grpc` over its own tls 1.3, http/2, hpack, and protobuf — no c,
no async runtime. all three clients re-measured together on 2026-08-17,
10,000 calls each, three repetitions with the client order ROTATED between
them so no client benefits from running first (this bench had an ordering
bias worth ~2x until #678, so the rotation is not ceremony):

| calls/sec | go | rust (tonic) | pith |
|---|---|---|---|
| 16 B, sequential | 3711 | 2716 | **4022** |
| 1 KiB, sequential | 3600 | 2616 | **3638** |
| 16 B, 8 concurrent, one connection | **13434** | 10937 | 7560 |
| 1 KiB, 8 concurrent, one connection | **9643** | 9426 | 6815 |

and the latency distribution at 16 B, which is where pith looks best:

| 16 B | go | rust (tonic) | pith |
|---|---|---|---|
| sequential median | 235 µs | 326 µs | **228 µs** |
| sequential p99 | 633 µs – 1.01 ms | 1.33 – 2.26 ms | **571 – 716 µs** |
| conc=8 median | **479 µs** | 623 µs | 919 µs |

**sequentially pith is now the fastest of the three** — ahead of grpc-go on
throughput, on median latency, and by the widest margin on p99, where its
tail is about half go's and a third of tonic's. that is the whole stack in
pith: its own tls 1.3, http/2, hpack, protobuf, no c and no async runtime.
tonic being the slowest of the three sequentially is worth knowing before
treating rust as the automatic ceiling here.

concurrency over a single connection is still the weaker spot: eight streams
lift pith ~1.9x where go lifts ~3.6x. one number makes that diagnosable
rather than merely bad — going from 16 B to 1 KiB narrows the conc=8 gap
from 1.78x to 1.41x while the sequential columns barely move. a gap that
shrinks as the payload grows is a FIXED PER-CALL COST being amortised, not
a throughput ceiling. that fixed cost is the internal pipeline — every
frame crosses worker → writer → socket → reader → worker, and each hop is a
thread handoff (a futex wake plus a context switch). the single reader/writer
is correct and required for hpack ordering. coalescing removed the redundant
handoffs on that path (below); the ones that remain are structural — one
context switch per pipeline thread per call — so the way to more parallelism
past that is more connections, also below.

read the conc=8 column with the box in mind: the server runs alongside the
client on a 2-core machine, so eight in-flight calls oversubscribe it by
construction and that column measures the machine as much as the runtime.
it is a good symptom detector and a bad optimisation target. the sequential
column is the clean measurement, and it is the one pith wins.

this benchmark paid for itself on the first run: client sockets had no
`TCP_NODELAY`, so nagle collided with the peer's delayed acks for a ~40ms
stall per round-trip — the pith client measured 22 calls/sec before the
fix and 2269 after. one socket option, and every request/response path
(http, grpc, the db drivers) benefits.

a later perf pass trimmed the client's per-call overhead: the four constant
grpc request headers are cached, the http/2 writer coalesces queued frames
into one write, data events share one empty header list, and — the largest
win — a single-frame request body is now sent inline rather than on a
spawned task, dropping one os thread per call. apples-to-apples best-of-7,
sequential 16 B went from ~2550 to ~3120 calls/sec (+23%) and 8-concurrent
from ~4860 to ~6260 (+29%); over the whole line that pass moved sequential
pith from ~2100 to ~2850, roughly ~50% to ~72% of grpc-go. a sweep after the
std thread-safety fixes and the freelist below put it at ~3175 (~77%). the box
is too noisy to resolve the smaller items (cached headers, coalescing, shared
event list) on wall-clock, so they stand on counted structural reductions.

a later pass inlined the whole request, not just its body. a client starts
single-threaded: one in-flight call runs synchronously on the caller over the
same lockstep codec the one-shot get() uses — no reader or writer task, no
channel handoff — and promotes to the multiplexing pipeline once, the first
time a second stream appears (the inline fast-path in
std/net/http2/connection.pith). a controlled before/after put a sequential
unary call at ~340 µs, down from ~368 (~8%); the table above is a fresh
quiet-machine sweep after the change, 16 B sequential at ~3375 (~83% of grpc-go).

the first cut of this shipped a flow-control bug worth recording. at promotion
it debited the connection send window by the total body bytes the inline phase
had put on the wire — reasoning that the threaded sender would otherwise
over-count the peer's receive window. but inline is strictly sequential, so by
the time each response was read the peer had already consumed that request and
re-granted the bytes; the debit double-counted. after a warmup of a few thousand
1 KiB calls it drove the window ~2 MB negative, and the first concurrent sends
stalled waiting for it to climb back — 1 KiB/8-concurrent fell ~25% (to ~3850)
while 16 B, whose debit stayed within the 64 KiB window, was untouched. the
fix is to leave the window alone at promotion: sends and grants already net out.
that restored 1 KiB/8-concurrent to ~5660, at or above where it began. the bug
hid because the fast-path's own tests used a 20-byte stand-in for the inline
total; it took a full grpc sweep on a quiet box to surface it.

the newest pass cuts handoffs on the concurrent path. a unary response is three
frames — response headers, the data message, trailer headers — and the reader
used to hand each to the waiting worker as its own event, waking it three times,
where each wake is a kernel context switch. now the reader marks a unary stream
when it opens, accumulates that stream's frames in a lock-free per-reader map,
and delivers one combined event at end-of-stream: one wake instead of three.
streaming rpcs are never marked, so their frames still arrive one at a time and a
server-streaming response stays incremental — it is never buffered to the end.
`perf stat` put context switches per call at 7.8 before and 5.0 after (-35%);
16 B/8-concurrent rose ~6025 to ~6832 (+13%) and 1 KiB ~5096 to ~6084 (+19%), at
equal or slightly lower cpu, taking single-connection scaling from ~1.7x to ~2x.
that was the last clearly-redundant handoff: the switches left over are one per
pipeline thread per call — the worker blocking for its reply, the reader on the
socket, the writer draining its queue — the floor for a reader/writer/worker
pipeline built on real os threads. go pays the same handoffs in userspace through
its m:n scheduler, which is most of why it does ~2x the calls at lower cpu;
closing that gap on one connection would need the same, and the cheaper lever is
more connections.

the 2026-08-16 pass took the syscalls out of the inline path. a traced
sequential call made five socket writes — HEADERS and DATA as a tls record
each, then a PING ack (grpc-go probes bandwidth with a ping alongside each
response) and two WINDOW_UPDATEs, each its own record and syscall — where
grpc-go makes ~1.8. now the request rides one write, HEADERS and DATA in a
single record, and the read loop queues what it owes the peer — acks and
flow-control grants — settling it in one write when the response completes,
or as soon as ungranted credit crosses 16 KiB (a quarter of the advertised
window) so a large response never stalls against an exhausted one. a
finished stream also no longer receives a stream-level grant it can never
spend. strace counts the result exactly: 2.0 writes per call, down from
5.0. interleaved a/b on the quiet box, 16 B sequential: ~3.2k to ~4.1k
calls/sec (+24%), median 281 to 228 µs — ahead of grpc-go's ~4.0k on the
same box for the first time. 8-concurrent is unchanged, as expected: the
threaded pipeline already batches through the coalescing writer, and its
remaining gap is per-call cpu in the handoff chain, a separate lever.

a follow-up pass indexed the hpack tables. the encoder used to scan all 61
static entries per header field before consulting its dynamic table — the
new perf-stats counters showed it as ~460 list reads per grpc call — and
the huffman decoder built a string key ("length:code") per input BIT and
hashed it into a string-keyed map. now the static table answers from
prebuilt maps (the small dynamic table is scanned first; the two sets are
provably disjoint, so the order is observably identical) and the huffman
map is keyed by a packed integer on the map's int fast path. component
a/b, 100k iterations each: encoding a six-field grpc header list 255 to
108 ms (2.4x), decoding a block with one huffman literal 1.9 s to 250 ms
(7.6x). the grpc echo moves little (~+4% at conc=8, within noise at
conc=1) because its steady-state fields are all table hits — but any
header that changes per response (date, set-cookie) is a huffman literal
every time from real servers, and first requests always are.

the deepest cut is an experiment, shipped off by default: a two-register
result ABI behind PITH_RESULT_REG=1. every `-> T!` function today returns
a heap-allocated three-slot box, and the perf counters put that at ~500
struct allocations per grpc call across the byte codecs. under the flag,
a non-generic, lambda-free function whose ok value is an Int or Bool
returns (is_ok, payload) in two registers instead — a `!` call site
consumes the pair with no allocation at all, and every other context
folds the pair into an ordinary box on the spot, so the ownership rules
never change. with the flag off, not one instruction moves (the full
regression suite is byte-for-byte the box world); with it on, the whole
tree passes regressions, the golden examples, and the leak gate. measured
on the shape it targets — std.binary's chained fallible reads, a u32+u64
read per round — struct allocations fall 14 to 2 per round and wall time
drops ~2.3x (54-66 to 23-29 ms per 200k rounds).

the second phase widened the gate from Int/Bool to every ok payload that
fits one register: String, Bytes, the list/map/set family, and struct
handles, whose reference count transfers through the register exactly as
it transferred through the box's ok slot. `catch` grew the same pair fast
path `!` already had, so a `f(x) catch fallback` loop allocates nothing on
either arm. Float still needs the f64 pair variant, impl methods still
box (method registration never records ok kinds), and the json/config/
toml/yaml modules stay excluded wholesale because their typed-decode
emitters call helpers by raw name. component a/b at 100k rounds: a
String!-returning catch loop falls 200,010 to 2 struct allocations, the
binary-read catch shape 400,000 to 0, http/2 frame serialization 33 to 19
per frame, hpack decode 59 to 48 per block. end to end, sequential grpc
echo gains ~3% (median 222 to 213 µs) and conc=8 is unchanged — that
path's cost is the futex handoff chain, not allocation.

the third phase brought methods and the legacy bridge in. a method on a
plain (non-interface, non-generic) impl now registers like a free
function — safe because pith interfaces dispatch statically, so no
indirect call can reach a pair signature — and the wrappers around
single-register legacy builtins stopped costing a box: a `return
byte_buffer_write_byte(...)` used to build a result box just so the pair
return could unpack it, and a caller in a non-pair context built a second
one. the wrap now hands its (flag, payload) registers straight to a
waiting `!`, `catch`, or pair return, and `return f()` forwards an
rcall'd pair the same way. component a/b at 100k rounds, box world to
flag-on: hpack encode 7 to 1 struct per block, hpack decode 59 to 25,
frame serialization 33 to 16, protobuf writes 10 to 5. sequential grpc
echo holds at ~+4% (median 226 to 217 µs); conc=8 stays put. unwrap_or
and `catch:` blocks then joined the same handshake `!` and inline catch
use, so every result-consuming form now runs box-free on a direct
pair-returning call — a 200k-round unwrap_or/block-catch loop fell from
400,032 struct allocations to zero. the json/config/toml/yaml family
then joined too: those modules were excluded wholesale because their
typed-decode emitters call helpers by resolved name outside the normal
call paths, and a plain call into a pair-returning function reads the
flag as the value — the decode call sites now consult the registry and
fold a registered helper's pair back into the box the decode machinery
expects. that put the flag on the event ledger benchmark for the first
time: total 355 to ~320 ms (−10%) with an identical digest. still
boxed: Float results and interface-impl and generic-impl methods — the
last slices of a default-on decision.

the threaded path then took the write fusion the inline path got above. at
concurrency a unary request reached the writer task as two messages — the
HEADERS under the encoder mutex, then the DATA behind it — where one would do.
a single-frame body now rides the HEADERS write, and the send credit it needs
is reserved through the mutex the header block already holds, which also saves
a mutex round trip per call. that reservation is all-or-nothing on purpose: a
caller that took a partial grant would have to hand the remainder back on every
error path, and one missed undo silently shrinks the connection window for the
life of the connection, so a connection with no window left simply falls back to
the ordinary flow-controlled send. bodies past one frame are untouched — they
still stream on their own task while the caller drains the response. the effect
is countable: `PITH_GREEN_STATS=1` over 3300 calls at conc=8 puts channel wakes
at 8174 before and 6625 after, about half a wake per call rather than a whole
one, because the coalescing writer was already absorbing part of the second
handoff. interleaved a/b, 14 alternating rounds of 20000 16 B calls at conc=8:
~8930 to ~9210 calls/sec (+3%) at 116 to 111 µs of client cpu per call (−4%),
the branch ahead in 10 of the 14 pairs. the run-to-run spread on this box is
wider than the gain (8.1k-9.9k either way), so the counted handoff is the solid
half of that result and the wall clock is the weak half. conc=1 is unchanged, as
it takes the inline path.

the green backend (see `docs/concurrency.md`) is that same in-userspace
scheduler, and this benchmark is the case it was built
for. running the reader, writer, and worker tasks as coroutines on one worker
(`PITH_GREEN_WORKERS=1`) turns every per-call handoff from a futex wake into a
userspace switch. measured on the 2-core dev box, conc=8, medians of 5 runs,
per-call counts over warmup+calls (2026-07-20 rerun).

**these absolute numbers predate the wake-path work of 2026-07-26** (the
coroutine stack pool, the channel condvar fix, and the slab-free wake), so
they understate green as it stands. re-running the same shape on 2026-07-27
at a smaller batch put pith at 5067 calls/sec os-thread, 8075 at one green
worker (+59%), and 7083 at two (+40%) — the same relationships the table
below records (+53% and +41%), so its conclusions hold. the table is left
at its original batch size rather than replaced with a smaller,
non-comparable run.

the cross-language shape was rerun in full on 2026-07-29: one local tls
server (`bench/grpc`), three prebuilt clients each timing 20000 unary
calls over a single connection after 2000 warmup calls, eight concurrent,
medians of three interleaved rounds. compile time is outside every lane —
the pith client is built once beforehand, the same as the go and rust
binaries.

| payload, conc=8 | pith | go | rust |
|---|---:|---:|---:|
| 16 B, calls/sec | 7476 | **14731** | 12005 |
| 16 B, median / p99 | 954 µs / 2.9 ms | 471 µs / 2.0 ms | 591 µs / 2.3 ms |
| 1 KiB, calls/sec | 7598 | **12366** | 9545 |
| 1 KiB, median / p99 | 957 µs / 2.5 ms | 555 µs / 2.5 ms | 717 µs / 3.3 ms |

all three clients now report the same metrics: per-call median and p99
from the full sorted latency set, timed with each language's monotonic
clock. pith sits at ~50-60% of go and ~60-80% of rust on throughput, with
a per-call median about 2x go's — one connection means one reader task,
and the two-core box splits it against the server and seven sibling
callers. the earlier revision of this table printed a pith "average"
computed as wall-clock over total calls, which under eight-way concurrency
flattered pith by nearly an order of magnitude; the percentile reporting
replaced it.

| 16 B, conc=8 | calls/sec | ctx-switches/call | cpu |
|---|---|---|---|
| os-thread | 6887 | 4.9 | 0.98 |
| green, 1 worker | 10570 | 0.55 | 0.69 |
| green, 2 workers | 9689 | 0.93 | 0.78 |

| 1 KiB, conc=8 | calls/sec | ctx-switches/call | cpu |
|---|---|---|---|
| os-thread | 6422 | 4.6 | 1.02 |
| green, 1 worker | 9090 | 0.57 | 0.70 |
| green, 2 workers | 7215 | 1.72 | 0.93 |

at one worker the mechanism does exactly what it was meant to: context switches
per call fall from ~4.9 to ~0.55 (the pipeline stops touching the kernel to hand a
frame between tasks), and that shows up in the wall clock — throughput up ~53% at
16 B and ~42% at 1 KiB while cpu drops below a core. that is close to the cpu
grpc-go spends here, so green-at-one-worker gets pith to about three quarters of
go's throughput at go's efficiency, from about half at 1.5x the cost on the
os-thread backend.

the default worker count used to lose this: at two workers green was *slower*
than the os-thread backend, because a pinned task's wake did a pool-wide notify
that also roused the second worker, which then spun through its (empty) queues and
contended on the shared task lock instead of parking — pure overhead, since a
single connection's pipeline pins to one worker and the other has nothing it can
run. giving each worker its own park spot (a pinned wake now nudges only the
task's owner, and an idle worker no longer counts a peer's un-stealable pinned
work as a reason to stay awake) removes that herd: at two workers green now beats
the os-thread backend, ~41% at 16 B and ~12% at 1 KiB, at lower cpu (the 16 B
margin is the noisier of the two — the two-worker path varies run to run).

two workers still trails one, though, and the reason is locality, not the herd.
the eight request tasks are spawned from the main thread, so they scatter across
both workers while the reader and writer sit on whichever worker promoted the
connection; the roughly half of calls whose task lands away from the reader/writer
pay a cross-worker wake per hop. one worker keeps the whole pipeline together and
pays none, which is why it is fastest for a single connection. closing that last
gap would need connection-aware placement — pinning a connection's request tasks
to the worker that owns its reader/writer — which the scheduler can't infer from a
plain spawn (a single connection wants its tasks packed onto one worker; an
independent fan-out or a larger connection pool wants them spread), so it is a
task-placement change above the scheduler, not a scheduler-locality one. on this
two-core box a connection pool can't show the spread paying off anyway: the client
already shares both cores with the go server it calls. more cores, or a server not
fighting the client for them, is what a pool would need to scale. the table at
the top of this section was measured on the os-thread backend, which was the
default when it was taken.

past a single connection there is a connection pool: `grpc.dial_pool` (and
`http2.open_pool`) opens n independent connections and rotates calls across
them round-robin, the same subchannel trick real grpc clients use. each
connection is its own tls session and reader/writer pipeline, so calls on
different connections run on different cores. it took landing the std
thread-safety fixes first — a shared per-connection reader was racing global
state and segfaulting under true parallelism. on this 2-core dev box the pool
buys only ~11% at pool=2 and ~15% at pool=4, because eight concurrent streams
already saturate both cores (the client shares them with the go server); the
pool pays off on hardware where a single connection's ~one-core pipeline is
the real ceiling.

worth recording what did *not* help grpc: allocation. profiling flagged
~13% of cpu in malloc/free, and a sequential call does ~800 small struct
allocations (mostly the result box built for every `T!` return). but a
sequential call is ~760 µs, nearly all of it blocked on socket, tls, and
those thread handoffs — the allocation is cpu time that barely touches
wall-clock. a per-thread struct freelist (recycling small blocks instead of
round-tripping the allocator) moved grpc by ~0% and struct-alloc-bound
compute by up to ~29%; it is a compute win, kept because it is free and
safe, not a grpc one. a deeper swing at the same target — returning small
results in two registers instead of a heap box — was prototyped and shelved:
it needs boxing thunks wherever such a function is used as a value (every
higher-order call), which is a large, delicate change for a compute-only
gain the freelist already mostly captures.

the freelist is per thread, and that had a cost the compute numbers did not
show. on the os-thread backend every spawned task is a fresh thread, so a
task that allocated a batch and returned built a pool, filled it once, and
tore it down without reusing a block — and the teardown was the expensive
part: callgrind put the thread-exit destructor's burst of deallocations at
8.1M of 33M instructions in a task-per-thread benchmark, a quarter of the
run, with glibc consolidating on every one. that made the pool about 35%
slower than no pool for that shape. the pool now releases its slot at thread
exit and the next thread adopts it, blocks and all, so a task's first
allocation is a reuse and nothing is torn down. the retained bytes are also
now bounded in bytes (256 KiB per slot, `STRUCT_POOL_MAX_BYTES`) rather than
in blocks per size bucket, which had quietly meant about 2.1 MiB per thread;
process retention is that budget times the peak number of live threads.
`bench/task_churn` is the reproducer, and `tooling/callgrind_ab.sh` is how
the cost was attributed after three timing-driven fixes had missed it.

a later thread-safety pass moved `std.binary`'s reader off shared global
maps into its own struct fields (so two http/2 reader threads can't race
them). that also dropped a global-map access per frame parse: a
re-measurement put pith at ~3040 calls/sec sequential 16 B (now matching
tonic, ~74% of grpc-go) and ~6300 8-concurrent (~2x scaling, up from
~1.8x) — the concurrent path gains more because it parses more frames.
the point of that change was correctness, not speed: before it, running
several pooled connections in parallel raced the global maps and crashed;
the throughput bump was a free side effect.

`bench/green_fanout` — spawn short tasks in batches of 64, await each,
repeat, and read peak rss from `/proc/self/status`. this is the shape a
server takes when it fans work out per request, and green now bounds its
memory: a finished task hands its slab slot back for the next spawn to
reuse and releases the closure it was spawned with, so the working set is
the tasks alive at once, not the total ever spawned.

| green, batch 64 | 200k tasks | 500k tasks |
|---|---:|---:|
| before reclaim | 90 mb | 226 mb |
| after reclaim | 3.1 mb | 3.1 mb |

before, rss climbed ~460 bytes per task and never came back — the slab
grew one entry per spawn and each task's closure was never freed. after,
it is flat: 500k tasks or five million, the peak is the batch. the
os-thread backend still keeps a record per task it has run, so its rss
still grows with the total (the closure release lands there too, but the
slot does not); giving that slab the same reclamation is a tracked
follow-up, and until then this bound is a green-only property.

spawn *speed* was a separate problem, fixed later: every green task
allocated a fresh 1 MiB coroutine stack (an mmap plus a guard-page
mprotect) and unmapped it on completion, costing a TLB shootdown across
every core. the kernel's address-space bookkeeping dominated spawn.
finished coroutines now donate their stacks to a pool and the next spawn
reuses one. spawning 20k tasks and awaiting them all, medians of 5,
interleaved with a go canary on the 2-core box (2026-07-26):

| 20k spawn + join | elapsed | peak rss |
|---|---:|---:|
| os threads (2026-07-26) | ~1450 ms | 174 mb |
| os threads (2026-07-29) | ~1017 ms | 3.5 mb |
| os threads (2026-08-08) | ~919 ms | 3.5 mb |
| green, before the pool | ~580 ms | 10 mb |
| green (2026-07-29) | ~56 ms | 3.2 mb |
| green (2026-08-08) | ~27 ms | 3.3 mb |
| go (batch twin, 2026-07-29) | ~8 ms | 3.8 mb |

the 2026-08-08 halving, 56 ms to ~27 ms, is the argument-ownership work
rather than anything scheduler-shaped: a container written straight into a
call was stranding its handle, and the spawn path builds one per task. the
os-thread row moved with the box, not with the fix, which is what says the
green number is real.

page faults over the run fell 21832 -> ~2500 and context switches 25748 ->
~3000. shrinking the stack from 1 MiB to 64 KiB changed nothing before the
pool, which is the tell: the cost was the *number* of mappings, not their
size. the 2026-07-29 rerun, against a minimal go twin of the same batch shape,
puts go clearly ahead on raw spawn/await (~7x) — go's scheduler reuses
goroutine stacks with no fd or slab work at all — while green holds an
~18x lead over pith's own os-thread backend at flat memory. the os-thread
rss collapse from 174 mb to 3.5 mb came free with the july ownership
sweep: the task table's records now release their payloads on eviction
like every other container.

`bench/chan_fanout` — the same fan-out shape with cross-language
comparators: four producer tasks push one million messages through a
bounded channel (capacity 256) and four consumer tasks drain them,
folding each into an order-independent sum. all four languages print the
same checksum. medians of 9 on this 2-core box, 2026-07-26:

| | pith `PITH_GREEN=0` | pith green | go | rust | zig |
|---|---:|---:|---:|---:|---:|
| total (2026-07-26) | 438ms | 171ms | **75ms** | 135ms | 135ms |
| total (2026-07-29) | — | ~133ms | ~75ms | ~75ms | ~240ms |
| peak rss | 3.0 mb | 3.0 mb | 2.0 mb | 2.4 mb | 2.7 mb |

the os-thread column carried a cost the table above predates. the first
channel reclamation (`#979`, august 2026) let the runtime free a closed and
drained channel by discovering for itself that nothing still named it: a
permanent stub, a hazard slot per thread, a limbo list, and a claim taken on
every send and recv. measured against the commit before it under the
interleaved protocol — 21 rounds, arm order rotated each round, a null a/b of
identical binaries first to learn the day's floor — the os-thread backend was
13% to 23% slower at the median; green, whose workers outlive the work, paid
about 2%. ablating the pieces one at a time attributed none of it: removing
the barrier alone, or the entire hazard protocol, moved the benchmark by less
than the null floor, and the per-piece ladder that followed turned out to be
code-layout noise (removing work read *slower* twice). only the whole was
trustworthy. the fix was to make the runtime not need any of it: channel
handles are now counted by the language like strings and lists (`#1020`), and
the guard, stub, and limbo are gone (`#1021`). the language-level count reads
8.8% faster than the guarded runtime at the median on os threads and 12%
slower than the pre-reclaim baseline (two earlier runs: 8% and 9%), against a
null floor of 4% to 5% on those days — about half the regression recovered,
the rest real but small. green is unchanged. the one candidate that was tried
and refuted for the remainder is padding the count onto its own cache line,
which read worse; the untested one is the park path's two atomics.

read the rows that oversubscribe os threads (pith os-thread, rust, zig)
with the box in mind: eight threads on two cores, so they swing run to run.
across two suite runs a week apart rust moved 135 -> 94 ms and zig 135 -> 204
with no code change on either side, and pith's os-thread row has been seen
anywhere from ~260 to ~940. the green and go rows are the stable ones and are
what the comparison rests on.

this table used to read 580 / 782 / 69 / 82 / 201 — pith last by ~8x,
with the green backend slower than os threads on the shape it was built
for. the story of closing it is worth keeping because each step was
measured and two plausible steps failed.

first, the history: splitting the channel's condvar by role and waking
only the opposite role recovered ~19% in 2026-07; dropping the global
handle-registry lock measured *2x worse* (it had been accidentally
spreading futex contention across two futexes); the lock-free mpmc ring
(2026-07-26) fixed the queueing but not the elapsed time; and the two
"obvious" scheduler fixes — spin-before-park and lock-free run queues —
were prototyped or measured out: spinning was flat at three budgets on
two cores, and per-worker queue contention measured two orders of
magnitude below the slab lock's, so those queues were never the problem.

what actually closed it, found by counters rather than intuition: at one
worker — zero lock contention, one park, eight futex wakes — the run
still made ~2 million channel wake slow-paths. rust's std condvar is
futex-based and pays a `futex(FUTEX_WAKE)` syscall on every notify even
with zero waiters, and the channel notified on every wake; green waiters
suspend coroutines and never condvar-wait, so every message bought two
syscalls that woke nobody. counting os-thread waiters per role under the
channel lock and signalling only when one is parked removed ~70% of the
run. moving each task's scheduling state into one atomic word in a
chunked side arena then took the slab lock off the wake and resume paths
entirely (contended slab acquires on the wake path: 133 → 0), worth a
further ~12% on the cross-worker mode.

what remains is placement: the green median mixes ~60ms runs (the
pipeline happened to pin to one worker) with ~130-170ms runs (it split),
and `PITH_GREEN_WORKERS=1` gives ~46ms — faster than go here. making the
scheduler colocate tasks that talk to each other, instead of leaving it
to luck, is the open lever. context switches tell the same story: green
went from 30.9k to 2.5k on this run — fewer than rust's std threads —
against go's 258.

**result and optional locals** — a `T!` or `T?` bound to a name lowers to a
three-slot heap value: a flag, the payload, and the error. releasing one
freed only those three slots, so the payload it owned was never dropped. a
loop that binds a fallible call a million times grew to ~277 mb; the same
loop written `x := call()!` stayed flat, because `!` hands the payload's
count to the caller instead of leaving it in the tuple.

a local now releases that payload when every use of it is provably safe:
the flag reads (`.is_ok`, `.is_err`, `== none`) and the payload reads
(`.ok`, `.err`). a flag read only looks at slot 0. a payload read borrows,
and takes a fresh count where the value escapes the read. either way the
local still holds the count it was built with, so the cleanup is the only
thing that drops it.

| 1m iterations, ~260-byte payload | before | after |
|---|---:|---:|
| optional local, probed with `== none` | 277 mb | **2.5 mb** |
| result local, probed with `.is_err` | 277 mb | **2.5 mb** |
| result local read through `.ok` | 277 mb | **2.5 mb** |

anything else keeps the shell-only release and may still leak: a local
passed to a call, returned whole, consumed by `catch` or `unwrap_or`, bound
by `if let`, or mentioned inside a closure — a capture retains the shell
and not the payload, so cascading there would free memory the closure goes
on to read. a result *parameter* never cascades either; it is a borrow, and
the caller owns the payload.

`.ok` was held out when this first landed, because whitelisting it made an
http/2 valgrind case read freed memory. the cause turned out to be a
channel `try_send` handing its value to another task without taking a
count, which the leak had been covering up; with that fixed the wider
whitelist is clean.

`bench/std_pipeline` — 50k records: csv read/write, transform, json,
gzip:

| phase | go | rust | pith |
|---|---|---|---|
| csv read | 87 | 47 | **5** |
| csv write | 191 | 60 | 257 |
| transform | 48 | 27 | 218 |
| total | 324 | 136 | 482 |

1.5x go overall (4.1x when this document began), 2026-07-21 rerun.
`transform` — per-row field extraction, url and path scanning, and a
hash — used to be the whole gap at 347ms. it walked each url a character
at a time with `s[i] == "/"`, and every one of those reads minted a
one-character heap string just to compare it. rewriting the url and path
scanners to compare raw bytes — and fixing the compiler to actually emit
the byte compare it had been silently skipping for character literals —
cut the phase to 218ms and dropped the run's cstring allocations from
7.1m to 2.9m. pith's csv read is still the fastest of the three. peak
rss at 200k records: go 238 mb, rust 266 mb, pith 239 mb — at parity
with go and below rust, down from 436 mb (1.7x go) earlier and 5.3x go
pre-reclaim. what remains of the gap is byte-buffer string assembly in
`csv write`, not per-character allocation.

`bench/event_ledger` — an ndjson event pipeline in four languages
(pith, go, rust, zig): decode json into structs, aggregate with maps
and a set, sign an hmac-sha256 summary. 200k events:

| phase | go | rust | zig | pith |
|---|---|---|---|---|
| gen | 127 | 31 | 19 | 322 |
| parse | 354 | 62 | 104 | **199** |
| analyze | 16 | 24 | 9 | 56 |
| total | 490 | 122 | 136 | **575** |

about 1.2x go on the total (2026-07-18 rerun), and `parse` — decoding
json into a struct —
is now faster than go's reflection decode: a flat scalar struct is
filled in a single pass straight into the struct, no intermediate map
and no per-field allocation. the remaining gap to go is `gen`, which is
string assembly. the aggregate checksum and the hmac digest come out
identical across all four languages, which is how the benchmark proves
they do the same work.

collection churn — a list and map built and dropped per iteration,
200k iterations:

| | go | rust | pith |
|---|---|---|---|
| peak rss | 8.0 mb | 2.0 mb | **2.6 mb** |
| runtime | 140ms | 61ms | 187ms |

constant memory, 3x under go's gc, matching rust's shape. the
url/path churn variant (heavy substring work) runs 712ms at the same
constant 2.6 mb.

`bench/zstd_codec.pith` — the pure-pith zstd decoder against the crate-backed
kernel, on a corpus built from the repo itself (2:1 to 11:1 — realistic
content-encoding shapes; run it with `make zstd-pure-bench`). the decoder
started at 129x the kernel and three optimization passes brought it to
5.5-8x, with the run-heavy case now beating the kernel outright:

| corpus | first measure | now | vs kernel |
|---|---|---|---|
| text 8kb | 3.4 mb/s | 56 | 8.0x |
| prose 281kb | ~5 | 108 | 7.0x |
| source 585kb | ~7 | 166 | 5.5x |
| json 500kb | ~8 | 343 | 6.2x |
| rle runs 78kb | 29 | 7,475 | **0.63x — faster** |

the story generalized well beyond zstd. the early cost was per-byte
runtime calls (raw blocks and matches copied a byte per call); bulk slice
writes, `copy_within` for overlapping matches, and a cached 64-bit
bitstream window fixed that. the rest was per-operation call overhead
that the compiler now removes for every pith program, not just this one:
`xs[i]` used to call into the runtime, heap-allocate an optional tuple,
unpack it, and release it — the consumer now collapses that whole pattern
to inline loads with the same loud bounds failure; `bits.band` used to
cross two call layers and is now a single native instruction; a word
load from bytes inlines to one 8-byte load and a mask. the decoder-side
share was fusing sequence decode into execution (the intermediate list of
sequence structs cost more than the arithmetic producing it) and packing
every table entry into a plain int, since a struct read in a hot loop is
a handle plus refcount traffic.

two measurement lessons from the same work, recorded here because they
will bite again: an operation's isolated microbenchmark cost overstates
its marginal cost in a real loop by about 5x (the cpu overlaps
independent calls — only differential measurement on the real loop
justifies a change), and a representation change must price the read
side, not just the write (three parallel int lists beat a struct list on
push and lost it all back reading the fields out).

the encoder side exists too, pure pith end to end: stored, huffman, and
full sequence blocks with repeat offsets, verified shape by shape against
the system zstd binary (`make zstd-encode-check` — the system binary must
decode every pith-compressed frame byte-identical, because a round trip
through our own decoder proved nothing twice on this code). one
optimization pass in, it stands here:

| corpus | encode mb/s | vs kernel | size vs kernel |
|---|---|---|---|
| text 8kb | 3 → **8** | 17x | 115% → **101%** |
| prose 284kb | 4 → **15** | 9x | 139% → **106%** |
| source 585kb | 6 → **20** | 12x | 140% → **107%** |
| json 500kb | 11 → **26** | 22x | 188% → **119%** |
| rle runs 78kb | 48 → **51** | 69x | 137% |

both columns moved together; neither was traded for the other. the size
win is mostly per-block fse tables — all three sequence streams used to
emit the predefined distributions, which cost most on data whose
histogram looks nothing like them, and json logs were the worst case at
1.88x. the encoder now histograms what a block actually uses and sends a
table description only when the estimated bits beat predefined by more
than the description costs; the finder also prefers the previous offset
(a repeat code is cheaper than an offset's magnitude) and holds a match
until the next position has been checked for a longer one.

the speed win is mostly writing bits forward instead of recording them.
the backward bitstream writer pushed every field as a value and a width
onto two lists and packed on a second pass; a packer flushing four bytes
at a time into the buffer runs that loop once. that plus removing
per-sequence scratch allocations moved the profile's largest cost from
handle-registry hashing (23%) to the match finder (31%) — which is now
where the next pass would start.

the remaining size gap is not cheaper sequences but fewer of them: json
spends 51,710 bytes on 19,134 sequences against only 3,986 bytes of
literals, so a hash chain with several candidates per bucket is the
untried lever. repeat mode for sequence tables is also unimplemented —
later blocks re-send descriptions nearly identical to the previous
block's, worth a few hundred bytes per multi-block frame at no
throughput cost.

`bench/cyclic_graph` — struct nodes wired into reference cycles
(parent<->child) and dropped, 2m of them. refcounting alone cannot
reclaim a cycle, so the strong version leaks; marking one edge of each
cycle `weak` breaks it and the whole graph reclaims:

| | strong (no weak) | weak edge |
|---|---|---|
| peak rss | 708 mb | **2 mb** |

this is the escape hatch for the one thing reference counting can't do
on its own. the default build has no gc pauses; a `weak` field is a
non-owning reference that reads back as `none` once its target is
freed. an experimental trial-deletion collector exists behind
`PITH_CYCLE_GC=1`, measured and deliberately left off by default —
see docs/ownership.md for the numbers and the recipe that works
(the flag plus explicit `gc_collect()` calls).

the weak-edge row was not always flat. a rerun on 2026-08-11 caught it
climbing linearly — ~120 mb at a million rings — where this table had
long claimed 2. a `weak` field initialized to `none` was storing a
freshly-allocated optional tuple and taking a weak reference to it, but
nothing released that tuple's strong count, so every instance of any
struct with a `weak` field leaked one header-sized allocation. the
constructor now stores a bare `0` for a `none` weak field, and the row
is flat again — the same 2 mb at 100k, 500k and a million rings. the
`leak_weak_field` growth case guards it. the lesson worth keeping: the
leak-growth gate had no weak-bearing shape, so a headline claim drifted
false for weeks with every other benchmark still passing.

`bench/closure_error` — the workload the collection benchmarks don't
reach: closures built, captured, called, and dropped every iteration,
and functions that fail with a heap error, propagate it up with `!`,
and get handled with catch and unwrap_or. 200k iterations, medians of
5 (checksums match across all three, so the work is equivalent):

| phase | go | rust | pith (before) | pith (now) |
|---|---|---|---|---|
| closures | 3 | 0 | 407 | **45** |
| errors | 54 | 33 | 137 | 133 |
| total | 57 | 33 | 543 | **178** |

the closure column is stage A of the plan below, now landed: closures
moved onto a magic-tagged header and off the global handle registry,
and the phase dropped from 407ms to 45ms — a 9x cut, checksum
unchanged. that takes the total from ~9x go down to ~3x. the residual
45ms (vs go's 3ms) is the heap box each closure still allocates, which
is separate, harder work — see the plan.

the error phase is untouched at ~133ms and reasonable: it allocates a
three-slot result tuple and a heap error string per failure, work go
and rust do too (2.5x go).

for the record, the slow version: a closure used to validate and
refcount through the global handle registry — a `Mutex<HashSet>`
locked on every new, retain, release, and validity check — while
strings and structs had already left that registry for a magic-tag
header (see the sprint below). the ~400k closures this benchmark
builds and drops took that lock several times each. the memory work
that prompted the rerun (reference-counting closures instead of
leaking them) had added the release lock, so removing the registry
paid that back too.

`bench/http_server` — a json api under `wrk -t2 -c8` on `/item?id=12345`,
this 2-core machine:

| | go | pith threaded |
|---|---|---|
| req/s | ~31,600 | **16,800** |
| rss | flat ~13 mb | flat (~1 b/req) |

2026-07-15 rerun (20s, `wrk -t2 -c8`): the threaded server — one spawned
os thread per connection — sustains ~16,800 req/s on this 2-core machine,
a bit over half go's ~31,600. go's netpoller stays well ahead.

the per-request growth was long recorded here as ~0.8 kb, then remeasured
2026-07-23 at **~2.8 kb/request** (twice, `wrk -t2 -c8` for 15s), and then
sat **flat** — ~200 kb total over 190k+ requests, about a byte each. that
held until sometime after 2026-07-23: the 2026-08-22 rerun measures ~63
bytes/request growth on the same harness, on both backends (#899). the
history below is the previous round of this same fight, kept because the
first attempt then blamed the wrong construct and the next one may want
the map.

getting there took two goes, and the first was a wrong turn worth
recording. the growth was first blamed on a result-typed local:
`serve_connection` reads each request as `req_result := read_...` then
`req := req_result.ok`, and such a `T!` local was being released as a bare
three-slot shell that never dropped its payload. that is a real bug, fixed
(the reclamation entry above), but fixing it barely moved the server —
remeasured after it, the server still grew ~2.8 kb/request. the request
object was not the leak.

the actual cause was a `for` loop leaking its iterable on an early return.
`serve_connection` and the header helpers it calls do things like

```
for part in query.split("&"):
    if ...:
        return part          # skips the loop's end-of-scope release
```

and a loop over a fresh iterable — a `split` result, a map's `keys()` list —
released that iterable only at the loop's normal end label. an early
`return`/`fail`/`!` from inside the body left the function without reaching
it, leaking the list and everything it held. two such sites on the request
path (the query split and a `for key in headers` lookup) accounted for
essentially the whole 2.8 kb. releasing open loops' iterables at the
function exit edges closes it, and the server holds flat under sustained
load.

build times: go cold 25.0s / warm 0.1s; pith compiles the benchmark
in 2.1s every time, and the entire self-hosted compiler in under 7s.

## why (measured, not guessed)

- ~~every string derive (`concat`, `substring`, `trim`) copies twice~~ —
  fixed (single allocation now). the runtime perf counters
  (`PITH_PERF_STATS=1`) show std_pipeline makes **zero** `pith_string_*`
  allocations (the copy-on-derive string type); the stdlib builds its
  string handling on byte buffers and list elements instead. the churn that
  was left hid in a different counter: indexing a string with `s[i]` mints a
  one-character heap **cstring**, so the url and path scanners allocated one
  per byte. moving them to byte-level scanning cut the run from 7.1m to 2.9m
  cstring allocations (the remaining 2.9m are csv field materialization),
  alongside 2.4m list pushes and 1.1m byte-buffer writes.
- ~~every list/map access takes a global mutex plus a hashset lookup to
  validate the handle~~ — fixed. validity is now a lock-free magic-tag read
  (`list_magic_ok`, `map_magic_ok`); the registry is touched only when a
  handle is registered or unregistered.
- ~~lists and maps store non-int elements as one heap allocation per element~~
  — fixed. packed storage is chosen by element *size*, not by element type
  (`uses_value_storage`: `elem_size == 8`), so strings, collections, structs,
  bytes and closures all live unboxed in `values8` alongside ints.

## the big one: memory was never freed (found july 2026, profiling — since fixed)

`perf` on std_pipeline shows ~23% of wall time inside the kernel zeroing
fresh pages (`clear_page_rep` plus fault handling). the reason: the native
path barely releases anything. the runtime perf counters show zero arc
allocations and zero arc releases in the whole run — bytes objects, c
strings, and most heap allocations are simply never freed, so the heap only
grows and every allocation touches brand-new zeroed pages.

measured at 200k records: pith peaks at 1.65 gb rss, go at 313 mb. the
benchmarks finish because the process exits before the leak matters; a
long-running server pays this as unbounded growth.

this was the single biggest performance and correctness item in the backend,
and it is now closed: the compiler emits releases for the native path. the
subsections below record how, in the order the pieces landed. std_pipeline's
peak rss at 200k records is 239 mb today, level with go's 238 mb — see the
table above. everything from here to the end of this section is history, not
an open problem.

### string arc (landed july 2026)

strings now reclaim. heap cstrings carry a refcount header, and the
compiler emits the ownership operations: retain on binding a borrowed
value, release on reassignment and at every return, transfer on returns,
retains at each escape point (struct fields, containers, tuples, closure
captures). params are borrows — an unmodified string parameter costs no
rc traffic at all — and concat/interpolation chains free their
intermediates as they fold.

measured on a 300k-iteration concat/substring/trim loop:

| | before | after |
|---|---|---|
| peak rss | 85.5 mb | 15.2 mb |
| runtime | 131ms | 81ms |

the header bought a second, unplanned win: `len()` on a heap cstring now
reads the stored length instead of running strlen. profiling showed the
compiler spent ~80% of its own runtime in strlen — every
`while i < s.len()` loop over a large string was quadratic. with the
header length, compiling the whole self-hosted frontend went from ~45
seconds to under 2 seconds. `make self-host` is now a 1.7s operation.

### collection arc (landed july 2026)

lists, maps, and sets are refcounted shared handles now: the emitter
retains on aliasing binds and escapes, releases on rebinding and at every
return, and containers created with a known element type release their
elements when the last count drops. removed and overwritten elements are
deliberately NOT released — a borrow of one may still be live — so those
leak until escape analysis can prove otherwise; only the free path
cascades.

the payoff shows on churn-shaped work (the server case): a loop building
a 50-element list and a small map per iteration, 200k iterations —

| | before | after |
|---|---|---|
| peak rss | 218 mb | 2.6 mb |
| runtime | 343ms | 179ms |

constant memory where growth was unbounded. std_pipeline's peak drops
more modestly (1.65 gb to 1.45 gb at 200k records) because its memory is
dominated by bytes objects and buffers, which still never free.

peak rss on the compiler itself barely moves: that memory is the ast and
token structures held in globals. bytes and structs are the next
reclamation targets, in that order.

### string statement temps (landed july 2026)

the remaining string leak wasn't bytes: it was per-character temps.
`s[i]` on a string mints a fresh one-character string, and every loop
like `while input[i] != ":"` leaked one per comparison. those chars now
classify as owned — a bind takes the count, a comparison releases its
operands in the same block — and empty collection literals in return
position pick up their declared type, so containers like stringbuffer's
parts list own their elements properly.

a url/path parsing loop (200k iterations over std.net.url and
std.os.path helpers):

| | before | after |
|---|---|---|
| peak rss | 439 mb | 40 mb |
| runtime | 1076ms | 1411ms |

eleven times less memory, at ~30% time cost in rc traffic on
char-heavy paths — the tradeoff favors long-running processes.
std_pipeline's peak drops 1.45 gb to 0.88 gb.

a later pass (2026-07-21) removed that tradeoff for the common case.
`s[i] == "/"` and `ord(s[i])` in a scan now read a raw byte instead of
minting a char at all — so there is no allocation to reclaim and no rc
traffic to pay — once two long-dead emitter optimizations were fixed to
actually fire (a single-character literal's stored value carries its
quotes, and a call argument is wrapped in an `arg` node; both checks were
looking straight past that). the comparison-heavy url and path scanners
were rewritten onto it, which is what took std_pipeline's transform from
347ms to 218ms above.

### argument temps (landed july 2026)

the last string-leak class: owned temps in argument position.
`parts.push(s.substring(start, i))` transferred the substring into the
container (which retains), but the temp's own creation count was never
released — the cstring counters showed container pushes and free-time
cascades perfectly balanced at 1.2m each, with exactly the creation
counts leaking. owned string arguments now release right after the
call they feed: callees borrow their params, and storing callees
(containers, struct fields, channels) add their own count.

with this, the url/path churn loop runs at **2.6 mb constant with
alloc == free exactly** — zero string leaks. std_pipeline's peak
drops to 0.66 gb (from 1.65 gb pre-arc). the accumulated rc traffic
now costs std_pipeline ~15% (961→1102ms); eliding provably-redundant
retain/release pairs is the next perf item, ahead of bytes
reclamation.

cranelift itself was generating unoptimized code until july 2026
(`opt_level` defaulted to "none"). turning it to "speed" bought only 2-3%
on these benchmarks, which confirms the hot path is the runtime above, not
the generated code.

## sprint plan and results

| change | std_pipeline total | catalog total (200k) |
|---|---|---|
| baseline (july 2026) | 1273ms | 102ms |
| opt_level=speed | 1236ms | 100ms |
| drop per-access handle lock | 888ms | 93ms |
| single-allocation string derives | no change | no change |
| string arc + o(1) cstring length | 804ms | 99ms |
| collection arc | 904ms* | 103ms |
| string temp reclaim | 961ms | 104ms |
| argument temp reclaim | 1102ms | 104ms |
| inline collection elements | | |

*collection arc costs std_pipeline ~10% in rc traffic; the churn table
above is what it buys.
| arc object-list rework | | |

target: std_pipeline within ~1.5x of go (about 460ms). compile time is
tracked too: `pith build bench/std_pipeline.pith` went 4.24s to 4.34s with
opt_level=speed.

## closure performance plan (stage A landed, july 2026)

`bench/closure_error` put a number on the one workload the collection
benchmarks never reach: closures were ~135x slower than go (407ms vs
3ms for 200k iterations building and dropping ~400k closures). the
error phase in the same benchmark is fine (2.5x go), so this section
is about closures. stage A has since landed and cut the closure phase
to 45ms (9x); the write-up below is kept as the record of what changed
and what is left.

**root cause, confirmed by reading the runtime.** a list validates a
handle with `list_magic_ok` — read a magic tag from the object's own
header, no lock (`collections/list.rs`). a closure validates with
`handle_registry::is_valid`, which takes a global `Mutex<HashSet>`
(`runtime_core.rs:354,361`). and it takes that lock on *every* closure
operation: `new` registers, `retain`/`release` and every `get_fn` /
`get_env` / `set_env` check validity, `release` at zero unregisters.
the "drop per-access handle lock" row in the sprint table above did
this for collections and bought 888ms on std_pipeline. closures were
never converted. so the fix is not new design — it is finishing a
migration that already happened for every other heap type.

### stage A — closures onto a magic header (landed, 407ms → 45ms)

`PithClosure` got a magic tag at offset 0 (`CLOSURE_MAGIC`), the same
shape structs use (`STRUCT_MAGIC` / `struct_base`,
`runtime_core.rs:1137,1146`):

- `pith_closure_new` writes the tag instead of calling `register`
- a `closure_base(handle)` helper null-checks, alignment-checks, and
  reads the tag; every `is_valid(.., Closure)` call site uses it
- `pith_closure_release` at its last count scrubs the tag (writes 0)
  before `dealloc`, instead of calling `unregister` — a use-after-free
  then reads a dead tag and returns the safe default, exactly as a
  freed struct does
- `HandleKind::Closure` and its registry calls come out

nothing about the closure lifecycle changes — the ref count, the
captured-slot release from #303, and the indirect-call abi all stay.
only the *validity mechanism* moves off the lock.

**risk.** the magic read dereferences the handle, where the registry
check did not. closures only ever reach these functions from the
checker's closure-typed path (a real box) or as 0 (null-guarded), the
same risk profile lists and structs already accept. the scrub-on-free
turns a stale handle into a safe miss rather than a wild call.

**verified.** `bench/closure_error` closure_ms fell from 407 to 45
(median of 5), checksum unchanged. valgrind stayed clean on the churn
(0 errors, 0 definite leaks — the scrub is what makes a double-free or
use-after-free a safe miss, not just unlikely). full suite, fixed
point, and seed all passed, since the runtime `.a` relinks into every
program.

**the residual, as predicted.** the lock was the dominant cost, and
removing it closed most of the gap. what stays is the 45ms vs go's
3ms: pith heap-allocates a box per closure where go and rust keep the
environment on the stack or inline it. that is a separate, harder
optimization (a closure arena or escape analysis) and only worth it if
a real workload still shows the box allocation now that the lock is
gone.

### stage B — the same lock on `AtomicInt` and the other primitives

`AtomicInt` (added for thread-safe contexts) went onto the registry
too, mirroring `Semaphore`. contexts are not a hot loop today, so it is
not proven costly — but it is the identical one-line-per-callsite
conversion and worth doing in the same pass. `Channel`, `Task`,
`Process`, `Semaphore`, `WaitGroup` also use the registry (`Mutex` and
`Channel` no longer do — both validate through their own magic tag);
most are created rarely, but channel send/recv could be hot in
channel-heavy code and deserves a measurement before converting. the
end state is that nothing validates a live handle through a global
lock.

### order and stopping rule

stage A landed first and alone — the measured win, its valgrind +
suite pass the gate for touching the others. that took the benchmark
total from 543ms to 178ms. stage B stays open and stays conditional:
convert a primitive only where a benchmark shows the lock (a channel
microbench for `Channel`, the context path for `AtomicInt`), not for
symmetry. the remaining gap to go's ~57ms total is now the error phase
and the per-closure box, not the lock.

## how to rerun

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

go and rust counterparts build per `bench/README.md`; `closure_error`
builds with `go build -o bench/closure_error_go bench/closure_error.go`
and `rustc -O bench/closure_error.rs -o bench/closure_error_rust`.
`chan_fanout_bench.sh` builds its own four (pith, go, rustc, zig) and
refuses to print a table unless every run agrees on the checksum; it
measures pith twice, once per backend.
