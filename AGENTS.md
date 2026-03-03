# AGENTS.md

## build & test loop

run these as you work. fix issues before moving on.

```
zig build              # compile the compiler
zig build test         # run all tests (~347 tests)
make check             # build + forge check all examples
forge check <file>     # type check a single file
forge check --json <f> # machine-readable diagnostics (for parsing)
forge build <file>     # compile .fg to native binary
forge run <file>       # compile and run
```

## project structure

bootstrap compiler written in zig. pipeline: lexer → parser → checker → codegen (C transpilation).

```
src/
  main.zig           CLI entry point (lex, parse, check, build, run)
  lexer.zig           tokenizer with indentation tracking
  parser.zig          recursive descent parser
  ast.zig             AST node types
  types.zig           type representation and type table
  checker.zig         semantic analysis and type checking
  codegen.zig         C transpilation backend
  errors.zig          error formatting, codes, and suggestions
  printer.zig         AST pretty-printer
  intern.zig          string interning
  forge_runtime.h     C runtime header (embedded via @embedFile)

docs/
  grammar.ebnf        formal EBNF grammar

examples/             .fg programs — all must pass forge check
```

## conventions

- **no panics.** the compiler never panics or crashes. every failure path returns an error.
- **error codes.** every diagnostic has a stable code: E0xx (lexer), E1xx (parser), E2xx (checker).
- **fg_ prefix.** user functions are prefixed `fg_` in generated C to avoid collisions.
- **method keys.** method types use `TypeName.method_name` format in the method_types map.
- **C transpilation.** codegen emits C, compiles with `zig cc`. output goes to `.forge-build/`.
- **string literals** from the lexer include surrounding quotes — codegen strips them.
- **snake_case** for functions/variables, **PascalCase** for types, in both zig and forge.

## testing

- unit tests live alongside source in each `.zig` file
- run `zig build test` — all tests must pass before committing
- `make check` runs `forge check` on every example — ensures no regressions
- after codegen changes, verify examples still compile: `forge run examples/hello.fg`
- 9 examples currently compile to native binaries (hello, functions, control_flow, structs, enums, operators, match, methods, collections)

## working on the compiler

1. read the relevant source file before modifying — understand existing patterns
2. check after every change: `zig build test && make check`
3. keep commits atomic and focused
4. if adding a new diagnostic, assign an error code (see `src/errors.zig`)
5. if adding codegen for a new construct, test with a `.fg` example

## known limitations

- lambdas, concurrency, and type aliases are parsed but not codegen'd
- for loops over collections not implemented in codegen
- generics monomorphization not implemented in codegen
- collection mutation (push, pop, delete) not implemented
