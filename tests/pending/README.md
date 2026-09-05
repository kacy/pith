# pending reproducers

Programs that describe a defect the compiler still has. Nothing runs this
directory. No target globs it, no gate registers it, and a case here is
expected to fail. Registering it instead would turn the gate red for
everybody, and a reproducer that has been written down and measured is worth
keeping even when the fix is somebody else's later change.

Each file's header says what fails, what the passing twin beside it does
differently, and, for a leak, the growth measured across the two round counts
`tooling/leak_check.sh` uses. A file that churns rounds reads `SHAPE` from the
environment to pick one shape, so a fix can be checked one row at a time:

```
pith build tests/pending/<reproducer>.pith
SHAPE=1 PITH_LEAK_ROUNDS=200000 ./tests/pending/<reproducer>
SHAPE=1 PITH_LEAK_ROUNDS=800000 ./tests/pending/<reproducer>
```

When the underlying defect is fixed, a leak reproducer moves to `tests/leaks/`
(swapping its `pendingprobe` import for `leakprobe`) and is registered in
`tooling/leak_check.sh`. One that answers wrong or crashes becomes a golden
under `tests/cases/` with its output in `tests/expected/`. Do not leave a
fixed shape here, and do not leave one in `tests/leaks/` unregistered:
`tooling/leak_check.sh` fails on an unregistered `leak_*.pith` for exactly
that reason.
