# pithgen

a grammar-directed program generator with crash oracles. where
`tools/fuzz` mutates source text to probe the frontend's loud-failure
guarantees, pithgen builds well-typed programs from a symbol table —
every reference valid, every expression the type its position asks
for — so nearly everything it emits gets PAST the checker and
exercises the emitter, the ir consumer, and the runtime's ownership
paths. the two harnesses are complementary: one asks "does hostile
text fail loudly", the other asks "does accepted code actually work".

## generation

`gen.rs` grows a typed AST over structs, enums (with and without
payloads), interfaces with associated types, generic structs and fns
(including self-referential `next: T?`), optionals in every position,
lists/maps/sets, closures, spawn/await/channels, module aliases across
1–3 modules, weak fields, and `T!` results with catch. the dials are
weighted toward the intersections that have historically broken:
generics × optionals × cross-module × enum payloads.

alongside every expression the generator keeps a small semantic tree
(`eval.rs`) and an environment of the values its locals hold, so each
emitted print also appends the line it must produce to the program's
expected output. this is bookkeeping over the generator's own choices,
not a pith interpreter: fn bodies are recorded as they are written and
replayed at call sites, match arms are resolved against the known
subject, channel pumps send a counted number of messages. constructs
whose printed value cannot be pinned (spawn interleavings, say) are
simply not printed. `gen --seed N` shows the prediction after the
files; `--out` writes it as expected.txt.

same seed, same program: the prng is splitmix64-seeded xoshiro256**,
and generation never consults time or environment. the expected
output is part of the same determinism contract.

## oracles

- **check-crash** — `pith check` dies by signal or fails with no
  diagnostic. always a bug.
- **build-fail** — check accepts, build fails. always a bug: this is
  the checked-means-buildable seam, and "IR consumer verifier error"
  here is a compiler defect by definition.
- **build-crash / run-crash** — a signal death anywhere. a clean
  "pith runtime error" exit 1 is a controlled failure, not a finding.
- **run-silent / run-hang** — the binary exits without its final
  marker, or outlives the timeout.
- **wrong-output** — the differential oracle. the generator picks every
  literal, every arithmetic operand, every enum variant, so while it
  builds the tree it also computes the exact line each print must
  produce. after a clean run the actual stdout is compared line by
  line against that prediction; any divergence is a silent wrong
  answer — the class of bug no crash oracle can see. a line whose
  value genuinely cannot be pinned at generation time can be marked
  as a wildcard the comparator skips; the summary counts them (the
  current generator emits none).
- **valgrind** (opt-in) — memcheck on the built binary.

findings dedup by a normalized signature (paths, numbers, and
generated identifiers stripped), so a 500-seed batch reports root
causes, not instances. a run-crash goes one step further: the crashed
binary is rerun under valgrind, the faulting address is parsed out,
and a coarse fault-site class (null-ish below 0x1000, small-int-as-
pointer below 0x100000, other) is folded into the signature, so two
different bad-pointer bugs that both end in SIGSEGV no longer share
one bucket. valgrind being absent just falls back to the plain
signature.

the wrong-output signature keeps the first mismatching line index and
the expected-vs-actual pair with digit runs collapsed; finding.txt
carries the surrounding context, plus expected.txt and stdout.txt for
a straight diff.

## ownership shapes and the sanitize oracle

most programs (85%) also get an ownership block: fixed helper declarations
(`ONode` with a strong `next` and a weak `back`, `OHolder`, `OSlot`, a
global list, churn and refill helpers) and a random subset of 23 shapes aimed
at the emitter's borrow-or-own decision. these are the shapes that have
broken before, plus their neighbors: a `for` loop whose body clears, removes,
overwrites or pushes onto the list it walks, directly or through a call, a
global or a closure; a field, element or optional passed alongside its owner
to a callee that overwrites it; weak fields read off a live owner, off a
temporary and through `if let`; optionals and payloads bound from a field
that is then cleared or rebound; early return, `continue` and nested loops
over mutated lists. every value they print is a fresh heap string the
generator computes, so a stale read shows up as filler.

on the plain toolchain these run clean, because the struct freelist recycles
a freed block before anything reads it. `--sanitize` changes how the built
binary runs. it links against whatever runtime `PITH_RUNTIME_LIB` names,
through the `CC` linker wrapper (both inherited from the environment), and
runs four times: freelist off and on, green backend on and off, with asan set
to recover. the oracles:

- **sanitizer**: an AddressSanitizer report other than the known-benign
  cstring header probe (a global-buffer-overflow just before an `alloc_`
  runtime static). the signature is the error kind plus the innermost
  generated frame of the access, the free and the allocation, so one bug is
  one bucket, and every distinct report in a program is recorded.
- **nondeterministic**: stdout differs between the four settings.
- run-crash and wrong-output as usual, on the canonical run (freelist off,
  green on).

```
make pithgen-sanitize PITHGEN_CC=/path/to/cc-asan \
    PITHGEN_RUNTIME_LIB=/path/to/asan/libpith_runtime.a PITHGEN_SEEDS=0..2000
```

build the asan runtime with `RUSTC_BOOTSTRAP=1 RUSTFLAGS="-Zsanitizer=address
-Zsanitizer-recover=address -Cforce-frame-pointers=yes" cargo build --release
-p pith-runtime --target x86_64-unknown-linux-gnu`. the make target hands
`PITHGEN_CC` and `PITHGEN_RUNTIME_LIB` to the run as `CC` and
`PITH_RUNTIME_LIB`, so the compiler it builds first stays uninstrumented. the
wrapper has to
add rustc's `librustc-stable_rt.asan.a` to the link. `pithgen reduce
--sanitize` reduces a sanitizer or nondeterministic finding under the same
matrix. there is no leak oracle yet: the generated programs run once, and a
growth check needs a repeated-round mode they do not have.

## reducing a finding

```
tooling/pithgen/target/release/pithgen reduce \
    --seed 393 --pith ./target/release/pith --out /tmp/reduced [--class run-crash]
```

regenerates the seed, confirms the finding reproduces, then
delta-debugs at statement granularity: drop one top-level item or one
main-body statement at a time, keep the removal when the same finding
persists, iterate to a fixed point. multi-module programs first get
one attempt at inlining the helper modules into main (aliases
stripped at token boundaries). crash and build findings match on
class plus signature; a wrong-output finding is tracked by its raw
mismatch pair, since line indexes shift as statements fall away. the
minimal program and a report land in the out dir.

## running it

```
cargo build --release --manifest-path tooling/pithgen/Cargo.toml

# print one program
tooling/pithgen/target/release/pithgen gen --seed 393

# hunt a seed range against the repo's compiler
tooling/pithgen/target/release/pithgen run \
    --seeds 0..500 --pith ./target/release/pith --out /tmp/pithgen
```

the crate is deliberately not a member of the compiler workspace and
has no dependencies, so building it never touches the compiler's
lockfile or build cache.

## first hunt, for the record

2000 seeds on v0.2.9: 511 hit the recursive-generic-struct checker
segfault, 299 the aliased no-payload-enum missing-offset build
failure, 122 the re-emitted-generic-body unknown-load-source build
failure, and 8 found a previously unknown teardown double-free
(interface associated-type enum payload aliasing a receiver field).
zero programs were rejected for syntax or type errors.
