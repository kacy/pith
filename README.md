# forge

a programming language where any coding agent is immediately productive.

no panics, no null, no data races. automatic memory management via ARC with
compile-time cycle prevention. result types everywhere. designed so that AI
coding agents can read the errors, apply fixes, and iterate — fast.

**status:** the compiler self-hosts — forge is written in forge. the
self-hosted compiler compiles itself and produces identical output across
stages (fixed-point verified). two backends: C transpilation and Cranelift
native code generation — both compile all 82 examples identically, and both
compile the compiler itself to a fixed point. the CLI handles build, run,
test, check, fmt, lint, lex, parse, doc, and more. 43 standard library
modules cover I/O, networking, encoding, hashing, JSON, TOML, process
management, tooling helpers, and more.

## quick start

requires [rust/cargo](https://rustup.rs/) for the Cranelift native backend.

```
cargo build --release
./target/release/forge run examples/hello.fg
make self-host
./self-host/forge_main check examples/hello.fg
```

## where to read first

- `README.md` for the high-level map
- `docs/architecture.md` for the compiler pipeline and subsystem boundaries
- `docs/concurrency.md` for channels, contexts, select, and task waits
- `docs/http_apps.md` for the higher-level http request/response layer
- `docs/text_and_bytes.md` for the string/bytes split and common helpers
- `docs/contributing.md` for the development loop and smoke checks
- `docs/tooling_stdlib.md` for glob, cli, diagnostic, and testing helpers
- `self-host/forge_main.fg` for the self-hosted frontend (lex/parse/check/fmt/lint/doc)
- `cranelift/cli/src/main.rs` for the native backend CLI (build/run)

## contributor fast path

if you are new to the codebase, the shortest useful path is:

1. run `cargo build --release`
2. run `./target/release/forge run examples/hello.fg`
3. run `make self-host`
4. run `./self-host/forge_main check examples/hello.fg`
5. read `docs/architecture.md`

common starting points:

- add a token or keyword: `self-host/lexer.fg`
- add syntax: `self-host/parser.fg`, `docs/grammar.ebnf`
- add type rules: `self-host/checker.fg`
- native code generation: `cranelift/codegen/src/ir_consumer.rs`

for example-facing output, prefer interpolation for direct value printing:

```fg
print("count: {items.len()}")
```

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

the self-hosted compiler handles the full pipeline: lex → parse → check →
codegen. 82 deterministic example programs compile and produce verified
output via the Cranelift native backend.

**language features:**
- function declarations, typed parameters, return types, calls
- struct declarations with typed fields, field access, constructors
- enum declarations with variant data
- interface declarations with method signatures
- impl blocks for structs with method implementations
- generic types, generic functions, and generic interfaces with bounds
- variable bindings with type inference (`x := 42`)
- mutability enforcement (`mut` required for reassignment)
- if/elif/else, while, for loops over collections and strings with scoping
- binary operators: arithmetic, comparison, logical, string concatenation
- unary operators: negate, not
- string interpolation and character iteration (`for c in string`)
- return type checking
- match expressions with exhaustiveness checking
- method calls and impl blocks
- pipe operator (`x | f`)
- collection literals: List, Map, Set with index expressions
- generics with monomorphization
- lambdas and closures (capturing lambdas with uniform closure ABI)
- result types (`T!` and `T!E`) with try propagation (`expr!`), `fail`, `catch`, `unwrap_or`, and `or_else`
- optional types (`T?`)
- tuples with field access (`t.0`, `t.1`)
- type aliases
- concurrency: spawn/await, Task[T], Mutex, WaitGroup, Semaphore, Channel, select, contexts, timers
- multi-module imports with `from ... import`

**standard library (47 modules):**
- string methods, type conversions, math builtins
- file I/O, env, args, exit, exec
- collection methods (push, remove, contains, keys, values, reverse, etc.)
- std.json, std.toml, std.csv, std.config — parse/encode config and data
- std.net.tcp, std.net.dns, std.net.url, std.net.http, std.net.websocket — networking
- std.hash, std.checksum, std.encoding, std.crypto, std.bits, std.bytes, std.binary — bytes, crypto, and encoding
- std.os.path, std.os.process, std.fs, std.glob — files, paths, and file discovery
- std.cli, std.diagnostic, std.testing, std.text.scanner — small tooling layers
- std.log, std.fmt, std.math, std.rand, std.time, std.datetime, std.uuid — common app helpers

for child processes, prefer `std.os.process.command(...)` and the structured
`run` / `output` / `start` flow. keep `std.io` for low-level stream work.

result types can carry either plain string errors or structured typed errors.
use bare `T!` when a string error is enough, and use `T!E` when callers need
to inspect the error payload.

```fg
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
- complete automatic reference counting (ARC) for all heap-allocated types
- string ARC with retain/release
- collection ARC for List, Map, Set
- closure ARC for lambda environments
- cycle collection with periodic mark-and-scan algorithm

**error codes:** every diagnostic has a stable code — E0xx (lexer),
E1xx (parser), E2xx (checker), E3xx (lint). see `docs/errors.md` for the
full reference.

## cli commands

```
forge build <file>             # compile to native binary
forge run <file>               # compile and run
forge test <file>              # run test declarations
forge check <file>             # type check and report errors
forge check --json <file>      # machine-readable JSON diagnostics
forge fmt <file>               # format source code (canonical style)
forge fmt --check <file>       # check if file is formatted (exit 1 if not)
forge lint <file>              # check conventions and best practices
forge lint --json <file>       # machine-readable lint output
forge lex <file>               # print token stream
forge parse <file>             # print AST
forge doc <file>               # generate documentation
forge doc --json <file>        # machine-readable doc output
forge doc --check <file>       # verify all public items documented
forge doc search <query>       # search stdlib by keyword
forge version                  # print version
forge help                     # print usage
```

## building

```
make build                 # build the Cranelift native backend
make self-host             # compile the self-hosted compiler via Cranelift
make bootstrap-verify      # verify Cranelift-compiled compiler works on all examples
make run-examples          # run the browseable deterministic examples
make run-regressions       # run deterministic regression cases
make test                  # run the full native + self-hosted test suite
make clean                 # remove build artifacts
```

## syntax highlighting on github

`.fg` files are temporarily mapped to python highlighting through `.gitattributes`.
this is a stopgap until forge is added to `github-linguist` with native support.
see `tooling/highlighting/` for the forge TextMate grammar, sample files, and
the upstream submission checklist.

## project layout

```
self-host/             compiler frontend — written in forge (~8,800 lines)
  forge_main.fg      CLI — check/fmt/lint/lex/parse/doc
  driver.fg          import resolution pipeline
  lexer.fg           tokenizer with indentation tracking
  parser.fg          recursive descent parser
  ast.fg             AST node representation
  printer.fg         AST pretty-printer
  checker.fg         type checker
  types.fg           type representation
  scope.fg           scope management
  formatter.fg       source code formatter
  linter.fg          convention linter
  errors.fg          human-readable error rendering
  docgen.fg          documentation generator and search

cranelift/             native code backend — Rust + Cranelift (~18,100 lines)
  cli/               CLI entry point (build/run/test + delegates to self-host)
  codegen/           AST-to-IR compilation, monomorphization, type inference
  runtime/           runtime library (ARC, collections, JSON, TOML, URL, concurrency)

std/                 standard library (43 native forge modules)
  cli.fg             command-line parsing helpers
  diagnostic.fg      reusable diagnostics for tools
  encoding.fg        base64/hex encoding
  fmt.fg             string formatting
  fs.fg              file I/O
  glob.fg            file pattern discovery
  hash.fg            SHA-256, FNV-1a
  json.fg            JSON parse/encode
  log.fg             structured logging
  math.fg            math builtins
  toml.fg            TOML parse/encode
  text/scanner.fg    source-like text cursor helpers
  net/tcp.fg         TCP connect/listen/accept/read/write/close
  net/dns.fg         DNS resolution
  net/url.fg         URL parsing and percent-encoding
  os/path.fg         file path manipulation
  os/process.fg      child process management

examples/            user-facing demos and sample programs
  expected/          expected output snapshots for deterministic demos
  imports/           helper modules used by example imports

tests/               regression and negative compiler fixtures
  cases/             deterministic regression programs
  expected/          expected output snapshots for regression cases
  invalid/           checker-invalid programs + expected error codes
  invalid_parse/     parser-invalid programs + expected error codes

examples/              82 deterministic .fg programs with verified expected output
docs/grammar.ebnf    complete EBNF for the language
docs/errors.md       error code reference (E0xx–E3xx)
docs/architecture.md compiler and ownership overview
docs/logging.md      structured logging guide
docs/contributing.md contributor setup and validation loop
```

## license

MIT
