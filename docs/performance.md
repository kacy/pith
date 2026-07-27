# performance

live notes on where pith's time goes and what the sprint is doing about it.
the april 2026 audit of the old c-transpiler era lives in
`docs/history/performance_audit_2026_04.md`.

numbers below are medians (5-7 trials) on one machine, july 2026. rerun with
the helpers in `bench/` before trusting them on different hardware.

## where pith stands

all numbers from one 2-core machine, medians of 5 where quick enough to
repeat; the tables below were fully rerun 2026-07-15 (after the arc
reclamation and weak-reference work), the grpc table again on 2026-07-19
after the unary-response coalescing, and std_pipeline again on 2026-07-21
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
the green backend to 171ms, ahead of rust and zig and 2.3x go, with
~46ms — faster than go on this box — when the pipeline is pinned to one
worker. the batch benchmarks measure compute and pith is competitive
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
no async runtime. 20,000 calls each, july 2026:

| calls/sec | go | rust (tonic) | pith |
|---|---|---|---|
| 16 B, sequential | 4011 | 2750 | **3353** |
| 1 KiB, sequential | 3862 | 2601 | **3220** |
| 16 B, 8 concurrent, one connection | 13845 | 11435 | **6832** |
| 1 KiB, 8 concurrent, one connection | 11242 | 9287 | **6084** |

sequentially pith is ~84% of grpc-go and a bit ahead of tonic — close, for
a stack that is pith all the way down (its own tls 1.3, http/2, hpack,
protobuf). concurrency over a single connection is the weaker spot: eight
streams lift pith ~2x where go and rust scale ~3.5x. the ceiling there is
not stream count but the per-call latency of the internal pipeline — every
frame crosses worker → writer → socket → reader → worker, and each hop is a
thread handoff (a futex wake plus a context switch). the single reader/writer
is correct and required for hpack ordering. coalescing removed the redundant
handoffs on that path (below); the ones that remain are structural — one
context switch per pipeline thread per call — so the way to more parallelism
past that is more connections, also below.

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

the experimental green backend (`PITH_GREEN=1`, see `docs/concurrency.md`) is
that same in-userspace scheduler, and this benchmark is the case it was built
for. running the reader, writer, and worker tasks as coroutines on one worker
(`PITH_GREEN_WORKERS=1`) turns every per-call handoff from a futex wake into a
userspace switch. measured on the 2-core dev box, conc=8, medians of 5 runs,
per-call counts over warmup+calls (2026-07-20 rerun).

**these absolute numbers predate the wake-path work of 2026-07-26** (the
coroutine stack pool, the channel condvar fix, and the slab-free wake), so
they understate green as it stands. re-running the same shape on 2026-07-27
at a smaller batch put pith at 5067 calls/sec os-thread, 8075 at one green
worker (+59%), and 7083 at two (+40%) — the same relationships the table
below records (+53% and +41%), so its conclusions hold. the table is left at
its original batch size rather than replaced with a smaller, non-comparable
run; re-measuring it at the documented batch is a tracked follow-up.

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
fighting the client for them, is what a pool would need to scale. green is off by
default, so the table at the top of this section is the os-thread backend.

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
it is flat: 500k tasks or five million, the peak is the batch. the default
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
| os threads | ~1450 ms | 174 mb |
| green, before the pool | ~580 ms | 10 mb |
| green, after the pool | ~50 ms | 10 mb |
| go | ~27 ms | 11 mb |

page faults over the run fell 21832 -> ~2500 and context switches 25748 ->
~3000. shrinking the stack from 1 MiB to 64 KiB changed nothing before the
pool, which is the tell: the cost was the *number* of mappings, not their
size. green is now within ~2x of go on this shape at the same memory, from
~29x, and roughly 30x faster than the os-thread backend, which pays a real
thread per task.

`bench/chan_fanout` — the same fan-out shape with cross-language
comparators: four producer tasks push one million messages through a
bounded channel (capacity 256) and four consumer tasks drain them,
folding each into an order-independent sum. all four languages print the
same checksum. medians of 9 on this 2-core box, 2026-07-26:

| | pith | pith green | go | rust | zig |
|---|---:|---:|---:|---:|---:|
| total | 438ms | 171ms | **75ms** | 135ms | 135ms |
| peak rss | 3.0 mb | 3.0 mb | 2.0 mb | 2.4 mb | 2.7 mb |

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

`bench/cyclic_graph` — struct nodes wired into reference cycles
(parent<->child) and dropped, 2m of them. refcounting alone cannot
reclaim a cycle, so the strong version leaks; marking one edge of each
cycle `weak` breaks it and the whole graph reclaims:

| | strong (no weak) | weak edge |
|---|---|---|
| peak rss | 708 mb | **2 mb** |

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
| rss | flat ~13 mb | flat (~1 b/req) |

2026-07-15 rerun (20s, `wrk -t2 -c8`): the threaded server — one spawned
os thread per connection — sustains ~16,800 req/s on this 2-core machine,
a bit over half go's ~31,600. go's netpoller stays well ahead.

the per-request growth was long recorded here as ~0.8 kb, then remeasured
2026-07-23 at **~2.8 kb/request** (twice, `wrk -t2 -c8` for 15s), and now
sits **flat** — ~200 kb total over 190k+ requests, so about a byte each,
which is noise.

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
./target/release/pith build bench/green_fanout.pith       # per-task memory under fan-out
PITH_GREEN=1 ./bench/green_fanout 200000                  # green: flat rss; drop the flag to see os-thread grow
PITH_GREEN=1 ./bench/green_fanout 500000                  # peak rss should barely move
bench/chan_fanout_bench.sh 1000000 9                      # channels, four languages, checksum-checked
```

go and rust counterparts build per `bench/README.md`; `closure_error`
builds with `go build -o bench/closure_error_go bench/closure_error.go`
and `rustc -O bench/closure_error.rs -o bench/closure_error_rust`.
`chan_fanout_bench.sh` builds its own four (pith, go, rustc, zig) and
refuses to print a table unless every run agrees on the checksum; it
measures pith twice, once per backend.
