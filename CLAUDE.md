# Forge — Project Plan

> An AI-native programming language where any coding agent is immediately
> productive because the ecosystem is designed for it.
>
> Bootstrapped in Zig. Self-hosted in Forge as soon as possible.

---

## Core Realization

We don't build a "forge agent" CLI. We don't build agent infrastructure at all.
**We build a language and ecosystem so well-structured that any coding agent —
Claude Code, Cursor, Copilot, whatever comes next — is immediately productive.**

The secret weapon isn't a custom agent. It's:
- An `AGENTS.md` in every package (Claude Code already reads this)
- A compiler with incredible error messages
- A linter that enforces conventions automatically
- A registry where `forge inspect` gives agents full source + context
- Tools so good that an agent's write → check → fix loop takes seconds

A developer using Claude Code just says:

```
"build a redis client package for forge. look at how the memcached
package did it. run forge check and forge test as you go."
```

Claude Code reads AGENTS.md, understands the conventions, inspects reference
packages, and builds. No special tooling. No custom orchestration.

---

## Very Important

* This should be very readible code for humans.
* If it feels too clever, either rework it for simplicity or add extensive comments explaining.
* Clear over clever when possible.
* You working style should mimic a human.
* Frequent, atomic commits instead of just completely finishing the file in one go.
* Use Zig best practices and patterns where possible. When we get to writing Forge, we'll get to express our own patterns.
* Forge NEVER panics or fails. It should be built from the explicit goal of being production grade.
* Add unit tests, but only for core functionality. Avoid tests for the sake of tests. However, you will want to definitely cover edge cases.
* The code and documentation should feel like it was written by a human.
* Commit frequently. Commits should be clear, atomic, and consistent history of changes.
* Commits should feel like a natural development process, not a complete product the first attempt.
* Pull Requests for complete features. Avoid massive pull requests. I'll be here to unblock you if you need it.
* Pull Requests should never have tasks. Remember to make this feel like a human wrote it.
* Opt for lower case things, both in documentation and commits. It feels more humand and dev focused.
* Be sure to include a .gitignore, Dockerfile, and Makefile.
* The gitignore file will NOT have a reference to CLAUD, but you should also never commit that file.
* Focus on ease of readibility. Remember, this code should be highly performant, but ultimately will be read by many humans.
* Tasks should never be added to pull requests.
* For every Pull Request, you are required to list: a summary, what was tested, and optionally design considerations. Never list tasks in Pull Requests.
* Almost never commit directly to main. If you have commits, it's most likely best to add it to a different branch so that you can create a PR. Sometimes for small changes it makes sense. Use your best judgement.
* Before finishing your code, identify if there are ways to simplify.
* Auto-merge almost all pull requests and pull remotely from main when completed. If you think it needs a human input, please pause after opening.
* Use subagents liberally and keep the context window clean.
* For non-trivial tasks, pause and ask "is there a more elegant solution"



---

## Why Zig

The bootstrap compiler is written in Zig. Not Rust.

**Philosophy alignment.** Zig's design ethos — simple, explicit, no hidden
control flow, no magic — mirrors what Forge is trying to be. Building Forge
in a language that shares its values keeps us honest. If something feels
wrong in Zig, it'd probably feel wrong in Forge too.

**Community.** The Zig community is small, positive, and pragmatic. There's
no cultural hostility toward AI-assisted development. We're building the
AI-native language — our toolchain's community should reflect that.

**Practical benefits:**
- Fast compile times (critical for compiler development iteration speed)
- C interop for free (useful for Cranelift/LLVM bindings later)
- `comptime` is genuinely powerful for compiler internals — type tables,
  opcode dispatch, AST node definitions
- Simple build system (`build.zig`, no cmake/cargo complexity)
- Produces small, fast native binaries with no runtime

**The risk:** Zig is pre-1.0 and still has breaking changes. We accept this
because the bootstrap compiler is temporary — we're self-hosting in Forge
as soon as the language can compile itself. The Zig code is scaffolding,
not a permanent foundation.

