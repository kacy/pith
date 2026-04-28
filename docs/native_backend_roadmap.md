# Native Backend Roadmap

This document turns the current Cranelift backend analysis into an execution
plan: what should stay Rust, what should move into Pith, and which language
features would make the examples read like a finished language instead of
backend demos.

## Current Inventory

Update: the first round of IR hardening has landed. Combined IR now carries
explicit call return kinds and field metadata, and CI checks those contracts
with `make bootstrap-ir-checks-only`. The remaining work is to make that
validated path the only native build path and keep deleting Rust-side frontend
policy.

Source totals:

- `self-host/*.pith`: about 22,540 lines
- `std/**/*.pith`: about 25,330 lines
- `cranelift/**/*.rs`: about 10,650 tracked lines

Rust breakdown:

- runtime: 8,040 lines
- codegen: 1,937 lines
- CLI: 491 lines

Largest Rust files:

- `cranelift/codegen/src/ir_consumer.rs`: 1,568
- `cranelift/runtime/src/collections/list.rs`: 952
- `cranelift/runtime/src/collections/map.rs`: 719
- `cranelift/runtime/src/host_fs.rs`: 609
- `cranelift/runtime/src/runtime_core.rs`: 572
- `cranelift/runtime/src/string_list.rs`: 555
- `cranelift/runtime/src/collections/set.rs`: 486

## Keep In Rust

These should remain Rust for the foreseeable future:

- Cranelift lowering and machine-code generation
- object emission and linker integration
- ARC and heap object management
- string storage and FFI representation
- list/map/set storage
- OS, process, DNS, and socket syscalls

The desired end state is not zero Rust. It is a thin Rust backend/runtime with
the language, compiler policy, and most standard-library logic living in Pith.

That line is even more true now that TLS has moved out of a Rust runtime module
and into `std.net.tls`, `std.net.tls13`, and the Pith-side crypto helpers that
support them.

## Migration Targets

### Phase 1: Delete Rust-side Backend Guessing

Status: mostly done. The IR emitter now writes explicit call return kinds, and
field instructions carry enough type metadata for the backend to stop guessing
common struct and string cases. The next step is enforcement: native builds
should always ask the IR driver to validate the combined contract.

Target files:

- `cranelift/codegen/src/ir_consumer.rs`
- `cranelift/codegen/src/lib.rs`

Delete candidates:

- metadata inference block: 359 lines
  - `ir_consumer.rs:1317-1675`
- symbol alias table block: about 205 lines
  - `ir_consumer.rs:1676-1880`

Why:

Rust currently infers:

- whether a call returns a string
- whether a call returns a struct
- which fields are string fields
- which runtime symbol a Pith name should map to
- fallback field offsets when type identity is incomplete

This is the source of a large share of backend bugs. Pith should emit this
metadata explicitly.

Required additions:

- IR should carry exact callee symbol names
- IR should carry exact return kinds
- IR should carry exact struct-return identities
- field access should carry exact field index or offset
- import lowering should resolve aliases before Rust sees them

Acceptance criteria:

- `ir_consumer.rs` no longer contains return-type inference loops
- most symbol dispatch becomes direct rather than heuristic
- backend bugs stop looking like “Rust guessed the wrong type”
- native build/run/test uses validated combined IR by default

### Phase 2: Move CLI Orchestration Into Pith

Target file:

- `cranelift/cli/src/main.rs`

Chunk sizes:

- dispatch: 67 lines
- usage/env handling: 28 lines
- frontend lookup plus parse/lex delegation: 67 lines
- IR-driver rename/import handling: 286 lines
- build/run/check wrappers: 166 lines

Why:

The Rust CLI is doing frontend policy work:

- finding `self-host/pith_main`
- scanning `from ... import ...`
- resolving module paths
- maintaining a hardcoded builtin-module skip list
- renaming globals and string IDs across imported IR

That logic belongs with:

- `self-host/pith_main.pith`
- `self-host/driver.pith`
- eventually a Pith-native backend driver layer

Desired end state:

Rust CLI becomes a thin shell around:

- read IR
- declare runtime imports
- lower with Cranelift
- link or run

Acceptance criteria:

- import graph walking lives in Pith
- builtin-module policy is expressed in Pith, not Rust
- `main.rs` shrinks toward a thin backend wrapper

### Phase 3: Remove Duplicated High-level Runtime Logic

