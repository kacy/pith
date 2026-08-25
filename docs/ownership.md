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
- a module-level global's slot owns its value the same way a local
  does, so assigning one releases what it held. nothing else would: a
  global is not in any function's exit cleanup, and the store itself
  only overwrites the slot. that release is what frees a container's
  elements, since a container's last count cascades into everything it
  owns — `xs = []` over a global used to strand every element the old
  container held, which is how the compiler front end grew tens of
  megabytes per analysis by resetting its tables the obvious way. a
  `threadlocal` global is the same rule per thread.
  `ir_assign_targets_global_slot` decides this, and it excludes two
  spellings. a `for` variable reads its own storage, so releasing there
  would drop a count the loop never took. an entry-module global whose
  name any function in the build also binds is left out altogether:
  entry globals are the ones emitted unprefixed, a local keeps its bare
  name, and the ir consumer resolves a bare name to a global's slot
  whenever one exists, so such a binding writes the global's slot and
  drops the count at its own scope exit. releasing again at the next
  assignment would be a double free, so those globals keep the old
  leak. the aliasing behind that exclusion is a separate, older defect:
  a local spelled like an entry global already reads and writes the
  global's slot, and crashes on its own with no release involved

call arguments:

- callees borrow their parameters; a callee that stores an argument
  adds its own count
- so an *owned* argument (a fresh `s[i]` char, a concat result, a
  fresh method result) dies once the call it fed returns — the caller
  releases it
- that covers every heap kind, not only strings. a container written
  straight into a call — `f([1, 2])`, `f({"k": v})`, `f(xs.map(g))` —
  is an allocation nothing else names, exactly like a struct literal, a
  lambda, or a fresh `Bytes`. `ir_owned_arg_kind_releases` is the list
  of kinds this applies to. leaving the container kinds off it is what
  made every such call strand its handle for the life of the process,
  about 165 bytes per list argument and 400 per map
- `tuple` is deliberately not on that list, and is the one kind that
  has to stay off. an optional and a result both lower to a tuple
  shell, so the kind does not say whether a register holds a real tuple
  literal or a wrapper whose ownership the result and optional
  extraction paths settle instead — `ir_emit_struct_positional_init`
  excludes tuple from its field retain for the same reason. a tuple
  written *syntactically* as a literal in an argument is still released,
  because the emitter can see the literal; it is a tuple-typed register
  of unknown origin that keeps the bounded leak
- exception: an argument stored directly into a container is skipped
  by that release (`skip_pos` in `ir_release_owned_method_args`),
  because ownership transfers into the container there rather than
  ending with the call. the store completes that transfer: an owned
  value goes through the variant of the store that keeps the caller's
  count instead of taking a second one — `pith_list_push_value_owned`,
  `pith_list_set_value_owned`, `pith_list_insert_value_owned`,
  `pith_map_insert_cstr_owned`, `pith_map_insert_ikey_owned`. the
  emitter picks the variant from the value's ownership alone
  (`ir_owned_container_store_name`); whether there is a count to take is
  the container's own business, since only it knows if it was built
  tagged. an untagged container takes nothing and the caller's count
  stays outstanding, which is a leak rather than an element nothing
  keeps alive
- a *borrowed* heap value stored into a container carries its kind with
  it, and the container takes the one count it needs —
  `pith_list_push_value_kind`, `pith_list_insert_value_kind` and
  `pith_list_set_value_kind` for a list, `pith_map_insert_cstr_kind`,
  `pith_map_insert_ikey_kind` and their `_owned_` twins for a map,
  chosen by `ir_list_kind_store_name` and `ir_map_kind_store_name` and
  gated on `ir_store_learns_kind`. the emitter adds no count of its own
  at a container store, which is what stopped a tagged list counting its
  elements twice. see "container flavors" for why the store rather than
  the constructor
- a channel send is the one store the emitter still counts for. a channel
  is not a counted container: it holds a raw handle between the send and
  the receive, with nothing on either side that could take a count, so
  the sender adds one (`ir_channel_send_needs_retain`) and the value
  outlives its local. that is a leak; a missing count would be a dangling
  element
