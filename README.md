# pith

a programming language where any coding agent is immediately productive.

no panics, no null, no data races. automatic memory management via
compiler-emitted reference counting. result types everywhere. designed so
that AI coding agents can read the errors, apply fixes, and iterate — fast.

**status:** the compiler self-hosts — pith is written in pith. the
self-hosted frontend emits Pith text IR, and the Rust/Cranelift backend lowers
that IR to native code. this path compiles the tracked example suite and the
compiler itself to a fixed point. the older C transpiler and Zig bootstrap are
historical; they are not the active build path. the CLI handles build, run,
test, check, fmt, lint, lex, parse, doc, and more. the standard library now
covers I/O, networking, native TLS 1.3, encoding, hashing, JSON, TOML,
process management, tooling helpers, and more.

## quick start

requires [rust/cargo](https://rustup.rs/) for the Cranelift native backend.

```
cargo build --release
./target/release/pith run examples/hello.pith
make self-host
./self-host/pith_main check examples/hello.pith
```

the first build on a fresh clone compiles the ir driver from the tracked
seed at `self-host/bootstrap/ir_driver.ir` — the compiler is written in
pith, so the seed is what breaks the circular dependency. after that,
everything rebuilds from source. `make refresh-bootstrap-seed` regenerates
the seed after ir contract changes.

## where to read first

- `README.md` for the high-level map
- `docs/architecture.md` for the compiler pipeline and subsystem boundaries
- `docs/concurrency.md` for channels, contexts, select, and task waits
- `docs/http_apps.md` for the higher-level http request/response layer
- `docs/text_and_bytes.md` for the string/bytes split and common helpers
- `docs/idiomatic_pith.md` for the current everyday style
- `docs/iterators.md` for the iterator protocol, range-for, and `std.iter`
- `docs/limitations.md` for the honest list of what doesn't work yet
- `docs/contributing.md` for the development loop and smoke checks
- `docs/tooling_stdlib.md` for glob, cli, diagnostic, and testing helpers
- `self-host/pith_main.pith` for the self-hosted frontend (lex/parse/check/fmt/lint/doc)
- `cranelift/cli/src/main.rs` for the native backend CLI (build/run)

## contributor fast path

if you are new to the codebase, the shortest useful path is:

1. run `cargo build --release`
2. run `./target/release/pith run examples/hello.pith`
3. run `make self-host`
4. run `./self-host/pith_main check examples/hello.pith`
5. read `docs/architecture.md`

common starting points:

- add a token or keyword: `self-host/lexer.pith`
- add syntax: `self-host/parser.pith`, `docs/grammar.ebnf`
- add type rules: `self-host/checker.pith`
- native code generation: `cranelift/codegen/src/ir_consumer.rs`

for example-facing output, prefer interpolation for direct value printing:

```pith
print("count: {items.len()}")
print("scores: {scores}")   # lists, maps, sets, and structs all render
```

a struct renders as `Name(field: value, ...)` unless it has a `show()`
impl, which then controls its display everywhere it interpolates.

use `std.fmt` when you need a reusable template or literal braces.

for incremental text assembly, prefer `std.io.string_buffer()` over long
concatenation loops.

for test-style comparisons in stdlib examples and helper programs, prefer
`std.testing.assert_eq(...)` and `assert_ne(...)`.

collections are shared handles by default. if you want a separate top-level
container before mutating it, use helpers like `std.collections.copy_list(...)`,
`copy_map(...)`, or `copy_set(...)`.

## what it looks like

```
fn greet(name: String) -> String:
    return "hello, {name}!"

struct Point:
    pub x: Int
    pub y: Int

fn distance(a: Point, b: Point) -> Int:
    dx := a.x - b.x
    dy := a.y - b.y
    return dx + dy

enum Shape:
    Circle(Float)
    Rectangle(Float, Float)
    Point

fn main():
    msg := greet("world")
    if msg != "":
        print(msg)
```

## what works today

the self-hosted compiler handles the frontend pipeline: lex → parse → check →
emit text IR. the Cranelift backend consumes that IR to produce native code.
85 deterministic example programs compile and produce verified output through
that path.

**language features:**
- function declarations, typed parameters, return types, calls
- struct declarations with typed fields, field access, constructors, and field defaults
- struct field assignment (`p.x = 5`, `self.x = x + 1` in methods); structs are heap-allocated reference values so updates are visible through every handle to the same instance
- enum declarations with variant data
- interface declarations with method signatures
- impl blocks for structs with method implementations, including per-instantiation specialization of methods on generic structs
- generic types, generic functions, and generic interfaces with bounds; type parameters are inferred through interface bounds (`fn f[I: Iterator[T], T](it: I)` infers `T` from the impl `I` provides), with multi-arg explicit calls `f[A, B](x)` as the escape hatch
- variable bindings with type inference (`x := 42`)
- mutability enforcement (`mut` required for reassignment)
- if/elif/else, while, for loops over collections and strings with scoping
- range-for `for i in 0..n` (exclusive) and `for i in 0..=n` (inclusive); both compile to a counted loop with no allocation
- iterator protocol: `for x in it` drives any value whose type provides `fn next() -> T?`, so user-defined iterators participate in the same `for` loop as built-in collections (see `docs/iterators.md`)
- binary operators: arithmetic, comparison, logical, string concatenation
- unary operators: negate, not
- string interpolation, character iteration (`for c in string`), and `{{`/`}}` to write a literal brace inside an interpolated string
- return type checking
- match expressions with exhaustiveness checking
- method calls and impl blocks; list method syntax (`xs.map(fn(x) => x * 2).filter(fn(x) => x > 0)`) alongside the eager free-function forms
- pipe operator (`x | f`)
- collection literals: List, Map, Set with index expressions and subscript assignment (`m[k] = v`)
- generics with monomorphization
- lambdas and closures (capturing lambdas with uniform closure ABI)
- result types (`T!` and `T!E`) with try propagation (`expr!`), `fail`, `catch`, `unwrap_or`, and `or_else`
- optional types (`T?`) with `== none` / `!= none` and `.value()` to extract the inner value once you have checked
- tuples with field access (`t.0`, `t.1`)
- type aliases
- concurrency: spawn/await, Task[T], Mutex, WaitGroup, Semaphore, Channel, select, contexts, timers
- multi-module imports with `from ... import`

**standard library (59 modules):**
- string methods, type conversions, math builtins
- file I/O, env, args, exit, exec
- collection methods (push, remove, contains, keys, values, reverse, etc.)
- std.iter — `Iterator[T]` interface, `Range`, `to_list`, and the lazy `map_iter` / `filter_iter` adapters that fuse without intermediate lists
- std.json, std.toml, std.csv, std.config — parse/encode config and data, including typed config decode
- std.net.tcp, std.net.dns, std.net.url, std.net.http, std.net.websocket, std.net.tls — networking
- std.hash, std.checksum, std.encoding, std.crypto, std.bits, std.bytes, std.binary — bytes, crypto, and encoding
- std.os.path, std.os.process, std.fs, std.glob — files, paths, and file discovery
- std.cli, std.diagnostic, std.testing, std.text.scanner — small tooling layers
- std.log, std.metrics, std.fmt, std.math, std.rand, std.time, std.datetime, std.uuid — common app helpers
- std.compress (gzip/zlib containers, stored blocks only — see limitations), std.archive (tar/zip), std.net.sse, std.data.table — archives, server-sent events, and tabular data
- std.regex — a pike-vm regular expression engine written in pith itself: linear-time matching with no pathological backtracking, capture groups, classes, alternation, anchors (no {n,m}, lazy quantifiers, backreferences, or lookaround)

for child processes, prefer `std.os.process.command(...)` and the structured
`run` / `output` / `start` flow. keep `std.io` for low-level stream work.

result types can carry either plain string errors or structured typed errors.
use bare `T!` when a string error is enough, and use `T!E` when callers need
to inspect the error payload.

```pith
struct ParseError:
    message: String

fn parse_port(text: String) -> Int!ParseError:
    if text == "":
        fail ParseError{"empty"}
    return 8080

fn require_port(text: String) -> Int!ParseError:
    return parse_port(text)!

fn main() -> Int!:
    print((require_port("8080") catch 9000).to_string())
    print((parse_port("") catch 9000).to_string())
    return 0
```

**memory management:**
- compiler-emitted reference counting for strings, lists, maps, sets,
  bytes, byte buffers, and structs — structs with heap fields get a
  generated destructor that releases them
- ownership follows a borrowed-by-default discipline: parameters are
  borrows (an unmodified argument costs no rc traffic), returns and
  constructions transfer, containers own their elements
- removed and overwritten container elements are deliberately not
  released while the container lives — a borrow of one may still be
  in flight — so those leak until the container dies; only the free
  path cascades
- closure environments do not reclaim yet, and reference cycles leak
  (there is no cycle collector in the native path)
- measured behavior and current numbers live in `docs/performance.md`

**real programs, in-tree:** `tools/sitegen/` is a small static site
generator (toml config, front matter, a markdown subset, layouts, tag
pages, a json feed), `tools/logscan/` analyzes web access logs
(combined-format parsing, gzip input, tabular aggregation, csv export),
`tools/apic/` is a json api client (http requests, dotted-path
extraction, pretty printing) whose golden check runs it against a pith
http server — both ends of the conversation in pith — and
`tools/parq/` runs a jobs file through a spawn-and-channels worker
pool. `make sitegen-check`, `make logscan-check`, `make apic-check`,
and `make parq-check` diff all four against golden files. they double
as the honest answer to "what does non-trivial pith look like."

**format specs:** interpolation takes rust-style specs after a colon:
`{total:8.2}` pads to width 8 with 2 decimals, `{n:04}` zero-pads,
`{name:<12}` and `{name:^12}` left-align and center. colons inside
strings or brackets never start a spec.

**error codes:** every diagnostic has a stable code — E0xx (lexer),
E1xx (parser), E2xx (checker), E3xx (lint). see `docs/errors.md` for the
full reference.

## cli commands

```
pith build <file>             # compile to native binary
pith run <file>               # compile and run
pith test <file>              # run test declarations
pith check <file>             # type check and report errors
pith check --json <file>      # machine-readable JSON diagnostics
pith fmt <file>               # format source code (canonical style)
pith fmt --check <file>       # check if file is formatted (exit 1 if not)
pith lint <file>              # check conventions and best practices
pith lint --json <file>       # machine-readable lint output
pith lex <file>               # print token stream
pith parse <file>             # print AST
pith doc <file>               # generate documentation
pith doc --json <file>        # machine-readable doc output
pith doc --check <file>       # verify all public items documented
pith doc search <query>       # search stdlib by keyword
pith package check            # type check package root from pith.toml
pith package test             # run package root tests
pith package lint             # lint package root
pith package doc              # generate package root documentation
pith package deps             # list local path dependencies
pith package lock             # write pith.lock for deterministic local deps
pith package install          # copy local dependencies into .pith/packages
pith package install --refresh # recopy cached local dependencies
pith package inspect [pkg]    # inspect current package or a dependency
pith package context [pkg]    # print a compact package summary for agents
pith version                  # print version
pith help                     # print usage
```

Package dependencies start with local paths in `pith.toml`:

```toml
[dependencies]
greeter = { path = "../greeter" }
```

That lets source code stay stable:

```pith
from greeter import greet
import greeter.extra as extra
```

## building

```
make build                 # build the Cranelift native backend
make self-host             # compile the self-hosted compiler via Cranelift
make bootstrap-verify      # verify Cranelift-compiled compiler works on all examples
make run-examples          # run the browseable deterministic examples
make run-regressions       # run deterministic regression cases
make status-audit          # print current corpus and source-size metrics
make test                  # run the full native + self-hosted test suite
make clean                 # remove build artifacts
```

## syntax highlighting on github

`.pith` files are temporarily mapped to python highlighting through `.gitattributes`.
this is a stopgap until pith is added to `github-linguist` with native support.
see `tooling/highlighting/` for the pith TextMate grammar, sample files, and
the upstream submission checklist.

## project layout

```
self-host/             compiler frontend — written in pith (~20,900 lines)
  pith_main.pith      CLI — check/fmt/lint/lex/parse/doc
  driver.pith          import resolution pipeline
  lexer.pith           tokenizer with indentation tracking
  parser.pith          recursive descent parser
  ast.pith             AST node representation
  printer.pith         AST pretty-printer
  checker.pith         type checker
  types.pith           type representation
  scope.pith           scope management
  formatter.pith       source code formatter
  linter.pith          convention linter
  errors.pith          human-readable error rendering
  docgen.pith          documentation generator and search

cranelift/             native code backend — Rust + Cranelift (~11,200 tracked lines)
  cli/               CLI entry point (build/run/test + delegates to self-host)
  codegen/           AST-to-IR compilation, monomorphization, type inference
  runtime/           runtime library (ARC, collections, JSON, TOML, URL, concurrency, crypto)

std/                 standard library (59 native pith modules, ~25,400 lines)
  cli.pith             command-line parsing helpers
  diagnostic.pith      reusable diagnostics for tools
  encoding.pith        base64/hex encoding
  fmt.pith             string formatting
  fs.pith              file I/O
  glob.pith            file pattern discovery
  hash.pith            SHA-256, FNV-1a
  json.pith            JSON parse/encode
  log.pith             structured logging
  math.pith            math builtins
  toml.pith            TOML parse/encode
  text/scanner.pith    source-like text cursor helpers
  net/tcp.pith         TCP connect/listen/accept/read/write/close
  net/tls.pith         native TLS 1.3 client and server streams
  net/tls13.pith       TLS 1.3 wire and key-schedule helpers
  net/dns.pith         DNS resolution
  net/url.pith         URL parsing and percent-encoding
  os/certs.pith        system root certificate loading
  os/path.pith         file path manipulation
  os/process.pith      child process management

examples/            user-facing demos and sample programs
  expected/          expected output snapshots for deterministic demos
  imports/           helper modules used by example imports

tests/               regression and negative compiler fixtures
  cases/             deterministic regression programs
  expected/          expected output snapshots for regression cases
  invalid/           checker-invalid programs + expected error codes
  invalid_parse/     parser-invalid programs + expected error codes

examples/            89 tracked .pith programs, including deterministic demos and live networking examples
docs/grammar.ebnf    complete EBNF for the language
docs/errors.md       error code reference (E0xx–E3xx)
docs/architecture.md compiler and ownership overview
docs/logging.md      structured logging guide
docs/contributing.md contributor setup and validation loop
```

## license

MIT
