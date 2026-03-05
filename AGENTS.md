# AGENTS.md

## build & test loop

the self-hosted compiler is the primary tool. run these as you work:

```
./self-host/forge_main build <file>    # compile .fg to native binary
./self-host/forge_main run <file>      # compile and run
./self-host/forge_main test <file>     # run test declarations
./self-host/forge_main check <file>    # type check (human-readable)
./self-host/forge_main check --json <f> # machine-readable diagnostics
./self-host/forge_main fmt <file>      # format source code
./self-host/forge_main fmt --check <f> # check formatting
./self-host/forge_main lint <file>     # check conventions
./self-host/forge_main lint --json <f> # lint (JSON output)
./self-host/forge_main lex <file>      # print token stream
./self-host/forge_main parse <file>    # print AST
```

## bootstrapping

the compiler is self-hosting — forge compiles itself.

```
make self-host           # build self-hosted compiler (requires zig bootstrap)
make bootstrap           # rebuild self-hosted compiler using itself
make bootstrap-verify    # verify fixed-point (C output identical)
make run-examples-self   # run all examples through self-hosted compiler
```

the zig bootstrap (`zig build`) is only needed for the initial build or
running zig-level unit tests. after `make self-host`, the self-hosted
compiler can rebuild itself indefinitely.

## project structure

```
self-host/
  forge_main.fg      unified CLI — build/run/test/check/fmt/lint/lex/parse
  codegen_main.fg    codegen entry point — emits C to stdout
  lexer.fg           tokenizer with indentation tracking
  parser.fg          recursive descent parser
  ast.fg             AST node representation
  printer.fg         AST pretty-printer
  checker.fg         type checker (~2,700 lines)
  types.fg           type representation
  scope.fg           scope management
  codegen.fg         C transpilation backend (~4,000 lines)
  driver.fg          compile pipeline and import resolution
  formatter.fg       source code formatter
  linter.fg          convention linter
  errors.fg          human-readable error rendering

runtime/
  forge_runtime.h    C runtime header — memory, strings, collections, I/O

bootstrap/           zig bootstrap compiler (archived, for reference)
  main.zig, lexer.zig, parser.zig, checker.zig, codegen.zig, etc.

examples/            40+ .fg programs — all compile to native binaries
docs/grammar.ebnf    complete EBNF for the language
docs/errors.md       error code reference (E0xx–E3xx)
```

## conventions

- **no panics.** the compiler never panics or crashes. every failure path returns an error.
- **error codes.** every diagnostic has a stable code: E0xx (lexer), E1xx (parser), E2xx (checker), E3xx (lint).
- **fg_ prefix.** user functions are prefixed `fg_` in generated C to avoid collisions.
- **g_ prefix.** self-hosted codegen uses `g_` prefix for globals (flat C namespace).
- **module prefixes.** all modules prefix private functions to avoid C namespace collisions
  (e.g., `f_` for formatter, `l_` for linter, `c_` for checker, `e_` for errors).
- **method keys.** method types use `TypeName.method_name` format in the method_types map.
- **C transpilation.** codegen emits C, compiles with `zig cc`. output goes to `.forge-build/`.
- **string literals** from the lexer include surrounding quotes — codegen strips them.
- **snake_case** for functions/variables, **PascalCase** for types.
- **closures.** all `fn(X) -> Y` parameters use `forge_closure_t` (uniform closure ABI).
- **concurrency.** `spawn`, `await`, `Task[T]`, `Mutex`, `WaitGroup`, `Semaphore` — all fully implemented.

## testing

- `make run-examples-self` runs all examples through the self-hosted compiler
- `make bootstrap-verify` checks that the compiler reaches a fixed point
- `zig build test` runs the zig bootstrap's ~360 unit tests
- `make check` runs `forge check` on every example
- after codegen changes, verify examples still compile: `./self-host/forge_main run examples/hello.fg`

## working on the compiler

1. read the relevant source file before modifying — understand existing patterns
2. check after every change: `./self-host/forge_main check <file>`
3. keep commits atomic and focused
4. if adding a new diagnostic, assign an error code (see `docs/errors.md`)
5. if adding codegen for a new construct, test with a `.fg` example
6. use `make bootstrap` to verify the compiler can still build itself

## known limitations

- collections passed to functions are copies — mutations don't propagate back
- `{`/`}` in string literals trigger interpolation — use `chr(123)`/`chr(125)`
- `for c in string` not supported — use `while i < s.len(): s[i]`
- methods.fg and concurrency.fg have known codegen issues with return type inference
