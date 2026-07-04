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

- every string derive (`concat`, `substring`, `trim`, interpolation) copies
  twice: once into an arc, once into the runtime allocation
  (`cranelift/runtime/src/string.rs`).
- every list/map access takes a global mutex plus a hashset lookup to
  validate the handle (`cranelift/runtime/src/handle_registry.rs`).
- lists and maps store non-int elements as one heap allocation per element
  (`collections/list.rs`, `collections/map.rs`); ints already have an
  unboxed fast path.
- arc keeps a global object list with o(n) removal and scans for cycles
  every 100 releases (`cranelift/runtime/src/arc.rs`).

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
| single-allocation string derives | | |
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
