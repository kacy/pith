# performance

live notes on where pith's time goes and what the sprint is doing about it.
the april 2026 audit of the old c-transpiler era lives in
`docs/history/performance_audit_2026_04.md`.

numbers below are medians (5-7 trials) on one machine, july 2026. rerun with
the helpers in `bench/` before trusting them on different hardware.

## where pith stands

all numbers from one 2-core machine in one sitting, july 2026,
medians of 5 where quick enough to repeat. rerun 2026-07-11 after a
long correctness and memory pass — the value-lowering fixes, the
closure lifecycle (closures are reference counted now, not leaked),
error-path cleanup, wider enum payload boxing, and atomic-cell
contexts. catalog 111ms (median of 104-130), pipeline 683ms (659-762),
go and rust comparators steady (go catalog ~393, go pipeline ~303,
rust catalog 68). flat against the previous 111ms / 671ms — the extra
retain/release traffic and cleanup those fixes add never shows on
these benchmarks, which build and drop collections rather than
closures or error paths. peak rss stayed healthy: catalog 51 mb at
200k iterations, pipeline 108 mb at 50k records. the takeaway held one
more time: correctness came free. the rust pipeline comparator's
source is no longer in bench/, so its column is historical. absolute
values drift a few percent between days; within a table they're
comparable.
comparators: go 1.24.4 (net/http, encoding/*), rust (a minimal
hand-rolled http server modeling the same blocking loop — not a full
stack, read it generously).

`bench/catalog_workload` — service-shaped compute: lookups, filtered
searches, batch json; 200k iterations:

| | go | rust | pith |
|---|---|---|---|
| total | 393ms | 68ms | **111ms** |

3.5x faster than go, within 1.6x of rust.

`bench/std_pipeline` — 50k records: csv read/write, transform, json,
gzip:

| phase | go | rust | pith |
|---|---|---|---|
| csv read | 85 | 52 | **6** |
| csv write | 191 | 63 | 271 |
| transform | 51 | 29 | 395 |
| total | 303 | 144 | 683 |

2.2x go overall (4.1x when this document began). peak rss at 200k
records: go 254 mb, rust 273 mb, pith 436 mb — 1.7x go, down from
5.3x pre-reclaim.

collection churn — a list and map built and dropped per iteration,
200k iterations:

| | go | rust | pith |
|---|---|---|---|
| peak rss | 8.0 mb | 2.0 mb | **2.6 mb** |
| runtime | 140ms | 61ms | 187ms |

constant memory, 3x under go's gc, matching rust's shape. the
url/path churn variant (heavy substring work) runs 712ms at the same
constant 2.6 mb.

`bench/closure_error` — the workload the collection benchmarks don't
reach: closures built, captured, called, and dropped every iteration,
and functions that fail with a heap error, propagate it up with `!`,
and get handled with catch and unwrap_or. 200k iterations, medians of
5 (checksums match across all three, so the work is equivalent):

| phase | go | rust | pith |
|---|---|---|---|
| closures | 3 | 0 | **407** |
| errors | 54 | 33 | **137** |
| total | 57 | 33 | **543** |

this is where pith is genuinely slow — about 9x go on the total, and
the closures alone are two orders of magnitude off. the error phase is
closer (2.5x go) and reasonable: it allocates a three-slot result
tuple and a heap error string per failure, work go and rust do too.

the closure gap is a fixable one and the cause is known. a closure
still validates and refcounts through the global handle registry — a
`Mutex<HashSet>` locked on every new, retain, release, and validity
check. strings and structs left that registry for a magic-tag header
(see the sprint below) and got much faster; closures never did. so
every one of the ~400k closures this benchmark builds and drops takes
that lock several times. giving closures a magic-tagged header, the
same treatment collections got, is the obvious next optimization and
this benchmark is here to measure it. the memory work that prompted
this rerun (reference-counting closures instead of leaking them) added
the release lock, so the fix pays that back too.

`bench/http_server` — a json api under wrk for two minutes:

| | go 1-core | go all-cores | rust minimal | pith single | pith threaded |
|---|---|---|---|---|---|
| req/s | 13,838 | 14,584 | 7,606 | **7,583** | 7,916 |
| rss | 12 mb flat | 13 mb flat | 2 mb flat | +1.3 kb/req | +1.3 kb/req |

single-threaded pith serves at parity with the single-threaded rust
comparator through a full http stack. the threaded server has reached
15,800 req/s on this machine when the load generator wasn't competing
for the same two cores; with wrk co-located it converges with the
single-threaded number. go's netpoller stays ahead either way. the
residual growth is ~1.3 kb/request, attributed down to header-name
copies and a few structs in the leak reporter.

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

## closure performance plan (open, july 2026)

`bench/closure_error` put a number on the one workload the collection
benchmarks never reach: closures are ~135x slower than go (407ms vs
3ms for 200k iterations building and dropping ~400k closures). the
error phase in the same benchmark is fine (2.5x go), so this section
is about closures.

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

### stage A — closures onto a magic header (the win)

give `PithClosure` a magic tag at offset 0, the same shape structs use
(`STRUCT_MAGIC` / `struct_base`, `runtime_core.rs:1137,1146`). then:

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

**verify.** `bench/closure_error` is the measuring stick — closure_ms
should fall from ~407 toward single or low-double digits. valgrind the
churn (it must stay clean — the scrub is what makes a double-free or
use-after-free impossible, not just unlikely). full suite + fixed
point + seed, since the runtime `.a` relinks into every program.

**expected result.** the lock is the dominant cost, so this should
close most of the gap. a residual stays: pith heap-allocates a box per
closure where go and rust can keep the environment on the stack or
inline it. that is a separate, harder optimization (a closure arena or
escape analysis) and only worth it if a real workload still shows the
box allocation after the lock is gone.

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

stage A first and alone — it is the measured win, and its valgrind +
suite pass is the gate for touching the others. stage B only where a
benchmark shows the lock (a channel microbench for `Channel`, the
context path for `AtomicInt`); do not convert a primitive just for
symmetry without a number. the goal is `bench/closure_error` total
from 543ms toward go's ~57ms, closures leading the drop.

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
