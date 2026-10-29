# pith

[![ci](https://github.com/kacy/pith/actions/workflows/ci.yml/badge.svg)](https://github.com/kacy/pith/actions/workflows/ci.yml)
[![license: mit](https://img.shields.io/badge/license-mit-blue.svg)](LICENSE)

a small language for writing servers. python-shaped syntax, native
binaries, result types instead of exceptions, reference counting
instead of a garbage collector. the compiler is written in pith, and
so is the standard library, all the way down: the tls 1.2 and 1.3
stack, the http/2 client and server, the postgres and mysql wire
protocols, the regex engine, gzip. when something goes wrong in the
stack, you can read the code that did it.

```pith
import std.web as web
import std.net.http as http

fn greet(req: web.Request) -> http.HttpResponse:
    return http.text(200, "hello, " + req.param("name"))

fn main() -> Int!:
    mut app := web.new()
    app = app.get("/", fn(req: web.Request) => http.text(200, "welcome"))
    app = app.get("/hello/:name", greet)
    return app.listen("0.0.0.0", 8080)!
```

that is a complete http server, and it comes with more than it shows.
every route is already wrapped in a trace span and red metrics; an
inbound `traceparent` joins the caller's trace, `GET /metrics` serves
prometheus text, and one `obs.init()` call ships spans and metrics to
an opentelemetry collector. `listen_tls` negotiates http/2 or http/1.1
over tls 1.2 and 1.3, a sigterm drains in-flight requests before the
process exits, and rate limiting, circuit breaking, sessions, csrf and
cors are middleware you add one line at a time. no dependencies, one
static binary, and all of it is standard library you can read
([docs/web.md](docs/web.md), [docs/telemetry.md](docs/telemetry.md)).

## the language in one file

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

enums carry payloads and `match` takes them apart. errors are values:
`T!` is a result, `fail` makes one, `!` passes it up, `catch` supplies
a fallback. a function that cannot fail cannot pretend otherwise.
interpolation takes format specs. there is no null, there are no
exceptions, and safe code does not panic.

## why you might want it

**the whole stack is readable.** 126 standard library modules, 39,000
lines, no c and no bindings: the tls implementation, http/1.1 and
http/2, websockets, grpc, the database wire protocols, json, toml,
yaml, compression, hashing, a linear-time regex engine. when the
language could not express something well, that became a language
feature to build rather than a foreign library to hide behind.

**it compiles itself, and ci proves it.** the lexer, parser, checker,
formatter, linter, language server and ir emitter are pith. the build
reaches a fixed point: the compiler compiles a compiler that compiles
an identical compiler, verified on every merge.

**errors are data.** `T!E` carries a typed payload when callers need
to inspect a failure, and `defer` / `errdefer` run cleanup on every
exit or only the failing one. see [docs/errors.md](docs/errors.md) and
[docs/defer.md](docs/defer.md).

**memory is managed at compile time.** the compiler emits retain and
release pairs under a borrowed-by-default discipline
([docs/ownership.md](docs/ownership.md)), so there are no gc pauses and
no runtime to tune. a struct graph breaks its own cycles with a `weak`
field.

**concurrency is structured, and green by default.** `spawn` and
`await` with `Task[T]`, channels, `select`, mutexes, wait groups,
contexts and timers, on an m:n green runtime with an epoll reactor
([docs/concurrency.md](docs/concurrency.md)). the os-thread backend is
one environment variable away.

**the tooling talks json.** `check`, `lint` and `doc` all take
`--json`, every diagnostic has a stable code, `fmt` has one canonical
style, and `pith lsp` speaks the language server protocol to neovim or
vs code ([docs/editors.md](docs/editors.md)).

**builds are fast.** hello world compiles in a tenth of a second, the
http benchmark server in about three, and the entire self-hosted
compiler in about five.

## the numbers

measured 2026-10-29 on a ryzen 9 9950x, every language pinned to the same
eight cores, against go 1.27 and rust 1.99 ports that do the same work and
print the same checksums. the full tables, one row per benchmark, are in
[bench/README.md](bench/README.md); the history and the profiling behind
them are in [docs/performance.md](docs/performance.md).

| workload | pith | go | rust |
|---|---:|---:|---:|
| json decode, 20M small objects | **815 ms** | 3535 ms | 1120 ms |
| substring search | **45 ms** | 220 ms | 207 ms |
| channel fan-out, 1M messages | **38 ms** | 49 ms | 462 ms |
| event ledger, 200k events (json, collections, crypto) | 70 ms | 95 ms | 51 ms |
| sort 100k ints, 30 rounds | 301 ms | 502 ms | 59 ms |
| csv encode | 418 ms | 308 ms | 130 ms |
| error handling through closures | 353 ms | 183 ms | 160 ms |
| sha256, 256 MB | 111 ms | 95 ms | 94 ms |
| http server at 256 connections, req/s | 1.83M | 2.20M (minimal), 984k (net/http) | 1.81M (hyper) |

- pith is faster than go on json decoding, text search, the event ledger
  and catalog workloads, int sorting, channels, spawning and grpc under
  concurrency. it is faster than rust on half the json shapes, text search,
  channels and task churn. its http server has a lower p99 latency than
  every go and rust server measured, except at 128 connections.
- go is ahead on closures, reference cycles, task scheduling, string
  building, string-keyed maps and raw http throughput against a minimal
  server.
- rust is ahead on most single-threaded compute, mostly by 1.4x to 5.5x.
  the outlier is walking decoded json (9.7x).

## what it does not do

the current list lives in [docs/limitations.md](docs/limitations.md),
kept current with the compiler and carrying issue numbers for the
open defects. the ones to know before you start:

- a strong reference cycle with no `weak` edge leaks. an opt-in
  trial-deletion collector exists behind `PITH_CYCLE_GC`, off by default.
- tls 1.2 is a fallback: ecdhe with aead suites only, and no session
  resumption. client certificates work on both versions. 1.3 is the full
  implementation, and `require_tls13()` refuses the fallback.
- no package registry. dependencies are local paths in `pith.toml`,
  with `lock` and `install` but nothing that fetches.
- no debugger. the language server answers a syntax error from the
  edited file alone, but a full check still walks the whole import
  closure.
- gzip compresses with fixed huffman only, and the regex engine skips
  counted repeats, lazy quantifiers and lookaround on purpose.

## quick start

you need [rust and cargo](https://rustup.rs/) for the native backend.

```
cargo build --release
./target/release/pith run examples/hello.pith
./target/release/pith new myapp
```

`pith new` scaffolds a package with a `pith.toml`, a `Makefile` and a
`Dockerfile`. to build the compiler with itself:

```
make self-host
./self-host/pith_main check examples/hello.pith
```

the first build compiles the ir driver from a tracked seed at
`self-host/bootstrap/ir_driver.ir`, which is what breaks the circular
dependency of a compiler written in its own language. after that
everything rebuilds from source.

## the standard library

126 modules, all pith. the areas, with the doc that goes deepest:

| area | modules | read |
|---|---|---|
| networking | tcp, dns, url, http, http2, websocket, tls, sse, grpc | [tls](docs/tls.md), [grpc](docs/grpc.md), [web](docs/web.md) |
| databases | sql, postgres, mysql, redis, and `db` on top | [db](docs/db.md) |
| web apps | web: routing, middleware, sessions, csrf, cors | [http apps](docs/http_apps.md) |
| data | json, toml, yaml, csv, config, table | [yaml](docs/yaml.md) |
| exact numbers | bigint, decimal | [numbers](docs/numbers.md) |
| bytes and crypto | hash, checksum, encoding, crypto, bits, binary | [auth](docs/auth.md) |
| compression | gzip, zlib, zstd, tar, zip | |
| text | regex, scanner, fmt, html, template, text, strings | [unicode](docs/unicode.md), [html](docs/html.md) |
| i18n | locales, message catalogs, cldr plural rules | [i18n](docs/i18n.md) |
| app plumbing | log, metrics, cli, env, testing, time, datetime, rand, uuid, math | [logging](docs/logging.md) |
| resilience | retry, rate limiting, circuit breaking | [resilience](docs/resilience.md) |
| observability | trace, prometheus, otlp, obs | [telemetry](docs/telemetry.md) |
| terminals | term: raw mode, input, styling, widgets, an elm-style runtime | [tui](docs/tui.md) |
| concurrency | concurrent: contexts, groups, timers; iter for lazy pipelines | [concurrency](docs/concurrency.md) |

the stdlib reference site is generated from the source comments, so
what you read there is what the compiler read.

## proof it is usable

real programs live in `tools/` and run against golden output in ci: a
static site generator, a web log analyzer, a json api client talking
to a pith http server, a worker pool over spawn and channels, the
protobuf code generator, and the documentation site generator that
builds the stdlib reference. they are the answer to "what does
non-trivial pith look like."

## cli

| command | what it does |
|---|---|
| `pith run <file>` | compile and run |
| `pith build <file>` | compile to a native binary |
| `pith test <file>` | run `test` blocks; `--filter <substr>` and `--tag <name>` select, `--json` reports records |
| `pith check <file>` | type check; `--json` for machine output |
| `pith fmt <file>` | format; `--check` to verify |
| `pith lint <file>` | conventions; `--json` available |
| `pith doc <file>` | docs; `--check` requires every pub item documented |
| `pith lsp` | language server over stdio |
| `pith new <dir>` | scaffold a package |
| `pith package <cmd>` | check, test, lint, doc, deps, lock, install, inspect |

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
make run-examples     # 129 example programs against snapshots
make status-audit     # current corpus and size numbers
```

## project layout

```
self-host/     the compiler, in pith: lexer, parser, checker,
               formatter, linter, docgen, language server, ir emitter
cranelift/     the native backend, in rust: ir consumer, codegen,
               runtime (arc, collections, net)
std/           the standard library
examples/      139 runnable programs, 132 with expected output
tests/         regression, invalid-program, leak, and golden fixtures
tools/         real programs in pith: codegen, generators, fuzzers,
               log and parquet readers
docs/          architecture, ownership, errors, grammar, limitations
```

`make status-audit` prints line counts, with comments and blank lines
excluded. start with [docs/architecture.md](docs/architecture.md) to
change the compiler, [docs/contributing.md](docs/contributing.md) for
the development loop, and [docs/testing.md](docs/testing.md) for how
tests are written and run.

## syntax highlighting on github

`.pith` files map to python highlighting via `.gitattributes` as a
stopgap. the textmate grammar and the linguist submission checklist
live in `tooling/highlighting/`.

## license

mit
