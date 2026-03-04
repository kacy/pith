# forge

a programming language where any coding agent is immediately productive.

no panics, no null, no data races. automatic memory management via ARC with
compile-time cycle prevention. result types everywhere. designed so that AI
coding agents can read the errors, apply fixes, and iterate — fast.

**status:** the compiler self-hosts. forge is written in forge — the
self-hosted compiler compiles itself and produces identical output. the zig
bootstrap is still maintained for testing and toolchain commands.

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

the bootstrap compiler handles the full pipeline: lex → parse → check → codegen.
all 21 example programs compile to native binaries via C transpilation.

the self-hosted compiler (`self-host/codegen_main.fg`) can compile any forge
program — including itself. it reaches a fixed point: the self-compiled
compiler produces identical output to the zig-compiled one.

**checked and compiling:**
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
- lambdas (non-capturing)
- result types (`T!`) with try propagation (`expr!`) and `fail`
- optional types (`T?`)
- tuples with field access (`t.0`, `t.1`)
- string methods (len, contains, split, trim, etc.)
- type conversions (to_string, to_int, to_float, parse_int, parse_float)
- file I/O (read_file, write_file), env, args, exit
- collection methods (push, remove, contains, keys, values, reverse, etc.)
- multi-module imports

**not yet implemented in codegen** (parses and type-checks fine):
concurrency (spawn/await), type aliases, closures (capturing lambdas).

**error codes:** every diagnostic has a stable code — E0xx (lexer),
E1xx (parser), E2xx (checker), E3xx (lint). see `docs/errors.md` for the
full reference.

## cli commands

```
forge lex <file>          # print token stream
forge parse <file>        # print AST
forge check <file>        # type check and report errors
forge check --json <file> # machine-readable JSON diagnostics
forge build <file>        # compile to native binary (via C transpilation)
forge run <file>          # compile and run
forge test <file>         # run test declarations
forge fmt <file>          # format source code (canonical style)
forge fmt --check <file>  # check if file is formatted (exit 1 if not)
forge lint <file>         # check conventions and best practices
forge lint --json <file>  # machine-readable lint output
```

self-hosted compiler:

```
forge run self-host/codegen_main.fg <file>  # compile using the forge-written compiler
```

## building

requires [zig 0.15.2](https://ziglang.org/download/).

```
zig build          # compile
zig build run      # compile and run
zig build test     # run ~360 tests
```

or with make:

```
make build         # compile (debug)
make release       # compile (release — ~30x faster compilation)
make test          # run tests
make check         # build + forge check all examples
make fmt           # format source
make clean         # remove build artifacts
```

## project layout

```
src/
  main.zig           CLI entry point (lex, parse, check, build, run, test, fmt, lint)
  lexer.zig          tokenizer with indentation tracking
  parser.zig         recursive descent parser
  ast.zig            AST node types
  checker.zig        type checker (two-pass: register, then check)
  types.zig          type representation and type table
  codegen.zig        C transpilation backend
  forge_runtime.h    C runtime header (embedded via @embedFile)
  formatter.zig      source code formatter (forge fmt)
  lint.zig           convention linter (forge lint)
  printer.zig        AST pretty-printer
  errors.zig         diagnostics, error codes, and source context
  intern.zig         string interning (arena-backed)
  io.zig             buffered I/O helpers

self-host/
  codegen_main.fg    entry point — compiles .fg files using the forge-written compiler
  lexer.fg           tokenizer (port of lexer.zig)
  parser.fg          recursive descent parser (port of parser.zig)
  ast.fg             AST node representation
  printer.fg         AST pretty-printer
  checker.fg         type checker
  types.fg           type representation
  scope.fg           scope management for checker
  codegen.fg         C transpilation backend (~3,950 lines)

examples/            .fg programs (21 compile to native binaries)
docs/grammar.ebnf    complete EBNF for the language
docs/errors.md       error code reference (E0xx–E3xx)
```

## license

MIT
