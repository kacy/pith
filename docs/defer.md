# defer

`defer` schedules a statement to run when control leaves the enclosing
block. It lets you pair a cleanup with the thing it cleans up: open a
file and defer closing it, take a lock and defer releasing it. The
cleanup then runs no matter how the block exits.

```pith
fn write_report(path: String) -> Int!:
    f := open(path)!
    defer f.close()

    f.write("line one")!
    f.write("line two")!
    return 2
```

`f.close()` runs whether the function returns normally, fails, or an `!`
propagates an error out of one of the `write` calls. You write the close
once, next to the open, and it runs on every exit.

## When it runs

A deferred statement runs on every exit from its block:

- Falling off the end of the block
- A `return`
- A `fail`
- A `!` or `?` that propagates an error (or an index, map, or `.value()` miss
  propagated inside a `T!` function)

It does not run if control never reaches the `defer`: a `defer` in an
`if` branch that is not taken never fires, because the defer is only
scheduled once execution reaches it.

```pith
fn maybe(x: Int):
    if x > 0:
        defer print("cleanup")   # only when x > 0
        print("work")
    print("done")
```

For `x > 0` this prints `work`, `cleanup`, `done`. The defer runs at the
end of the `if` block rather than at the end of the function.

## Order

Within a scope, defers run last-in-first-out. The last one you wrote is
the first to run, which is what you want when a later step depends on an
earlier one:

```pith
conn := connect()!
defer conn.close()
buf := conn.attach_buffer()!
defer buf.flush()        # flushes before the connection closes
```

Nested blocks unwind innermost first: an inner block's defers all run
before the defers of the block around it.

## In loops

A `defer` inside a loop body belongs to that iteration. It runs at the
end of each pass, and also on the way out through `break` or `continue`:

```pith
for item in items:
    lock(item)
    defer unlock(item)       # unlocks at the end of every iteration
    process(item)
```

## What you can defer

A deferred statement is a plain side effect, usually a call. The
compiler rejects shapes that would make the cleanup itself exit the
scope, because that would re-enter the cleanup in progress:

- No `return`, `fail`, `break`, or `continue`
- No `!` or `?` propagation
- No nested `defer` or `locked` block
- No binding (its name would never be in scope anyway)

If you need any of those, move the logic into a helper and defer a call
to it.

## errdefer

`errdefer` is the error-only sibling of `defer`. It runs only when the
scope exits through an error (a `fail`, a `!` or `?` that propagates, or an
index/map/`.value()` miss propagated inside a `T!` function). It
stays silent on a normal `return`, on falling off the end, and on `break`
or `continue`. Use it for cleanup that should happen only when something
went wrong: undo a half-finished change, roll back a transaction, delete a
partial file.

The motivating case is a transaction. You begin it, do the work, and
commit. If any step fails, you want a rollback, but never after a
successful commit:

```pith
fn transfer(db: Db, payer: Int, payee: Int, cents: Int) -> Int!:
    tx := db.begin()!
    errdefer tx.rollback()       # only if we leave through an error

    tx.debit(payer, cents)!      # any of these can fail and propagate
    tx.credit(payee, cents)!
    tx.commit()!
    return cents
```

If `debit` or `credit` fails, the `!` propagates and the `errdefer` rolls
the transaction back on the way out. If everything succeeds, the `commit`
runs and the `errdefer` does not. A plain `defer tx.rollback()` would roll
back every time, undoing the commit you just made.

`errdefer` follows all the same rules as `defer`: same allowed shapes,
same last-in-first-out order, same scope and loop behavior. The two
interleave by registration. Given

```pith
defer a()
errdefer b()
defer c()
```

A success exit runs `c`, then `a` (the `errdefer` is skipped); an error
exit runs `c`, then `b`, then `a`. Because `errdefer` only makes sense
where an error can leave the function, the compiler requires the enclosing
function to return a result (`T!`).

## How it works

`defer` is a compile-time rewrite with no runtime machinery. The compiler
re-emits the deferred statement at each exit edge of the block, in
reverse order, just before it releases the block's values. Because the
statement is physically placed at every exit, conditional and looping
defers are correct without extra work. There is no runtime list of pending
cleanups to walk. This is the same approach Zig takes.

`errdefer` uses the same machinery: each entry is tagged as always-run
or error-only, and the success exits (return, fall-through, break,
continue) skip the error-only ones while the error exits (`fail`, `!`, `?`,
and propagated `T!` misses) emit them all.