Target files:

- `cranelift/runtime/src/json.rs`
- `cranelift/runtime/src/toml.rs`
- `cranelift/runtime/src/lib.rs`

Obvious duplication:

- Pith JSON: `std/json.pith` (510 lines)
- Rust JSON: `cranelift/runtime/src/json.rs` (488 lines)
- Pith TOML: `std/toml.pith` (519 lines)
- Rust TOML: `cranelift/runtime/src/toml.rs` (290 lines)
- Pith URL/path:
  - `std/net/url.pith` (260 lines)
  - `std/os/path.pith` (81 lines)
- Rust URL/path/smart glue:
  - `cranelift/runtime/src/lib.rs:2163-3233` (1,071 lines)

Why:

The current split duplicates ownership and creates contract drift:

- JSON exists in both Rust and Pith
- TOML exists in both Rust and Pith
- URL/path helpers exist in both Rust and Pith
- “smart” helpers paper over representation mismatches rather than fixing them

Recommended ownership model:

- Rust runtime owns primitive FFI and storage
- Pith stdlib owns parsing, formatting, and user-facing APIs

Near-term deletion targets inside `runtime/src/lib.rs`:

- `pith_json_parse` smart fallback behavior
- `pith_smart_to_string`
- `pith_smart_encode`
- `pith_url_*` convenience layer, if Pith stdlib owns URL semantics
- path helpers that are better modeled as stdlib wrappers over smaller intrinsics
- obvious stubs like process/channel placeholders when they are not part of the
  intended long-term contract

Acceptance criteria:

- one implementation per feature
- no hidden “smart” type heuristics at runtime
- runtime contracts become explicit and boring

### Phase 4: Collapse Runtime Import Tables

Target file:

- `cranelift/codegen/src/lib.rs`

Why:

Once Phase 1 and Phase 3 land, the backend no longer needs a large pile of
symbol aliases for high-level stdlib functions. The runtime import table should
mostly describe real low-level intrinsics.

Acceptance criteria:

- fewer aliases
- fewer Pith names with Rust-side special cases
- more one-to-one mappings between emitted IR and imported runtime symbols

## Recommended Execution Order

Use this order. It minimizes churn and avoids deleting infrastructure before
contracts are explicit.

1. Make IR metadata explicit and delete Rust inference
2. Move import graph and IR orchestration into Pith
3. Remove duplicated JSON/TOML/URL/path ownership
4. Shrink runtime import tables

This order matters. Phases 2 and 3 are much easier once Rust no longer has to
guess what the frontend meant.

## Example Ergonomics: What To Add

The example corpus shows the biggest quality gaps directly.

Observed frequency in `examples/*.pith`:

- `print(...)`: 785
- string concatenations with `+`: 653
- `.to_string()`: 165
- interpolation sites: 291
- manual `while i < list.len()` loops: 20

Several examples now import `std.fmt`, but many examples still build strings
manually.

### Highest-value language/library additions

#### 1. Better Formatting

Examples like `url_ops`, `data_pipeline`, and `strings_demo` are dominated by:

- `"label: " + value`
- `x.to_string()`
- nested concatenation for mixed values

Needed:

- stronger interpolation
- a real `format(...)`
- or both

Goal:

- `print("port: {port(full_url)}")`
- `print("overall best: {best.name} ({best.score})")`

#### 2. Generic Display / Show

Examples still write bespoke renderers for common data:

- matrix pretty-printers
- list-to-string helpers
- manual struct field formatting

Needed:

- built-in display for lists, maps, sets, tuples, and structs
- or a trait/interface-based `show`

Goal:

- `print(matrix)`
- `print(records)`
- `print(counts)`

#### 3. Better Collection Combinators

The examples want `map`, `filter`, and `reduce`, but today that path is still
a feature gap rather than a polished standard capability.

Needed:

- reliable higher-order collection operations
- closure support that is pleasant on the hot path
- consistent typing and backend lowering for generic collection transforms

#### 4. Better Iteration

Many examples still use:

- `while i < list.len():`
- explicit indexing
- manual accumulation loops

Needed:

- `for i, x in list`
- `for k, v in map`
- better `enumerate`
- cleaner range and slice iteration

#### 5. Better Test Ergonomics

The example suite leans heavily on golden stdout output. That is good for
end-to-end coverage, but it is not the nicest surface for language examples.

Needed:

