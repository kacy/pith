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
any function. a check that takes a few lines to set up belongs in a helper, and
the test body calls it once per input:

```pith
fn assert_parses(input: String, want: Int):
    parsed := parse(input)
    assert(parsed.is_ok)
    assert_eq(parsed.ok, want)

test "the parser handles every documented form":
    assert_parses("1", 1)
    assert_parses("0x10", 16)
```

if a module defines or imports its own function called `assert_eq`, that one
wins; the built-in only fills a name nothing else has claimed. that is the
switch behind the table-driven form below: `std.testing`'s assertions record a
failure and carry on where the built-ins end the process.

## table-driven cases

a list of inputs checked by one body is a common shape, and written as a loop it
loses what the runner is for: the first row that fails ends the whole block, the
failure names the assertion rather than the row that provoked it, and there is
no way to ask for one row back.

`each` runs a body once per labeled row and reports every row as a result of its
own:

```pith
from std.testing import case, each

test "an origin is matched whole, not by prefix or suffix":
    guard := middleware(origins(["https://app.example.com"]))

    each([
        case("a suffix past the boundary", "https://app.example.com.evil.test"),
        case("the origin buried in a path", "https://evil.test/https://app.example.com"),
        case("plain http", "http://app.example.com"),
        case("another port", "https://app.example.com:8443"),
        case("a subdomain", "https://sub.app.example.com"),
], fn(spoofed: String) => guard(ok_handler, cross_origin("GET", spoofed)).header_value(ALLOW_ORIGIN) == "")
```

a row passes when the body answers true and nothing inside it recorded a failed
check. one that does not is named where it sits, and the rows after it still
run:

```
  ok   [1] a suffix past the boundary
  ok   [2] the origin buried in a path
  FAIL [3] plain http -- the case did not hold
  ok   [4] another port
  ok   [5] a subdomain
  an origin is matched whole, not by prefix or suffix ... FAILED
```

for a list of lookalikes that matters, because one that gets through is rarely
the only one.

a row carries whatever the body needs, so the input-and-expected table is a
table of pairs:

```pith
    each([
        case("empty", ("", 0)),
        case("one character", ("a", 1)),
        case("several", ("abcd", 4)),
], fn(c: (String, Int)) => c.0.len() == c.1)
```

### reporting both sides

a body that answers yes or no reports its label and nothing else. when the two
values are worth seeing, check them with `std.testing`'s assertions inside the
body and return `true`: the check prints both sides and `each` names the row it
belonged to underneath. a multi-line body is bound to a name first, because a
`fn(x):` block does not fit inside an argument list:

```pith
from std.testing import case, each, check

test "doubling":
    doubled := fn(n: Int):
        check(n + n, n * 2, "doubled")
        return true

    each([case("one", 1), case("two", 2)], doubled)
```

reach for `std.testing`'s assertions rather than the built-ins inside a case
body. the built-ins end the process where they fail, which under `each` takes
every row after the failing one with it, and the row that failed is never named:

```
  ok   [1] first
assertion failed
  a builtin assertion reaches a case body through a helper ... FAILED
```

`std.testing`'s record the failure instead, so the table finishes and `each`
says which row it was. (a closure body cannot call the built-ins at all today —
`assert` inside a `fn(x):` block fails to compile; they reach a case body only
through a helper the body calls.)

`pith fmt` closes a multi-line list at column zero, which is why the `]` above
sits where it does — the same shape the tables in `std/crypto/password.pith` and
`std/hash.pith` already have.

### selecting one row

a row's identity is the test's name, ` / `, and the row's label, and `--filter`
matches on that:

```
$ pith test std/web/cors.pith --filter "an origin is matched whole, not by prefix or suffix / another port"
  ok   [4] another port
  an origin is matched whole, not by prefix or suffix ... ok

1 passed, 0 failed, 10 filtered out
```

the number in brackets is the row's position in the table, not in the run, so a
filtered run names the same row the full run did. a filter that matches the
test's name alone runs the whole table, the way it always has.

a filter that names a row which is not there runs the test with no rows
selected, and a test that checked nothing reports as passing:

```
$ pith test rows.pith --filter "rows / gamma"
  rows ... ok

1 passed, 0 failed, 1 filtered out
```

