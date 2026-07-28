# string and collection ownership

how the compiler decides where retain and release calls go. the rules
are small, but they are load-bearing: every leak, dangling pointer,
and double free the emitter has ever produced traces back to one of
them being broken somewhere.

## the model

heap strings carry a refcount in their header
(`cranelift/runtime/src/runtime_core.rs`). retain and release are
no-ops for string literals and null, so emitted code never needs to
know where a string came from — it can retain or release any string
register safely.

collections (lists, maps, sets) own their elements. a tagged container
holds exactly one count per heap element: taken on insert, dropped the
moment that element leaves, whether it was removed, overwritten,
cleared, or carried off when the container itself is freed. this only
works if the container was built with the constructor that matches its
element type (see "container flavors" below).

because eviction drops the container's count, anything that reads an
element and keeps it past a later mutation must hold a count of its
own. the emitter arranges that: binding, assigning, or storing a
borrowed element retains it first. sets need no count at all, since
they copy element bytes into their own storage instead of holding the
caller's handle.

## the rules

variables:

- every string a variable holds is one owned count
- binding or assigning a *borrowed* string (a variable read, a
  collection element, a struct field) retains it first; fresh call
  results already arrive owned
- rebinding or reassigning releases the value being replaced
- a return hands its count to the caller: a returned variable is
  excluded from the function's exit cleanup, a returned borrow is
  retained
- each return releases every string local the function owns
- a `for` loop variable is a borrow rather than an owned local: it
  reads an element the container still holds, and takes no count of
  its own. so it gets storage of its own for the length of the body,
  which keeps it out of the slot a local of the same name uses — where
  the rebind's release-the-old, and the exit cleanup on an early
  return, would drop a count the loop never took

call arguments:

- callees borrow their parameters; a callee that stores an argument
  adds its own count
- so an *owned* argument (a fresh `s[i]` char, a concat result, a
  fresh method result) dies once the call it fed returns — the caller
  releases it
- exception: an argument stored directly into a container is skipped
  by that release (`skip_pos` in `ir_release_owned_method_args`),
  because ownership transfers into the container there rather than
  ending with the call. the store completes that transfer: an owned
  value goes through the variant of the store that keeps the caller's
  count instead of taking a second one — `pith_list_push_value_owned`,
  `pith_list_set_value_owned`, `pith_map_insert_cstr_owned`,
  `pith_map_insert_ikey_owned`. the emitter picks the variant from the
  value's ownership alone (`ir_owned_container_store_name`); whether
  there is a count to take is the container's own business, since only
  it knows if it was built tagged. an untagged container takes nothing
  and the caller's count stays outstanding, which is a leak rather than
  an element nothing keeps alive
- an index key is an argument like any other, and the container-store
  exception does not cover it: a lookup reads the key and keeps nothing,
  a store copies the key bytes into the map's own storage, so an owned
  key in `m[a + ":" + b]` dies with the operation either way. the stored
  *value* is still under the exception. the object being indexed is
  deliberately left alone — container indexing borrows the element out
  of the container, so releasing a temporary container there would free
  the value just returned

lambdas and function values:

- a lambda's return transfers like a function return: borrowed
  strings are retained on the way out (`ir_closure_return_kinds`
  records what a closure returns so call sites treat the result as
  owned)
- naming a function as a value mints a closure. `f := shout` and
  `xs.map(shout)` both lower to `closure_ref`, which allocates with a
  count of one, so the expression is owned the same way a lambda
  literal is: the bind transfers rather than retains, and an argument
  position releases after the call. `ir_expr_is_named_function_value`
  decides this, and it mirrors the two emit paths that allocate — a
  bare name (`ir_emit_ident_load`) and a module member such as
  `hash.sha256_bytes` (`ir_emit_field_access_expr`). saying "owned"
  where no closure was allocated would release a count nobody took
