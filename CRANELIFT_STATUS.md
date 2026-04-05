# Cranelift Native Backend

The Cranelift backend compiles Forge programs directly to native machine code
via a self-hosted IR emitter. The pipeline is fully self-hosted on the frontend
(lex/parse/check/emit_ir in Forge), with Rust handling IR consumption and
native codegen.

## Architecture

```
Forge source (.fg)
  → self-hosted IR emitter (ir_emitter.fg → text IR)
  → ir_consumer.rs (text IR → Cranelift IR)
  → Cranelift native code generation
  → object file (.o)
  → system linker (gcc)
  → native executable
```

## Status: 90/90 Deterministic Examples Passing

The current self-hosted frontend plus Cranelift backend passes all 90
deterministic expected-output examples in this repository.

Two examples remain environment-dependent rather than compiler-dependent:
`net_basics` and `net_echo`. In this sandbox they do not provide a stable
socket/DNS signal, so they are excluded from the deterministic pass count.

The deterministic suite covers:
structs, enums, match, generics, lambdas/closures, collections (List/Map/Set),
string methods, error propagation (try/fail), concurrency (spawn/await),
JSON/TOML/URL parsing, file I/O, path/process helpers, and more.

## Codebase (~11,000 lines Rust)

| Component | Lines | Purpose |
|-----------|-------|---------|
| `cranelift/runtime/src/lib.rs` | ~3,240 | Core FFI runtime |
| `cranelift/runtime/src/collections/` | ~2,405 | List, Map, Set |
| `cranelift/codegen/src/ir_consumer.rs` | ~1,970 | Text IR → Cranelift IR |
| `cranelift/runtime/src/string.rs` | ~650 | String operations |
| `cranelift/cli/src/main.rs` | ~630 | CLI (build/run/check/parse/lex) |
| `cranelift/codegen/src/lib.rs` | ~545 | Runtime function declarations, struct registry |
| `cranelift/runtime/src/json.rs` | ~490 | JSON parser (arena-based DOM) |
| `cranelift/runtime/src/toml.rs` | ~290 | TOML parser |
| `cranelift/codegen/src/linker.rs` | ~125 | Object file linking |

## Self-Hosting Status: Complete

The Cranelift backend compiles the entire self-hosted compiler into a working
native binary. The self-hosted compiler plus stdlib source surface is now well
past 19k lines, with the frontend and most language logic already living in
Forge rather than Rust.

**Verified:**
- `forge version`, `lex`, `parse`, `check` — all work
- `forge build` / `forge run` — compiles and executes all 53 examples
- Fixed-point reached: C output is byte-for-byte identical whether the
  compiler was compiled via C transpilation or Cranelift (837,451 bytes)

## Building

```
cargo build --release                           # build the Cranelift backend
./target/release/forge run examples/hello.fg    # compile and run
./target/release/forge build examples/hello.fg  # compile to native binary
```
