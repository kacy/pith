# performance

live notes on where pith's time goes and what the sprint is doing about it.
the april 2026 audit of the old c-transpiler era lives in
`docs/history/performance_audit_2026_04.md`.

numbers below are medians (5-7 trials) on one machine, july 2026. rerun with
the helpers in `bench/` before trusting them on different hardware.

## where pith stands

all numbers from one 2-core machine, medians of 5 where quick enough to
repeat; the tables below were fully rerun 2026-07-15 (after the arc
reclamation and weak-reference work). the standout change since the last
rerun: std_pipeline's peak rss fell to parity with go (239 vs 238 mb,
was 1.7x), and the cyclic-graph benchmark below shows weak references
reclaiming reference cycles refcounting alone can't. json struct decode
stays faster than go's reflection decode — a flat struct of required
scalars decodes in a single pass, filled straight into the struct.

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
no async runtime. 20,000 calls each, july 2026:

| calls/sec | go | rust (tonic) | pith |
|---|---|---|---|
| 16 B, sequential | 3955 | 3002 | **3175** |
| 1 KiB, sequential | 3820 | 2786 | **2943** |
| 16 B, 8 concurrent, one connection | 13585 | 11224 | **5753** |
| 1 KiB, 8 concurrent, one connection | 10990 | 8981 | **5600** |

sequentially pith is ~77% of grpc-go and about matches tonic — close, for
a stack that is pith all the way down (its own tls 1.3, http/2, hpack,
protobuf). concurrency over a single connection is the weak spot: eight
streams lift pith ~1.8x where go and rust scale ~3.4x. the ceiling there is
not stream count but the per-call latency of the internal pipeline — every
frame crosses worker → writer → socket → reader → worker, and each hop is a
thread handoff (a futex wake plus a context switch). the single reader/writer
is correct and required for hpack ordering, so the way past one connection is
more connections, below.

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
pith from ~2100 to ~2850, roughly ~50% to ~72% of grpc-go. the table above
is a later sweep still, after the std thread-safety fixes and the freelist
below, at ~3175 (~77%). the box is too noisy to resolve the smaller items
(cached headers, coalescing, shared event list) on wall-clock, so they stand
on counted structural reductions; a quiet machine would sharpen the numbers.

the newest pass inlines the whole request, not just its body. a client now
starts single-threaded: one in-flight call runs synchronously on the caller
over the same lockstep codec the one-shot get() uses — no reader or writer
task, no channel handoff — and the client promotes to the multiplexing pipeline
exactly once, the first time a second stream appears (the inline fast-path in
std/net/http2/connection.pith). because promotion latches to the old pipeline
the instant there is concurrency, the 8-concurrent path is unchanged by design;
the win is entirely on sequential traffic, the common grpc unary shape. a
controlled before/after on the same box put a sequential unary call at ~340 µs,
down from ~368 (~8%), and a clean sweep read 16 B sequential around 3400
calls/sec, up from 3175 — still ~80% of grpc-go, which drifted up the same day.
a full three-client re-sweep for the table above is pending a quieter machine:
this dev box was too contended to trust the comparators, which swung roughly 2x
between back-to-back runs.

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

`bench/std_pipeline` — 50k records: csv read/write, transform, json,
gzip:

| phase | go | rust | pith |
|---|---|---|---|
| csv read | 72 | 47 | **4** |
| csv write | 198 | 61 | 250 |
| transform | 38 | 25 | 348 |
| total | 310 | 134 | 610 |

2.0x go overall (4.1x when this document began), 2026-07-15 rerun.
`transform` — heavy per-row string building — is the whole gap; pith's
csv read is the fastest of the three. peak rss at 200k records: go
238 mb, rust 266 mb, pith 239 mb — now at parity with go and below rust,
down from 436 mb (1.7x go) earlier and 5.3x go pre-reclaim. the
reclamation work closed the memory gap entirely on this workload; the
runtime gap is string-assembly throughput, not allocation.

`bench/event_ledger` — an ndjson event pipeline in four languages
(pith, go, rust, zig): decode json into structs, aggregate with maps
and a set, sign an hmac-sha256 summary. 200k events:

| phase | go | rust | zig | pith |
|---|---|---|---|---|
| gen | 118 | 29 | 19 | 307 |
| parse | 353 | 60 | 104 | **186** |
| analyze | 14 | 24 | 9 | 59 |
| total | 486 | 111 | 129 | **566** |

about 1.2x go on the total (2026-07-15 rerun), and `parse` — decoding
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

`bench/cyclic_graph` — struct nodes wired into reference cycles
(parent<->child) and dropped, 2m of them. refcounting alone cannot
reclaim a cycle, so the strong version leaks; marking one edge of each
cycle `weak` breaks it and the whole graph reclaims:

| | strong (no weak) | weak edge |
|---|---|---|
| peak rss | 368 mb | **10 mb** |

this is the escape hatch for the one thing reference counting can't do
on its own. pith has no cycle collector by design (no gc pauses); a
`weak` field is a non-owning reference that reads back as `none` once
its target is freed. see docs/ownership.md.

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
| rss | flat ~13 mb | +0.8 kb/req |

2026-07-15 rerun (20s, `wrk -t2 -c8`): the threaded server — one spawned
os thread per connection — sustains ~16,800 req/s on this 2-core machine,
a bit over half go's ~31,600. go's netpoller stays well ahead. the
residual growth is down to ~0.8 kb/request (from ~1.3–1.6 earlier), a
real per-request leak in the request path that is still unfixed —
throughput is steady but rss climbs under sustained load.

build times: go cold 25.0s / warm 0.1s; pith compiles the benchmark
in 2.1s every time, and the entire self-hosted compiler in under 7s.

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
`Process`, `Mutex`, `Semaphore`, `WaitGroup` also use the registry;
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
```

go and rust counterparts build per `bench/README.md`; `closure_error`
builds with `go build -o bench/closure_error_go bench/closure_error.go`
and `rustc -O bench/closure_error.rs -o bench/closure_error_rust`.
