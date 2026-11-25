# Architecture

## Compiler map

Pith currently has one active compiler pipeline:

- `self-host/`: the Pith frontend, tools, import resolution, checking, and IR emission
- `cranelift/`: the Rust/Cranelift backend, runtime, object generation, and linking

The active pipeline is:

1. Lex source text into tokens
2. Parse tokens into an AST
3. Type-check and resolve imports
4. Emit Pith text IR
5. Lower text IR to Cranelift IR
6. Emit an object file and link it with the Rust runtime using the system C compiler (`$CC`, defaulting to `cc`)

End to end, that is:

```
pith source (.pith)
  -> self-hosted frontend: lex, parse, check
  -> self-hosted ir emitter (ir_emitter_core.pith and satellites) -> text ir
  -> ir_consumer.rs (text ir -> cranelift ir)
  -> cranelift native code generation
  -> object file (.o)
  -> system linker ($CC / cc)
  -> native executable
```

Steps 1 through 4 are Pith compiling Pith: the frontend builds itself.
`make bootstrap` rebuilds the compiler with the Cranelift-compiled version of
itself, and `make bootstrap-ir-checks` verifies the IR contract, its
invariants, and that the emitted IR reaches a fixed point on the deterministic
corpus.

Networking and protocol layers now live mostly in the Pith stdlib. That
includes `std.net.http`, `std.net.websocket`, and the native TLS stack (1.3 with a 1.2 fallback) in
`std.net.tls` / `std.net.tls13` / `std.net.tls12`. Rust stays on the lower-level runtime side for
storage, syscall-facing helpers, and the Cranelift backend.

## Ownership boundaries

- The lexer and parser own syntax-only concerns and should never guess at types
- The checker owns name resolution, type resolution, imports, and diagnostics
- IR emission assumes checked input and focuses on stable text IR
- CLI modules should only coordinate user-facing flows; they should not duplicate compiler setup
- Stdlib protocol layers should own wire semantics and user-facing behavior;
  lower-level runtime code should stay boring and explicit

If a change requires repeated lex/parse/check setup, it belongs in
`self-host/driver.pith` or the relevant self-hosted tool module.
If a change only affects object output or linking, it belongs in `cranelift/`.

## Change map

### Add a token or keyword

- The lexer: `std/pith/lexer.pith`. The compiler reads it through
  `self-host/lexer.pith`, and `std/pith/highlight.pith` colors code from it,
  so a new token kind may need a class there too
- If syntax changes: update `docs/grammar.ebnf`

### Add syntax

- Parser: `self-host/parser.pith`
- AST shape: `self-host/ast.pith`
- Examples and docs: add or update an example under `examples/`

### Add or change a type rule

- Self-hosted checker: `self-host/checker.pith`
- Diagnostics reference: `docs/errors.md` if a new stable code is introduced

### Add or change code generation

- Lowering engine (expressions, statements, ownership, function
  lifecycle): `self-host/ir_emitter_core.pith`
- Its satellites, each owning one concern, include `ir_builder` (output
  buffer and per-function state), `ir_alias_registry` (aliases,
  import renames, global kinds), `ir_struct_registry` and
  `ir_method_tables` (shapes and symbols), `ir_generics`
  (declarations and specializations), `ir_decode_emit` (JSON/TOML/config
  decode calls), `ir_match_emit` (match lowering), and
  `ir_call_emit` (call/label wrappers)
- The satellites never import the core back. Where one needs an
  expression emitted in place, it takes the emitter as an explicit
  fn-value parameter (grep for `emit_expr`)
- IR driver: `self-host/ir_driver.pith`
- Cranelift lowering: `cranelift/codegen/src/ir_consumer.rs` (the IR
  instruction format is specified in [ir-contract.md](ir-contract.md), which
  is authoritative for both sides)
- Runtime support: `cranelift/runtime/src/` if native code needs new helpers

### Add or change TLS or protocol behavior

- Pith stdlib protocol logic: `std/net/tls.pith`, `std/net/tls13.pith`, `std/net/tls12.pith`, `std/net/http.pith`, `std/net/http2/*.pith`, `std/net/grpc.pith`, `std/net/websocket.pith`
- Crypto helpers used by TLS: `std/crypto/*.pith`
- Add Rust runtime support only when the stdlib truly needs a new low-level primitive

### Change CLI behavior

- Self-hosted CLI: `self-host/pith_main.pith`
- Native backend CLI: `cranelift/cli/src/main.rs`

### Change language server behavior

- Server loop and routing: `self-host/lsp_server.pith`
- Documents and position conversion: `self-host/lsp_state.pith`
- Diagnostics, hover, symbols, formatting, code actions: `self-host/lsp_features.pith`
- Definition, references, completion, rename: `self-host/lsp_navigation.pith`
- Semantic tokens, signature help, inlay hints: `self-host/lsp_tokens.pith`
- Framing and JSON-RPC envelope: `std/lsp/transport.pith`, `std/lsp/protocol.pith`

## Mental model for new contributors

Start at the CLI entry point, then follow one command end to end:

1. `cranelift/cli/src/main.rs`
2. `self-host/pith_main.pith`
3. `self-host/driver.pith`
4. `self-host/checker.pith`

That path shows most of the compiler lifecycle with minimal generated-output noise.