there is no result line above it, which is the tell — a table that ran prints
one line per row. the test cannot skip itself instead, because a test is free
to hold more than one table and the first of them cannot know whether a later
one matches.

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
handy when you drive the tests through a wrapper script. one row of a
table-driven test is reachable the same way — see [selecting one
row](#selecting-one-row).

## tagging tests

a test can carry labels, and a run can select or exclude them. the labels are
identifiers in brackets between the name and the colon:

```pith
test "reads rows from the live database" [slow, database]:
    ...
```

that is the bracketed list the language spells everywhere else, so a tagged
test brings in no new punctuation and nothing to quote. a tag is a name rather
than a string because it is used as one: a run asks for it and it is either
there or it is not. matching text is what `--filter` is for.

`--tag` runs only the tests carrying that label, `--exclude-tag` runs
everything but them, and either can be passed more than once:

```
$ pith test std/sql.pith --tag slow
$ pith test std/sql.pith --exclude-tag slow --exclude-tag database
```

with no `--tag` every test qualifies. with a selection a test needs one of the
selected labels, so two `--tag`s are a union. an excluded label removes a test
either way, so exclusion wins over selection when a test carries both.

tags and `--filter` compose as one conjunction: the tags say which tests the
run is about, the filter picks a name out of those. `--tag slow --filter
scramble` runs the slow tests whose name contains scramble and nothing else.
neither widens the other, and a test the tags excluded is not reachable by
naming it in a filter.

a tag on a test that holds a table covers the whole table. selecting the tag
runs every row, and one row is still reachable on its own through `--filter`,
exactly as it was before the test was tagged.

a run that names a tag no test in the file carries says so and fails:

```
$ pith test std/sql.pith --tag databse

0 passed, 0 failed, 12 filtered out
no test carries the tag "databse"
```

a mistyped tag would otherwise be a green run of nothing, which is the one
failure a CI step cannot catch by itself. `--exclude-tag` is held to the same
rule: excluding a label nothing carries means the invocation is not doing what
it says.

## machine-readable output

`--json` reports the run as one json object per line instead of prose. three of
the lines that run prints:

```
$ pith test tests/testrunner/fixture.pith --json
{"type":"test","name":"arithmetic holds","file":"tests/testrunner/fixture.pith","tags":["fast"],"outcome":"passed","duration_ms":0,"message":null,"position":null}
{"type":"case","name":"[3] gamma","test":"rows report one by one","file":"tests/testrunner/fixture.pith","tags":["table"],"outcome":"failed","message":"the case did not hold"}
{"type":"summary","passed":2,"failed":2,"skipped":1,"filtered_out":0,"unmatched_tags":[]}
```

a `test` record carries the test's name, the file it came from, its tags, its
outcome — `passed`, `failed`, `skipped` or `filtered` — and how long it took in
milliseconds. a failed one also carries the assertion's message and the
position it failed at; a skipped one carries the reason. a `case` record is one
row of a table or one `std.testing` check, and names the test it belongs to as
well as itself, because a row labeled `[3] gamma` says nothing on its own. the
run ends with one `summary` record holding the tally and any tag the run named
that nothing carries.

it is json lines rather than one json document because a run is a stream
written by more than one process: the child prints its own rows and its own
skip, the parent prints the verdict once the child is gone, and a test that
crashes still leaves every record written before it. a document has to be
closed by whoever opened it, which a process that died cannot do, and buffering
the whole run to close it would throw away the streaming the fork-per-test
design is built on. every reader already handles a line at a time — `jq` reads
it without a flag, and so does anything that loops over lines:

```
$ pith test std/sql.pith --json | jq -r 'select(.outcome == "failed" and .position)
    | "\(.position.file):\(.position.line): \(.name): \(.message)"'
```

the position is where a built-in assertion in the file under test failed. only
the module being tested is compiled with those positions in it, so an assertion
that fails inside an imported helper reports its message and a null position. a
`std.testing` check reports its own message, which already names both sides, and
no position.

a test's own output is not a record. what a test prints still goes to stdout
where it printed it, and a failing built-in assertion still prints its message
to stderr, so a reader takes the lines that parse as json and leaves the rest.

`--json`, `--tag` and `--exclude-tag` also read from the environment
(`PITH_TEST_JSON`, `PITH_TEST_TAGS`, `PITH_TEST_EXCLUDE_TAGS`) the way
`--filter` reads `PITH_TEST_FILTER`. a wrapper script that drives the tests
sets those and needs to know nothing about the flags.

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
`assert_dir_exists(path)`, `with_temp_dir(prefix, run)` for a scoped filesystem
sandbox, and `case`/`each` for the table-driven form above.

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
- **the runner's own output** — `make test-runner-goldens` runs
  `tests/testrunner/fixture.pith` eleven ways and compares every line against
  `tests/testrunner/expected/`. the json records are a contract with whatever
  reads them, so they are pinned line for line rather than grepped: a record
  that quietly drops a field still passes a grep and breaks every reader.
  durations vary, so `"duration_ms":<n>` is rewritten to `"duration_ms":<ms>`
  before the comparison — the field stays pinned present and well-formed, only
  its value is dropped.

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

one thing is not here yet: benchmarks. tagging, machine-readable output and
skipping have all landed — see [tagging tests](#tagging-tests),
[machine-readable output](#machine-readable-output) and `skip_test` above, the
last of which is what lets the live suites fold in and skip themselves when
their service is not reachable. the leak gate covers a curated set of ownership shapes rather than
every program; a case is added as the last step of fixing a shape, not the
first, which is why the gate reads as a list of leaks that no longer happen.
