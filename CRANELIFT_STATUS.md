# Cranelift Native Backend

The Cranelift backend compiles Pith programs directly to native machine code
via a self-hosted IR emitter. The pipeline is fully self-hosted on the frontend
(lex/parse/check/emit_ir in Pith), with Rust handling IR consumption and
native codegen.

## Architecture

```
Pith source (.pith)
  → self-hosted IR emitter (ir_emitter_core.pith and satellites → text IR)
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

## codebase

`make status-audit` prints the live line counts for `self-host/`, `std/`, and
`cranelift/`, separating library code from test code and excluding comments and
blank lines. Per-file sizes are a `wc -l` away. Figures are deliberately not
copied here: the table that used to sit in this section had drifted to roughly
half the real numbers, and nothing catches that.

What lives where:

| Component | Purpose |
|-----------|---------|
| `cranelift/runtime/src/` | runtime storage, ARC, collections, OS/IO, crypto helpers |
| `cranelift/codegen/src/` | text IR to Cranelift lowering and link support |
| `cranelift/cli/src/` | CLI (build/run/check/parse/lex) |
| `cranelift/codegen/src/ir_consumer.rs` | text IR to Cranelift IR |
| `cranelift/runtime/src/collections/` | list, map, and set runtimes |
| `cranelift/runtime/src/host_fs.rs` | file and host filesystem helpers |
| `cranelift/runtime/src/runtime_core.rs` | core runtime glue |
| `cranelift/runtime/src/crypto.rs` | AEAD, x25519, signature, and TLS-facing crypto kernels |

## Self-Hosting Status: Complete

The Cranelift backend compiles the entire self-hosted compiler into a working
native binary. The self-hosted compiler plus stdlib comes to roughly 65,000
lines of library code, with the frontend and most language logic already living
in Pith rather than Rust.

**Verified:**
- `pith version`, `lex`, `parse`, `check` — all work
- `pith build` / `pith run` — compiles and executes the tracked example suite
- `std.net.tls` owns the client and server handshakes for TLS 1.3 and the
  TLS 1.2 fallback, in Pith
- Fixed-point reached: the self-hosted frontend and Cranelift backend rebuild
  the compiler and pass the tracked example, stdlib, and regression suites

## Building

```
cargo build --release                           # build the Cranelift backend
./target/release/pith run examples/hello.pith    # compile and run
./target/release/pith build examples/hello.pith  # compile to native binary
```