- a callee that may hand an argument straight back out takes no count on
  either branch, so the caller takes one on the *result* instead.
  `m.get_default(k, d)` returns `d` unchanged when the key is missing and
  the map's own value when it is present, and neither is a count the
  caller owns. retaining the result settles both: on a miss the count
  lands on the fallback, which the ordinary owned-argument release then
  drops back to one; on a hit it lands on the map's value, which is what
  lets a container result outlive the entry it was read from. the order
  is load-bearing — retain first, because on the miss branch the release
  drops the very register the retain just counted.
  `ir_method_result_retains_over_args` names the callees this applies to,
  and `ir_expr_is_borrowed` classifies their result as owned to
  match. `unwrap_or` is the same shape and never reaches this code: both
  spellings have their own emitter and build the fallback inside the arm
  that returns it
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
count somewhere else. a set of closures needs no count at all, since a
set copies element bytes into its own storage rather than holding the
caller's handle.

a map's values carry the same tag a list's elements do — the runtime
shares one `ListTypeTag` and one retain/release pair between them — so a
map owns a count on every heap value it holds, of every kind. that used
to be cstring-only: a list, map, set, struct, bytes or closure written
into a map got no count from the map and none released on eviction, and
the emitter added a compensating count at the call site so the value
would at least outlive the caller's local. nothing ever dropped it.

the tag comes from the *store* rather than only the constructor, which
is what makes that sound. a container normally picks its flavor at
construction from the checked type, but `ir_emit_map_literal` reads its
value kind off the literal's first entry and an empty `{}` has none — so
`f({})` built a map that could never own what was put into it, however
well the checker had typed the parameter. a store instead carries the
value's kind (`pith_map_insert_cstr_kind` and friends) and the map
adopts it, so ownership no longer depends on the constructor having
guessed right. `MapImpl::adopt_value_tag` only adopts into an *empty*
map, which is what keeps the change count-neutral: there are no
already-stored values whose counts a new tag would start releasing. a
map that cannot adopt — one already holding values under a different
tag — takes the compensating count in the runtime instead, exactly the
one the emitter used to add, so the fallback is a leak and never a freed
value. the constructor still picks the flavor where it can, because a
map that owns strings has always been built that way and an int-keyed
map with primitive values keeps its scalar fast path; adoption drops out
of that fast path the moment the map holds handles.

a list store carries the element's kind for the same reason and with
the same fallback (`pith_list_push_value_kind`,
`pith_list_insert_value_kind`, `pith_list_set_value_kind`, and
`ListImpl::adopt_element_tag`), but it fixes the opposite error. a list
was tagged correctly nearly always, so the runtime store already
retained what it was handed — and the emitter, with nothing at the store
to tell a tagged list from an untagged one, retained a second time. only
one of the two counts was ever dropped, so every borrowed container or
struct pushed into a list outlived it, about 215 bytes a `List[List]`
store and 96 a `List[Row]` one. the kind is that missing evidence: the
list answers whether it owns the kind, takes exactly one count when it
does, and adds the compensating one itself when it does not. because the
same `store_kind` decides both whether to carry the kind and whether to
skip the emitter-side retain, the two cannot disagree. adoption is gated
on the list being empty, on 8-byte storage and on holding no other heap
kind already, so it is count-neutral, never reinterprets a narrower
element as a handle, and never retags a list whose constructor and store
disagree — that last one falls to the leak instead.
only the borrowed stores carry a kind; an owned value transfers its
count through the `_owned_` variants instead of taking one.

sets are a list element type too. the runtime's element tag had no
`Set` variant, so a `List[Set]` was the one collection-of-collections
that could not be tagged at all: `xs.push({1, 2})` stranded its element
for the life of the process, and the value position of `xs.insert(i, v)`
had to keep its count for want of anywhere to transfer it to. the
variant exists now (`pith_list_new_set`), which is what let `insert`
join the other stores on the transferring path.

