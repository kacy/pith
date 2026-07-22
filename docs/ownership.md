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

collections (lists, maps, sets) own their elements. a string-valued
map retains values on insert and releases them when overwritten or
dropped. this only works if the map was built with the string-valued
constructor — see "container flavors" below.

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

call arguments:

- callees borrow their parameters; a callee that stores an argument
  adds its own count
- so an *owned* argument (a fresh `s[i]` char, a concat result, a
  fresh method result) dies once the call it fed returns — the caller
  releases it
- exception: an argument stored directly into a container transfers
  its count to the container, and the caller must not release it
  (`skip_pos` in `ir_release_owned_method_args`)

lambdas and function values:

- a lambda's return transfers like a function return: borrowed
  strings are retained on the way out (`ir_closure_return_kinds`
  records what a closure returns so call sites treat the result as
  owned)

## container flavors

a map or list is built by a constructor that matches its element
type: string-valued maps use the retaining constructor, int maps the
plain one. the flavor is chosen at emission time from the declared or
checked type — which is why an empty `{}` literal must know what it
is initializing. losing that information across a module boundary is
exactly the bug that broke the emitter split twice; imported globals
now carry their type kinds so a reassigned `{}` keeps the right
flavor.

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
- **removed or overwritten container elements are not released until
  the container itself dies.** a borrow of an element may still be in
  flight, so the free is deferred to the container's own cleanup.
- **arc reclaims memory, but it does not run your cleanup.** closing a
  file, rolling back a transaction, or releasing a lock is a side effect
  arc knows nothing about, and the error path (`fail`, `!`) is exactly
  where it is easy to forget. reach for `defer` (see [defer.md](defer.md)):
  a deferred statement runs on every exit from its scope — fall-through,
  `return`, `fail`, and `!` propagation alike — right before arc frees the
  locals, so the cleanup still sees them.
- **a result consumed with `catch` or `unwrap_or` can leak its ok
  value** when that value was freshly built (a returned tuple, a
  concatenated string). the consumer has no way to tell a fresh ok
  value from a borrowed one — a returned collection element — so it
  cannot release without risking a double-free on the borrowed case,
  and errs toward the leak. tuples themselves are fully reclaimed:
  a tuple frees its box at the last count and releases any heap value
  it holds, the same as a struct.

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
in a loop holds flat, outside the cycle case above.

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
