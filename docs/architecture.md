# architecture

## compiler map

pith currently has two compiler implementations:

- `self-host/`: the primary implementation for product work
- `bootstrap/`: the zig bootstrap used for initial builds, unit tests, and
  safer structural refactors

both follow the same pipeline:

1. lex source text into tokens
2. parse tokens into an AST
3. type-check and resolve imports
4. transpile to C
5. compile the generated C with `zig cc`

networking and protocol layers now live mostly in the Pith stdlib. that
includes `std.net.http`, `std.net.websocket`, and the native TLS 1.3 stack in
`std.net.tls` / `std.net.tls13`. Rust stays on the lower-level runtime side for
storage, syscall-facing helpers, and the Cranelift backend.

the bootstrap implementation now keeps that flow explicit:

- `bootstrap/main.zig`: process setup, argument parsing, command dispatch
- `bootstrap/cli/`: one file per CLI behavior (`check`, `build`, `test`, etc.)
- `bootstrap/pipeline.zig`: shared source loading, diagnostics, parse, and check setup
- `bootstrap/build_support.zig`: `.pith-build/` layout, runtime header emission, child process helpers

## ownership boundaries

- lexer/parser own syntax-only concerns and should never guess at types
- checker owns name resolution, type resolution, imports, and diagnostics
- codegen assumes checked input and focuses on stable C emission
- CLI modules should only coordinate user-facing flows; they should not duplicate compiler setup
- stdlib protocol layers should own wire semantics and user-facing behavior;
  lower-level runtime code should stay boring and explicit

if a change requires repeated lex/parse/check setup, it belongs in `bootstrap/pipeline.zig`.
if a change only affects filesystem output or child-process execution, it belongs in `bootstrap/build_support.zig`.

## change map

### add a token or keyword

- zig bootstrap: `bootstrap/lexer.zig`
- self-hosted compiler: `self-host/lexer.pith`
- if syntax changes: update `docs/grammar.ebnf`

### add syntax

- parser: `bootstrap/parser.zig`, `self-host/parser.pith`
- AST shape: `bootstrap/ast.zig`, `self-host/ast.pith`
- examples/docs: add or update an example under `examples/`

### add or change a type rule

- bootstrap checker: `bootstrap/checker.zig`
- self-hosted checker: `self-host/checker.pith`
- diagnostics reference: `docs/errors.md` if a new stable code is introduced

### add or change code generation

- bootstrap backend: `bootstrap/codegen.zig`
- self-hosted backend: `self-host/codegen.pith`
- runtime support: `runtime/pith_runtime.h` if the emitted C needs new helpers

### add or change tls or protocol behavior

- Pith stdlib protocol logic: `std/net/tls.pith`, `std/net/tls13.pith`, `std/net/http.pith`, `std/net/websocket.pith`
- crypto helpers used by tls: `std/crypto/*.pith`
- only add Rust runtime support when the stdlib truly needs a new low-level primitive

### change CLI behavior

- bootstrap CLI parsing/dispatch: `bootstrap/main.zig`, `bootstrap/cli/`
- self-hosted CLI: `self-host/pith_main.pith`

## mental model for new contributors

start at the CLI entrypoint, then follow one command end to end:

1. `bootstrap/main.zig`
2. `bootstrap/cli/run_check.zig`
3. `bootstrap/pipeline.zig`
4. `bootstrap/checker.zig`

that path shows most of the compiler lifecycle with minimal generated-output noise.
