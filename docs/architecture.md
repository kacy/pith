# architecture

## compiler map

forge currently has two compiler implementations:

- `self-host/`: the primary implementation for product work
- `bootstrap/`: the zig bootstrap used for initial builds, unit tests, and
  safer structural refactors

both follow the same pipeline:

1. lex source text into tokens
2. parse tokens into an AST
3. type-check and resolve imports
4. transpile to C
5. compile the generated C with `zig cc`

the bootstrap implementation now keeps that flow explicit:

- `bootstrap/main.zig`: process setup, argument parsing, command dispatch
- `bootstrap/cli/`: one file per CLI behavior (`check`, `build`, `test`, etc.)
- `bootstrap/pipeline.zig`: shared source loading, diagnostics, parse, and check setup
- `bootstrap/build_support.zig`: `.forge-build/` layout, runtime header emission, child process helpers

## ownership boundaries

- lexer/parser own syntax-only concerns and should never guess at types
- checker owns name resolution, type resolution, imports, and diagnostics
- codegen assumes checked input and focuses on stable C emission
- CLI modules should only coordinate user-facing flows; they should not duplicate compiler setup

if a change requires repeated lex/parse/check setup, it belongs in `bootstrap/pipeline.zig`.
if a change only affects filesystem output or child-process execution, it belongs in `bootstrap/build_support.zig`.

## change map

### add a token or keyword

- zig bootstrap: `bootstrap/lexer.zig`
- self-hosted compiler: `self-host/lexer.fg`
- if syntax changes: update `docs/grammar.ebnf`

### add syntax

- parser: `bootstrap/parser.zig`, `self-host/parser.fg`
- AST shape: `bootstrap/ast.zig`, `self-host/ast.fg`
- examples/docs: add or update an example under `examples/`

### add or change a type rule

- bootstrap checker: `bootstrap/checker.zig`
- self-hosted checker: `self-host/checker.fg`
- diagnostics reference: `docs/errors.md` if a new stable code is introduced

### add or change code generation

- bootstrap backend: `bootstrap/codegen.zig`
- self-hosted backend: `self-host/codegen.fg`
- runtime support: `runtime/forge_runtime.h` if the emitted C needs new helpers

### change CLI behavior

- bootstrap CLI parsing/dispatch: `bootstrap/main.zig`, `bootstrap/cli/`
- self-hosted CLI: `self-host/forge_main.fg`

## mental model for new contributors

start at the CLI entrypoint, then follow one command end to end:

1. `bootstrap/main.zig`
2. `bootstrap/cli/run_check.zig`
3. `bootstrap/pipeline.zig`
4. `bootstrap/checker.zig`

that path shows most of the compiler lifecycle with minimal generated-output noise.
