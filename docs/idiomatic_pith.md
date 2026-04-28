# idiomatic pith

This is the current style for everyday Pith code. It favors direct data flow,
small helpers, and examples that read like application code instead of compiler
tests.

## printing and strings

Prefer interpolation for one-off values:

```pith
print("count: {items.len()}")
print("best: {record.name} ({record.score})")
```

Use `std.fmt` when formatting is reused or when a value is a common collection
shape:

```pith
import std.fmt as fmt

print("scores: {fmt.ints(scores)}")
print("names: {fmt.join_strings(names, ", ")}")
```

Use `std.io.string_buffer()` for builders and parsers that append in a loop.
Do not build long strings by repeatedly adding `value.to_string()` unless that
really is the clearest expression.

## collections

Use `for item in items` or `for item, index in items` before reaching for a
manual `while index < items.len()` loop.

Use `std.collections` for common transforms:

```pith
import std.collections as collections

names := collections.map_list(records, fn(record: Record) => record.name)
engineering := collections.filter_list(records, fn(record: Record) => record.category == "engineering")
total := collections.fold_list(records, 0, fn(acc: Int, record: Record) => acc + record.score)
best := collections.max_by(records, fn(record: Record) => record.score)!
```

Collections are shared handles. If a function needs to mutate its own top-level
container, start with `copy_list`, `copy_map`, or `copy_set`.

## errors and tests

Use bare `T!` for simple string errors. Use `T!SomeError` when callers need to
inspect the error payload.

Prefer `catch`, `unwrap_or`, and `or_else` when they make recovery clearer than
manual `is_err` branching.

Write colocated `test` declarations for stdlib behavior. Use
`std.testing.assert_eq` and `assert_ne` for normal comparisons, and keep golden
stdout examples for end-to-end behavior.

## packages

`pith new <name>` should produce a project that can immediately run:

```sh
make check
make test
make lint
make fmt
```

Keep public functions documented, keep examples small, and prefer one module per
file with directories as namespaces.
