# iterators

Pith has one iterator protocol. Built-in collections use it, the
range-for sugar uses it, and your own types use it the same way. If a
value's type provides

```pith
fn next() -> T?
```

then `for x in value:` drives it — the loop pulls until `next()`
returns `none`. The lazy adapters in `std.iter` take any value with
that shape, so once your struct has a `next`, you can chain
`map_iter` / `filter_iter` over it without writing anything extra.

## the protocol

Any struct that implements `next() -> T?` is an iterator:

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

fn main():
    for v in Counter(0, 5):
        print(v)
```

Two things to keep in mind:

- `next()` is destructive. The receiver's state advances each call, so
  iterating a value twice in a row will see an empty sequence the
  second time. Construct a fresh one if you need to walk again.
- The element type comes from `T?` in the signature. Returning `none`
  ends the loop; returning a wrapped value yields it to the loop body.

## range-for

For "do this N times" or indexing into a parallel list, use range-for
instead of building a list of integers:

```pith
for i in 0..n:        # half-open, [0, n)
    do(i)

for i in 0..=n:       # inclusive, [0, n]
    do(i)
```

Both forms compile to a counted loop with no allocation. Use
`std.iter.Range` only when you want a value you can hold, store, or
hand to an adapter; the sugar is faster otherwise.

## std.iter

`std.iter` wraps the protocol with a small set of reusable pieces:

```pith
from std.iter import Iterator, Range, to_list, map_iter, filter_iter
```

- `Iterator[T]` — the interface (`fn next() -> T?`). Bound on it when
  you want a generic function that walks any iterator:
  `fn to_list[I: Iterator[T], T](it: I) -> List[T]`.
- `Range(lo, hi)` — a `[lo, hi)` value you can hold. Carries its own
  cursor and follows the protocol, so it plugs into the adapters. The
  for-loop sugar is faster when you just want a counted loop.
- `to_list(it)` — drains any iterator into a `List[T]`. `T` is
  inferred from the `Iterator[T]` bound, so you don't write the type
  argument.
- `map_iter(src, f)` / `filter_iter(src, p)` — lazy adapters. Each call
  wraps the source iterator with a closure and produces a new iterator
  whose `next()` pulls from the source on demand.

The adapter constructors are named `map_iter` / `filter_iter` rather
than `map` / `filter` so they don't collide with the eager free-function
forms over `List` that pith has had for longer. Use the method form
(`xs.map(...)`, `xs.filter(...)`) on a list when you want eager
results; reach for the iter form when you want fusion or you're working
with a non-list source.

## lazy fusion

A chain of adapters is walked exactly once, at the terminal:

```pith
from std.iter import Range, to_list, map_iter, filter_iter

fn main():
    out := to_list(
        map_iter(
            filter_iter(Range(0, 10), fn(x: Int) => x > 5),
            fn(x: Int) => x * 10,
        ),
    )
    print(out)   # [60, 70, 80, 90]
```

`to_list` pulls from `map_iter`, which pulls from `filter_iter`, which
pulls from `Range`. There is no intermediate list between stages, so
`to_list` is the only allocation in the whole pipeline.

Each call site monomorphizes the adapter for its concrete source and
closure types, so `self.src.next()` inside the adapter resolves to the
right concrete `next` at compile time. No dynamic dispatch — that's
why nested adapters fuse.

## writing your own adapter

The same pattern works for adapters you write yourself. The shape is:
hold a source iterator (and any state you need), implement
`Iterator[T]` on the struct, and write `next()` to pull from
`self.src.next()`:

```pith
from std.iter import Iterator

struct Take[I, T]:
    src: I
    left: Int

impl Iterator[T] for Take:
    fn next() -> T?:
        if self.left <= 0:
            return none
        self.left = self.left - 1
        return self.src.next()

fn take[I: Iterator[T], T](src: I, n: Int) -> Take[I, T]:
    return Take[I, T](src, n)
```

A few practical notes:

- Generic struct method calls on `self.<field>` are specialized per
  instantiation, so `self.src.next()` dispatches to the concrete
  source's `next` at IR time.
- When you've already checked an optional, `n.value()` extracts the
  inner value: `if n == none: return none; return f(n.value())`.
- The compiler infers `T` from the `Iterator[T]` bound on the source
  type. If inference can't see through a particular call (rare in
  practice), the multi-arg explicit form `take[Range, Int](r, 3)`
  is always available as the escape hatch.

## when to use what

Use `for i in 0..n:` for counted loops over integers — it doesn't
allocate. Use `xs.map(...)` / `xs.filter(...)` when you're transforming
a list and want the result list right there. Reach for the `std.iter`
adapters when fusion matters (long sources, multiple stages, early
termination, non-list inputs). And write your own `fn next() -> T?`
when the source is yours and has state that doesn't fit any of the
above — cursors, channels, parsers.