the constructor a literal picks comes from the checked type of the
literal node, so for a list it is only as good as what the checker
recorded there (a map has the store as its second chance, above).
an empty `[]` or `{}` has no type of its own and takes one from context.
the checker propagates that context for an annotated bind, a `return`, a
struct constructor, an assignment and a call or method argument, and the
emitter separately reads the annotation itself for a bind. an empty `{}`
needs one extra step in either route, because the parser makes every
empty brace pair a `map` node: a `Set` target crosses the kinds, and
without that `m.get_default(k, {})` on a set-valued map built a map,
which quietly swallowed every `add` against it.

argument position is the newest of those and the narrowest.
`propagate_empty_argument_collection_type` retargets only a literal with
no elements, at the four places an argument's declared type is known —
`check_argument_type` for the builtins, `check_callable_type_with_args`
for calls and fn values, the user-method loop and the enum-payload loop.
an empty literal has nothing that could disagree with the parameter, so
the recorded type cannot be a lie and no element check is needed; a
literal that has elements types itself and is left exactly as it was, so
a genuinely wrong element type still reports rather than being stamped
over. what it builds is the container the equivalent two-line `mut xs:
List[String] := []` then `f(xs)` already built, which is a shape these
rules cover rather than a new one. the leftovers are in the gap list
below.

the lists `map` and `filter` produce pick their flavor the same way,
from the checked element type of the call, and the two differ in where
the count comes from. a mapper hands back a value nothing else is
holding, so the push transfers: the loop stops tracking the result
there and the list keeps the count the mapper returned. a filter keeps
the source list's own elements, so its push retains, which is what lets
the result outlive the list it was filtered from. an element type with
no constructor of its own — a boxed enum, an optional or a result —
still builds an untagged list, the same gap literals have.

a generic body is the one place the checked types cannot pick the
flavor, because specialization suppresses them. an empty list bound
with an annotation (`mut out: List[T] := []`) resolves the element
type from that annotation under the active substitution instead, so
`algo.sort_by_key` over structs builds a struct-tagged list whose
stores own their elements. an untagged out list owned nothing, and
everything it handed back lived on counts its source dropped at its
own scope exit.

the list methods implemented in the runtime rather than the emitter
decide their own flavor: `slice` and `sort` copy the source list's tag
(`reverse` builds nothing, it reorders in place), `split` is
string-tagged, `map.keys()` is tagged when the
keys are strings, and `map.values()` when the values are heap values.

## where the code lives

- the variable rules: statement lowering in
  `self-host/ir_emitter_core.pith` (binds, assigns, returns)
- the argument rules: `ir_release_owned_method_args`,
  `ir_release_owned_args` and the call paths near them. the
  kinds they release are `ir_owned_arg_kind_releases`; the position
  they must not release is the container store's, and the callees that
  take a count on their own result instead are
  `ir_method_result_retains_over_args`
- the element tags: `ListTypeTag` and the `pith_list_new_*`
  constructors in `cranelift/runtime/src/collections/list.rs`, the
  entries for them in `cranelift/runtime-abi/runtime_functions.txt`,
  the name mapping in `self-host/ir_metadata.pith`, and the kind-to-
  constructor table `ir_list_ctor_for_elem_kind`. adding a tag means
  all four
- the map value tag: `MapImpl.val_tag` and `adopt_value_tag` in
  `cranelift/runtime/src/collections/map.rs`, which reuse `ListTypeTag`
  and its `retain_element`/`release_element` pair so a map and a list
  can never disagree about what a kind means
- the kind-carrying stores: `ListImpl::adopt_element_tag` and the
  `pith_list_*_value_kind` entry points in `collections/list.rs`,
  `adopt_value_tag` and the `pith_map_insert_*_kind` ones in
  `collections/map.rs`. the emitter side is `ir_store_learns_kind`,
  `ir_list_kind_store_name`, `ir_map_kind_store_name` and
  `ir_element_tag_code` — that last one writes the wire codes, which are
  part of the ir contract and must not be renumbered
