# performance

live notes on where pith's time goes and what the sprint is doing about it.
the april 2026 audit of the old c-transpiler era lives in
`docs/history/performance_audit_2026_04.md`.

numbers below are medians (5-7 trials) on one machine, july 2026. rerun with
the helpers in `bench/` before trusting them on different hardware.

## where pith stands

all numbers from one idle machine, july 2026, medians of 5 where the
benchmark is quick enough to repeat. comparators: go 1.24.4 net/http
and encoding/*, rust with a minimal hand-rolled http server (a fair
model of the same single-threaded blocking loop pith uses, but not a
full http stack — read that column generously).

`bench/catalog_workload` — in-process service shape: lookups, filtered
searches, batch json; 200k iterations:

| | go | rust | pith |
|---|---|---|---|
| total | 409ms | 72ms | **123ms** |

3.3x faster than go, within 1.7x of rust.

`bench/std_pipeline` — 50k records through csv read/write, per-record
transform, json, gzip:

| phase | go | rust | pith |
|---|---|---|---|
| csv read | 80 | 50 | **6** |
| csv write | 181 | 63 | 294 |
| transform | 44 | 27 | 464 |
| total | 312 | 143 | 766 |

2.5x go overall. peak rss at 200k records: go 310 mb, rust 273 mb,
pith 456 mb — within 1.5x of both, down from 5.3x when this document
started.

collection churn — a list and map built and dropped per iteration,
200k iterations (the long-running-server allocation shape):

| | go | rust | pith |
|---|---|---|---|
| peak rss | 16.6 mb | 2.1 mb | **2.6 mb** |
| runtime | 136ms | 63ms | 202ms |

constant memory, 6.4x under go's gc, matching rust's shape.

`bench/http_server` — a json api under wrk for two minutes, rss
sampled throughout:

| | go net/http | rust minimal | pith |
|---|---|---|---|
| throughput | 13,683/s | 8,060/s | **8,720/s** |
| rss | 12 mb flat | 2 mb flat | grows 1.6 kb/request |

**pith out-serves the rust comparator** and sits within 1.6x of go's
net/http — which multiplexes goroutines even on one core, where pith
and the rust loop both block on accept. when this benchmark was
created pith served 277 req/s and grew 60 kb per request; the growth
line is now 1.6 kb (header-map strings and a few residual structs,
attributed and queued).

the thread-per-connection story took two rounds. round one (spawn per
accept, no keep-alive, condvar-mutex registry guards) measured 9x
slower than the single-threaded loop — a thread spawn per request,
and 2.3x lock overhead before any concurrency existed. round two
supplied the three missing ingredients: http keep-alive (the servers
now use serve_connection_fd, so a connection is one long-lived
handler), a rewritten pith mutex (one compare-exchange each way,
spin-then-yield — the old one paid a handle-registry lock plus a
condvar per operation), and locked std/io registries whose critical
sections never cover a socket read or write.

| 60s sustained, 2 cores | single-threaded | thread per connection |
|---|---|---|
| pith keep-alive | 8,334/s | **15,823/s** |

threaded pith beats go's single-core 13,683/s and sits within 1.25x
of go using both cores (19,854/s). the io registries are now safe
under concurrent handlers, which also makes the readme's no-data-races
claim true for spawned code that shares streams.

seventh landing (byte comparisons): `s[i] == ":"` compared two
freshly allocated one-character strings; scanners paid an allocation,
a refcount round-trip, and a string equality per character per test.
single-byte comparisons — an index against a one-character literal,
a chr(n), or another index — now lower to an allocation-free byte_at
and an integer compare, and ord(s[i]) reads the byte directly. a
scanner loop making three tests per character across 200k strings
runs with zero string allocations. url/path churn drops to 643ms and
std_pipeline to 657ms, the best recorded.

build times: go cold 20.5s / warm 0.1s; pith compiles the same
program in 2.0s, every time. `make self-host` — the compiler
compiling itself — is 2.1s with the full ownership discipline active.

## why (measured, not guessed)

- ~~every string derive (`concat`, `substring`, `trim`) copies twice~~ —
  fixed (single allocation now), but it turned out not to matter for these
  benchmarks: the runtime perf counters (`PITH_PERF_STATS=1`) show
  std_pipeline makes **zero** `pith_string_*` allocations. the stdlib builds
  its string handling on byte buffers and list elements instead, so the
  transform cost is 2.4m list pushes and 1.1m byte-buffer writes per run.
- every list/map access takes a global mutex plus a hashset lookup to
  validate the handle (`cranelift/runtime/src/handle_registry.rs`).
- lists and maps store non-int elements as one heap allocation per element
  (`collections/list.rs`, `collections/map.rs`); ints already have an
  unboxed fast path.
- arc keeps a global object list with o(n) removal and scans for cycles
  every 100 releases (`cranelift/runtime/src/arc.rs`).

## the big one: memory is never freed (found july 2026, profiling)

`perf` on std_pipeline shows ~23% of wall time inside the kernel zeroing
fresh pages (`clear_page_rep` plus fault handling). the reason: the native
path barely releases anything. the runtime perf counters show zero arc
allocations and zero arc releases in the whole run — bytes objects, c
strings, and most heap allocations are simply never freed, so the heap only
grows and every allocation touches brand-new zeroed pages.

measured at 200k records: pith peaks at 1.65 gb rss, go at 313 mb. the
benchmarks finish because the process exits before the leak matters; a
long-running server pays this as unbounded growth.

fixing this is the single biggest performance and correctness item in the
backend — bigger than everything in the table below combined. it needs the
compiler to emit releases (or a region strategy) for the native path, not
just runtime tweaks.

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

## how to rerun

```
./target/release/pith build bench/std_pipeline.pith
for i in 1 2 3 4 5; do ./bench/std_pipeline 50000; done   # take medians
./target/release/pith build bench/catalog_workload.pith
for i in 1 2 3 4 5; do ./bench/catalog_workload 200000; done
```

go and rust counterparts build per `bench/README.md`.
