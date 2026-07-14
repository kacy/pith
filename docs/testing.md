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

inside a `test` block, use the built-in `assert` and `assert_eq`:

- `assert(cond)` fails the test when `cond` is false.
- `assert_eq(a, b)` fails when the two values differ, and compares by value:
  integers, floats, strings, bytes, integer lists, and string lists all compare
  their contents, not their heap identity. the failure message shows both sides
  decoded:

  ```
  assertion failed: [1, 2] != [1, 3]
  assertion failed: "hello" != "world"
  ```

maps, sets, and structs still compare by identity — `assert_eq` on those checks
whether they are the same value, not whether their contents match. compare their
fields or elements directly when you need a deep check.

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

`std.testing` is a separate helper library, not the assertion surface for `test`
blocks. its checks (`assert_eq`, `assert_ne`, `check_true`, and friends) count
passes and failures and print them, then you call `done()` to print a summary —
a counting model meant for a standalone `fn main()` test program:

```pith
from std.testing import assert_eq, done

fn main():
    assert_eq(1 + 1, 2)
    done()
```

one sharp edge to know about: because those checks only tally, a failing
`std.testing.assert_eq` **does not fail a `test` block**. the block still exits
cleanly and the runner marks it `ok`. so inside a `test` block, always reach for
the built-in `assert` / `assert_eq`.

what `std.testing` is genuinely good for is the utilities the built-ins do not
cover: `assert_contains(text, part)`, `assert_file_exists(path)`,
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
- **leak checks** — `make memcheck` runs a curated set under valgrind, so an arc
  regression that double-frees or leaks is caught before it lands.

## on the roadmap

a few things are not here yet: skipping and tagging tests (so the live suites can
fold in and be skipped by default), running any test under the leak checker,
benchmarks, and machine-readable output for CI.