- the runtime counts: `cranelift/runtime/src/runtime_core.rs` and the
  per-type files under `collections/`

if you are adding a statement or call shape, decide explicitly which
of the rules above applies to every string that flows through it —
"it seemed to work" is how the dangling ones got in.

## what is and isn't reclaimed

reference counting frees a value the moment its last count drops, with
no gc pause. that covers the common case completely. the deliberate
gaps, all bounded leaks rather than dangling pointers:

- **unmarked reference cycles leak by default.** a graph that closes a
  loop of strong references keeps its own count above zero, so arc alone
  never reclaims it. a struct can name its own type in a field and an
  optional field owns its target, so a linked structure — a parent that
  owns a child which points back at the parent — closes a strong cycle.
  the first-class fix is the `weak` keyword (below): mark the back edge
  `weak` — a field, a local binding, or a binding a closure captures —
  and the ring reclaims deterministically.
  `pith lint` flags a strong cycle between a module's structs (E306) at
  a field that can break it, so the edge is named before the leak is.

  for cycles nobody marked there is an experimental trial-deletion
  collector, off by default and enabled by running the process with
  `PITH_CYCLE_GC=1`. with the flag on, a release that leaves a count
  above zero records the object as a *suspect* — the only place a
  garbage cycle can root — and a background thread periodically stops
  the world, subtracts the edges internal to the suspect graph from each
  member's count, and frees the members no outside reference still
  holds (Bacon-Rajan trial deletion). a collection runs when enough
  suspects accumulate (`PITH_CYCLE_GC_THRESHOLD`, default 4096), or on
  demand from `std.concurrent.gc_collect()`, which waits for the attempt
  and reports whether it ran; `std.concurrent.gc_stats()` prints the
  collector's counters. some shapes stay uncollectable even with the
  flag on and degrade to the leak they always were: a cycle through a
  channel's buffer (a channel holds raw handles, not counted edges), a
  cycle through a task handle, and a cycle through a container that
  never learned its element kind — none of those edges are visible to
  the collector's tracers. with the flag off — the default — the only
  cost is one predicted branch per release, and cycles behave exactly
  as this section always described: they leak, bounded, never dangling.

  the flag stays off by default on measured grounds (2026-08-16, five
  interleaved trials per bench, flag off vs on with the same binaries).
  the automatic threshold mode cannot keep up with churn-heavy cycle
  creation: a tight loop building and dropping a two-node strong ring
  filled the 65,536-suspect buffer almost immediately, and 378,340
  suspects from a 200,000-round run overflowed — an overflowed suspect
  is discarded, and if it rooted a garbage ring that ring is
  permanently uncollectable. only 3 collections ran, about a third of
  the rings were freed, and peak rss came out *higher* than with the
  flag off (117 mb vs 103 mb at 400k rings) because the suspect buffer
  sits on top of the leak. the time cost is real too: the cycle-churn
  bench runs about 2x slower flag-on (120 → 250 ms median) and the
  std-pipeline bench about 20% slower (550 → 670 ms, disjoint ranges);
  the catalog, event-ledger and channel-fanout benches read neutral.
  what does work is the flag plus an explicit
  `std.concurrent.gc_collect()` at points the program chooses — the
  leak gate's ring case runs flat that way, collecting every 4096
  rounds, where the same binary grows by a ring per round on the
  threshold alone. so: prefer `weak` edges; if you need the collector,
  run with `PITH_CYCLE_GC=1` and call `gc_collect()` at natural
  quiesce points rather than relying on the threshold.
- **a value sent down a channel outlives its local.** a channel holds a
  raw handle between the send and the receive and is not a counted
  container, so the sender adds a count nothing drops. it is the last
  store the emitter counts for; every other one carries the value's kind
  and lets the container decide.