**Self-hosting is the goal.** Start in Zig → get Forge to the point where
it can compile itself → rewrite the compiler in Forge → Zig served its
purpose. This is the same path Go and Rust took (both started in C/OCaml).
It's also a powerful forcing function — dogfooding the language on a
serious project (its own compiler) surfaces design problems early.

---

## Project Phases

```
Phase 1: Bootstrap Compiler in Zig       (Months 1-5)
Phase 2: Standard Library                 (Months 3-7)
Phase 3: Toolchain                        (Months 5-9)
Phase 4: Self-Hosting                     (Months 8-11)
Phase 5: Package Ecosystem                (Months 8-12)
Phase 6: Community Launch                 (Months 11-14)
```

Phases overlap intentionally. You can't design a good stdlib without
writing real programs, and you can't design good tooling without a stdlib.

---

## Phase 1: Bootstrap Compiler in Zig

**Goal:** A working compiler written in Zig that compiles Forge programs to
native code. Not feature-complete — just enough to start writing real things
and eventually compile the Forge compiler itself.

### 1.1 Language Grammar & Parser

Define the formal grammar and build a hand-written recursive descent parser.

```
Decisions (finalized):
├── Indentation-based blocks (Python-style)
├── Expression-based (last expr = return value)
├── `:=` for binding, `mut` for mutability
├── `!` for Result return types, `?` for propagation, `fail` for errors
├── String interpolation: "{expr}"
├── Pattern matching: `match` with exhaustiveness checking
├── Generics: Type[T] not Type<T> (avoids < ambiguity)
├── Newline-terminated statements (no semicolons)
├── Lambda: fn(x) => x * 2 (short), fn(x): block (multi-line)
├── Methods: object.method(), defined with `self` in impl blocks
├── Private by default, `pub` to export
├── File-based modules (one file = one module, directory = namespace)
└── No operator overloading (keeps things predictable for agents)
```

**Implementation in Zig:**

The parser is hand-written recursive descent. No parser generator — we need
full control over error messages and error recovery. Zig's `comptime` helps
here for building lookup tables (keyword maps, operator precedence tables).

```
Compiler source layout:
bootstrap/
├── build.zig
├── src/
│   ├── main.zig           — CLI entry point
│   ├── lexer.zig          — tokenizer
│   ├── parser.zig         — recursive descent parser
│   ├── ast.zig            — AST node types (comptime-generated variants)
│   ├── types.zig          — type representation and type checker
│   ├── checker.zig        — semantic analysis and type checking
│   ├── memory.zig         — ARC analysis and cycle detection
│   ├── codegen.zig        — code generation backend
│   ├── errors.zig         — error formatting and suggestions
│   ├── intern.zig         — string interning (comptime hash map)
│   └── util.zig           — allocators, arena, helpers
└── tests/
    ├── lexer_tests.zig
    ├── parser_tests.zig
    ├── checker_tests.zig
    └── snapshots/         — expected AST/error output for test files
```

