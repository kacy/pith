# AGENTS.md

## build & test loop

run these as you work. fix issues before moving on.

```
zig build              # compile the bootstrap compiler
zig build test         # run all tests (~360 tests)
make check             # build + forge check all examples
forge check <file>     # type check a single file
forge check --json <f> # machine-readable diagnostics (for parsing)
forge build <file>     # compile .fg to native binary
forge run <file>       # compile and run
forge test <file>      # run test declarations
forge fmt <file>       # format source code
forge lint <file>      # check conventions
```

## self-hosted compiler

the compiler is self-hosting — forge compiles itself. use the self-hosted
CLI for build/run/test without the zig bootstrap:

```
make self-host                            # build the self-hosted compiler
self-host/forge_main build <file>         # compile to native binary
self-host/forge_main run <file>           # compile and execute
self-host/forge_main test <file>          # compile and run tests
```

codegen-only (outputs C to stdout):

```
forge run self-host/codegen_main.fg <file>        # emit C code
forge run self-host/codegen_main.fg -- --test <f>  # emit C with test runner
```

## project structure

bootstrap compiler in zig, self-hosted compiler in forge.
pipeline: lexer → parser → checker → codegen (C transpilation).

```
src/
  main.zig           CLI entry point (lex, parse, check, build, run, test, fmt, lint)
  lexer.zig          tokenizer with indentation tracking
  parser.zig         recursive descent parser
  ast.zig            AST node types
  types.zig          type representation and type table
  checker.zig        semantic analysis and type checking
  codegen.zig        C transpilation backend
  forge_runtime.h    C runtime header (embedded via @embedFile)
  formatter.zig      source code formatter
  lint.zig           convention linter
  errors.zig         error formatting, codes, and suggestions
  printer.zig        AST pretty-printer
  intern.zig         string interning

self-host/
  forge_main.fg      CLI entry point — build/run/test
  codegen_main.fg    codegen entry point — emits C to stdout
  lexer.fg           tokenizer (port of lexer.zig)
  parser.fg          recursive descent parser (port of parser.zig)
  ast.fg             AST node representation
  printer.fg         AST pretty-printer
  checker.fg         type checker
  types.fg           type representation
  scope.fg           scope management
  codegen.fg         C transpilation backend (~4,000 lines)

examples/            21 .fg programs — all compile to native binaries
docs/grammar.ebnf    complete EBNF for the language
docs/errors.md       error code reference (E0xx–E3xx)
```

## conventions

- **no panics.** the compiler never panics or crashes. every failure path returns an error.
- **error codes.** every diagnostic has a stable code: E0xx (lexer), E1xx (parser), E2xx (checker), E3xx (lint).
- **fg_ prefix.** user functions are prefixed `fg_` in generated C to avoid collisions.
- **g_ prefix.** self-hosted codegen uses `g_` prefix for globals (flat C namespace).
- **method keys.** method types use `TypeName.method_name` format in the method_types map.
- **C transpilation.** codegen emits C, compiles with `zig cc`. output goes to `.forge-build/`.
- **string literals** from the lexer include surrounding quotes — codegen strips them.
- **snake_case** for functions/variables, **PascalCase** for types, in both zig and forge.
- **closures.** all `fn(X) -> Y` parameters use `forge_closure_t` (uniform closure ABI).

## testing

- unit tests live alongside source in each `.zig` file
- run `zig build test` — all tests must pass before committing
- `make check` runs `forge check` on every example — ensures no regressions
- after codegen changes, verify examples still compile: `forge run examples/hello.fg`
- all 21 examples compile to native binaries (including closures, generics, collections, etc.)

## working on the compiler

1. read the relevant source file before modifying — understand existing patterns
2. check after every change: `zig build test && make check`
3. keep commits atomic and focused
4. if adding a new diagnostic, assign an error code (see `src/errors.zig`)
5. if adding codegen for a new construct, test with a `.fg` example
6. mirror changes in both bootstrap (src/) and self-hosted (self-host/) compilers

## known limitations

- concurrency (spawn/await) parsed but not codegen'd
- type aliases parsed but not codegen'd
- collections passed to functions are copies — mutations don't propagate back
- `{`/`}` in string literals trigger interpolation — use `chr(123)`/`chr(125)`
