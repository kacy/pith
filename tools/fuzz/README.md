# fuzz

The fuzzer is an adversarial harness that feeds the compiler frontend
programs it never asked for and checks that it always answers loudly.

It is itself a Pith program (`fuzz.pith` drives it, `fuzz_gen.pith`
builds and mutates the inputs). It relies on the compiler's loud-failure
gates: a healthy compiler never exits silently, so "the compiler said
nothing" is a reliable signal that something is wrong.

## The three oracles

1. **It never goes quiet.** For any input, `check` exits 0 or 1 with a
   message. A crash, a hang, or an exit with no message is a bug.
2. **Formatting is a fixed point.** Anything that checks clean formats
   idempotently, and the formatted output still checks clean.
3. **Checked means buildable.** A program the checker accepts must
   compile. A clean check followed by a failed build is a silent seam,
   and this harness exists to find that class of bug.

## Running it

```
make fuzz-check     # generated + mutated corpus, fixed seed, deterministic (ci gate)
make fuzz           # generated + corpus mutation, wider net
```

To run it directly, with options:

```
./target/release/pith build tools/fuzz/fuzz.pith
./tools/fuzz/fuzz --count 300 --seed 7 --build-every 5
./tools/fuzz/fuzz --no-mutate          # skip the corpus-mutation pass
```

Findings are written to `tools/fuzz/findings/` (gitignored) as the exact
program that triggered them, ready to minimize.

## Two halves

The **generated** half builds small programs from the grammar out. Most
don't type-check, which is expected: the harness tests that lex, parse, and
check survive anything (with `fmt` and `build` exercised on programs that
check clean). `make fuzz-check` runs both halves plus `tools/fuzz-tls` with a
fixed seed.

The **mutation** half takes working programs from `tests/cases` and damages
them the way a hasty edit might: dropping lines, splicing in stray
punctuation, truncating mid-block. It first surfaced two silent-input gaps (a
stray character dropped between valid tokens, and an unresolvable import
accepted quietly). With those closed, it runs inside the CI gate too.
`make fuzz` runs a wider, longer search on top.