- **arc reclaims memory, but it does not run your cleanup.** closing a
  file, rolling back a transaction, or releasing a lock is a side effect
  arc knows nothing about, and the error path (`fail`, `!`) is exactly
  where it is easy to forget. reach for `defer` (see [defer.md](defer.md)):
  a deferred statement runs on every exit from its scope — fall-through,
  `return`, `fail`, and `!` propagation alike — right before arc frees the
  locals, so the cleanup still sees them. for cleanup that should happen
  only when something went wrong — roll back a half-finished transaction,
  delete a partial file — use `errdefer`, which runs on the error exits
  and stays quiet on a normal return. `pith lint` reports the narrow case
  where a resource is built locally, used in place and never closed
  (E307). the full survey of what needs a manual close, and the plan for
  whether a destructor hook should ever replace the convention, is in
  [destructors_roadmap.md](destructors_roadmap.md).
- **a result consumed with `catch` or `unwrap_or` can leak its ok
  value** when that value was freshly built (a returned tuple, a
  concatenated string). the consumer has no way to tell a fresh ok
  value from a borrowed one — a returned collection element — so it
  cannot release without risking a double-free on the borrowed case,
  and errs toward the leak. tuples themselves are fully reclaimed:
  a tuple frees its box at the last count and releases any heap value
  it holds, the same as a struct.
- **a tuple literal written straight into a call is released once the
  call returns**, the same as every other owned argument kind. the kind
  alone could never justify this — an optional and a result both lower
  to a tuple shell, so a tuple-TYPED register might be a wrapper whose
  count the extraction paths still hand on — but a syntactic `(a, b)` in
  the source is unambiguous: the box is one only this call ever saw, and
  a storing callee takes its own count. any other tuple-typed register
  in argument position keeps the old bounded leak rather than risking a
  double free.
  a fresh OPTIONAL in that position is narrower and is now reclaimed:
  when the argument expression is itself a call — `take(mk(i))` — the
  register is a wrapper the caller alone owns, and the same callee-body
  walk that qualifies a named tuple local's call argument proves the
  callee leaves the caller sole owner; a qualifying argument releases
  its live payload and shell through the cascade helper right after the
  call returns. a callee that extracts an rc-payload optional, or one
  the walk cannot read (imported, builtin, method, closure), keeps the
  argument on the leak side as before. results in argument position, and
  tuple-typed registers the emitter cannot tie back to a literal, still
  strand their box.
- **a collection literal the checker cannot type builds an untagged
  container.** `[]` and `{}` have no type of their own and take one from
  context. an annotated bind, a `return`, a struct constructor, an
  assignment and a call or method argument all supply one, so `f([])`,
  `xs.push([])`, `xs.insert(i, [])` and `m.get_default(k, {})` build the
  same tagged container `mut xs: List[String] := []` then `f(xs)` builds.
  an optional parameter is covered too: `f(v: List[String]?)` takes the
  type from the container inside the optional and wraps the tagged
  container in a `Some`, which is also what stops the callee reading a
  bare list handle as an optional tuple. a nested literal is covered too:
  an argument literal that HAS elements but checks as an error because an
  empty literal is nested inside (`f([[]])`) runs the same element-checked
  coercion an assignment uses, so the declared type is recorded through
  every nested literal and the inner list is tagged. the safety net for
  anything that still slips through is the store: an untagged list handed
  a container, struct, `Bytes` or closure adopts that kind at the first
  store and owns it from then on, the same second chance a map has —
  string elements are the one kind the second chance does not cover,
  because string stores stay on the constructor path. an empty *map*
  literal is not on this list at all: its values are counted from the
  store rather than the constructor, so `f({})` owns what is put into it
  whatever the checker managed to record.
