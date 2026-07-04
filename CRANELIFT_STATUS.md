# Cranelift Native Backend

The Cranelift backend compiles Pith programs directly to native machine code
via a self-hosted IR emitter. The pipeline is fully self-hosted on the frontend
(lex/parse/check/emit_ir in Pith), with Rust handling IR consumption and
native codegen.

## Architecture

```
Pith source (.pith)
  → self-hosted IR emitter (ir_emitter.pith → text IR)
  → ir_consumer.rs (text IR → Cranelift IR)
  → Cranelift native code generation
  → object file (.o)
  → system linker (gcc)
  → native executable
```

## status

the current self-hosted frontend plus Cranelift backend handles the tracked
deterministic example suite and the native tls stack used by the live tls echo
coverage in this repo.

some networking examples remain environment-dependent rather than
compiler-dependent. those are still better treated as live probes than as
portable deterministic signals.

The deterministic suite covers:
structs, enums, match, generics, lambdas/closures, collections (List/Map/Set),
string methods, error propagation (try/fail), concurrency (spawn/await),
JSON/TOML/URL parsing, file I/O, path/process helpers, and more.

## codebase (~11,200 lines Rust)

counts drift as the code moves; `make status-audit` prints the live totals.

| Component | Lines | Purpose |
|-----------|-------|---------|
| `cranelift/runtime/src/` | 8,386 | runtime storage, ARC, collections, OS/IO, crypto helpers |
| `cranelift/codegen/src/` | 2,172 | text IR → Cranelift lowering and link support |
| `cranelift/cli/src/` | 517 | CLI (build/run/check/parse/lex) |
| `cranelift/codegen/src/ir_consumer.rs` | 1,779 | text IR → Cranelift IR |
| `cranelift/runtime/src/collections/list.rs` | 835 | list runtime |
| `cranelift/runtime/src/collections/map.rs` | 767 | map runtime |
| `cranelift/runtime/src/host_fs.rs` | 474 | file and host filesystem helpers |
| `cranelift/runtime/src/runtime_core.rs` | 666 | core runtime glue |
| `cranelift/runtime/src/string_list.rs` | 428 | string list helpers |
| `cranelift/runtime/src/crypto.rs` | 396 | AEAD, x25519, signature, and TLS-facing crypto kernels |

## Self-Hosting Status: Complete

The Cranelift backend compiles the entire self-hosted compiler into a working
native binary. The self-hosted compiler plus stdlib source surface is now well
past 40k lines, with the frontend and most language logic already living in
Pith rather than Rust.

**Verified:**
- `pith version`, `lex`, `parse`, `check` — all work
- `pith build` / `pith run` — compiles and executes the tracked example suite
- `std.net.tls` now owns both client and server TLS 1.3 handshakes in Pith
- Fixed-point reached: the self-hosted frontend and Cranelift backend rebuild
  the compiler and pass the tracked example, stdlib, and regression suites

## Building

```
cargo build --release                           # build the Cranelift backend
./target/release/pith run examples/hello.pith    # compile and run
./target/release/pith build examples/hello.pith  # compile to native binary
```
