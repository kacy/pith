# performance audit

## status: march 2026

audit of both the zig bootstrap compiler and the self-hosted forge compiler.
findings ordered by severity and impact.

---

## critical

### runtime map implementation — O(n) everything

`forge_runtime.h` lines 353–451. `Map[K,V]` is backed by parallel arrays
with linear scan for every operation:

```c
// forge_map_get_by_string scans all keys on every lookup
for (int64_t i = 0; i < map.len; i++) {
    if (forge_string_eq(keys[i], key)) { ... }
}
```

every `map.get()`, `map.insert()`, and `map.contains()` is O(n). this
affects ALL compiled forge programs, not just the compiler. any program
using maps with more than a few dozen entries will hit this.

**fix:** replace with a real hash table (open addressing, robin hood, or
similar). the `forge_map_t` struct needs a `capacity` field and hash
buckets. this is the single highest-impact change possible.

### codegen parallel list lookups

`codegen.fg` lines 110–134. the self-hosted compiler uses parallel lists
(`g_mangled_keys`/`g_mangled_vals`) instead of `Map[Int, String]` because
map codegen for that type combination was broken at the time:

```
fn g_mangled_lookup(tid: Int) -> String:
    mut i := 0
    while i < g_mangled_keys.len():
        if g_mangled_keys[i] == tid:
            return g_mangled_vals[i]
        i = i + 1
    return ""
```

this is called on every type reference during emission. O(n) per lookup
where n is the number of distinct mangled types. for large programs this
compounds quickly.

**fix:** fix `Map[Int, String]` codegen in the bootstrap compiler, then
replace parallel lists with a proper map.

---

## high priority

### string building in codegen — O(n²)

`codegen.fg` lines 269–287. `g_mangle_name()` builds a mangled name
character by character using string concatenation:

```
result = result + ch
```

each concatenation allocates a new string. for a name like
`Pair[Int,String]` (15 chars), this does 15 allocations and copies.

**fix:** collect characters into a `List[String]` and call `.join("")`
at the end, or pre-allocate the result.

### zig bootstrap: redundant type table scan

`codegen.zig` lines 871–896. `buildGenericInstName` does a `TypeTable.lookup()`
to check existence, then iterates the same HashMap manually to find the
stable key pointer:

```zig
if (self.type_table.lookup(lookup)) |_| {
    var it = self.type_table.name_map.iterator();
    while (it.next()) |entry| {
        if (std.mem.eql(u8, entry.key_ptr.*, lookup)) {
            return entry.key_ptr.*;
        }
    }
}
```

the HashMap already found the entry — iterating again to get the key
pointer is O(n) on top of the O(1) lookup.

**fix:** return the key pointer from `TypeTable.lookup()`, or use
`getEntry()` / `getKeyPtr()` on the HashMap directly.

### zig bootstrap: linear module declaration scan

`checker.zig` lines 577–603. `findPublicDecl` and `findAnyDecl` scan all
declarations in a module to find one by name:

```zig
for (module.decls) |*decl| {
    const decl_name = getDeclName(decl) orelse continue;
    if (std.mem.eql(u8, decl_name, name)) { ... }
}
```

this is called per imported symbol. for modules with many declarations
and many importers, it adds up.

**fix:** build a `name → Binding` HashMap during the registration pass
and use it for lookups.

---

## medium priority

### 8-pass AST iteration in codegen

`codegen.fg` lines 3688–3800. `g_emit_module()` iterates the root node's
children 8 separate times — once for structs, once for enums, once for
function forward decls, once for method forward decls, once for function
defs, once for method defs, once for generic instances, once for tests.

each pass does the same `pub` unwrapping logic. for a file with n
top-level declarations, this is 8n iterations.

**fix:** single pre-pass to bucket children by kind into separate lists,
then iterate each list once.

### linear import dedup

`codegen_main.fg` lines 75–81. `cm_visited` is a `List[String]` checked
via linear scan to deduplicate imported modules:

```
for v in cm_visited:
    if v == file_path:
        already = true
```

**fix:** use `Map[String, Bool]` (once map codegen is fixed) or a
`Set[String]` for O(1) membership checks.

### lambda/tuple type table scan

`codegen.zig` lines 3460–3507. when inferring the type of a lambda or
tuple expression, the type checker scans the entire type table looking
for a matching function or tuple type:

```zig
for (items, 0..) |ty, idx| {
    switch (ty) {
        .function => |func| { ... }
    }
}
```

**fix:** maintain a secondary index keyed by `(param_types, return_type)`
for function types and `(element_types)` for tuple types.

---

## low priority

### error message string concatenation

`checker.fg` throughout. error messages built via `"msg " + var`:

```
g_checker_error("type mismatch: expected " + expected + " but got " + actual, ...)
```

this only runs on error paths, so it doesn't affect normal compilation
performance. worth noting but not worth optimizing.

---

## recommended order of attack

1. **runtime hash maps** — replaces O(n) maps with O(1) for all forge
   programs. this is the single biggest unlock.
2. **fix Map[Int, String] codegen** — unblocks the self-hosted compiler
   from using real maps instead of parallel list workarounds.
3. **pre-bucket AST children by kind** — reduces 8 passes to 1+8 targeted
   iterations.
4. **string building with List.join** — fixes O(n²) name mangling.
5. **zig bootstrap fixes** — only if still actively maintaining the
   bootstrap compiler. lower priority as self-hosting matures.