- `map`, `filter` and `reduce` open-code their loop rather than going
  through the general method call, so the release that reclaims an
  owned argument does not run for them. each releases the function it
  was handed once the loop is done, and only when that function
  arrived owned — a local or a struct field holding the closure
  belongs to its owner

## container flavors

a map or list is built by a constructor that matches its element
type: string-valued maps use the retaining constructor, int maps the
plain one. the flavor is chosen at emission time from the declared or
checked type — which is why an empty `{}` literal must know what it
is initializing. losing that information across a module boundary is
exactly the bug that broke the emitter split twice; imported globals
now carry their type kinds so a reassigned `{}` keeps the right
flavor.

closures are an element type like any other: a `List[fn(...) -> T]`
uses the closure constructor, so its push retains and its free-time
cascade releases. a route table and a middleware chain are exactly
that shape, and untagged they held handles they did not own — which
was survivable only while every closure that reached one was leaking a
count somewhere else. maps and sets of closures are still untagged,
because a map's value counting is cstring-only; a borrowed closure
stored in one gets its count added at the store instead
(`ir_container_store_needs_retain`), which leaks rather than dangles.

the lists `map` and `filter` produce pick their flavor the same way,
from the checked element type of the call, and the two differ in where
the count comes from. a mapper hands back a value nothing else is
holding, so the push transfers: the loop stops tracking the result
there and the list keeps the count the mapper returned. a filter keeps
the source list's own elements, so its push retains, which is what lets
the result outlive the list it was filtered from. an element type with
no constructor of its own — a boxed enum, an optional or a result —
still builds an untagged list, the same gap literals have.

the list methods implemented in the runtime rather than the emitter
decide their own flavor: `slice` and `sort` copy the source list's tag
(`reverse` builds nothing, it reorders in place), `split` is
string-tagged, `map.keys()` is tagged when the
keys are strings, and `map.values()` when the values are heap values.

## where the code lives

- the variable rules: statement lowering in
  `self-host/ir_emitter_core.pith` (binds, assigns, returns)
- the argument rules: `ir_release_owned_method_args` and the call
  paths near it
- the runtime counts: `cranelift/runtime/src/runtime_core.rs` and the
  per-type files under `collections/`

if you are adding a statement or call shape, decide explicitly which
of the rules above applies to every string that flows through it —
"it seemed to work" is how the dangling ones got in.

## what is and isn't reclaimed

reference counting frees a value the moment its last count drops, with
no gc pause. that covers the common case completely. the deliberate
gaps, all bounded leaks rather than dangling pointers:

- **reference cycles are not collected.** there is no cycle collector,
  so a graph that closes a loop of strong references keeps its own count
  above zero and leaks. a struct can name its own type in a field and an
  optional field owns its target, so a linked structure — a parent that
  owns a child which points back at the parent — closes a strong cycle.
  the fix is the `weak` keyword (below): mark the back edge `weak` and
  the ring reclaims. the other way to build a cycle is a closure that
  captures a binding which transitively holds the closure — for example
  a list that contains a closure capturing that same list; there is no
  weak capture yet, so that shape still leaks.
- **a struct value stored straight into a container is still counted
  twice.** strings, bytes, and nested collections hand the container the
  count the temporary was holding; struct values keep taking a second
  one. what made this unsafe to change was a separate bug in loop
  variables, and that one is fixed: a `for` variable used to write its
  borrowed element into the same named slot a later `:=` of that name
  reused, so the rebind released a count the loop never took, and the
  extra count a struct store takes was all that absorbed it. a loop
  variable now gets storage of its own for the length of the body, so
  the only thing left here is to make the struct store transfer its
  count like the other kinds do. `make leak-check` has no case for this
  shape; one belongs in the change that fixes it.
- **arc reclaims memory, but it does not run your cleanup.** closing a
  file, rolling back a transaction, or releasing a lock is a side effect
  arc knows nothing about, and the error path (`fail`, `!`) is exactly
  where it is easy to forget. reach for `defer` (see [defer.md](defer.md)):
  a deferred statement runs on every exit from its scope — fall-through,
  `return`, `fail`, and `!` propagation alike — right before arc frees the
  locals, so the cleanup still sees them. for cleanup that should happen
  only when something went wrong — roll back a half-finished transaction,
  delete a partial file — use `errdefer`, which runs on the error exits
  and stays quiet on a normal return.
