# Cranelift Native Backend

The Cranelift backend compiles Forge programs directly to native machine code,
bypassing C transpilation. It produces identical output to the C transpiler on
all 43 deterministic test examples.

## Architecture

```
Forge source (.fg)
  → self-hosted parser (AST text)
  → Cranelift IR text parser
  → two-pass compilation (declare, then define)
  → Cranelift native code generation
  → object file (.o)
  → system linker (gcc)
  → native executable
```

## Status: Feature-Complete for Examples

Both backends produce identical output on all 43 deterministic examples,
covering: structs, enums, match, generics, lambdas/closures, collections
(List/Map/Set), string methods, error propagation (try/fail), concurrency
(spawn/await), JSON/TOML/URL parsing, file I/O, and more.

## Codebase (~18,100 lines Rust)

| Component | Lines | Purpose |
|-----------|-------|---------|
| `cranelift/codegen/src/compiler.rs` | ~4,860 | AST-to-IR compilation, method dispatch |
| `cranelift/codegen/src/lib.rs` | ~2,280 | Runtime function declarations, struct registry |
| `cranelift/codegen/src/parser.rs` | ~1,980 | AST text parser |
| `cranelift/codegen/src/ast.rs` | ~710 | AST node types |
| `cranelift/codegen/src/monomorphize.rs` | ~565 | Generic instantiation |
| `cranelift/runtime/src/lib.rs` | ~3,040 | Core FFI runtime |
| `cranelift/runtime/src/collections/` | ~2,090 | List, Map, Set |
| `cranelift/runtime/src/json.rs` | ~490 | JSON parser (arena-based DOM) |
| `cranelift/runtime/src/toml.rs` | ~290 | TOML parser |
| `cranelift/runtime/src/string.rs` | ~650 | String operations |
| `cranelift/cli/src/main.rs` | ~340 | CLI (build/run/test/check/parse/lex) |

## Comparison with C Transpiler

| Metric | C Transpiler | Cranelift |
|--------|-------------|-----------|
| Examples passing | 43/43 | 43/43 |
| Binary size | ~2.4 MB | ~4.9 MB |
| Compile time | ~90 ms | ~270 ms |
| Execution speed | ~3.5 ms | ~3.9 ms |
| Self-hosts compiler | Yes | Not yet |

Binary size difference is due to static linking of the Rust runtime.
Compile time difference is from the extra AST text parsing step.
Execution speed is effectively identical (dominated by process startup).

## Remaining Work for Self-Hosting

The Cranelift backend cannot yet compile the self-hosted compiler (18 modules,
13,800 lines). Key gaps:

- **Tuple construction** — `(a, b)` syntax not yet parsed/compiled
- **`pub` global linkage** — parsed but not applied (globals always Local)
- **Nested map type inference** — `Map[String, Map[...]]` value kind not propagated

All core self-hosting patterns (struct with List field, for-in over struct
fields, integer-key maps, string methods, error propagation, recursive lookups)
are verified working via `examples/self_host_patterns.fg`.

## Building

```
cargo build --release                           # build the Cranelift backend
./target/release/forge run examples/hello.fg    # compile and run
./target/release/forge build examples/hello.fg  # compile to native binary
```
