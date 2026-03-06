# forge

a programming language where any coding agent is immediately productive.

no panics, no null, no data races. automatic memory management via ARC with
compile-time cycle prevention. result types everywhere. designed so that AI
coding agents can read the errors, apply fixes, and iterate — fast.

**status:** the compiler self-hosts — forge is written in forge. the
self-hosted compiler compiles itself and produces identical output across
stages. all 40 example programs compile and run. the CLI handles 17 commands:
build, run, test, check, fmt, lint, lex, parse, doc, and more. 13 standard
library modules cover I/O, networking, encoding, hashing, JSON, TOML, and
process management.

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
codegen. all 40 example programs compile to native binaries via C
transpilation.

**language features:**
- function declarations, typed parameters, return types, calls
- struct declarations with typed fields, field access, constructors
- enum declarations with variant data
- variable bindings with type inference (`x := 42`)
- mutability enforcement (`mut` required for reassignment)
- if/elif/else, while, for loops over collections with scoping
- binary operators: arithmetic, comparison, logical, string concatenation
- unary operators: negate, not
- string interpolation
- return type checking
- match expressions with exhaustiveness checking
- method calls and impl blocks
- pipe operator (`x | f`)
- collection literals: List, Map, Set with index expressions
- generics with monomorphization
- lambdas and closures (capturing lambdas with uniform closure ABI)
- result types (`T!`) with try propagation (`expr!`) and `fail`
- optional types (`T?`)
- tuples with field access (`t.0`, `t.1`)
- type aliases
- concurrency: spawn/await, Task[T], Mutex, WaitGroup, Semaphore, Channel
- multi-module imports

**standard library (13 modules):**
- string methods, type conversions, math builtins
- file I/O, env, args, exit, exec
- collection methods (push, remove, contains, keys, values, reverse, etc.)
- std.json, std.toml — parse/encode
- std.net.tcp, std.net.dns, std.net.url — networking
- std.hash, std.encoding — SHA-256, FNV-1a, base64, hex
- std.os.path, std.os.process — path manipulation, child processes
- std.log, std.fmt, std.fs, std.math — logging, formatting, file ops

**error codes:** every diagnostic has a stable code — E0xx (lexer),
E1xx (parser), E2xx (checker), E3xx (lint). see `docs/errors.md` for the
full reference.

## cli commands

```
forge build <file>             # compile to native binary (via C transpilation)
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

requires [zig 0.15.2](https://ziglang.org/download/) for the initial bootstrap.

```
make self-host             # build the self-hosted compiler (via zig bootstrap)
make bootstrap             # rebuild the compiler using itself
make bootstrap-verify      # verify fixed-point (C output identical across stages)
```

zig bootstrap (archived, for unit tests):

```
zig build                  # compile bootstrap
zig build test             # run ~360 unit tests
zig build release          # release build (~30x faster compilation)
```

other make targets:

```
make build                 # compile zig bootstrap (debug)
make release               # compile zig bootstrap (release)
make test                  # run zig unit tests
make check                 # build + forge check all examples
make run-examples          # run all examples (bootstrap)
make run-examples-self     # run all examples (self-hosted)
make fmt                   # format zig source
make clean                 # remove build artifacts
```

## project layout

```
self-host/
  forge_main.fg      unified CLI — build/run/test/check/fmt/lint/lex/parse/doc
  codegen_main.fg    codegen entry point — emits C to stdout
  driver.fg          compile pipeline and import resolution
  lexer.fg           tokenizer with indentation tracking
  parser.fg          recursive descent parser
  ast.fg             AST node representation
  printer.fg         AST pretty-printer
  checker.fg         type checker
  types.fg           type representation
  scope.fg           scope management
  codegen.fg         C transpilation backend (~4,000 lines)
  formatter.fg       source code formatter
  linter.fg          convention linter
  errors.fg          human-readable error rendering
  docgen.fg          documentation generator and search

runtime/
  forge_runtime.h    C runtime header — memory, strings, collections, I/O

bootstrap/           zig bootstrap compiler (archived, for reference and unit tests)
  main.zig, lexer.zig, parser.zig, checker.zig, codegen.zig, etc.

std/                 standard library (13 native forge modules)
  encoding.fg        base64/hex encoding
  fmt.fg             string formatting
  fs.fg              file I/O
  hash.fg            SHA-256, FNV-1a
  json.fg            JSON parse/encode
  log.fg             structured logging
  math.fg            math builtins
  toml.fg            TOML parse/encode
  net/tcp.fg         TCP connect/listen/accept/read/write/close
  net/dns.fg         DNS resolution
  net/url.fg         URL parsing and percent-encoding
  os/path.fg         file path manipulation
  os/process.fg      child process management

examples/            40 .fg programs — all compile to native binaries
docs/grammar.ebnf    complete EBNF for the language
docs/errors.md       error code reference (E0xx–E3xx)
```

## license

MIT