- **a result consumed with `catch` or `unwrap_or` can leak its ok
  value** when that value was freshly built (a returned tuple, a
  concatenated string). the consumer has no way to tell a fresh ok
  value from a borrowed one — a returned collection element — so it
  cannot release without risking a double-free on the borrowed case,
  and errs toward the leak. tuples themselves are fully reclaimed:
  a tuple frees its box at the last count and releases any heap value
  it holds, the same as a struct.
- **a result or optional bound to a name can leak its payload.** `T!` and
  `T?` lower to a three-slot value, and releasing one frees those slots
  without dropping the payload they own. a local whose every use is a
  probe (`.is_ok`, `.is_err`, `== none`) or a payload read (`.ok`, `.err`)
  now releases its payload: a probe never touches it, and a read borrows,
  taking a fresh count only where the value escapes. either way the local
  is still the last owner. any other use leaks as before — passed to a
  call, returned whole, consumed by `catch` or `unwrap_or`, bound by
  `if let`, or captured by a closure, which takes the shell and not the
  payload. writing `x := call()!` avoids the whole question — `!` hands
  the count to you rather than leaving it in the tuple.

none of these produce a dangling pointer; the discipline trades a
bounded leak for that guarantee.

## weak references

a `weak` field holds an optional target without keeping it alive. it is
how you break a strong cycle: in a parent/child graph, let the parent own
the child through a normal optional field and mark the child's back
pointer `weak`.

```pith
struct Node:
    value: Int
    mut next: Node?      # owns the next node
    weak parent: Node?   # refers back without owning
```

only an optional struct field can be `weak` — the checker rejects
anything else (E249). a weak field never touches its target's strong
count, so it cannot be the reference that holds a node alive. when the
last strong owner drops, the target is reclaimed even while weak
references still point at it.

reading a weak field resolves liveness on the spot. while the target is
alive the field reads back as `Some(target)`; once the target has been
reclaimed it reads back as `none`, so a weak reference never dangles:

```pith
if node.parent != none:
    print("parent: {node.parent.value().value}")   # alive
# ... after the parent's last strong owner drops ...
if node.parent != none:                             # now false
    ...                                             # not taken
```

under the hood a weak field stores the target pointer directly rather
than an owning box, takes a weak reference on assignment, and drops it in
the struct's destructor. the target's header carries a separate weak
count and a dead flag; the header outlives the value just long enough for
weak reads to observe that the value is gone and return `none`. the
`bench/cyclic_graph` benchmark builds and drops two million parent/child
rings: with the back edge `weak` peak memory stays flat (about 2 MB),
and with a strong back edge the same run leaks every ring (about 730 MB).

closures are reference counted like any other heap value. a closure
carries its own count and a release tag per captured slot; the last
release walks the environment, drops the count the closure took on
each captured value, and frees the box. a closure that dies locally is
released at scope exit, and one that escapes transfers its count the
same way a returned struct does — so building and discarding closures
in a loop holds flat, outside the cycle case above. naming a function
is the same story: `f := shout` allocates a closure that wraps the
function into the closure abi, and that closure is reclaimed at the
end of the scope it was bound in.

## threads and tasks

`spawn` runs a closure on a real os thread. reference counts are
atomic, so retaining and releasing the same value from two threads is
safe. the container *contents* are not synchronized, though: a list or
map is a plain buffer behind a handle, so two tasks mutating the same
collection race on that buffer.

the rule, until a checker enforces it: **do not share a mutable
collection across tasks — pass data through a channel instead.** a
channel hands the value over rather than aliasing it, which keeps each
task's mutations to itself. immutable values and independent copies
(`std.collections.copy_list` and friends) are also safe to hand off.
