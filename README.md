# Pith

[![ci](https://github.com/kacy/pith/actions/workflows/ci.yml/badge.svg)](https://github.com/kacy/pith/actions/workflows/ci.yml)
[![license: mit](https://img.shields.io/badge/license-mit-blue.svg)](LICENSE)

Pith is a small language for writing servers, with Python-shaped syntax,
native binaries, result types instead of exceptions, reference counting
instead of a garbage collector, and memory safety across tasks: the
compiler refuses a program that would let two tasks change one buffer.
The compiler is written in Pith, and so is the standard library,
including the TLS 1.2 and 1.3 protocol, the HTTP/2 client and server,
the Postgres and MySQL wire protocols, the regex engine and gzip. When
something goes wrong in the stack, you can read the code that did it.

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

That is a complete HTTP server. Every route is already wrapped in a
trace span and RED metrics. An inbound `traceparent` joins the caller's
trace, `GET /metrics` serves Prometheus text, and one `obs.init()` call
ships spans and metrics to an OpenTelemetry collector. `listen_tls`
negotiates HTTP/2 or HTTP/1.1 over TLS 1.2 and 1.3, and one
`shutdown.on_signals()` call makes a SIGTERM drain in-flight requests
before the process exits. Rate limiting, circuit breaking, sessions,
CSRF and CORS are middleware you add one line at a time. There are no
dependencies, the program builds to one static binary, and all of it is
standard library code you can read
([docs/web.md](docs/web.md), [docs/telemetry.md](docs/telemetry.md)).

## The language in one file

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

Enums carry payloads, and `match` takes them apart. Errors are values:
`T!` is a result, `fail` makes one, `!` passes it up, and `catch`
supplies a fallback. A function that cannot fail cannot pretend
otherwise. Interpolation takes format specs. There is no null and there
are no exceptions. An out-of-range index or a missing map key
propagates as an error inside a `T!` or `T!String` function and stops
the process elsewhere. Integer arithmetic traps on overflow at every
width, and so do integer conversions, `abs`, `Float.to_int` and shift
counts outside 0 to 63. The `wrapping_*` methods opt out
([docs/numbers.md](docs/numbers.md)).

## Why you might want it

**The whole stack is readable.** The standard library is 134 modules
and 51,000 lines of Pith: the TLS implementation, HTTP/1.1 and HTTP/2,
WebSockets, gRPC, the database wire protocols, JSON, TOML, YAML,
compression, hashing and a linear-time regex engine. When the language
could not express something well, the project added a language feature
instead of wrapping a foreign library. The exceptions are in the Rust
runtime: the cryptographic primitives (AEAD ciphers, key exchange,
signatures) come from the `ring` crate, zstd links the reference
libzstd, and a few hot loops such as the SHA block functions have
native versions.

**It compiles itself, and CI checks it.** The lexer, parser, checker,
formatter, linter, language server and IR emitter are written in Pith.
The build reaches a fixed point: the compiler compiles a compiler that
compiles an identical compiler, and CI verifies this on every merge.

**Errors are data.** `T!E` carries a typed payload when callers need
to inspect a failure, and `defer` / `errdefer` run cleanup on every
exit or only on the failing one. See [docs/errors.md](docs/errors.md)
and [docs/defer.md](docs/defer.md).

**One bad request does not take down the server.** A runtime error
inside an HTTP or gRPC handler, such as an out-of-range index, a
missing map key, a division by zero or an integer overflow, fails that
request with a 500 (`INTERNAL` for gRPC), and the server keeps serving.
A stack overflow still ends the process, and so does a runtime error
in a task the handler spawned
([docs/failure_isolation.md](docs/failure_isolation.md)).

**Memory is managed at compile time.** The compiler emits retain and
release pairs under a borrowed-by-default discipline
([docs/ownership.md](docs/ownership.md)), so there are no GC pauses and
no runtime to tune. A struct graph breaks its own cycles with a `weak`
field. A checker in CI follows every retain and release the compiler
emits and fails the build when a function's counts do not balance on
every path. Over the test corpus it proves over 99% of functions
balanced and leaves the rest undecided
([docs/ir-contract.md](docs/ir-contract.md)). Valgrind and leak-growth
checks also run on every merge, and ASan and TSan run nightly
([docs/testing.md](docs/testing.md)).

**Concurrency is structured, and green by default.** Pith has `spawn`
and `await` with `Task[T]`, channels, `select`, mutexes, wait groups,
contexts and timers, on an M:N green runtime with an epoll reactor
([docs/concurrency.md](docs/concurrency.md)). One environment variable
switches to the OS-thread backend.

**Shared state needs its lock, and the compiler checks.** A `guard`
block ties a mutex to the data it protects, and touching that data
outside a `locked` block on that mutex is a compile error. In a program
that spawns, a mutable global that is neither guarded nor `threadlocal`
is also a compile error, and so is a spawn or channel send that would
leave a collection reachable from two tasks. `pith lint` reports a call
that parks while holding a mutex, two mutexes taken in opposite orders,
and a call that retakes a mutex its caller holds
([docs/concurrency.md](docs/concurrency.md)).

**The tooling speaks JSON.** `check`, `lint` and `doc` all take
`--json`, every diagnostic has a stable code, `fmt` has one canonical
style, and `pith lsp` speaks the Language Server Protocol to Neovim or
VS Code ([docs/editors.md](docs/editors.md)).

**Builds are fast.** On the benchmark machine below, hello world
compiles in about 40 ms, the HTTP benchmark server in about 1.6
seconds, and the entire self-hosted compiler in about 1.3.

## The numbers

These numbers were measured on 2026-11-12 on a Ryzen 9 9950X, with every
language pinned to the same eight cores. The Go 1.27 and Rust 1.99 ports
do the same work and print the same checksums. The full tables, one row per benchmark, are in
[bench/README.md](bench/README.md). The history and the profiling behind
them are in [docs/performance.md](docs/performance.md).

| Workload | Pith | Go | Rust |
|---|---:|---:|---:|
| JSON decode, 20M small objects | **710 ms** | 3523 ms | 1118 ms |
| Substring search | **45 ms** | 221 ms | 220 ms |
| Channel fan-out, 1M messages | **40 ms** | 49 ms | 465 ms |
| Event ledger, 200k events (JSON, collections, crypto) | 65 ms | 95 ms | 51 ms |
| Sort 100k ints, 30 rounds | 300 ms | 504 ms | 59 ms |
| CSV encode | 224 ms | 309 ms | 131 ms |
| Error handling through closures | 289 ms | 182 ms | 163 ms |
| SHA-256, 256 MB | 110 ms | 95 ms | 97 ms |
| HTTP server at 256 connections, req/s | 1.92M | 2.23M (minimal), 1.00M (net/http) | 1.74M (hyper) |
| API gateway, plain forwarding at 64 / 1024 connections, req/s | **473k / 474k** | 283k / 346k (net/http) | No port |

- Pith is faster than Go on JSON decoding, text search, the event ledger
  and catalog workloads, CSV encoding, map updates, int sorting, channels,
  spawning and parallel compute. It is faster than Rust on half the JSON
  shapes, text search, channels, task churn and gRPC calls. Its HTTP server
  handles more requests per second than hyper up to 256 connections, and
  has the lowest p99 latency of every Go and Rust server measured at every
  connection count.
- `examples/gateway`, a reverse proxy with auth, retries and a circuit
  breaker, forwards 1.2 to 1.7x as many requests per second as the same
  gateway in Go's net/http, with a p99 3.6 to 5.7x lower and 40 to 70% less
  memory. With ES256 tokens to check, the two are level at 256 and 1024
  connections.
- Go is ahead on closures, reference cycles, task scheduling, concurrent
  gRPC calls and raw HTTP throughput against a minimal server.
- Rust is ahead on most single-threaded compute, mostly by 1.3x to 5.6x.
  The outlier is walking decoded JSON (6.0x).

These numbers come from one machine, with clients and servers on the same
box over loopback. The gateway has no Rust port. One change behind its
numbers makes a mutex wait longer when CPU-bound tasks share its worker.
[examples/gateway/README.md](examples/gateway/README.md)
has the details.

## What it does not do

The current list is in [docs/limitations.md](docs/limitations.md),
which is kept current with the compiler and gives issue numbers for the
open defects. These are the ones to know before you start:

- A strong reference cycle with no `weak` edge leaks. An opt-in
  trial-deletion collector exists behind `PITH_CYCLE_GC`, off by default.
- TLS 1.2 is a fallback: ECDHE with AEAD suites only, and no session
  resumption. Client certificates work on both versions. TLS 1.3 is the
  full implementation, and `require_tls13()` refuses the fallback.
- There is no package registry. Dependencies are local paths in
  `pith.toml`, with `lock` and `install` but nothing that fetches.
- There is no debugger, and a runtime error prints its message without a
  stack trace.
- `.value()` on a `none` propagates as an error inside a `T!` or `T!String`
  function and stops the program elsewhere. Use `if let`, `match` or
  `unwrap_or` when the optional can be empty.
- Gzip compresses with fixed Huffman codes only, and the regex engine
  leaves out backreferences, lazy quantifiers and lookaround on purpose.
  Compiling a pattern that uses one fails with an error that names the
  feature.

## Install

```
curl -fsSL https://pithlang.com/install.sh | sh
```

The script installs the toolchain in `~/.pith` and prints the line to add
to your `PATH`. Set `PITH_HOME` to install somewhere else, or
`PITH_VERSION` to pin a release such as `v0.2.12`. Prebuilt toolchains
cover Linux and macOS on x86_64 and arm64. Pith links each program with
the system C compiler, so you also need `cc`: the Xcode command line tools
on macOS, or GCC or Clang on Linux.

```
printf 'fn main():\n    print("hello")\n' > hello.pith
pith run hello.pith
```

## Build from source

You need [Rust and Cargo](https://rustup.rs/) for the native backend.

```
cargo build --release
make self-host
./target/release/pith run examples/hello.pith
./target/release/pith new myapp
```

`pith new` scaffolds a library package with a `pith.toml`, a `Makefile`,
a test and an `AGENTS.md`. `make self-host` builds the self-hosted
compiler (`check`, `fmt`, `lint`, `doc`, `lsp`, `new`, `package`):

```
./self-host/pith_main check examples/hello.pith
```

The first build compiles the IR driver from a tracked seed at
`self-host/bootstrap/ir_driver.ir`. The seed breaks the circular
dependency of a compiler written in its own language. After that,
everything rebuilds from source.

## The standard library

The standard library has 134 modules, all written in Pith. The table
lists the areas, each with the doc that covers it in the most depth:

| Area | Modules | Read |
|---|---|---|
| Networking | tcp, unix, dns, url, http, http2, websocket, tls, sse, grpc, socks5 | [TLS](docs/tls.md), [gRPC](docs/grpc.md), [web](docs/web.md) |
| Databases | sql, postgres, mysql, redis, and `db` on top | [db](docs/db.md) |
| Web apps | web: routing, middleware, sessions, CSRF, CORS, Markdown pages | [HTTP apps](docs/http_apps.md) |
| Data | json, toml, yaml, csv, config, table | [YAML](docs/yaml.md) |
| Exact numbers | bigint, decimal | [numbers](docs/numbers.md) |
| Bytes and crypto | hash, checksum, encoding, crypto, bits, binary | [auth](docs/auth.md) |
| Compression | gzip, zlib, zstd, tar, zip | |
| Text | regex, scanner, fmt, html, template, markdown, text, strings | [Unicode](docs/unicode.md), [HTML](docs/html.md), [Markdown](docs/markdown.md) |
| i18n | Locales, message catalogs, CLDR plural rules | [i18n](docs/i18n.md) |
| App plumbing | log, metrics, cli, env, testing, time, datetime, rand, uuid, math | [logging](docs/logging.md) |
| Resilience | Retry, rate limiting, circuit breaking | [resilience](docs/resilience.md) |
| Observability | trace, prometheus, otlp, obs | [telemetry](docs/telemetry.md) |
| Terminals | term: raw mode, input, styling, widgets, an Elm-style runtime | [TUI](docs/tui.md) |
| Concurrency | concurrent: contexts, groups, timers; iter for lazy pipelines | [concurrency](docs/concurrency.md) |

The stdlib reference site is generated from the source comments, so it
documents the same source the compiler reads.

## Proof it is usable

Real programs in `tools/` run against golden output or integration
suites in CI: `purl` (a `curl`-compatible HTTP/1.1 and HTTP/2 client), a
static site generator, a web log analyzer, a JSON API client talking
to a Pith HTTP server, a worker pool over spawn and channels, the
protobuf code generator, and the documentation site generator that
builds the stdlib reference. They show what non-trivial Pith looks like.

## CLI

| Command | What it does |
|---|---|
| `pith run <file>` | Compile and run |
| `pith build <file>` | Compile to a native binary |
| `pith test <file>` | Run `test` blocks; `--filter <substr>` and `--tag <name>` select, `--json` reports records |
| `pith check <file>` | Type check; `--json` for machine output |
| `pith fmt <file>` | Format; `--check` to verify |
| `pith lint <file>` | Conventions; `--json` available |
| `pith doc <file>` | API docs; `--check` requires every pub item documented, or that a `.md` file's Pith code type checks |
| `pith doc <topic>` | Show a guide from `docs/` in the terminal, such as `pith doc markdown` |
| `pith lsp` | Language server over stdio |
| `pith new <dir>` | Scaffold a package |
| `pith package <cmd>` | check, test, lint, doc, deps, lock, install, inspect, context |

Local packages resolve through `pith.toml`:

```toml
[dependencies]
greeter = { path = "../greeter" }
```

```pith
from greeter import greet
```

## Building and testing

```
make build            # native backend
make self-host        # the compiler, compiled by itself
make test             # the full suite
make run-examples     # 128 deterministic example programs against snapshots
make status-audit     # current corpus and size numbers
```

## Project layout

```
self-host/     the compiler, in pith: lexer, parser, checker,
               formatter, linter, docgen, language server, ir emitter
cranelift/     the native backend, in rust: ir consumer, codegen,
               runtime (arc, collections, net)
std/           the standard library
examples/      131 runnable programs (plus examples/gateway), 128 with expected output
tests/         regression, invalid-program, leak, and golden fixtures
tools/         real programs in pith: purl (http client), codegen, generators,
               fuzzers, a log analyzer, a worker pool
docs/          architecture, ownership, errors, grammar, limitations
```

`make status-audit` prints line counts, with comments and blank lines
excluded. To change the compiler, start with
[docs/architecture.md](docs/architecture.md). Read
[docs/contributing.md](docs/contributing.md) for the development loop,
and [docs/testing.md](docs/testing.md) for how tests are written and
run.

## Syntax highlighting on GitHub

`.pith` files map to Python highlighting through `.gitattributes` as a
stopgap. The TextMate grammar and the Linguist submission checklist
are in `tooling/highlighting/`.

## License

MIT
