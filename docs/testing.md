# testing

pith tests live next to the code they cover. a `test` block is a named body that
the compiler collects and runs when you ask it to:

```pith
test "scan maps rows into typed values":
    rows := [row([integer(1)], ["id"]), row([integer(2)], ["id"])]
    ids := scan(rows, fn(r: Row) => r.as_int(0))
    assert_eq(ids, [1, 2])
```

run the tests in a file with `pith test`:

```
$ pith test std/sql.pith
  scan maps rows into typed values ... ok
  scan_one maps the first row or none ... ok

2 passed, 0 failed
```

## how the runner works

`pith test` compiles the file's `test` blocks into a small binary and runs it.
each test runs in its own forked process. that isolation matters: a failing
assertion, or even a hard crash like an out-of-bounds index, ends only that one
test. the rest still run, and every result is reported. the process exits
non-zero if any test failed, so `pith test` fits straight into a `make` target or
CI step.

there is no shared state between tests. one test cannot leave a global, an open
handle, or a spawned thread behind for the next one, because the next test starts
from a fresh copy of the process.

## assertions

the built-in assertions are `assert`, `assert_eq` and `assert_ne`:

- `assert(cond)` fails when `cond` is false.
- `assert_eq(a, b)` fails when the two values differ, and `assert_ne(a, b)` when
  they do not. both compare by value: integers, floats, strings, bytes, integer
  lists, and string lists all compare their contents, not their heap identity.
  the failure message shows both sides decoded:

  ```
  assertion failed: [1, 2] != [1, 3]
  assertion failed: "hello" != "world"
  ```

maps, sets, and structs still compare by identity — `assert_eq` on those checks
whether they are the same value, not whether their contents match. compare their
fields or elements directly when you need a deep check.

a failed assertion ends the process on the spot. under `pith test` that process
is the forked child running one test, so the runner records one failure and
carries on with the rest.

### assertions in helpers

the assertions are ordinary calls, not a `test` block dialect, so they work in
any function. that is what makes a table of cases worth writing: the check goes
in a helper and the test body feeds it rows.

```pith
struct Case:
    input: String
    want: Int

fn assert_parses(c: Case):
    assert_eq(parse(c.input), c.want)

test "the parser handles every documented form":
    for c in cases():
        assert_parses(c)
```

a loop that stopped iterating passes as loudly as one that checked everything,
so assert the row count too:

```pith
    assert_eq(checked, 7)
```

if a module defines or imports its own function called `assert_eq`, that one
wins; the built-in only fills a name nothing else has claimed.

## skipping a test

`skip_test(reason)` marks the current test skipped and stops it right there.
nothing after the call runs, the runner counts it as skipped rather than passed
or failed, and a skipped test never fails the run. it is the way to fold a test
that needs something it might not have — a database, a network peer — into the
same file as everything else:

```pith
test "reads rows from the live database":
    if not database_reachable():
        skip_test("no database reachable")
    ...
```

```
  reads rows from the live database ... skipped (no database reachable)

0 passed, 0 failed, 1 skipped
```

## running a subset

pass `--filter` to run only the tests whose name contains a substring:

```
$ pith test std/mysql.pith --filter scramble
  mysql_native_password scramble matches a known vector ... ok
  caching_sha2_password scramble matches a known vector ... ok

2 passed, 0 failed, 3 filtered out
```

the filter also reads from the `PITH_TEST_FILTER` environment variable, which is
handy when you drive the tests through a wrapper script.

## std.testing

`std.testing` is a helper library for a different shape of test: a standalone
`fn main()` that checks a great many things and reports them all. its checks
(`assert_eq`, `assert_ne`, `check_true`, and friends) count passes and failures
and print them as they go, then `done()` prints a summary. a failure does not
stop the run, so one broken case does not hide the next twenty:

```pith
from std.testing import assert_eq, done

fn main():
    assert_eq(1 + 1, 2)
    done()
```

these also work inside a `test` block: a check that fails there fails the block,
because recording a failure sets the same process verdict a built-in assertion
does. prefer the built-ins anyway — they read better and they print both sides
of the comparison.

what `std.testing` adds beyond the built-ins is the utilities they do not cover:
`assert_contains(text, part)`, `assert_file_exists(path)`,
`assert_dir_exists(path)`, and `with_temp_dir(prefix, run)` for a scoped
filesystem sandbox.

## the other test suites

colocated `test` blocks are the everyday path, but the project leans on a few
other kinds of test, all wired through the `Makefile`:

- **golden output** — a program under `tests/cases/` whose stdout is compared
  against `tests/expected/<name>.txt`. good for end-to-end behavior. run with
  `make run-regressions`.
- **rejected programs** — files under `tests/invalid/` (and `tests/invalid_parse/`)
  that must fail to compile, guarding error messages and negative cases. run with
  `make check-invalid`.
