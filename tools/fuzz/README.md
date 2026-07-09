# fuzz

an adversarial harness that feeds the frontend programs it never
asked for and checks that it always answers loudly.

the fuzzer is itself a pith program (`fuzz.pith` drives it,
`fuzz_gen.pith` builds and mutates the inputs). it leans on the
compiler's loud-failure gates: because a healthy compiler never exits
silently, "the compiler said nothing" is a reliable signal that
something is wrong.

## the three oracles

1. **it never goes quiet.** for any input, `check` exits 0 or 1 with a
   message. a crash, a hang, or an empty-handed exit is a bug.
2. **formatting is a fixed point.** anything that parses formats
   idempotently, and the formatted output still parses.
3. **checked means buildable.** a program the checker accepts must
   compile. a clean check followed by a failed build is a silent seam
   — the class this harness exists to find.

## running it

```
make fuzz-check     # generated programs, fixed seed, deterministic (ci gate)
make fuzz           # generated + corpus mutation, wider net
```

direct, with options:

```
./target/release/pith build tools/fuzz/fuzz.pith
./tools/fuzz/fuzz --count 300 --seed 7 --build-every 5
./tools/fuzz/fuzz --no-mutate          # skip the corpus-mutation pass
```

findings are written to `tools/fuzz/findings/` (gitignored) as the
exact program that triggered them, ready to minimize.

## two halves

the **generated** half builds small programs from the grammar out.
most don't type-check, which is fine — the point is that parse, fmt,
and check survive anything. this half is the ci gate: seeded and
green.

the **mutation** half takes working programs from `tests/cases` and
damages them the way a hasty edit might — dropping lines, splicing in
stray punctuation, truncating mid-block. it first surfaced two
silent-input gaps (a stray character dropped between valid tokens, an
unresolvable import accepted quietly); with those closed it runs
inside the gate too. `make fuzz` runs a wider, longer search on top.
