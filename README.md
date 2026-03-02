# forge

a programming language where any coding agent is immediately productive.

no panics, no null, no data races. automatic memory management via ARC with
compile-time cycle prevention. result types everywhere. designed so that AI
coding agents can read the errors, apply fixes, and iterate — fast.

**status:** early bootstrap. the compiler is being written in zig and will
self-host once the language is expressive enough.

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

the bootstrap compiler handles lexing, parsing, and type checking.

**checked:**
- function declarations, typed parameters, return types, calls
- struct declarations with typed fields, field access
- enum declarations with variant data
- variable bindings with type inference (`x := 42`)
- mutability enforcement (`mut` required for reassignment)
- if/elif/else, while, for loops with scoping
- binary operators: arithmetic, comparison, logical, string concatenation
- unary operators: negate, not
- string interpolation
- return type checking

**not yet checked** (parses fine, returns error sentinel in the checker):
method calls, match, lambdas, collection literals, generics, interfaces,
impl blocks, type aliases, try/unwrap, pipe operator.

## cli commands

```
forge lex <file>     # print token stream
forge parse <file>   # print AST
forge check <file>   # type check and report errors
```

## building

requires [zig 0.15.2](https://ziglang.org/download/).

```
zig build          # compile
zig build run      # compile and run
zig build test     # run 191 tests
```

or with make:

```
make build         # compile
make test          # run tests
make check         # build + forge check all examples
make fmt           # format source
make clean         # remove build artifacts
```

## project layout

```
src/
  main.zig         CLI entry point (lex, parse, check commands)
  lexer.zig        tokenizer with indentation tracking
  parser.zig       recursive descent parser
  ast.zig          AST node types
  checker.zig      type checker (two-pass: register, then check)
  types.zig        type representation and type table
  printer.zig      AST pretty-printer
  errors.zig       diagnostics with source context
  intern.zig       string interning (arena-backed)
  io.zig           buffered I/O helpers

examples/          .fg programs that pass forge check
docs/grammar.ebnf  complete EBNF for the language
```

## license

MIT
