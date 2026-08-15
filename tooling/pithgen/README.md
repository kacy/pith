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

same seed, same program: the prng is splitmix64-seeded xoshiro256**,
and generation never consults time or environment.

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
- **valgrind** (opt-in) — memcheck on the built binary.

findings dedup by a normalized signature (paths, numbers, and
generated identifiers stripped), so a 500-seed batch reports root
causes, not instances.

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
