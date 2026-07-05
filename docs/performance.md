# performance

live notes on where pith's time goes and what the sprint is doing about it.
the april 2026 audit of the old c-transpiler era lives in
`docs/history/performance_audit_2026_04.md`.

numbers below are medians (5-7 trials) on one machine, july 2026. rerun with
the helpers in `bench/` before trusting them on different hardware.

## where pith stands

`bench/catalog_workload` (in-process service shape: lookups, filtered
searches, batch json; 200k iterations):

| | go | rust | pith |
|---|---|---|---|
| total | 22ms* | 3ms* | 100ms / 5ms* |

*go and rust at 10k iterations for scale: go 22ms, rust 3ms, pith 5-6ms.
pith beats go here and sits within ~2x of rust. compute-shaped code is fine.

`bench/std_pipeline` (50k records: csv read/write, per-record transform,
json, gzip):

| phase | go | rust | pith |
|---|---|---|---|
| csv read | 86 | 47 | 6 |
| csv write | 180 | 57 | 390 |
| transform | 44 | 27 | 839 |
| total | 308 | 133 | 1236 |

string-heavy code is the problem: the transform phase turns url and path
fields into fresh strings per record, and pith pays for it at ~19x go.

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

peak rss on the compiler itself barely moves: that memory is the ast and
token structures, which still never free. collections, bytes, and structs
are the next reclamation targets, in that order.

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
| inline collection elements | | |
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
