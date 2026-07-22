# defer

`defer` schedules a statement to run when control leaves the enclosing
block. it lets you pair a cleanup with the thing it cleans up: open a
file and defer closing it, take a lock and defer releasing it. the
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
propagates an error out of one of the `write` calls. you write the close
once, next to the open, and stop worrying about the exits.

## when it runs

a deferred statement runs on every exit from its block:

- falling off the end of the block
- a `return`
- a `fail`
- a `!` that propagates an error

it does not run if control never reaches the `defer` — a `defer` in an
`if` branch that is not taken never fires. that falls out of the model:
the defer is only scheduled once execution reaches it.

```pith
fn maybe(x: Int):
    if x > 0:
        defer print("cleanup")   # only when x > 0
        print("work")
    print("done")
```

for `x > 0` this prints `work`, `cleanup`, `done` — the defer runs at the
end of the `if` block, not the end of the function.

## order

within a scope, defers run last-in-first-out. the last one you wrote is
the first to run, which is what you want when a later step depends on an
earlier one:

```pith
conn := connect()!
defer conn.close()
buf := conn.attach_buffer()!
defer buf.flush()        # flushes before the connection closes
```

nested blocks unwind innermost first: an inner block's defers all run
before the defers of the block around it.

## in loops

a `defer` inside a loop body belongs to that iteration. it runs at the
end of each pass, and also on the way out through `break` or `continue`:

```pith
for item in items:
    lock(item)
    defer unlock(item)       # unlocks at the end of every iteration
    process(item)
```

## what you can defer

a deferred statement is a plain side effect, usually a call. the
compiler rejects shapes that would make the cleanup itself exit the
scope, because that would re-enter the very cleanup in progress:

- no `return`, `fail`, `break`, or `continue`
- no `!` or `?` propagation
- no nested `defer`
- no binding (its name would never be in scope anyway)

if you need any of those, move the logic into a helper and defer a call
to it.

## errdefer

`errdefer` is the error-only sibling of `defer`. it runs only when the
scope exits through an error — a `fail` or an `!` that propagates — and
stays silent on a normal `return`, on falling off the end, and on `break`
or `continue`. use it for cleanup that should happen only when something
went wrong: undo a half-finished change, roll back a transaction, delete a
partial file.

the motivating case is a transaction. you begin it, do the work, and
commit. if any step fails, you want a rollback — but not after a
successful commit:

```pith
fn transfer(db: Db, from: Int, to: Int, cents: Int) -> Int!:
    tx := db.begin()!
    errdefer tx.rollback()       # only if we leave through an error

    tx.debit(from, cents)!       # any of these can fail and propagate
    tx.credit(to, cents)!
    tx.commit()!
    return cents
```

if `debit` or `credit` fails, the `!` propagates and the `errdefer` rolls
the transaction back on the way out. if everything succeeds, the `commit`
runs and the `errdefer` does not — no rollback after a good commit. a
plain `defer tx.rollback()` would roll back every time, undoing the commit
you just made.

`errdefer` follows all the same rules as `defer`: same allowed shapes,
same last-in-first-out order, same scope and loop behavior. the two
interleave by registration. given

```pith
defer a()
errdefer b()
defer c()
```

a success exit runs `c`, then `a` (the `errdefer` is skipped); an error
exit runs `c`, then `b`, then `a`. because `errdefer` only makes sense
where an error can leave the function, the compiler requires the enclosing
function to return a result (`T!`).

## how it works

`defer` is a compile-time rewrite with no runtime machinery. the compiler
re-emits the deferred statement at each exit edge of the block, in
reverse order, just before it releases the block's values. because the
statement is physically placed at every exit, conditional and looping
defers are correct for free. there is no runtime list of pending
cleanups to walk. this is the same approach zig takes.

`errdefer` rides the same machinery: each entry is tagged as always-run
or error-only, and the success exits (return, fall-through, break,
continue) skip the error-only ones while the error exits (fail, `!`) emit
them all.
