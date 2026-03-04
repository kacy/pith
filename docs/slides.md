---
marp: true
theme: default
paginate: true
---

# Building Forge
## From Bootstrap to Self-Hosting

How we built a programming language for AI coding agents —
and then rewrote its compiler in itself.

---

# Why Forge Exists

- AI coding agents generate millions of lines of code — but current languages punish them with null pointer exceptions, data races, and panics
- Agents cannot debug a segfault. They cannot reason about undefined behavior
- We need a language where **every failure is a value**, not a crash

### Design goals

- No panics, no null, no data races
- Automatic memory management via ARC
- Result types everywhere
- Stable error codes that agents can parse and act on

---

# Forge at a Glance

```
fn greet(name: String) -> String:
    return "hello, {name}!"

fn main():
    message := greet("world")
    if message != "":
        print(message)
```

- Indentation-based syntax — no braces, no semicolons
- Type inference with `:=`
- String interpolation with `{expr}`
- Python-like readability, Rust-like safety

---

# Every Failure is a Value

```
fn divide(a: Int, b: Int) -> Int!:
    if b == 0:
        fail "division by zero"
    return a / b

fn half_divide(a: Int, b: Int) -> Int!:
    result := divide(a, b)!
    return result / 2

fn find_positive(n: Int) -> Int?:
    if n > 0:
        return n
    return none
```