- **a result or optional bound to a name can leak its payload.** `T!` and
  `T?` lower to a three-slot value, and releasing one frees those slots
  without dropping the payload they own. a local whose every use is a
  probe (`.is_ok`, `.is_err`, `== none`), a payload read (`.ok`, `.err`),
  a `match` on an optional local, an argument of a same-module call whose
  body provably only reads the parameter, or — for a result — an
  extraction (`r.unwrap_or(d)`, `r catch d`, `r!`) releases its payload:
  a probe never touches it, a read borrows, a qualifying callee takes no
  count of its own (and retains anything a result extraction hands out),
  and an extraction hands its consumer a freshly retained count, so in
  every case the local is still the last owner of the count the shell
  arrived with. a match qualifies because its arm binding retains its own
  count — tracked like any owned local, released by the exit cleanup or
  transferred by a return — so the subject keeps the count it had. a
  match on a RESULT local still disqualifies: its binding path takes no
  count of its own.
  the call-argument entry is judged by walking the callee's declaration
  with the same use rules as the local itself, transitively through
  same-module forwarding — so a callee that stores, returns, sends, or
  captures the parameter disqualifies the argument, and an imported,
  builtin, constructor, or closure-typed callee (whose body cannot be
  walked) is never a qualifying position. only a bare-name free-function
  call qualifies; a method-call argument (`obj.foo(r)`) stays on the leak
  side, since resolving the receiver's method to a body is a separate
  question left unwalked. any other use leaks as before
  — returned whole, bound by `if let`, passed across a module boundary,
  or captured by a closure, which takes the shell and not the payload.
  an optional extracted *inside* the callee (`o.unwrap_or(d)`) also
  disqualifies the argument, deliberately: that extraction transfers the
  borrowed shell's payload count out, so the caller is no longer the
  payload's owner. one more shape stays on the leak side: rebinding one
  name to results of different payload types (`r := await ta` then
  `r := await tb` where the two tasks return different `T!`s) — the
  cleanup path is keyed by name and cannot pick one payload shape, so it
  frees only the shell. bind results of different types to different
  names.

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

a local binding can be weak too. `weak name := expr` holds a struct value
without keeping it alive, and every read yields the same optional a weak
field read does — `Some(target)` while a strong owner still exists,
`none` once the target has been reclaimed:

```pith
mut cache: List[Session] := [Session(id: 1)]
weak current := cache[0]
# ... later, possibly after the cache evicted the session ...
if current == none:
    print("session gone")
```

a weak binding takes its type from its value (no annotation), must hold a
struct, and is immutable — a weak slot is an edge that either still
reaches its target or does not, not a variable to swap (E261 for each).
its name must also be unique in its function: the slot is keyed by name
across the whole function body, so reusing it for any other binding —
strong, loop variable, or match payload — is rejected rather than left to
read through the wrong lowering.

a closure captures a weak binding weakly. the environment slot holds the
bare target pointer under a weak reference — never a strong count — and
every read inside the closure resolves liveness fresh, exactly like the
binding itself. that is what lets a closure over stream state break its
own cycle: a callback stored on the object it reads from is the classic
strong ring (object owns closure, closure owns object), and routing the
back edge through a weak binding tears it down when the object's owner
drops:

```pith
mut s := Stream(val: 1, cb: fn(x: Int) => x)
weak ws := s
s.cb = fn(x: Int):
    if ws == none:
        return 0 - 1              # the stream is gone; report, not crash
    return ws.value().val + x     # alive: read through the weak edge
```

`tests/leaks/leak_weak_capture_cycle` pins this flat: the same ring with a
strong capture grows by every ring ever built, and with the weak capture
it holds steady no matter how many rounds run.

one closure interaction is still rejected rather than half-supported: a
weak binding cannot be DECLARED inside a lambda body (E261) — lambda
bodies share the enclosing emission's local tracking today, so the
binding would lower against the wrong frame. declare the weak binding
outside and capture it instead, which is also the shape the cycle break
needs.

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

`spawn` runs a closure on a green task, which linux schedules across a
pool of os threads (see [concurrency.md](concurrency.md)). reference
counts are atomic, so retaining and releasing the same value from two
of those threads is safe. the container *contents* are not synchronized, though: a list or
map is a plain buffer behind a handle, so two tasks mutating the same
collection race on that buffer.

the rule, until a checker enforces it: **do not share a mutable
collection across tasks — pass data through a channel instead.** a
channel hands the value over rather than aliasing it, which keeps each
task's mutations to itself. immutable values and independent copies
(`std.collections.copy_list` and friends) are also safe to hand off.
