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

An empty literal takes its element type from an annotation when you give one
(`mut names: List[String] := []`), and otherwise from the first value stored
into it — `mut tasks := []` followed by `tasks.push(spawn worker())` types
`tasks` as a list of tasks. Annotate when the first store sits far from the
declaration and a reader would have to hunt for it.

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

## cleanup with `defer` and `errdefer`

Pair a cleanup with the thing it cleans up, right where you acquire it. `defer`
schedules a statement to run on every exit from the block — falling off the end,
a `return`, a `fail`, or a `!` that propagates. You write the close once, next to
the open, and the error path can't skip it:

```pith
fn write_report(path: String) -> Int!:
    f := open(path)!
    defer f.close()

    f.write("line one")!    # if this fails, f still closes on the way out
    f.write("line two")!
    return 2
```

Reach for it wherever you'd otherwise close a file, unlock a mutex, or release
any resource by hand. Defers within a block run last-in-first-out, and a defer
inside a loop belongs to that iteration.

`errdefer` is the error-only sibling: it runs when the block exits through a
`fail` or a `!`, and stays quiet on a normal return. Use it to undo a
half-finished change — the transaction rollback is the motivating case:

```pith
fn transfer(db: Db, src: Int, dst: Int, cents: Int) -> Int!:
    tx := db.begin()!
    errdefer tx.rollback()   # only if we leave through an error

    tx.debit(src, cents)!
    tx.credit(dst, cents)!
    tx.commit()!
    return cents
```

If `debit` or `credit` fails, the `!` propagates and the rollback runs. If the
`commit` succeeds, the `errdefer` does not fire — a plain `defer` would roll back
the commit you just made. See [defer.md](defer.md) for the full rules, including
what you can and can't defer.

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

## safe access on containers

`map[k]`, `list[i]`, `s[i]`, and `bytes[i]` are the "I know it's there"
form. If the key or index isn't there, the runtime prints a structured
`pith runtime error: ...` and exits. That's the right behavior for
tables you just populated, loops that already bounded `i`, and struct
fields you invariantly initialized — a miss is a real bug and should
crash loudly, not flow through as a zero.

Inside a `T!` function, `map[k]` and `list[i]` instead propagate the
miss as `fail "index out of bounds"`, which the caller can `catch` or
`unwrap_or`. So the same syntax means "assert" outside a Result context
and "propagate" inside — no special operator required:

```pith
fn config_value(cfg: Map[String, Int], key: String) -> Int!:
    return cfg[key]                              # propagates on miss

fn main() -> Int!:
    port := config_value(cfg, "PORT") catch 8080 # recover here
    ...
```

When you want to _observe_ the miss without crashing or propagating —
distinguishing "not present" from "present with value 0" — reach for
the `.get()` methods, which return `T?`:

```pith
count := stats.get("hits").unwrap_or(0)          # None -> 0, Some(0) -> 0
hit := stats.get("hits")                         # distinguishes the two:
if hit != none:                                  #   Some means it was recorded,
    record(hit?)                                 #   none means missing
first := xs.first()                              # List[T] -> T?
last := xs.last()                                # ditto
peek := xs.get(i)                                # index-checked
letter := s.get(i)                               # String -> String?
```

Prefer `.get(k).unwrap_or(d)` over `Map.get_default(k, d)`. Both work,
but the composed form is more general (chains with `.map`, `.and_then`,
etc.) and reads left-to-right.

`env.get(key)` returns `String?` — `Some(value)` when set (including
empty string), `none` when unset.

The same rule applies to iterators (`std.iter.next() -> T?`) and channel
receives (`Channel[T].recv() -> T?`): the safe-access surface uses `T?`,
the assertion surface uses direct dereference.

Write colocated `test` declarations for stdlib behavior. Inside a `test` block
use the built-in `assert` and `assert_eq` (which compares by value and reports
decoded failures); keep golden stdout examples for end-to-end behavior. See
[testing.md](testing.md) for the full picture.

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