- **live servers** — integration tests under `tests/live/` that need a real
  server and are run on demand. the database ones (`db_postgres_live`,
  `db_mysql_live`, `db_redis_live`) are `test` blocks that `skip_test` when their
  server is not reachable, so `make db-live-tests` stays green with or without a
  running server and verifies the drivers where one exists.
- **invalid access** — `make memcheck` runs a curated set under valgrind, so an
  arc regression that double-frees or reads freed memory is caught before it
  lands.
- **leak growth** — `make leak-check` runs the cases under `tests/leaks/` at two
  round counts and fails when memory grew between them. this is the other half
  of `memcheck`, which has its leak check switched off on purpose.
- **crash sites** — `make check-no-panics` scans the rust sources for anything
  that can stop the process and fails on any site that is not justified in
  place. see below.

## the crash guard

the rust runtime is linked into every pith program, so a panic in it is a crash
in somebody's server. `make check-no-panics` scans `cranelift/*/src` for the
constructs that stop a process — `panic!`, `unreachable!`, `.unwrap()`,
`.expect(...)` — plus `std::mem::transmute`, `std::mem::forget` and
`from_utf8_unchecked`, which reinterpret memory and want the same scrutiny. `std::process::exit` is scanned
in the runtime and the codegen crate only: the cli and the build script are
programs, and a program exiting non-zero after printing a diagnostic is normal.

a deliberate site is justified with a marker comment on the line directly above
it:

```rust
// panic-guard: strict list indexing out of bounds is a program bug with no value to return.
std::process::exit(1);
```

the marker moves with the code. the guard used to keep a list of regexes
matching exact source lines instead, which went stale as soon as anything was
reformatted or added, so the gate reports a marker whose next line is not a
guarded site — a marker left behind by a deleted trap is a failure too.

test code is skipped: `#[cfg(test)]` items are compiled out of the shipped
runtime, and an `.unwrap()` in a test is how a test reports failure. the skip
runs from the attribute line to the closing brace at the attribute's own
indentation; an item whose brace never turns up is reported rather than
silently swallowing the rest of the file.

prefer `runtime_fatal!` over `panic!` for a condition the runtime cannot
recover from. a panic on a runtime thread does not reliably stop the process:
`run_task` catches every panic a green task raises, so a panic anywhere the
spawn path reaches kills the task with `join.done` never set and hangs its
awaiters forever, and a panic inside a `Once` poisons it for every later caller.
`runtime_fatal!` prints a `pith runtime error:` line and exits, which is one
diagnosable death instead of a silent wedge.

## the leak growth gate

`make leak-check` builds each program under `tests/leaks/` and runs it twice, once
at `PITH_LEAK_ROUNDS=200000` and once at `800000`, then compares the peak resident
set the two runs reported. a program that leaks k bytes per round moves its peak
by k times the six hundred thousand extra rounds. a correct one parks at its
working set and reports the same number either way. the target prints the
difference for every case and exits non-zero when one of them clears 2 mb.

the number to watch is that difference and not a ceiling, because a ceiling is a
fact about the runtime rather than about the case. it drifts whenever the
allocator, the freelists or the stack pool change size, so it has to be retuned
to stay meaningful, and a gate that gets retuned is a gate that gets waved
through. a difference only cares about the slope, which is zero for every program
that does not leak, whatever the runtime is doing underneath it.

valgrind's own leak check is the obvious tool here and the wrong one. the runtime
keeps a struct freelist, a coroutine stack pool, per-arena node pools and its
worker threads alive for the life of the process. all of that is still reachable
at exit and none of it is a bug, so a real leak would arrive buried in megabytes
of output nobody would read twice.

the leaks this was built from ran twenty to ninety bytes a round, so the quietest
of them still moves the peak by twelve megabytes over the extra rounds. noise on
a flat case measures under two hundred kilobytes run to run. 2 mb sits an order
of magnitude above the noise and well under the smallest real signal. a case that
clears the limit is measured again before it is called a failure, so one spike on
a loaded machine cannot turn the gate red by itself. the whole target takes about
ten seconds.

to add a case, drop a `.pith` file in `tests/leaks/` that imports `leakprobe`,
runs its churn `probe.rounds()` times, and prints `probe.peak_kb()` and nothing
else. then list it in the `cases` array in `tooling/leak_check.sh`. keep the round
body allocation-heavy and free of anything that is supposed to grow: a collection
that keeps filling up looks exactly like a leak.

## on the roadmap

a few things are not here yet: tagging tests, benchmarks, and machine-readable
output for CI. skipping has since landed — see `skip_test` above, which is what
lets the live suites fold in and skip themselves when their service is not
reachable. the leak gate covers a curated set of ownership shapes rather than
every program; a case is added as the last step of fixing a shape, not the
first, which is why the gate reads as a list of leaks that no longer happen.
