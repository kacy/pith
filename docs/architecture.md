# architecture

## compiler map

pith currently has one active compiler pipeline:

- `self-host/`: the Pith frontend, tools, import resolution, checking, and IR emission
- `cranelift/`: the Rust/Cranelift backend, runtime, object generation, and linking

the active pipeline is:

1. lex source text into tokens
2. parse tokens into an AST
3. type-check and resolve imports
4. emit Pith text IR
5. lower text IR to Cranelift IR
6. emit an object file and link it with the Rust runtime using `gcc`

the older Zig bootstrap and C transpiler are historical implementation paths.
they are useful context when reading old notes, but they are not tracked as the
current build/run path.

networking and protocol layers now live mostly in the Pith stdlib. that
includes `std.net.http`, `std.net.websocket`, and the native TLS stack (1.3 with a 1.2 fallback) in
`std.net.tls` / `std.net.tls13`. Rust stays on the lower-level runtime side for
storage, syscall-facing helpers, and the Cranelift backend.

## ownership boundaries

- lexer/parser own syntax-only concerns and should never guess at types
- checker owns name resolution, type resolution, imports, and diagnostics
- IR emission assumes checked input and focuses on stable text IR
- CLI modules should only coordinate user-facing flows; they should not duplicate compiler setup
- stdlib protocol layers should own wire semantics and user-facing behavior;
  lower-level runtime code should stay boring and explicit

if a change requires repeated lex/parse/check setup, it belongs in
`self-host/driver.pith` or the relevant self-hosted tool module.
if a change only affects object output or linking, it belongs in `cranelift/`.

## change map

### add a token or keyword

- self-hosted compiler: `self-host/lexer.pith`
- if syntax changes: update `docs/grammar.ebnf`

### add syntax

- parser: `self-host/parser.pith`
- AST shape: `self-host/ast.pith`
- examples/docs: add or update an example under `examples/`

### add or change a type rule

- self-hosted checker: `self-host/checker.pith`
- diagnostics reference: `docs/errors.md` if a new stable code is introduced

### add or change code generation

- lowering engine (expressions, statements, ownership, function
  lifecycle): `self-host/ir_emitter_core.pith`
- its satellites, each owning one concern: `ir_builder` (output
  buffer and per-function state), `ir_alias_registry` (aliases,
  import renames, global kinds), `ir_struct_registry` and
  `ir_method_tables` (shapes and symbols), `ir_generics`
  (declarations and specializations), `ir_decode_emit` (json/toml/
  config decode calls), `ir_match_emit` (match lowering),
  `ir_call_emit` (call/label wrappers)
- the satellites never import the core back; where one needs an
  expression emitted in place it takes the emitter as an explicit
  fn-value parameter (grep for `emit_expr`)
- IR driver: `self-host/ir_driver.pith`
- Cranelift lowering: `cranelift/codegen/src/ir_consumer.rs` (the ir
  instruction format is documented at the top of that file)
- runtime support: `cranelift/runtime/src/` if native code needs new helpers

### add or change tls or protocol behavior

- Pith stdlib protocol logic: `std/net/tls.pith`, `std/net/tls13.pith`, `std/net/http.pith`, `std/net/websocket.pith`
- crypto helpers used by tls: `std/crypto/*.pith`
- only add Rust runtime support when the stdlib truly needs a new low-level primitive

### change CLI behavior

- self-hosted CLI: `self-host/pith_main.pith`
- native backend CLI: `cranelift/cli/src/main.rs`

### change language server behavior

- server loop and routing: `self-host/lsp_server.pith`
- documents and position conversion: `self-host/lsp_state.pith`
- diagnostics/hover/symbols/formatting: `self-host/lsp_features.pith`
- framing and json-rpc envelope: `std/lsp/transport.pith`, `std/lsp/protocol.pith`

## mental model for new contributors

start at the CLI entrypoint, then follow one command end to end:

1. `cranelift/cli/src/main.rs`
2. `self-host/pith_main.pith`
3. `self-host/driver.pith`
4. `self-host/checker.pith`

that path shows most of the compiler lifecycle with minimal generated-output noise.
