# pith

[![ci](https://github.com/kacy/pith/actions/workflows/ci.yml/badge.svg)](https://github.com/kacy/pith/actions/workflows/ci.yml)
[![license: mit](https://img.shields.io/badge/license-mit-blue.svg)](LICENSE)

a small language that compiles to native code and is written in
itself. python-shaped syntax, result types instead of exceptions,
reference counting instead of a garbage collector. the compiler is
about 23,000 lines of pith; the standard library is another 31,000,
and it goes deeper than you'd guess — the TLS 1.3 client, the native
http/2 client, the regex engine, the postgres and mysql wire protocols,
and gzip compression are all pith source you can read.

```pith
enum Shape:
    Circle(Float)
    Rect(Float, Float)

fn area(s: Shape) -> Float:
    match s:
        Shape.Circle(r) => 3.14159 * r * r
        Shape.Rect(w, h) => w * h

fn parse_radius(text: String) -> Float!:
    n := parse_int(text)!
    if n <= 0:
        fail "radius must be positive"
    return n.to_float()

fn main() -> Int!:
    shapes := [Shape.Circle(2.0), Shape.Rect(3.0, 4.0)]
    mut total := 0.0
    for s in shapes:
        total = total + area(s)
    print("total area: {total:.2}")

    r := parse_radius("3")!
    print("one more: {area(Shape.Circle(r)):.2}")

    fallback := parse_radius("") catch 1.0
    print("fallback radius: {fallback}")
    return 0
```

that's most of the flavor in one file: enums carry payloads and match
destructures them, errors are values that propagate with `!` and get
absorbed with `catch`, and interpolation takes format specs
(`{total:.2}`). no null, no exceptions, no panics in safe code.

## why it exists

mostly to answer a question: how much of a real toolchain can one
small language carry on its own back? the answer so far:

- **it compiles itself.** the lexer, parser, type checker, formatter,
  linter, and ir emitter are pith. the build reaches a fixed point —
  the compiler compiles a compiler that compiles an identical
  compiler — and ci verifies that on every merge.
- **errors are values.** `T!` is a result, `T?` is an optional,
  `fail` produces an error, `!` propagates it, `catch` gives a
  fallback. functions that can't fail can't lie about it.
- **memory is reference-counted by the compiler.** retain/release
  pairs are emitted at compile time following a borrowed-by-default
  discipline (docs/ownership.md). no gc pauses. there is no cycle
  collector, but a struct graph can break its own cycles with a `weak`
  field — mark the back edge `weak` and a parent/child ring reclaims.
- **the stdlib doesn't shell out.** TLS 1.3, http/1.1 and http/2, websockets, json,
  toml, csv, tar, zip, gzip (interops with system gzip in both
  directions), sha-256, a linear-time regex engine — written in pith.
  when the language couldn't express something well, that became a
  language feature to build rather than a binding to hide behind.
- **tooling is machine-readable.** check, lint, and doc all take
  `--json`; every diagnostic has a stable code (docs/errors.md);
  `fmt` has one canonical style. scripts and editors get the same
  interface people do.
- **builds are quick.** the benchmark app compiles in about 2 seconds,
  the entire self-hosted compiler in under 7.

## what it doesn't do

the honest list lives in [docs/limitations.md](docs/limitations.md)
and stays current. highlights you should know before diving in:
strong reference cycles leak unless broken with a `weak` field, closure
environments that capture back to themselves leak (no weak captures yet),
removed container elements aren't reclaimed until the container dies,
tls is 1.3-only with no 1.2 fallback, and
performance sits between go and rust on service-shaped work but
behind go on heavy string churn — measured numbers, not vibes, in
[docs/performance.md](docs/performance.md).

## quick start

you need [rust/cargo](https://rustup.rs/) for the native backend.

```
cargo build --release
./target/release/pith run examples/hello.pith
make self-host
./self-host/pith_main check examples/hello.pith
```

the first build compiles the ir driver from the tracked seed at
`self-host/bootstrap/ir_driver.ir` — the compiler is written in pith,
so the seed breaks the circular dependency. after that everything
rebuilds from source.

## errors as data

string errors are fine until callers need to inspect the failure.
`T!E` carries a typed payload:

```pith
struct ParseError:
    message: String

fn parse_port(text: String) -> Int!ParseError:
    if text == "":
        fail ParseError{"empty"}
    return 8080

fn main() -> Int!:
    print((parse_port("8080") catch 9000).to_string())
    print((parse_port("") catch 9000).to_string())
    return 0
```

## the language, briefly

structs (heap references with generated destructors), enums with
payloads, interfaces with generic bounds, impl blocks with
per-instantiation specialization, closures with capture, tuples,
type aliases. `mut` is required for reassignment. `for` drives
collections, strings, ranges (`0..n`, `0..=n`), and anything with a
`fn next() -> T?` method. match checks exhaustiveness. generics
monomorphize. concurrency is structured: spawn/await with `Task[T]`,
channels, select, mutexes, wait groups, contexts, timers.

the full grammar is [docs/grammar.ebnf](docs/grammar.ebnf); the tour
with idioms is [docs/idiomatic_pith.md](docs/idiomatic_pith.md).

## the standard library, briefly

77 modules, ~36,000 lines, all pith. the areas: io and filesystems
(fs, glob, path, process), networking (tcp, dns, url, http, http2,
websocket, tls, sse), databases (sql, postgres, mysql, redis — pure-pith
wire protocols with tls, prepared statements, and pooling), data (json,
toml, csv, config, table), bytes and crypto (hash, checksum, encoding,
crypto, bits, binary), compression and archives (gzip/zlib, tar, zip),
text (regex, scanner, fmt), app plumbing (log, metrics, cli, env, testing,
diagnostic, time, datetime, rand, uuid, math), observability (trace,
prometheus, otlp, obs — opentelemetry traces and metrics, see
[docs/telemetry.md](docs/telemetry.md)), and lazy iterators (std.iter).

## proof it's usable

four real programs live in `tools/` and run against golden output in
ci: a static site generator (toml config, markdown subset, layouts,
feeds), a web log analyzer (combined format, gzip input, csv export),
a json api client that talks to a pith http server — both ends of the
conversation in pith — and a worker pool running a jobs file through
spawn and channels. they are the honest answer to "what does
non-trivial pith look like."

## cli

```
pith run <file>          compile and run
pith build <file>        compile to a native binary
pith test <file>         run test declarations (--filter <substr> to select)
pith check <file>        type check (--json for machine output)
pith fmt <file>          format (--check to verify)
pith lint <file>         conventions (--json available)
pith doc <file>          docs (--check verifies pub items documented)
pith lex / parse         token stream / ast, for the curious
pith package <cmd>       check/test/lint/doc/deps/lock/install/inspect
```

local packages resolve through `pith.toml`:

```toml
[dependencies]
greeter = { path = "../greeter" }
```

```pith
from greeter import greet
```

## building and testing

```
make build            # native backend
make self-host        # the compiler, compiled by itself
make test             # the full suite
make run-examples     # 92 example programs against snapshots
make status-audit     # current corpus and size numbers
```

## project layout

```
self-host/     the compiler, in pith (~23,000 lines): lexer, parser,
               checker, formatter, linter, docgen, ir emitter
cranelift/     the native backend, in rust (~12,500 lines): ir
               consumer, codegen, runtime (arc, collections, net)
std/           the standard library (77 modules)
examples/      94 runnable programs with expected output
tests/         regression, invalid-program, and golden fixtures
tools/         the four real programs
docs/          architecture, ownership, errors, grammar, limitations
```

start with [docs/architecture.md](docs/architecture.md) if you want
to change the compiler, [docs/contributing.md](docs/contributing.md)
for the development loop, and [docs/testing.md](docs/testing.md) for how
tests are written and run.

## syntax highlighting on github

`.pith` files map to python highlighting via `.gitattributes` as a
stopgap. the textmate grammar and the linguist submission checklist
live in `tooling/highlighting/`.

## license

mit