**Why hand-written in Zig specifically:**
- Zig's explicit allocator model means the parser won't leak memory
- `comptime` keyword tables compile to perfect hash lookups
- Tagged unions (Zig's error unions) map naturally to AST nodes
- No hidden allocations — we know exactly where memory goes
- The resulting parser binary is small and fast

**Deliverable:** `forge parse file.fg` prints the AST.

### 1.2 Type System

```
Type system features:
├── Primitives: Int, UInt, Float, Bool, String, Bytes
├── Sized integers: Int8, Int16, Int32, Int64, UInt8, UInt16, UInt32, UInt64
├── Compound: List[T], Map[K, V], Set[T], Tuple (A, B, C)
├── Optional: T? is sugar for Option[T]
├── Result: T! is sugar for Result[T, Error]
│           T!E is sugar for Result[T, E]
├── Structs: nominal, with default field values
├── Enums: algebraic data types with associated data
├── Interfaces: like Rust traits, but simpler
│   ├── No associated types (too complex for agents)
│   ├── No lifetime parameters (ARC eliminates the need)
│   ├── No orphan rules (any package can impl any interface for any type)
│   │   └── Conflicts resolved at link time with clear error messages
│   └── Interface bounds on generics: fn foo[T: Display + Hash](x: T)
├── Generics: monomorphized for performance, interface-bounded
├── No null. Option[T] everywhere.
├── No implicit conversions. All conversions are explicit methods.
└── No inheritance. Composition via struct embedding (like Go).
```

**Zig-specific implementation notes:**
- Type representations use Zig's tagged unions — natural fit for a type system
- Generic monomorphization leverages `comptime` for template expansion
- String interning for type names uses Zig's `std.StringHashMap` with arena
- Type equality checking is pointer-based after interning (fast)

**Deliverable:** `forge check file.fg` type-checks and reports errors.

### 1.3 Memory Model — ARC + Cycle Prevention

```
Memory model:
├── Automatic Reference Counting (ARC)
│   ├── Every heap allocation has a reference count
│   ├── Count incremented on share, decremented when scope exits
│   ├── Object freed when count hits zero
│   ├── Deterministic destruction (no GC pauses)
│   └── Non-atomic refcount for single-task access (optimization)
│
├── Move semantics by default
│   ├── b := a  → a is moved, no longer usable
│   ├── Function calls move arguments by default
│   ├── ref for borrowing: fn foo(x: ref Thing)
│   ├── mut ref for mutable borrowing: fn foo(x: mut ref Thing)
│   └── .copy() for explicit deep copy
│
├── Compile-time cycle prevention
│   ├── Compiler builds a type ownership graph
│   ├── Cycles in the ownership graph → compile error
│   ├── weak keyword to break cycles: parent: weak Node?
│   ├── weak refs return Option (auto nil-checked)
│   └── Catches 99% of real-world leaks at compile time
│
├── No manual memory management
│   ├── No malloc/free, no unsafe, no raw pointers
│   └── Drop interface for custom cleanup
│
└── Performance
    ├── Small types (< 64 bytes) stack-allocated, no refcount
    ├── Compiler elides refcount ops when ownership is clear
    ├── Arena[T] available for bulk allocation patterns
    └── Escape analysis promotes heap to stack when possible
```

**No borrow checker.** ARC + moves covers our safety guarantees. We trade
a small runtime cost (refcount operations) for a dramatically simpler
programming model. For hot paths, compiler elision and arena allocators
close the gap.

**Zig-specific:** Zig's own explicit allocator model informs our ARC
implementation. The bootstrap compiler's memory management is a good
rehearsal for Forge's — both languages believe memory should be visible
and predictable.

**Deliverable:** Programs with ownership cycles are rejected at compile time.

### 1.4 Code Generation

```
Backend: Cranelift (via C API)
├── Why Cranelift:
│   ├── Fast compilation (designed for JIT, great for edit-check loops)
│   ├── Good native code quality (not LLVM-tier, but close enough)
│   ├── C API available (cranelift-c, bindable from Zig)
│   ├── Active development by the Wasmtime team
│   └── Can add LLVM backend later for release-optimized builds
│
├── Why not LLVM directly:
│   ├── LLVM compile times are slow (bad for agent iteration loops)
│   ├── LLVM C++ dependency is heavy
│   └── Cranelift-first, LLVM-second mirrors Zig's own approach
│
├── Alternative: C transpilation
│   ├── Simplest possible backend — emit C, let cc handle codegen
│   ├── Good for bootstrapping: get programs running fast
│   ├── Then replace with Cranelift for real codegen
│   └── Recommended as Phase 1a if Cranelift bindings cause friction
│
└── Zig-specific:
    ├── Zig's build system can link C libraries trivially
    ├── Cranelift C API bindings are straightforward in Zig
    └── Zig ships its own libc — no system dependency headaches
```

**Pragmatic approach:** Start with C transpilation to get programs running in
week 3-4. Replace with Cranelift once the language is stable enough that we're
not changing codegen constantly. C output also makes debugging easier early on —
you can read the generated code.

**Deliverable:** `forge build file.fg` → native binary. `forge run file.fg` → runs it.

### 1.5 Error Messages

Error messages are how agents (and humans) learn the language. They're a
first-class feature, not an afterthought.

```
Requirements:
├── Machine-parseable: forge check --format json
├── Exact location: file, line, column, span
├── Fix suggestions for every common error
├── Plain language (no "cannot infer lifetime 'a in scope")
├── Show relevant code with underline markers
├── Error codes that link to docs (E001, E002, ...)
└── Agent-optimized: JSON output includes suggested replacement text
```

Example:

```
error[E012]: type mismatch
  → src/main.fg:15:10

  14 │ fn double(x: Int) -> String:
  15 │   x * 2
     │   ^^^^^ this is Int, but the function returns String

  fix: change the return type to Int
  14 │ fn double(x: Int) -> Int:

  docs: https://forge-lang.dev/errors/E012
```

JSON output (what agents actually parse):

```json
{
  "code": "E012",
  "severity": "error",
  "message": "type mismatch: expected String, got Int",
  "file": "src/main.fg",
  "line": 15,
  "col": 10,
  "fix": {
    "description": "change return type to Int",
    "replacement": {
      "file": "src/main.fg",
      "line": 14,
      "old": "-> String:",
      "new": "-> Int:"
    }
  }
}
```

An agent reads the JSON, applies the fix, re-runs `forge check`. Tight loop.

---

## Phase 2: Standard Library

**Goal:** Enough stdlib to build real networked applications and, critically,
enough to write a compiler — because Phase 4 is self-hosting.

### Priority (informed by self-hosting needs):

```
Tier 1 — Required for self-hosting the compiler:
├── std.io          — Reader, Writer, Buffer, file I/O
├── std.bytes       — Bytes, BytesMut, byte manipulation
├── std.string      — String methods, UTF-8, formatting, interning
├── std.collections — List, Map, Set, Deque, sorted collections
├── std.math        — basic math, checked arithmetic
├── std.fs          — file read/write/walk (compiler reads source files)
├── std.env         — args, environment variables, exit codes
├── std.path        — file path manipulation
├── std.test        — test runner, assertions
└── std.fmt         — string formatting internals

Tier 2 — Required to build real applications:
├── std.time        — Duration, Instant, sleep, timers
├── std.net.tcp     — TcpStream, TcpListener, SocketAddr
├── std.net.dns     — DNS resolution
├── std.net.tls     — TLS client (bindings to system TLS or BearSSL)
├── std.sync        — Mutex, Channel, Semaphore, WaitGroup
├── std.task        — spawn, await, structured concurrency
├── std.json        — JSON parse/encode (built-in)
├── std.toml        — TOML parse/encode
├── std.log         — structured logging
├── std.hash        — hashing (SHA-256, xxhash, etc.)
└── std.rand        — random number generation

Tier 3 — Quality of life:
├── std.regex       — regular expressions
├── std.url         — URL parsing
├── std.encoding    — base64, hex, percent-encoding
├── std.compress    — gzip, zstd
└── std.process     — spawn child processes
```

**Note:** The stdlib is initially implemented in Zig (as built-in functions
the compiler knows about) and later rewritten in Forge during self-hosting.
Same approach Go used — the Go stdlib was originally backed by C runtime
functions.

### Stdlib design principles:

```
1. No async coloring. I/O is async-capable by default (like Zig).
   No separate sync and async versions of functions.

2. verb_noun function names: read_file, write_bytes, parse_json.
   Not File::open or BufReader::new.

3. Every public function has a doc comment with at least one example.
   forge doc --check enforces this.

4. Specific, flat error types per module. No Box<dyn Error>.
   Cross-module ? propagation via the Error interface.

5. No feature flags. The stdlib is always complete. No conditional
   compilation for basic functionality.
```

---

## Phase 3: Toolchain

**Goal:** The complete developer (and agent) experience.

### 3.1 Core Tools

```
forge build          — compile to native binary
forge run            — compile and run
forge check          — type check without full compile (fast)
forge check --json   — machine-readable type check output (for agents)
forge test           — run tests
forge test --fuzz    — run fuzz tests
forge bench          — run benchmarks
forge lint           — check conventions and best practices
forge lint --fix     — auto-fix lint violations
forge fmt            — auto-format code (canonical, non-negotiable)
forge doc            — generate documentation
forge doc --check    — verify all public items documented
forge doc search     — search docs by keyword (agents use this)
forge coverage       — line/branch coverage report
forge new <n>     — scaffold new package (includes AGENTS.md)
forge add <dep>      — add a dependency
forge publish        — publish to registry
forge inspect <pkg>  — read any published package's source
```

### 3.2 `forge inspect` — The Agent's Best Friend

The most important tool for AI-assisted development. When an agent needs
to understand how something works, it runs `forge inspect`.

```bash
# Show package structure and metadata
$ forge inspect memcached
  memcached@1.2.0 — A memcached client with connection pooling
  src/
    lib.fg, client.fg, protocol.fg, connection.fg, pool.fg, types.fg
  Author: @sam  |  Downloads: 4,201  |  Coverage: 94%  |  Fuzz: yes

# Show the public API surface (what agents need most)
$ forge inspect memcached --api
  pub fn connect(opts: ConnectOptions = .default) -> Client!
  pub fn Client.get(key: String) -> Value!
  pub fn Client.set(key: String, value: Bytes, ttl: Duration? = .None) -> !
  pub fn Client.delete(key: String) -> Bool!

# Read a specific source file
$ forge inspect memcached --file src/pool.fg

# Show just the types
$ forge inspect memcached --types

# Search across all published packages
$ forge inspect --search "connection pool"
  memcached@1.2 — src/pool.fg (Channel-based pool with semaphore)
  postgres@0.9 — src/pool.fg (Similar pattern, with health checks)
```

### 3.3 `forge lint` — Convention Enforcement

Agents learn conventions by running the linter, not by reading docs.

```
Enforced rules:
├── Fallible functions must return ! (Result type)
├── snake_case for functions/variables, PascalCase for types
├── No unused variables (warn) or imports (error)
├── Config types must use default values (Options pattern)
├── Exhaustive match expressions
├── Public functions must have doc comments
├── Max 4 levels of indentation
├── Imports sorted and grouped (std, external, local)
└── No empty error catches (must handle or explicitly ignore)

Every violation includes a fix suggestion.
forge lint --fix applies them automatically.
```

### 3.4 AGENTS.md — Ships With Every Package

Every `forge new` generates this. Claude Code, Cursor, Copilot — any agent
that reads project files gets the full briefing.

```markdown
# AGENTS.md

## Build & Test Loop
Run these as you work. Fix issues before moving on.
  forge check          — type check (run after every file change)
  forge check --json   — machine-readable output (use if parsing errors)
  forge lint           — convention violations
  forge lint --fix     — auto-fix what it can
  forge test           — run tests
  forge test --fuzz    — fuzz test parsers and protocol code

## Finding Things
Don't guess. Look things up.
  forge doc search "tcp"        — search the standard library
  forge inspect <package>       — read a published package's source
  forge inspect <pkg> --api     — see just the public API
  forge inspect <pkg> --file f  — read a specific source file

## Conventions
`forge lint` catches all of these, but here's the gist:
- Functions that can fail return `T!`. Use `?` to propagate errors.
- There is no panic, no unwrap, no crash. Every failure is a Result.
- Function names: snake_case verb_noun (fetch_user, parse_command)
- Types: PascalCase (HttpClient, ParseError)
- Config: Options types with defaults. No builder pattern.
- Private by default. `pub` to export.
- One file = one module. Directory = namespace.

## Building a New Package
1. `forge inspect <similar_package>` — study how others did it
2. Write types.fg first, run `forge check`, build on top
3. Check after every file — don't batch up errors
4. Tests alongside code, not after
5. Fuzz anything that parses external input
6. `forge lint` before you publish

## Publishing
`forge publish` enforces: type check, lint, tests, fuzz, docs.
Fix what it flags.
```

### 3.5 LSP / Editor Support

```
Language Server Protocol implementation:
├── Autocomplete (type-aware)
├── Go to definition / find references
├── Inline type hints
├── Real-time error diagnostics
├── Quick fixes from lint suggestions
├── Hover documentation
└── Signature help in function calls

Built on the compiler's own parser and type checker (shared code).
This means the LSP is always consistent with forge check.

Priority: HIGH. Agents work in editors. Good LSP = better agent output.
```

---

## Phase 4: Self-Hosting

**Goal:** Rewrite the Forge compiler in Forge. Retire the Zig bootstrap.

This is the most important milestone in the project. When Forge can compile
itself, it proves the language is real.

### 4.1 Strategy

```
Self-hosting roadmap:
│
├── Stage 1: "Can Forge write a compiler?"
│   ├── Write a Forge lexer in Forge (port from Zig)
│   ├── Write a Forge parser in Forge (port from Zig)
│   ├── Compile these with the Zig bootstrap compiler
│   ├── Verify: Forge parser can parse all .fg files
│   └── This stage tests: string handling, collections, pattern matching
│
├── Stage 2: "Can Forge compile itself?"
│   ├── Port the type checker to Forge
│   ├── Port the memory model analysis to Forge
│   ├── Port the code generator to Forge
│   ├── Compile the Forge compiler with itself
│   └── Verify: self-compiled compiler produces identical output to Zig one
│
├── Stage 3: "Retire Zig"
│   ├── Run full test suite against self-hosted compiler
│   ├── Benchmark: self-hosted compiler performance vs Zig bootstrap
│   ├── If perf is acceptable → Zig bootstrap moves to bootstrap/ archive
│   ├── All future compiler development happens in Forge
│   └── CI runs: Forge compiler compiles itself as a regression test
│
└── Stage 4: "Dogfood loop"
    ├── Every compiler improvement is written in Forge
    ├── Every pain point in writing the compiler informs language design
    ├── This is the most valuable feedback loop in the project
    └── "If it's annoying to write a compiler in Forge, fix Forge"
```

### 4.2 What Self-Hosting Teaches Us

Compilers stress-test a language in ways applications don't:

```
Compiler needs → Forge feature validation:
├── Complex data structures (AST)     → tests enums, pattern matching
├── String manipulation (source code) → tests String, formatting
├── Hash maps (symbol tables)         → tests Map, hashing
├── File I/O (reading source)         → tests std.fs, std.io
├── Error handling (diagnostics)      → tests Result, Error, fail
├── Performance (large files)         → tests ARC overhead, allocations
├── Recursive structures (AST trees)  → tests cycle prevention, weak refs
└── Code organization (many modules)  → tests module system, visibility
```

If Forge can comfortably express its own compiler, it can express anything.

### 4.3 Timeline Within Phase 4

```
Month 8  — Port lexer and parser to Forge
Month 9  — Port type checker to Forge
Month 10 — Port codegen, compile self
Month 11 — Full test suite passes, retire Zig bootstrap
```

---

## Phase 5: Package Ecosystem

**Goal:** A registry where packages are discoverable, inspectable, and
trustworthy — for both humans and agents.

### 5.1 Registry (forge.dev)

```
CLI:
├── forge publish     — upload package
├── forge add <pkg>   — add dependency
├── forge search <q>  — full-text search (names, descriptions, APIs)
├── forge inspect     — CLI access to full source and metadata

Web UI (forge.dev/packages/<n>):
├── README (auto-generated if not provided)
├── Full API reference (auto-generated from doc comments)
├── Source browser
├── Test results, coverage, fuzz status
├── Dependency graph
├── Download stats
├── "Used by" reverse dependency list
└── Bug reports / issues
```

### 5.2 Publish Pipeline (Automated, No Human Gate)

```
forge publish runs:
  1. forge check        — no type errors
  2. forge lint         — no convention violations
  3. forge test         — all tests pass
  4. forge test --fuzz  — fuzz passes (if targets exist)
  5. forge coverage     — >= 80% line coverage
  6. forge doc --check  — all public items documented
  7. No duplicate name
  8. Valid forge.toml with license
  9. Package size < 10MB
```

Published immediately on passing. No waiting, no approval queue.

### 5.3 Trust Model

```
Computed automatically from usage signals:
├── new        — < 1 week old, < 100 downloads
├── growing    — > 100 downloads, > 1 week, no unresolved bugs
├── stable     — > 1000 downloads, > 3 months, good track record
└── core       — manually promoted for critical infrastructure

Trust is displayed but doesn't restrict usage. Information, not permission.
```

### 5.4 Bootstrap Packages

Before launch, we build 15-20 essential packages using Claude Code to seed
the ecosystem and dogfood the workflow.

```
Bootstrap packages:
├── http         — HTTP/1.1 client and server
├── redis        — Redis client with pooling and pipelining
├── postgres     — PostgreSQL client
├── sqlite       — SQLite bindings
├── websocket    — WebSocket client and server
├── cli          — command line argument parsing
├── uuid         — UUID generation
├── csv          — CSV parsing
├── retry        — retry with backoff
├── rate-limit   — rate limiting
├── jwt          — JWT encoding/decoding
├── bcrypt       — password hashing
├── template     — string templating
├── cron         — cron scheduling
└── semver       — semantic version parsing
```

Built using Claude Code + AGENTS.md. If the process is painful, we fix
the toolchain. If the agent struggles, we improve AGENTS.md. This is the
quality check before we tell the world "agents work great with Forge."

---

## Phase 6: Community Launch

### 6.1 Documentation

```
forge-lang.dev/tour          — 30-minute interactive language tour
forge-lang.dev/guide         — full guide (types, errors, memory, async)
forge-lang.dev/stdlib        — auto-generated stdlib reference
forge-lang.dev/cookbook       — recipes: HTTP server, CLI tool, Redis client
forge-lang.dev/agents        — using Forge with AI coding agents
forge-lang.dev/contribute    — how to contribute packages
forge-lang.dev/self-hosting  — the story of bootstrapping Forge
```

### 6.2 Launch Strategy

```
Month 11-12: Soft launch
├── Blog post: "Forge: a language for the agent era"
│   ├── The pitch: no panics, no leaks, AI agents love it
│   ├── Demo: Claude Code builds a Redis client in 5 minutes
│   ├── The self-hosting story (Zig → Forge)
│   └── Comparison: same program in Rust/Go/Forge (token count)
├── Share with Zig, Go, Python communities (NOT Rust — avoid the drama)
├── Hacker News, Lobsters, Reddit r/ProgrammingLanguages
└── Short video demos (< 3 min each)

Month 12-13: Community building
├── Discord server
├── GitHub discussions
├── "Package of the week" — showcase agent-built packages
├── "Build X in Forge" tutorial series
└── Track and publicize: time for Claude Code to build package X

Month 14+: Ecosystem growth
├── "Forge 100" — list of 100 packages the ecosystem needs
├── Community members claim and build (with agents)
├── Conference talks (Strange Loop, FOSDEM, local meetups)
├── Podcast appearances (CoRecursive, Changelog, etc.)
└── Partnerships with AI coding tool teams
```

---

## Technical Decisions Summary

| Decision | Choice | Rationale |
|---|---|---|
| Bootstrap language | Zig | Simple, fast, good community, C interop, temporary |
| Self-hosting target | Forge | Dogfood the language, prove it's real, retire Zig |
| Codegen (bootstrap) | C transpilation → Cranelift | C first for speed, Cranelift for real codegen |
| Codegen (self-hosted) | Cranelift, LLVM later | Fast compiles for agents, LLVM for release builds |
| Memory model | ARC + move semantics | Simple mental model, no borrow checker |
| Async model | Built-in, no coloring | No sync/async split |
| Error handling | Result types, `!`, `?`, `fail` | No panics, no exceptions, errors are data |
| Module system | File-based | Simple, obvious, no configuration |
| Generics | Monomorphized, interface-bounded | Performance + readable constraints |
| Agent support | AGENTS.md + CLI tools | Works with any agent, no vendor lock-in |
| Registry | Automated quality gates | Fast publishing, trust from usage |

---

## Timeline

```
PHASE 1: BOOTSTRAP COMPILER (ZIG)
Month 1   — Grammar (EBNF), lexer, parser skeleton
Month 2   — Parser complete, AST, basic type checker
Month 3   — Full type system, generics, interfaces
Month 4   — ARC model, cycle detection, move analysis
Month 5   — C transpilation backend, first programs run
            [MILESTONE: Hello World compiles and runs]

PHASE 2: STANDARD LIBRARY
Month 3-4 — Tier 1 stdlib (io, bytes, string, collections, fs)
Month 5-6 — Tier 1 complete (net.tcp, sync, task, test)
Month 6-7 — Tier 2 stdlib (json, toml, tls, logging)
            [MILESTONE: Can build real networked apps]

PHASE 3: TOOLCHAIN
Month 5   — forge check, build, run, test
Month 6   — forge lint, fmt, doc
Month 7-8 — forge inspect, search, publish
Month 8   — LSP + editor support
            [MILESTONE: Full toolchain operational]

PHASE 4: SELF-HOSTING
Month 8   — Port lexer + parser to Forge
Month 9   — Port type checker to Forge
Month 10  — Port codegen, Forge compiles itself
Month 11  — Full test suite, retire Zig bootstrap
            [MILESTONE: Forge compiles itself]

PHASE 5: PACKAGE ECOSYSTEM
Month 8-9  — Registry (forge.dev) live
Month 9-11 — Bootstrap 15+ packages with Claude Code
             [MILESTONE: Ecosystem seeded and agent-tested]

PHASE 6: LAUNCH
Month 11  — Docs, tour, cookbook complete
Month 12  — Soft launch (blog, demos, community channels)
Month 13  — Public launch, v0.1
Month 14  — Forge 100 challenge, conference talks
            [MILESTONE: Public, growing community]
```

---

## Risks & Mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Zig breaking changes during bootstrap | Delays, rewrite churn | Pin Zig version. Bootstrap is temporary — self-host ASAP. |
| ARC too slow for hot paths | Perf-sensitive users leave | Compiler elision + arena allocators. Benchmark early and often. |
| Self-hosting takes longer than expected | Delays launch | Can launch with Zig bootstrap. Self-hosting is a milestone, not a blocker. |
| Indentation syntax = whitespace wars | Community friction | `forge fmt` is canonical and non-negotiable. |
| "Why not just use Go/Zig?" | Adoption | Go has GC. Zig has no ecosystem. Neither is AI-native. |
| Agent-built packages are low quality | Trust problems | Automated quality gates. Trust earned through usage. |
| Scope creep | Delays everything | Every feature must pass: "Does this make agents more productive?" |
| Cranelift C bindings from Zig are painful | Codegen delays | Start with C transpilation. Cranelift can wait. |

---

## Success Metrics

```
At 5 months (bootstrap working):
  - Forge programs compile and run via Zig bootstrap
  - Tier 1 stdlib complete
  - "forge check" gives helpful errors with fix suggestions

At 8 months (toolchain + self-hosting begins):
  - Full toolchain operational (check, lint, test, fuzz, inspect)
  - Claude Code can build a package from AGENTS.md alone
  - Forge lexer + parser written in Forge and compiling

At 11 months (self-hosted):
  - Forge compiles itself
  - 15+ packages in registry, all built with Claude Code
  - Zig bootstrap retired

At 14 months (launched):
  - Public release, v0.1
  - 50+ packages
  - 500+ GitHub stars
  - Community Discord active
  - Multiple AI agents work well with Forge

At 18 months (traction):
  - 200+ packages
  - 1000+ developers have tried Forge
  - At least one company using Forge in production
  - Self-hosting story published and well-received
```

---

## Where to Start This Week

```
Day 1:
  - Create forge-lang/forge on GitHub
  - Initialize Zig project: zig init
  - Write README with the 1-paragraph pitch
  - Pin Zig version in build.zig.zon

Day 2-3:
  - Write the EBNF grammar (formal, complete)
  - Define token types in lexer.zig
  - Write the lexer (keywords, operators, indentation tracking)
  - 50+ lexer test cases

Day 4-5:
  - Define AST node types in ast.zig
  - Start the parser: expressions, bindings, function definitions
  - 50+ parser test cases with snapshot testing

Day 6-7:
  - Parser: structs, enums, match, if/for
  - Parser: imports, module structure
  - forge parse prints AST for a complete example program
  - Total: 100+ test cases

End of Week 1:
  $ forge parse examples/hello.fg
  (prints a correct AST)

That's the foundation. Everything else builds on this.
```