- `T!` — result type (value or error). No exceptions
- `T?` — optional type. No null
- `!` operator propagates errors (like Rust's `?`)
- Exhaustive `match` prevents unhandled cases

---

# Powerful Type System

```
fn show[T: Display](x: T) -> String:
    return x.display()

struct Stack[T]:
    items: List[T]

impl Stack[T]:
    fn push(item: T):
        self.items.push(item)

enum Shape:
    Circle(Float)
    Rectangle(Float, Float)
```

- Generic structs, enums, and functions with type inference
- Interface bounds constrain type parameters
- Methods via `impl` blocks with implicit `self`
- Algebraic data types with pattern matching

---

# The Compiler Pipeline

```
Source (.fg)
    │
    ▼
  Lexer          tokenize, track indentation
    │
    ▼
  Parser         recursive descent → AST
    │
    ▼
  Checker        2-pass type checking
    │
    ▼
  Codegen        emit C source
    │
    ▼
  zig cc         compile to native binary
    │
    ▼
  Native Binary
```

Four clean stages. No intermediate representation — AST goes directly to C.

---

# Why C Transpilation?

- **Not LLVM.** LLVM is 100M+ lines, slow compile times, massive dependency
- **C runs everywhere.** Every platform has a C compiler
- **Debuggable output.** You can read the generated C
- **`zig cc`** wraps clang with zero-config cross-compilation

The entire runtime is a single header file:

```c
typedef struct {
    const char *data;
    int64_t len;
} forge_string_t;

#define FORGE_STRING_LIT(s) \
    ((forge_string_t){ .data = (s), .len = sizeof(s) - 1 })
```

838 lines. String ops, collections, print, helpers. That's it.

---

# Index-Based Type System

All types live in a flat array. `TypeId` is a plain `u32`.

```
TID_INT        := 0
TID_FLOAT      := 2
TID_BOOL       := 3
TID_STRING     := 4
TID_VOID       := 6
TID_FIRST_USER := 16    # user types start here
```

AST nodes use the same pattern — arena-indexed, no pointers:

```
pub struct Node:
    pub kind: String
    pub value: String
    pub children: List[Int]     # indices, not pointers

pub fn add_node(kind: String, value: String,
                children: List[Int]) -> Int:
    nodes.push(Node(kind, value, children))
    return nodes.len() - 1      # return index
```

O(1) type equality. No pointer chasing. No graph traversal.

---

# Two-Pass Type Checker

### Pass 1 — Register
Walk top-level declarations. Record names and type signatures in module scope.

### Pass 2 — Check
Walk function bodies and bindings. Resolve types. Create child scopes.

**Why two passes?**
- Functions can call each other in any order — no forward declarations
- Error sentinel pattern: when a sub-expression has type `ERR`, further checks are skipped to prevent cascading noise
- Stable error codes (E0xx–E3xx) with `--json` output for agent consumption

---

# The Bootstrap Compiler

**20,320 lines of Zig** across 14 files.

```
checker.zig     8,292 lines    type system — the hardest part
codegen.zig     3,889 lines    C transpilation
parser.zig      3,009 lines    recursive descent
lexer.zig       1,408 lines    tokenizer
runtime.h         838 lines    C runtime header
main.zig          675 lines    CLI entry point
+ 8 more files
```

- Written from scratch — no parser generators, no dependencies
- Zig chosen for: no hidden allocations, comptime, C interop, no runtime
- ~360 unit tests ensure correctness before self-hosting begins
- This is **stage 0** — it compiles Forge to C, then `zig cc` produces native binaries

---

# The Journey to Self-Hosting

Before we could rewrite the compiler in Forge, we needed the language features:

1. Generics with monomorphization
2. Methods and impl blocks
3. Collections (List, Map, Set)
4. Result types and try propagation
5. Match with exhaustiveness checking
6. Module/import system

Then, one module at a time:

- **PR #59** — `lexer.fg`: first self-hosted component (838 lines)
- **PR #61** — `parser.fg`: recursive descent parser in Forge (1,386 lines)
- `checker.fg`: full 2-pass type checker — the hardest module (2,558 lines)
- **PR #63** — `codegen.fg`: C transpilation closes the loop (3,766 lines)

Each module validated against the bootstrap compiler's output before moving on.

---

# Bootstrap vs. Self-Hosted

```
Module          Zig (bootstrap)     Forge (self-host)
─────────       ───────────────     ─────────────────
Lexer              1,408 lines           838 lines
Parser             3,009 lines         1,386 lines
Checker            8,292 lines         2,558 lines
Codegen            3,889 lines         3,766 lines
Supporting         3,722 lines         1,416 lines
─────────       ───────────────     ─────────────────
Total             20,320 lines         9,964 lines
```

The self-hosted compiler is **~49% the size** of the bootstrap.

Forge's expressiveness — type inference, result types, string interpolation — eliminates boilerplate that Zig requires you to write by hand.

---

# Forge Compiling Forge

```
lexer.fg   ──┐
parser.fg  ──┤  compiled by Zig      ┌──────────────────┐
checker.fg ──┤  bootstrap (stage 0)  │ Stage 1 compiler  │
codegen.fg ──┘  ─────────────────►   │ (native binary)   │
                                     └────────┬─────────┘
                                              │
                                              ▼
                                     can compile any .fg
                                     file — including itself
```

- **Stage 0:** Zig bootstrap compiles the `.fg` files → C → native binary
- **Stage 1:** That binary *is* a Forge compiler written in Forge
- **Stage 2 (future):** Stage 1 compiles its own source, producing an identical binary

The self-hosted compiler passes **20/20 conformance tests** today.

---

# Where We Are Now

- **31,122** total lines of code (Zig + Forge)
- **~360** unit tests
- **22** example programs, 19 compile to native binaries
- **Stable error codes** E0xx through E3xx with `--json` diagnostics
- **Formatter** (`forge fmt`) and **linter** (`forge lint`) built in
- **Security hardening:** depth limits, UTF-8 validation, no-panic guarantee
- **Self-hosted compiler** passes 20/20 conformance tests
- **Zero panics** in the bootstrap compiler — every error is a diagnostic

---

# What's Next

- **Stage 2 bootstrap** — self-hosted compiler compiles itself
- **ARC memory management** — replace "leak everything" with automatic reference counting
- **Closures** — capturing lambdas
- **Concurrency runtime** — spawn/await backed by real threads
- **LSP / editor integration** — for AI agent toolchains
- **Package manager and standard library**

### The goal

Forge becomes the default language AI agents write — and write *in*.
