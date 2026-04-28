# performance audit

## status: april 2026

audit of both the zig bootstrap compiler and the self-hosted pith compiler.
all issues below have been fixed.

this snapshot predates the native tls move, but the high-level conclusion still
holds: the expensive parts worth fixing were compiler/runtime hot paths, not the
old Rust TLS module we have since deleted.

---

## critical — FIXED

### runtime map implementation — O(n) → O(1)

`pith_runtime.h`. replaced parallel-array maps with hash-indexed maps
using open addressing and linear probing. dense keys/values arrays
preserved for codegen compatibility. splitmix64 for int keys, FNV-1a
for string keys. 8-bucket initial capacity, 2x growth at 75% load.

### codegen parallel list lookups — FIXED

`codegen.pith`. replaced `g_mangled_keys`/`g_mangled_vals` parallel lists
with `g_mangled: Map[Int, String]`. deleted manual lookup/has/set helpers.
now O(1) per lookup via the runtime hash map.

---

## high priority — FIXED

### string building in codegen — O(n²) → O(n)

`codegen.pith` `g_mangle_name()`. replaced char-by-char string concatenation
with `List[String]` + `.join("")`. single allocation instead of n allocations.

### zig bootstrap: redundant type table scan — FIXED

`codegen.zig` `buildGenericInstName()`. replaced full HashMap iterator scan
with `name_map.getKeyPtr()` — one-line O(1) lookup.

### zig bootstrap: linear module declaration scan — FIXED

`checker.zig` `resolveFromImport()`. builds a `StringHashMap(DeclMeta)` once
per imported module, then looks up each imported name in O(1). `findAnyDecl`
kept as linear scan since it's only called on error paths.

---

## medium priority — FIXED

### 8-pass AST iteration in codegen — FIXED

`codegen.pith` `g_emit_module()`. single pre-pass buckets children by kind
(struct/enum/fn/impl/test), then each emission phase iterates only its
bucket. reduces from 6n iterations to n + 6×bucket_size.

### linear import dedup — FIXED

`codegen_main.pith`. `cm_visited` changed from `List[String]` to
`Map[String, Bool]`. dedup check is now O(1) `contains_key` instead of
O(n) linear scan.

### lambda/tuple type table scan — FIXED

`codegen.zig`. `FnSigKey` and `TupleSigKey` caches built eagerly at
CEmitter init by scanning the type table once. lambda and tuple type
inference now does O(1) hash lookup instead of O(n) linear scan.

---

## low priority — not fixing

### error message string concatenation

only runs on error paths. doesn't affect normal compilation performance.
not worth the complexity to optimize.
