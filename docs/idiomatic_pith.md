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
manual `while index < items.len()` loop. When you actually need an integer
index, use range-for instead of building a list of integers:

```pith
for i in 0..items.len():
    print("{i}: {items[i]}")

for i in 0..=10:    # inclusive upper bound
    total = total + i
```

Both forms compile to a counted loop with no allocation. They're the
default for "do this N times" or "index into a parallel list".

For everyday list transforms, prefer the method form on the list — it
reads top to bottom and chains:

```pith
names := records.map(fn(r: Record) => r.name)
seniors := records.filter(fn(r: Record) => r.years >= 5)
total := records.reduce(0, fn(acc: Int, r: Record) => acc + r.score)
```

The free-function forms in `std.collections` (`map_list`, `filter_list`,
`fold_list`, `max_by`, …) are still there when you already hold a
function and want to pipe it, or when you need a helper the method
surface doesn't cover yet:

```pith
import std.collections as collections

best := collections.max_by(records, fn(record: Record) => record.score)!
```

When a pipeline runs over a large collection or you only need the first
few elements, switch to the lazy adapters in `std.iter`. The chain
fuses and the source is walked once. See `docs/iterators.md` for the
protocol and worked examples.

Collections are shared handles. If a function needs to mutate its own top-level
container, start with `copy_list`, `copy_map`, or `copy_set`.

Struct fields can be updated in place. Structs are heap-allocated reference
values, so a write through one handle is visible through every other handle
to the same instance — useful for stateful iterators and small caches, and a
foot-gun when you actually wanted a copy:

```pith
struct Counter:
    cur: Int
    hi: Int

impl Counter:
    fn next() -> Int?:
        if self.cur >= self.hi:
            return none
        v := self.cur
        self.cur = self.cur + 1
        return v
```

Build a struct positionally, or name the fields when that reads better. Named
fields can come in any order, and any field with a default may be left out.

```pith
struct Config:
    host: String
    port: Int = 8080
    tls: Bool = false

dev := Config(host: "localhost")
prod := Config(host: "example.com", port: 443, tls: true)
```

Named fields aren't available on generic structs yet — those still take
positional arguments.

## errors and tests

Use bare `T!` for simple string errors. Use `T!SomeError` when callers need to
inspect the error payload.

Prefer `catch`, `unwrap_or`, and `or_else` when they make recovery clearer than
manual `is_err` branching.

## absence: `T?` vs `T!`

Pith has two distinct shapes for "no value here". Pick by intent, not by mood:

- **`T?` (Optional)** — for *predictable* absence the caller routinely
  handles. Lookup misses, end-of-stream, optional config fields, channel
  closure. Construct with `none`; consume with `match`, `== none`, or `?`
  inside a `T!` function. Examples: `Map.get`, `Channel.recv`,
  `std.strings.get`.
- **`T!` (Result)** — for *unexpected* failure the caller usually wants to
  propagate. I/O, parsing, network, anything with an external cause.
  Construct with `fail`; consume with `!`, `catch`, `unwrap_or`, `or_else`.
  Examples: `fs.read`, `parse_port`, `tls.connect`.

Rule of thumb: if the caller would write `if x == none: ...` more often than
`!`, use `T?`. If they'd write `let v = x!` more often than `match`, use `T!`.
Don't double-wrap (`T?!` or `T!?`) unless absence and failure are genuinely
distinct outcomes — usually one or the other tells the whole story.

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