- `assert_eq`
- `assert_ne`
- maybe lightweight snapshot helpers

This would make examples shorter and more semantic.

#### 6. String Builder / Buffer

Parsers, encoders, printers, and many examples would benefit from a mutable
append-oriented string buffer instead of repeated concatenation.

## Which Examples Improve First

These would benefit immediately from the additions above:

- `examples/url_ops.pith`
- `examples/data_pipeline.pith`
- `examples/strings_demo.pith`
- `examples/matrix_math.pith`
- `examples/json_ops.pith`
- `examples/toml_ops.pith`

The common pattern is not “missing power”. It is “too much ceremony”.

## Immediate Next Steps

If work begins now, the first concrete implementation steps should be:

1. Use validated combined IR for native build/run/test
2. Simplify `ir_consumer.rs` further now that it can trust emitted metadata
3. Move the remaining CLI orchestration policy out of `cranelift/cli/src/main.rs`
4. Decide whether JSON/TOML/URL ownership belongs primarily in Pith stdlib or
   Rust runtime, then delete the duplicate side
5. Keep adding small formatting, display, and collection helpers that let
   examples read like normal application code

## Proposed IR Contract For Phase 1

The fastest way to delete Rust-side guessing is to make the self-hosted emitter
tell the backend exactly what it already knows.

### Current problem

Today the Rust backend tries to recover facts like:

- “does this call return a string?”
- “does this function return a `TypeInfo`?”
- “is this field load a string field?”
- “which runtime symbol does this imported call really mean?”

That creates duplicate logic and drift between:

- `self-host/ir_emitter.pith`
- `cranelift/codegen/src/ir_consumer.rs`
- `cranelift/codegen/src/lib.rs`

### Suggested changes

#### 1. Calls should carry an exact lowered callee name

Current shape:

```text
call REG NAME NARGS ARG...
```

Suggested direction:

```text
call REG NAME RETKIND NARGS ARG...
```

Where:

- `NAME` is already fully lowered
  - examples: `url_to_string`, `toml_get_int`, `pith_path_basename`
- `RETKIND` is one of:
  - `void`
  - `int`
  - `float`
  - `bool`
  - `string`
  - `list`
  - `list_string`
  - `map`
  - `map_int`
  - `set`
  - `struct:TypeName`
  - `unknown`

This removes the need for Rust to maintain:

- string-return inference
- struct-return inference
- large portions of symbol remapping logic

#### 2. Field loads should carry explicit type identity

Current shape is effectively:

```text
field REG OBJ FIELD
```

Suggested direction:

```text
field REG OBJ STRUCT FIELD_INDEX FIELD_KIND FIELD_NAME
```

Example:

```text
field 12 4 TypeInfo 1 string name
```

Rust should not need to infer offsets by searching every known struct layout.

#### 3. Function headers should carry return kind directly

Current shape:

```text
func NAME NPARAM RETTYPE
```

That value is currently too weak because most real type knowledge is still
re-inferred later. Keep the header, but make sure the emitter always writes the
same return-kind vocabulary used by `call`.

#### 4. Global declarations should carry concrete storage kind

Current shape already hints at this, but it should be normalized.

Suggested storage vocabulary:

- `global NAME int`
- `global NAME bool`
- `global NAME string`
- `global NAME list`
- `global NAME list_string`
- `global NAME map`
- `global NAME map_int`
- `global NAME set`
- `global NAME struct:TypeName`

That will make imported-global rewriting simpler and reduce backend fallback
logic.

### Expected Rust deletions after this contract lands

Once the new IR contract is emitted consistently, these Rust helpers should
shrink or disappear:

- `call_returns_string`
- `known_struct_returning_fn`
- `field_offset_for_name`
- `split_typed_field_name`
- `collect_struct_returning_funcs`
- `collect_string_returning_funcs`
- large parts of `resolve_func_name`

### Implementation order inside Phase 1

1. Teach `self-host/ir_emitter.pith` to emit `RETKIND` and explicit field info
2. Update `self-host/ir_driver` to preserve the new IR format
3. Update `ir_consumer.rs` to trust emitted metadata
4. Delete the old inference helpers
5. Re-run deterministic examples and self-host rebuild

## Non-goals

Do not try to:

- rewrite Cranelift lowering in Pith
- replace low-level runtime storage with Pith code
- eliminate Rust entirely

That would fight the architecture instead of tightening it.
