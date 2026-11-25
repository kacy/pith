# String and collection ownership

This document describes how the compiler decides where retain and release
calls go. The rules are small, and each one matters: every leak, dangling
pointer, and double free the emitter has ever produced traces back to one
of them being broken somewhere.

## What is guaranteed

A program that compiles never reads freed memory, frees a value twice, or
writes past a buffer, including when several tasks run at once. Three
things fall outside that guarantee: a strong cycle with no `weak` edge
leaks, some runtime errors (an index out of range outside a `T!`
function, a stack overflow) stop the process, and a send that races a
channel close can lose its value. The rules the compiler enforces on
values that tasks share are listed in docs/concurrency.md.

## The model

Heap strings carry a refcount in their header
(`cranelift/runtime/src/runtime_core.rs`). Retain and release are
no-ops for string literals and null, so emitted code never needs to
know where a string came from: it can retain or release any string
register safely.

Collections (lists, maps, sets) own their elements. A tagged container
holds exactly one count per heap element. The count is taken on insert
and dropped the moment that element leaves, whether it was removed,
overwritten, cleared, or carried off when the container itself is freed.
This only works if the container was built with the constructor that
matches its element type (see "Container flavors" below).

Because eviction drops the container's count, anything that reads an
element and keeps it past a later mutation must hold a count of its
own. The emitter arranges that: binding, assigning, or storing a
borrowed element retains it first. Sets need no count at all, since
they copy element bytes into their own storage instead of holding the
caller's handle.

## The rules

Variables:

- Every string a variable holds is one owned count.
- Binding or assigning a *borrowed* string (a variable read, a
  collection element, a struct field) retains it first. Fresh call
  results already arrive owned.
- Rebinding or reassigning releases the value being replaced.
- A name bound under different kinds at different sites (a string in
  one branch and an int in another, a list in one match arm and bytes
  in the next) has no single kind to release it under, so it gets a
  side slot recording the kind of the count its current value holds.
  Each binding site retains a borrowed value under its own kind,
  releases the outgoing value under the recorded kind, and records its
  own kind in its place: none for a plain value, or for a view that
  takes no count, such as a tuple pattern's element. The exit cleanup
  releases whatever the last binding on the path taken left in the
  slot. Returning such a name retains the value for the caller instead
  of handing over the slot's count (#1137).
- A return hands its count to the caller: a returned variable is
  excluded from the function's exit cleanup, and a returned borrow is
  retained.
- Each return releases every string local the function owns.
- A `for` loop variable is a borrow rather than an owned local: it
  reads an element the container still holds, and takes no count of
  its own. So it gets storage of its own for the length of the body,
  which keeps it out of the slot a local of the same name uses. In that
  slot, the rebind's release-the-old and the exit cleanup on an early
  return would drop a count the loop never took. A `return` or `fail`
  of the loop variable retains it like any other borrow and leaves a
  local of the same name in the exit cleanup, and a `defer` written
  before the loop reads that local even when an exit inside the body
  runs it. The one exception is a loop over a string: `char_at` mints a
  fresh one-byte string per iteration and no container holds it, so the
  loop owns that count. The slot is released at the step label (which
  `continue` reaches) and cleared, at the end label (which `break`
  reaches with the byte still bound), and at the function's exit edges
  alongside the loop's owned iterable. The body still sees a borrow, so
  `return ch` and `x := ch` retain as they always did.
- A list loop owns its variable the same way when the body could drop
  an element: when it writes a field, an element or a global, calls a
  container method that evicts an element, or calls code not proven to
  avoid all of those (`ir_subtree_keeps_storage`). Without the count,
  `for s in xs: xs.clear()` freed `s` while the body still read it. Such
  a loop also stops at the list's current length, so an element the body
  removed is never read from an emptied slot, while the length taken at
  the start still caps the walk. A body that keeps storage, which covers
  most loops, borrows as before and pays nothing.
- A catch-all arm (`x => ...`) and the names in a tuple pattern are views
  of the match subject and take no count of their own. When the subject is
  borrowed and an arm could drop it, the match pins the subject in a hidden
  local for its length (`ir_match_pin_kind`). An arm can drop it when it
  writes a field, an element or a global, calls code not proven to keep
  storage, or rebinds the local the subject was read from. The merge
  releases the pin. A return leaves it to the exit cleanup, and a break or
  a continue leaves it to the next pass through the match. Without the pin,
  `match h.c: x => h.c = other` freed `x` while the arm still read it. A
  match whose arms keep storage pays nothing.
- A module-level global's slot owns its value the same way a local
  does, so assigning one releases what it held. Nothing else would: a
  global is not in any function's exit cleanup, and the store itself
  only overwrites the slot. That release is what frees a container's
  elements, since a container's last count cascades into everything it
  owns. A `threadlocal` global follows the same rule per task (per
  thread on the OS-thread backend), and the runtime drops its count when
  the task ends. `ir_assign_targets_global_slot` decides this, and it
  excludes two spellings, both of which read storage of their own. A
  `for` variable reads the loop's slot, so releasing there would drop a
  count the loop never took. A local, a parameter or a pattern binding
  spelled like the global reads the function's own storage: a global is
  emitted under a reserved symbol (`__g_<name>`), a binding keeps its
  written name, and the two no longer meet in the flat IR namespace. That
  split is what lets the release be unconditional.

Call arguments:

- Callees borrow their parameters. A callee that stores an argument
  adds its own count.
- An argument read out of a field, a container element or a `mut` global
  (`f(o, o.name)`, `g(xs, xs[0])`, `h(config)`) is pinned for the call:
  retained when it is evaluated and released when the call returns. The
  borrow alone left the owner's count as the only one, and a callee that
  overwrote the field, evicted the element or reassigned the global freed
  its own parameter. A builtin, or a function recorded by
  `ir_record_storage_keepers` as writing no field, element or global,
  cannot do that, so its arguments are not pinned. A local, a parameter or
  a global that is not `mut` is never pinned, since nothing outside the
  calling frame can rebind it.
- An unlock, a `done`, a semaphore release or an `AtomicInt` store or
  `compare_set` lets another task take a lock the caller held and replace
  what the caller read under it, so a callee that makes one is not a
  storage keeper. The one exception is a compare_set that returns at once
  when it succeeds (`if cell.compare_set(old, new): return`), the shape of
  a lock-free retry loop. A function whose only releases look like that
  uses none of its arguments after one, so its arguments are not pinned
  either. A function that calls it does run on afterwards, so the
  exception does not carry over to its callers.
- A read out of a field or an element is pinned the same way, whatever
  the callee, when an operand evaluated after it can drop storage
  (`ir_later_operands_drop`). This covers the left side of a concat, an
  interpolation part, a tuple element, a map key, a builtin's argument or
  receiver, and the container, key and receiver of a store. For example,
  `o.name + rename(o)`, `m.insert(o.name, rename(o))` and
  `h.o.name = swap(h)` each freed the read before it was used.
- A builtin method result the emitter classifies as borrowed is pinned
  the same way. `s.to_string()` on a String is `s` itself, so it is pinned
  when `s` would be: `f(o.name.to_string(), rename(o))` freed the name
  under the argument. A list search (`o.items.contains(rename(o))`,
  `index_of`) pins its list against its target, and a reduce whose
  initial value can drop storage walks a copy of its list, as it does
  when its function can.
- An assignment reads the value it replaces only after the new value has
  been evaluated and has taken its count. `o.name = rename(o)` used to read
  the old name first, and `rename` had already released it, so the store
  released it a second time. The same order applies to a global.
- `xs.get(i)`, `xs.first()`, `xs.last()` and `m.get(k)` return a fresh
  optional around an element the container still owns. When the optional
  is kept (bound, stored, returned or passed on), the call site retains the
  element and gives the optional its destructor, so a later `xs.clear()`
  cannot free the element under the local. An extraction that uses the
  optional on the spot (`if let`, `while let`, a match subject, `.value()`,
  `unwrap_or`, a test against `none`, an interpolation part) leaves it
  borrowing, so a loop of `if let x = xs.get(i)` takes no counts.
- So an *owned* argument (a fresh `s[i]` char, a concat result, a
  fresh method result) dies once the call it fed returns. The caller
  releases it.
- That covers every heap kind, not only strings. A container written
  straight into a call (`f([1, 2])`, `f({"k": v})`, `f(xs.map(g))`)
  is an allocation nothing else names, exactly like a struct literal, a
  lambda, or a fresh `Bytes`. `ir_owned_arg_kind_releases` is the list
  of kinds this applies to.
- `tuple` is deliberately not on that list, and is the one kind that
  has to stay off. An optional and a result both lower to a tuple
  shell, so the kind does not say whether a register holds a real tuple
  literal or a wrapper whose ownership the result and optional
  extraction paths settle instead. `ir_emit_struct_positional_init`
  excludes tuple from its field retain for the same reason. A tuple
  written *syntactically* as a literal in an argument is still released,
  because the emitter can see the literal. The bounded leak remains for a
  tuple-typed register of unknown origin.
- Exception: an argument stored directly into a container is skipped
  by that release (`skip_pos` in `ir_release_owned_method_args`),
  because ownership transfers into the container there instead of
  ending with the call. The store completes that transfer: an owned
  value goes through the variant of the store that keeps the caller's
  count instead of taking a second one, namely
  `pith_list_push_value_owned`, `pith_list_set_value_owned`,
  `pith_list_insert_value_owned`, `pith_map_insert_cstr_owned` and
  `pith_map_insert_ikey_owned`. The emitter picks the variant from the
  value's ownership alone (`ir_owned_container_store_name`). Whether there
  is a count to take is the container's own business, since only it knows
  if it was built tagged. An untagged container takes nothing and the
  caller's count stays outstanding, which is a leak rather than an element
  nothing keeps alive.
- A *borrowed* heap value stored into a container carries its kind with
  it, and the container takes the one count it needs. The list stores
  are `pith_list_push_value_kind`, `pith_list_insert_value_kind` and
  `pith_list_set_value_kind`, and the map stores are
  `pith_map_insert_cstr_kind`, `pith_map_insert_ikey_kind` and their
  `_owned_` twins. They are chosen by `ir_list_kind_store_name` and
  `ir_map_kind_store_name` and gated on `ir_store_learns_kind`. The
  emitter adds no count of its own at a container store, which is what
  stopped a tagged list counting its elements twice. See "Container
  flavors" for why the count is taken at the store and not by the
  constructor.
- `m[k] = m[k] + d` and `m.insert(k, m[k] + d)` lower to one call that
  reads, adds and writes the slot in a single probe
  (`pith_map_upsert_add_cstr` and its `_ikey` and `_bkey` twins, chosen by
  `ir_map_upsert_add_call_name`). It is not an exception to any of the
  above: the emitter only reaches for it when the map's values are
  integers, which is a map that owns no counts at all, and the runtime
  exits loudly if it is ever handed a counted one.
- A channel send is a store into a counted container, so a borrowed value
  sent takes a count there (`ir_channel_send_needs_retain`). Whoever
  receives the value consumes that count (the receive hands its optional
  shell a destructor for the payload). Otherwise the channel's own free
  consumes it: the free drains whatever was never received and releases
  each value by the payload tag the constructor recorded. An owned value
  transfers.
- A callee that may hand an argument straight back out takes no count on
  either branch, so the caller takes one on the *result* instead.
  `m.get_default(k, d)` returns `d` unchanged when the key is missing and
  the map's own value when it is present, and neither is a count the
  caller owns. Retaining the result settles both. On a miss the count
  lands on the fallback, which the ordinary owned-argument release then
  drops back to one. On a hit it lands on the map's value, which is what
  lets a container result outlive the entry it was read from. The order
  matters: retain first, because on the miss branch the release drops
  the very register the retain just counted.
  `ir_method_result_retains_over_args` names the callees this applies to,
  and `ir_expr_is_borrowed` classifies their result as owned to
  match. `unwrap_or` is the same shape and never reaches this code: both
  spellings have their own emitter and build the fallback inside the arm
  that returns it.
- An index key is an argument like any other, and the container-store
  exception does not cover it. A lookup reads the key and keeps nothing,
  and a store copies the key bytes into the map's own storage, so an owned
  key in `m[a + ":" + b]` dies with the operation either way. The stored
  *value* is still under the exception. A map *literal* is a store like
  any other and follows both rules: `{k(): "v-" + tag}` releases the key
  and transfers the value. It did not do so until the two lines that say
  so were written into `ir_emit_map_entry_insert`. A set element is a key
  and not a value: a set copies the element bytes too, so a set literal
  releases a freshly built element and takes nothing on it. The object
  being indexed is deliberately left alone. Container indexing borrows the
  element out of the container, so releasing a temporary container there
  would free the value just returned.

Lambdas and function values:

- A lambda's return transfers like a function return: a borrowed value
  of any RC kind (a string, a captured optional shell or result box, a
  struct, a container) is retained on the way out.
  `ir_closure_return_kinds` records what a closure returns so call sites
  treat the result as owned.
- Naming a function as a value mints a closure. `f := shout` and
  `xs.map(shout)` both lower to `closure_ref`, which allocates with a
  count of one, so the expression is owned the same way a lambda
  literal is: the bind transfers instead of retaining, and an argument
  position releases after the call. `ir_expr_is_named_function_value`
  decides this, and it mirrors the two emit paths that allocate: a
  bare name (`ir_emit_ident_load`) and a module member such as
  `hash.sha256_bytes` (`ir_emit_field_access_expr`). Saying "owned"
  where no closure was allocated would release a count nobody took.
- Calling a fn value the expression itself produced (`mk(i)(1)`, an
  immediately-invoked lambda, a payload pulled out of an optional)
  releases that callee once the call returns. The callee is a value
  like any other and the call is its only consumer, so skipping the
  release stranded the whole environment allocation, once per call.
  A borrowed callee (a local, a struct field, a container element)
  belongs to its owner and is left alone. A plain name never reaches
  this path at all, because it lowers to a direct call.
- A closure called out of a struct field or a list element (`h.f(h.name)`,
  `fs[0](x)`) is pinned for the call, along with any field or element
  arguments, the same way an argument read from one is pinned. The body is
  user code and can overwrite the field or empty the list. Without the pin,
  `h.f = g` inside the closure freed the environment it was running in.
- `map`, `filter` and `reduce` open-code their loop instead of going
  through the general method call, so the release that reclaims an
  owned argument does not run for them. Each releases the function it
  was handed once the loop is done, and only when that function
  arrived owned. A local or a struct field holding the closure
  belongs to its owner.
- Those loops hold only borrows while they call the function: of the
  list, of each element, and of a function read out of a field. When the
  function can drop storage (a lambda whose body does not keep storage, a
  function not recorded as a keeper, or any closure value), the loop walks
  a copy of the list, which counts every element, and pins the function.
  Without the copy, `o.items.map(f)` where `f` replaces `o.items` freed the
  list in the middle of the walk.
- A list nothing else holds (`xs.map(f)` as the receiver of `.filter(g)`,
  a literal, a call result) can't be dropped by anyone, so it is walked as
  it is and released once the walk is done. It used to be walked and never
  released, so every chained `map`, `filter` and `reduce` leaked its
  receiver.
- A reduce's accumulator holds one count of its value. A borrowed initial
  value takes one before the walk, and each step releases the value it
  handed the function once the function has returned the next one, so the
  result arrives owned. The steps used to release nothing, which leaked
  every intermediate value, and a reduce over an empty list handed back a
  borrowed initial value as if it were owned.

## Container flavors

A map or list is built by a constructor that matches its element
type: string-valued maps use the retaining constructor, and int maps the
plain one. The flavor is chosen at emission time from the declared or
checked type, which is why an empty `{}` literal must know what it
is initializing. Losing that information across a module boundary is
exactly the bug that broke the emitter split twice. Imported globals
now carry their type kinds so a reassigned `{}` keeps the right
flavor.

Closures are an element type like any other: a `List[fn(...) -> T]`
uses the closure constructor, so its push retains and its free-time
cascade releases. A route table and a middleware chain are exactly
that shape, and untagged they held handles they did not own. That
was survivable only while every closure that reached one was leaking a
count somewhere else. A set of closures needs no count at all, since a
set copies element bytes into its own storage instead of holding the
caller's handle.

A map's values carry the same tag a list's elements do (the runtime
shares one `ListTypeTag` and one retain/release pair between them), so a
map owns a count on every heap value it holds, of every kind.

The tag comes from the *store* rather than only the constructor, which
is what makes that sound. A container normally picks its flavor at
construction from the checked type, but `ir_emit_map_literal` reads its
value kind off the literal's first entry and an empty `{}` has none. So
`f({})` built a map that could never own what was put into it, however
well the checker had typed the parameter. A store instead carries the
value's kind (`pith_map_insert_cstr_kind` and friends) and the map
adopts it, so ownership no longer depends on the constructor having
guessed right. `MapImpl::adopt_value_tag` only adopts into an *empty*
map, which is what keeps the change count-neutral: there are no
already-stored values whose counts a new tag would start releasing. A
map that cannot adopt (one already holding values under a different
tag) takes a compensating count in the runtime instead, so the
fallback is a leak and never a freed value. The constructor still picks
the flavor where it can, because a map that owns strings has always been
built that way and an int-keyed map with primitive values keeps its
scalar fast path. Adoption drops out of that fast path the moment the
map holds handles.

A list store carries the element's kind for the same reason and with
the same fallback (`pith_list_push_value_kind`,
`pith_list_insert_value_kind`, `pith_list_set_value_kind`, and
`ListImpl::adopt_element_tag`), but it fixes the opposite error. A list
was tagged correctly nearly always, so the runtime store already
retained what it was handed, and the emitter, with nothing at the store
to tell a tagged list from an untagged one, retained a second time. Only
one of the two counts was ever dropped, so every borrowed container or
struct pushed into a list outlived it, about 215 bytes a `List[List]`
store and 96 a `List[Row]` one. The kind is that missing evidence: the
list answers whether it owns the kind, takes exactly one count when it
does, and adds the compensating one itself when it does not. Because the
same `store_kind` decides both whether to carry the kind and whether to
skip the emitter-side retain, the two cannot disagree. Adoption is gated
on the list being empty, on 8-byte storage and on holding no other heap
kind already. So it is count-neutral, never reinterprets a narrower
element as a handle, and never retags a list whose constructor and store
disagree. That last case falls to the leak instead.
Only the borrowed stores carry a kind. An owned value transfers its
count through the `_owned_` variants instead of taking one.

Sets are a list element type too: a `List[Set]` is built by
`pith_list_new_set`, so `xs.push({1, 2})` and `xs.insert(i, v)` transfer
their counts like the other stores.

The constructor a literal picks comes from the checked type of the
literal node, so for a list it is only as good as what the checker
recorded there (a map has the store as its second chance, above).
An empty `[]` or `{}` has no type of its own and takes one from context.
The checker propagates that context for an annotated bind, a `return`, a
struct constructor, an assignment and a call or method argument, and the
emitter separately reads the annotation itself for a bind. An empty `{}`
needs one extra step in either route, because the parser makes every
empty brace pair a `map` node: a `Set` target crosses the kinds. Without
that step, `m.get_default(k, {})` on a set-valued map built a map,
which quietly swallowed every `add` against it.

Argument position is the newest of those and the narrowest.
`propagate_empty_argument_collection_type` retargets only a literal with
no elements, at the four places an argument's declared type is known:
`check_argument_type` for the builtins, `check_callable_type_with_args`
for calls and fn values, the user-method loop and the enum-payload loop.
An empty literal has nothing that could disagree with the parameter, so
the recorded type cannot be a lie and no element check is needed. A
literal that has elements types itself and is left exactly as it was, so
a genuinely wrong element type still reports instead of being stamped
over. What it builds is the container the equivalent two-line `mut xs:
List[String] := []` then `f(xs)` already built, and these rules
cover that shape. The leftovers are in the gap list below.

The lists `map` and `filter` produce pick their flavor the same way,
from the checked element type of the call, and the two differ in where
the count comes from. A mapper hands back a value nothing else is
holding, so the push transfers: the loop stops tracking the result
there and the list keeps the count the mapper returned. A filter keeps
the source list's own elements, so its push retains, which is what lets
the result outlive the list it was filtered from. An element type with
no constructor of its own (a boxed enum, an optional or a result)
still builds an untagged list, the same gap literals have.

A generic body is typed against each set of concrete types just before it
is emitted (docs/generics.md), so the checked types pick the flavor there
too. `mut out: List[T] := []` at `T = Post` builds a struct-tagged list
whose stores own their elements. An annotation is still resolved under the
active substitution for the same answer. A struct built inside the body,
a plain one or a generic one by its bare name, lowers inline: each
borrowed field takes a count of its own and the instance gets its
destructor, the same as the identical line outside a generic body. Every
bind, push and field store in the body takes its count the way a concrete
body's does, and the exit releases what the frame still holds.

The shape that once kept a bare-name generic instance from carrying a
destructor is safe for the same reason. The timer engine in std.term.app
copies each spec out of a command's list into the engine's own list inside
a generic body. When that body took no counts, the copy rode on the
command's count, and a destructor on the command freed the spec and its
closure while the engine still held both. With the body typed, the push
into the engine's list takes its own count, so the command's destructor
drops only the command's. The tests tests/cases/test_generic_container_share
and test_generic_bare_construction_dtor pin the shape.

The list methods implemented in the runtime instead of the emitter
decide their own flavor: `slice` and `sort` copy the source list's tag
(`reverse` builds nothing; it reorders in place), `split` is
string-tagged, `map.keys()` is tagged when the
keys are strings, and `map.values()` when the values are heap values.

## Where the code lives

- The variable rules: statement lowering in
  `self-host/ir_emitter_core.pith` (binds, assigns, returns).
- The argument rules: `ir_release_owned_method_args`,
  `ir_release_owned_args` and the call paths near them. The
  kinds they release are `ir_owned_arg_kind_releases`, the position
  they must not release is the container store's, the callees that
  take a count on their own result instead are
  `ir_method_result_retains_over_args`, and the one whose receiver
  relinquishes its own count into the result (map `take`) is
  `ir_method_result_transfers_count`.
- The element tags: `ListTypeTag` and the `pith_list_new_*`
  constructors in `cranelift/runtime/src/collections/list.rs`, the
  entries for them in `cranelift/runtime-abi/runtime_functions.txt`,
  the name mapping in `self-host/ir_metadata.pith`, and the
  kind-to-constructor table `ir_list_ctor_for_elem_kind`. Adding a tag
  means changing all four.
- The map value tag: `MapImpl.val_tag` and `adopt_value_tag` in
  `cranelift/runtime/src/collections/map.rs`, which reuse `ListTypeTag`
  and its `retain_element`/`release_element` pair so a map and a list
  can never disagree about what a kind means.
- The kind-carrying stores: `ListImpl::adopt_element_tag` and the
  `pith_list_*_value_kind` entry points in `collections/list.rs`, and
  `adopt_value_tag` and the `pith_map_insert_*_kind` ones in
  `collections/map.rs`. The emitter side is `ir_store_learns_kind`,
  `ir_list_kind_store_name`, `ir_map_kind_store_name` and
  `ir_element_tag_code`. That last one writes the wire codes, which are
  part of the IR contract and must not be renumbered.
- The runtime counts: `cranelift/runtime/src/runtime_core.rs` and the
  per-type files under `collections/`.

If you are adding a statement or call shape, decide explicitly which
of the rules above applies to every string that flows through it.
"It seemed to work" is how the dangling ones got in.

## What is and isn't reclaimed

Reference counting frees a value the moment its last count drops, with
no GC pause. That covers the common case completely. The deliberate
gaps are all bounded leaks rather than dangling pointers:

- **Unmarked reference cycles leak by default.** A graph that closes a
  loop of strong references keeps its own count above zero, so ARC alone
  never reclaims it. A struct can name its own type in a field and an
  optional field owns its target, so a linked structure (a parent that
  owns a child which points back at the parent) closes a strong cycle.
  The first-class fix is the `weak` keyword (below). Mark the back edge
  `weak` (a field, a local binding, or a binding a closure captures)
  and the ring reclaims deterministically.
  `pith lint` flags a strong cycle between a module's structs (E306) at
  a field that can break it, so the edge is named before the leak is.

  For cycles nobody marked there is an experimental trial-deletion
  collector, off by default and enabled by running the process with
  `PITH_CYCLE_GC=1`. With the flag on, a release that leaves a count
  above zero records the object as a *suspect* (the only place a
  garbage cycle can root), and a background thread periodically stops
  the world, subtracts the edges internal to the suspect graph from each
  member's count, and frees the members no outside reference still
  holds (Bacon-Rajan trial deletion). A collection runs when enough
  suspects accumulate (`PITH_CYCLE_GC_THRESHOLD`, default 4096), or on
  demand from `std.concurrent.gc_collect()`, which waits for the attempt
  and reports whether it ran. `std.concurrent.gc_stats()` prints the
  collector's counters. Some shapes stay uncollectable even with the
  flag on and degrade to the leak they always were: a cycle through a
  channel's buffer (a channel holds raw handles, not counted edges), a
  cycle through a task handle, and a cycle through a container that
  never learned its element kind. None of those edges are visible to
  the collector's tracers. With the flag off (the default), the only
  cost is one predicted branch per release, and cycles behave exactly
  as this section always described: they leak, bounded, never dangling.

  The flag stays off by default on measured grounds (2026-08-16, five
  interleaved trials per bench, flag off vs on with the same binaries).
  The automatic threshold mode cannot keep up with churn-heavy cycle
  creation. A tight loop building and dropping a two-node strong ring
  filled the 65,536-suspect buffer almost immediately, and 378,340
  suspects from a 200,000-round run overflowed. An overflowed suspect
  is discarded, and if it rooted a garbage ring that ring is
  permanently uncollectable. Only 3 collections ran, about a third of
  the rings were freed, and peak RSS came out *higher* than with the
  flag off (117 MB vs 103 MB at 400k rings) because the suspect buffer
  sits on top of the leak. The time cost is real too: the cycle-churn
  bench runs about 2x slower flag-on (120 → 250 ms median) and the
  std-pipeline bench about 20% slower (550 → 670 ms, disjoint ranges).
  The catalog, event-ledger and channel-fanout benches read neutral.
  What does work is the flag plus an explicit
  `std.concurrent.gc_collect()` at points the program chooses. The
  leak gate's ring case runs flat that way, collecting every 4096
  rounds, where the same binary grows by a ring per round on the
  threshold alone. So prefer `weak` edges. If you need the collector,
  run with `PITH_CYCLE_GC=1` and call `gc_collect()` at natural
  quiesce points instead of relying on the threshold.
- **A channel is a counted value like any other.** A handle is retained on
  copy and released at scope exit, and the channel frees at its last owner,
  releasing any values still queued by their payload tag. A value sent is
  owned by the channel until received, and a receive hands it to the
  receiver.
- **ARC reclaims memory; it runs your cleanup only where a type asks for
  it.** Closing a file, rolling back a transaction, or releasing a lock is
  a side effect ARC knows nothing about, and the error path (`fail`, `!`)
  is exactly where it is easy to forget. A struct can opt in with
  `impl Drop` (see [Destructors](#destructors) below): its `drop` method
  then runs from the destructor the compiler already attaches, at the
  moment the last value naming it goes away. For everything else, reach
  for `defer` (see [defer.md](defer.md)). A deferred statement runs on
  every exit from its scope (fall-through, `return`, `fail`, and `!`
  propagation alike) right before ARC frees the locals, so the cleanup
  still sees them. For cleanup that should happen only when something
  went wrong, such as rolling back a half-finished transaction or deleting
  a partial file, use `errdefer`, which runs on the error exits and stays
  quiet on a normal return. `defer` also answers a question a destructor
  cannot: *when*. A deferred close runs at the end of the block, while a
  drop runs whenever the last holder lets go, which may be a task or a
  container away. The survey of every std type that still needs a manual
  close, and the order the rest are meant to move over in, is in
  [destructors_roadmap.md](destructors_roadmap.md).
- **A result consumed with `catch` or `unwrap_or` can leak its ok
  value** when that value was freshly built (a returned tuple, a
  concatenated string). The consumer has no way to tell a fresh ok
  value from a borrowed one (a returned collection element), so it
  cannot release without risking a double-free on the borrowed case,
  and errs toward the leak. Tuples themselves are fully reclaimed:
  a tuple frees its box at the last count and releases any heap value
  it holds, the same as a struct.
- **A tuple literal written straight into a call is released once the
  call returns**, the same as every other owned argument kind. The kind
  alone could never justify this. An optional and a result both lower
  to a tuple shell, so a tuple-*typed* register might be a wrapper whose
  count the extraction paths still hand on. A syntactic `(a, b)` in
  the source is unambiguous, though: the box is one only this call ever
  saw, and a storing callee takes its own count. Any other tuple-typed
  register in argument position keeps the old bounded leak instead of
  risking a double free.
  A fresh *optional* in that position is judged the same way, by its
  spelling rather than its kind. A call or method call whose checked type
  is an optional (`take(mk(i))`), an `await` of one, a bare `none`, and a
  plain value the checker widened into a `T?` parameter (`take(3)`,
  `take(point)`) are each a shell nothing but this call ever saw, and the
  caller releases the shell right after the call returns without reading
  the callee's body. That is sound because an optional parameter is a
  borrow of the shell, and every way a callee can keep it or take its
  payload retains. A struct construction, a field assignment, a container
  store, an index store and a channel send retain the shell. A lambda
  capture, a bind and a returned parameter retain it too. `unwrap_or`,
  `.value()`, an `if let` binding and a `match` arm retain the payload
  they hand out. So the caller's count is the only one it has to drop. A
  shell the callee kept survives on the callee's count. A shell nobody
  kept dies here and takes with it whatever it owns through its
  destructor (a wrap-built `Some`, a widened value, a transferring runtime
  getter's shell). A shell that borrows its payload (`xs.get(i)` forwarded
  out of a callee) owns nothing and frees only its box. The release is the
  plain shell release, never the payload cascade: the cascade drops the
  payload explicitly, which is wrong as soon as a second owner holds the
  same shell.
  `tests/cases/test_optional_arg_callee_spellings` reads back every keep
  and extract spelling, same-module, cross-module and through a generic
  receiver, under Valgrind with the freelist off.
  A fresh *result* in that position (`take(r())` against `fn take(x: Int!)`,
  a method call, an `await` of a task returning `T!`) follows the same
  convention. A result parameter is a borrow of the box, and every way a
  callee can keep it or take its payload takes a count of its own. A
  struct construction, a field assignment, a container store, a lambda
  capture, a bind and a returned parameter retain the box (a channel
  cannot carry a result, so a send is not a spelling).
  `catch` and `unwrap_or` retain the payload they hand out. `!` and a
  `.ok`/`.err` read hand out a borrow the escape site retains. A `match`
  binding on a result is an uncounted alias of the box, and its escape
  retains the box. So the caller drops its one count right after the call
  without reading the callee. What differs is the box itself: a result box
  carries no destructor (its payload leaves with the count when an owned
  box is opened, and the shell frees as three slots), so a plain release
  would strand a heap payload the callee only read, and the payload
  cascade would empty a box the callee kept. So the release asks the
  runtime whether the caller's count is the box's last
  (`pith_struct_strong_count`) and drops the live payload only then, in a
  per-signature helper (`__result_arg_release_<ok><err>`). The box is
  released either way. A signature with no heap slot at all takes the
  plain release (a bare `Int!` is not one: its error is a string). A box
  the callee kept survives on the keeper's count with its payload intact,
  and what happens to it from there is what has always happened to a
  kept result (see the entry below on results bound to a name).
  `tests/cases/test_result_arg_callee_spellings` reads back every spelling
  under Valgrind with the freelist off.
  A result box stored in a holder (a struct field, or a list or map
  element) gets a destructor at the store: the stack cascade helper for its
  signature, which releases the live payload and zeroes its slot, and
  leaves the box itself to the runtime. The holder's release then drops the
  payload along with the box. A cascade or a result-argument release that
  runs first also zeroes the slot, so the payload is released exactly once
  (#1167). The unshare a forwarding function runs on a shared box gives
  that box the same destructor. When its only owner later returns such a
  box, the destructor is cleared first: in the unshare when the caller
  holds the only count, and in the register return when it takes the
  payload. The caller takes over the payload's count and frees the shell,
  so a destructor left in place would release the payload while the
  caller still uses it.
  Tuple-typed registers the emitter cannot tie back to a literal still
  strand their box.
- **A collection literal the checker cannot type builds an untagged
  container.** `[]` and `{}` have no type of their own and take one from
  context. An annotated bind, a `return`, a struct constructor, an
  assignment and a call or method argument all supply one, so `f([])`,
  `xs.push([])`, `xs.insert(i, [])` and `m.get_default(k, {})` build the
  same tagged container `mut xs: List[String] := []` then `f(xs)` builds.
  An optional parameter is covered too: `f(v: List[String]?)` takes the
  type from the container inside the optional and wraps the tagged
  container in a `Some`, which is also what stops the callee reading a
  bare list handle as an optional tuple. A nested literal is covered too.
  An argument literal that *has* elements but checks as an error because an
  empty literal is nested inside (`f([[]])`) runs the same element-checked
  coercion an assignment uses, so the declared type is recorded through
  every nested literal and the inner list is tagged. A literal written
  into a holder (a struct field or its `= {}` default, a list element, a
  map value) is built from that recorded type instead of from the
  holder's container kind alone, and a target that is an optional
  container (`m: Map[String, String]?`, `return {}` from a
  `-> Set[String]?`) records the container inside it. The same recorded
  type reaches an empty literal nested in a returned literal
  (`return [{}]`), a tuple element (`({}, 1)`) and a generic function's
  argument. A value stored into an optional tuple slot, as in `({}, 1)`
  for a `(Map[String, String]?, Int)`, is wrapped in its Some the way an
  argument to an optional parameter is, and the literal is built from the
  optional's inner type. A generic struct built as `Bag()` takes its
  instance from the place it is stored into, so its `= {}` and `= []`
  defaults are built from that instance's field types, inside a generic
  body too. With no such place the constructor is refused (E222), and a
  generic struct's field default that names a type parameter must be one
  an instance can build from its own field type (E317). The safety net for
  anything that still slips through is the store: an untagged list or map
  handed a container, struct, `Bytes` or closure adopts that kind at the
  first store and owns it from then on. String values are the one kind
  the second chance does not cover, because string stores stay on the
  constructor path: an untagged `Map[String, String]` takes no count of a
  borrowed string put into it, and reads it after its owner frees it.
  `Saved(lists: {})` built exactly that map until the holder path read
  the recorded type. The count checker reports a borrowed counted value
  stored into a container built untagged in the same function, and any
  struct built over an unresolved type parameter (`Bag[<error>]`), whose
  defaults are stored into elsewhere (`check_untagged_stores` in
  `cranelift/codegen/src/count_check/`).
- **A result or optional bound to a name can leak its payload.** A boxed `T!`
  lowers to a three-slot shell and `Some(v)` of `T?` lowers to a two-slot shell
  (`none` is null). Releasing a shell without a payload destructor frees those
  slots without dropping the payload they own. A local whose every use is a
  probe (`.is_ok`, `.is_err`, `== none`), a payload read (`.ok`, `.err`),
  a `match` on an optional local, an argument of a call whose callee
  provably only reads the parameter, or, for a result, an
  extraction (`r.unwrap_or(d)`, `r catch d`, `r!`) releases its payload.
  A probe never touches it, a read borrows, a qualifying callee takes no
  count of its own (and retains anything a result extraction hands out),
  and an extraction hands its consumer a freshly retained count, so in
  every case the local is still the last owner of the count the shell
  arrived with. A match qualifies because its arm binding retains its own
  count (tracked like any owned local, released by the exit cleanup or
  transferred by a return), so the subject keeps the count it had. A
  match on a *result* local still disqualifies: its binding path takes no
  count of its own.
  The call-argument entry is judged by walking the callee's declaration
  with the same use rules as the local itself, transitively through
  forwarding. So a callee that stores, returns, sends, or captures the
  parameter disqualifies the argument, and a builtin, constructor, generic
  or closure-typed callee is never a qualifying position. A same-module
  free function is walked at the call. An imported function, a method of
  a user struct or enum (`obj.foo(r)`) and a module-qualified call
  (`mod.foo(r)`) are answered by a record their own module makes before
  it emits any body: which result and optional parameters of each
  non-generic function and plain method pass the walk
  (`ir_record_borrow_params`). A module is emitted after everything it
  imports, so the record is always there for an imported callee. While
  a module's records are being made, a call to one of its own methods
  whose record is not made yet answers "not safe". A named argument to
  one of these callees stays on the leak side, since the record is by
  position. For an optional, any other use leaks as before: returned
  whole, bound by `if let`, or captured by a closure, which takes the
  shell and not the payload. A *result* local that owns its payload
  (bound from a call, an await or a match) and fails the walk releases it
  at the function's exit the way a fresh argument does: through
  `__result_arg_release_<ok><err>`, which drops the payload only when the
  local's count is the box's last. A box some other owner still holds
  keeps its payload for that owner. A rebind still releases the old box
  as a bare shell, since an alias of its payload may be read after the
  rebind.
  An extraction *inside* the callee (`o.unwrap_or(d)`, `o.value()`) does
  not disqualify the argument: on a parameter subject it retains what it
  hands out, so the caller stays the payload's owner. One more shape
  stays on the leak side: rebinding one name to results of different
  payload types (`r := await ta` then `r := await tb` where the two tasks
  return different `T!`s). The cleanup path is keyed by name and cannot
  pick one payload shape, so it frees only the shell. Bind results of
  different types to different names.

None of these produce a dangling pointer. The discipline trades a
bounded leak for that guarantee.

## Destructors

A struct that implements `Drop` gets its `drop` method called when its
last strong count drops, before the fields are released and the memory
freed. A value that stands for something outside the heap, such as a
registry slot or a handle into a table, can give that thing back this
way without a caller having to remember to:

```pith
pub struct Config:
    pub handle: Int

impl Config:
    fn close():
        registry_drop(self.handle)      # a no-op on a handle already dropped

impl Drop for Config:
    fn drop():
        self.close()
```

`Drop` is a language-level interface: nothing declares it and nothing is
imported for it. The compiler recognizes the impl by name, and the rules
it checks are E268:

- Only a struct can implement it, and only in the module that declares
  the struct. The destructor is generated per declaration and spells the
  method symbol from the declaring module, which is also what keeps two
  modules' same-named structs from trading cleanup.
- A generic struct can. Each instance gets its own destructor, which
  calls the `drop` checked against that instance's type arguments, so
  `Guard[String]` and `Guard[List[Int]]` each run the body as written for
  their own `T`. An enum cannot, generic or not: its destructor releases
  tagged payloads rather than fields.
- The impl declares exactly `fn drop()`, with no parameters and no
  return value, and nothing else beside it. The runtime calls it with the
  bare pointer and reads nothing back.

At the last release, the runtime notices the count reach zero and calls
the struct's destructor pointer, and the generated body calls `drop`
first, then releases the RC fields exactly as it always did. Every
field is still live while `drop` runs, so it can read them. The same body
runs when a value dies inside a container, an optional, a struct field or
a closure environment, because the destructor pointer sits in the object's
header rather than on any binding. The cycle collector does not run it
for a member of a garbage cycle. It frees every member of a cycle in one
pass, and a `drop` that ran inside that pass could reach another member
through a strong field and keep it past the free. So a garbage cycle that
runs through a `Drop` struct is left alone and leaks, along with
everything that struct reaches. A `Drop` struct the collector reaches
only through a cycle member's field, outside any cycle, dies by an
ordinary release when that member is freed, and its `drop` runs then. A
weak reference read during that pass answers none for a cycle member,
the same as for a value whose `drop` is running.

There are two rules for a `drop` body. First, `self` must not escape it,
since the value is freed the moment `drop` returns, and the checker
refuses any use that could keep it (E318). A `drop` body may use `self`
to read and write its fields, as the receiver of a method call, and as an
argument, and the method or function it is lent to is checked under the
same rule for its receiver or that parameter, through every call it
makes. A local bound straight from `self` (`me := self`) counts as
`self`. The checker refuses storing it in a field, a global, a container
or a channel, returning it, building a value around it, capturing it in a
closure or a `spawn`, and passing it to a builtin or anything else whose
body the checker cannot follow. The values a `drop` copies out of the
fields are ordinary values and may go anywhere. The runtime runs the
destructor under a guard count, so the retain and release a bind or a
call takes balance inside the body instead of re-entering the release at
zero, and a weak reference to the value read inside its own `drop`
answers none. Second, `drop` should be idempotent with the type's
explicit closer.
The rule in std is that whoever builds a resource closes it, and that
rule stands. An explicit `close()` still says *when* the resource goes
back, which matters for a server config a listener is still handshaking
on, and the destructor covers the caller that forgot or took an error
exit past the call. A closer backed by a map removal is idempotent for
free, and that is the shape every `Drop` type in std has. An ECDH key
pair's `close()` frees memory by address, and a later key can be given
the same address, so the runtime cannot tell a second close from a close
of the new key. The pair records the close in the value itself (its
handle goes to -1), so the destructor after a close finds nothing to
free.

`std.bytes.ByteBuffer` needs no such record. `free()` and `take_bytes()`
empty the buffer and leave it usable, and only its destructor frees the
storage. A `ByteArray` frees its storage on drop as well. Both keep their
handle in a private field, and the `byte_buffer_*` builtins are callable only
from `std.bytes`, so no copy or made-up number can reach the memory (E251).

The io readers, writers and buffers, files, processes, byte buffers and
byte arrays, ECDH key pairs, TLS configs, connections and listeners, HTTP,
HTTP/2 and Unix connections and streams, WebSocket sessions, and database
handles implement `Drop`. What each io type releases is listed in
docs/io_foundation.md, and the full set across std is in
docs/destructors_roadmap.md ("stage 2").

`tests/cases/test_drop_hook` pins where the hook fires (scope exit, a
rebind, a list or a struct field dying, a value returned to a caller),
`tests/cases/test_drop_lend` what a `drop` body may still do with `self`,
`tests/cases/test_drop_generic` the same points for a generic struct over
several type arguments and across modules,
`tests/invalid/drop_escape_*` the escapes E318 refuses, and
`tests/leaks/leak_tls_config_drop` pins that a TLS config built and never
closed no longer holds its registry slot. `tests/leaks/leak_io_registry_drop`
and `tests/leaks/leak_byte_array_drop` do the same for the io registries,
byte arrays and key pairs, and `tests/cases/test_io_drop_resources` counts
open fds around files, child processes and websocket sessions that are
never closed.

## Weak references

A `weak` field holds an optional target without keeping it alive. It is
how you break a strong cycle: in a parent/child graph, let the parent own
the child through a normal optional field and mark the child's back
pointer `weak`.

```pith
struct Node:
    value: Int
    mut next: Node?      # owns the next node
    weak parent: Node?   # refers back without owning
```

Only an optional struct field can be `weak`. The checker rejects
anything else (E249). A weak field never touches its target's strong
count, so it cannot be the reference that holds a node alive. When the
last strong owner drops, the target is reclaimed even while weak
references still point at it.

Reading a weak field resolves liveness on the spot. While the target is
alive the field reads back as `Some(target)`. Once the target has been
reclaimed it reads back as `none`, so a weak reference never dangles:

```pith
if node.parent != none:
    print("parent: {node.parent.value().value}")   # alive
# ... after the parent's last strong owner drops ...
if node.parent != none:                             # now false
    ...                                             # not taken
```

Under the hood a weak field stores the target pointer directly rather
than an owning box, takes a weak reference on assignment, and drops it in
the struct's destructor. The target's header carries a separate weak
count and a dead flag. The header outlives the value just long enough for
weak reads to observe that the value is gone and return `none`. The
`bench/cyclic_graph` benchmark builds and drops two million parent/child
rings. With the back edge `weak`, peak memory stays flat (about 2 MB),
and with a strong back edge the same run leaks every ring (about 730 MB).

Each read upgrades the reference in one atomic step: it takes a strong
count on the target while it is still alive, or reads back `none` once it
is gone, and returns a fresh optional that owns that count. So a binding of
a weak read (`back := node.next.value().back`) owns its optional like any
call result and releases the count at scope exit. A check followed by a
later retain could revive a target another task had just destroyed, and
classifying the read as borrowed once freed the target twice or leaked one
optional per bound read.

The field's own weak count is what keeps a dead target's header allocated,
so a store into the field and a read of it in another task must not
interleave. The read loads the pointer and upgrades it in one runtime call
(`pith_struct_weak_load_field`), and the store swaps in the new pointer in
another (`pith_struct_weak_store_field`) before it drops the old weak
count. Both take the same striped lock, so a store waits until a reader
that loaded the old pointer has its strong count. A separate load and
upgrade let a store free the header between the two, and the upgrade then
read freed memory (`tests/cases/test_weak_field_store_across_tasks`).

The lock is needed only when another task can reach the holder. Every
route to another task marks a struct shared before the other task can see
it (a spawn's captures, a send, a global, a task's result, a store into a
shared value; see `cranelift/runtime/src/share.rs`), and nothing clears the
mark. A read and a store in two different tasks therefore both find their
holder marked, and both take the lock. An unmarked holder is reachable only
from the running task, so compiled code tests the mark inline and reads or
swaps the field of an unmarked holder without the lock or any locked
instruction. `tests/cases/test_weak_field_lock_after_crossing` races a store
and a read after each route. With the lock on every store,
`bench/cyclic_graph` ran 27% slower.

Every consumer treats the read as that fresh optional. A local, a field, a
struct construction, a list or map store and a return keep the count for as
long as they hold the optional. A test against `none`, an extraction
(`.value()`, `unwrap_or`, `?`), an `if let`, a `while let`, a match, a call
argument, an interpolation and a discarded read release it when they are
done, and a store into another weak slot takes its weak count and then
releases the optional. `tests/cases/test_weak_read_strong_slot` pins the
strong slots, and `tests/leaks/leak_weak_read_consumers` pins that each
consumer drops the count it was handed.

A local binding can be weak too. `weak name := expr` holds a struct value
without keeping it alive, and every read yields the same optional a weak
field read does, `Some(target)` while a strong owner still exists and
`none` once the target has been reclaimed:

```pith
mut cache: List[Session] := [Session(id: 1)]
weak current := cache[0]
# ... later, possibly after the cache evicted the session ...
if current == none:
    print("session gone")
```

A weak binding takes its type from its value (no annotation), must hold a
struct, and is immutable (E261 for each). A weak slot is an edge that
either still reaches its target or does not. It is not a variable to swap.
Its name must also be unique in its function: the slot is keyed by name
across the whole function body, so reusing it for any other binding
(strong, loop variable, or match payload) is rejected instead of being
left to read through the wrong lowering.

A closure captures a weak binding weakly. The environment slot holds the
bare target pointer under a weak reference, never a strong count, and
every read inside the closure resolves liveness fresh, exactly like the
binding itself. That is what lets a closure over stream state break its
own cycle. A callback stored on the object it reads from is the classic
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

One closure interaction is still rejected rather than half-supported: a
weak binding cannot be *declared* inside a lambda body (E261). Lambda
bodies share the enclosing emission's local tracking today, so the
binding would lower against the wrong frame. Declare the weak binding
outside and capture it instead, which is also the shape the cycle break
needs.

Closures are reference counted like any other heap value. A closure
carries its own count and a release tag per captured slot. The last
release walks the environment, drops the count the closure took on
each captured value, and frees the box. A closure that dies locally is
released at scope exit, and one that escapes transfers its count the
same way a returned struct does, so building and discarding closures
in a loop holds flat, outside the cycle case above. Naming a function
works the same way: `f := shout` allocates a closure that wraps the
function into the closure ABI, and that closure is reclaimed at the
end of the scope it was bound in.

## Threads and tasks

`spawn` runs a closure on a green task, which Linux schedules across a
pool of OS threads (see [concurrency.md](concurrency.md)). Retaining and
releasing the same value from two of those threads is safe. A struct's
counts are plain loads and stores while only the task that built it can
reach it, and atomic once it is marked shared. It is marked when it can
reach another task: through a spawn's captures, a task's result, a channel
send, a module global, or a store into a struct, list or map that is already
shared, and everything it reaches is marked with it.
`cranelift/runtime/src/share.rs` lists the crossings, and `PITH_RC_CHECK=1`
aborts a run in which a task updates the plain counts of a struct it did not
build. Strings, bytes, lists, maps and closures keep atomic counts.

The container *contents* are not synchronized, though: a list or map is a
plain buffer behind a handle, so two tasks mutating the same collection race
on that buffer.

The rule: **do not share a mutable collection across tasks; pass data
through a channel instead.** The checker enforces it at a `spawn`
argument and a channel send (E248), for a collection and for a struct whose
fields reach one outside a `guard` block, for a closure that can reach
another task and captures such a value, for a value stored into a guarded
collection (E276), and for a global through the sharing rule (E309). It
follows a buffer through every name and call that can reach it: a value
handed over, sent or stored under a lock must hold nothing another name
still reaches, and a global or a frozen field may not be changed in place
through any name.

The field itself is a pointer too. Reassigning a field that holds a counted
value releases the old value, and a task reading the field at that moment may
be taking a count on memory that is being freed. So the same checks refuse a
struct whose fields reach a counted field that some code reassigns after the
struct is built, unless the field is in a `guard` block. A field no code
reassigns is settled and behaves like a `frozen` one. A field holding a
number or a flag is exempt. A send hands over the handle, so the value sent
must be one the sender does not keep: built in the call, a local the sender
does not use again, or an independent copy (`std.collections.copy_list` and
friends). A spawn evaluates its arguments on the new task, so a copy made in
a spawn's arguments reads the original there. Make the copy before the spawn.

### Frozen fields

A field declared `frozen` is set when its struct is built and never changed
after: it cannot be reassigned, and nothing can change its value in place
through a path that runs through it (E275). So it is safe for any number of
tasks to read at once, with no lock, and E248 does not count it as a shared
buffer:

```pith
struct Router:
    frozen routes: List[Route]
    frozen middleware: List[Middleware]
```

The router in std.web, a request's header and query maps, a metrics series'
labels and an HTTP/2 pool's connection list are frozen. In a program that
spawns tasks, the value is followed past the field too: changing it through
a local it was copied into, a parameter it was passed as, or a value that
holds it is refused (E275), and so is storing it into a value a caller
handed in. A frozen field may hold a module global's buffer, or another
frozen field's, since no name may change either. `frozen` is a keyword only
before a field name, so a variable named `frozen` still works.

## Finding a use after free

A count bug in emitted code shows up as a value read after its last
release, and the runtime cannot see most of these. Generated code reads a
struct field, an optional's payload or a list element with plain loads, and
with the struct and closure pools on, the freed block usually already holds
another value. A sanitizer build sees only the reads the runtime makes,
because generated code is not instrumented.

`PITH_QUARANTINE=1` turns those reads into a report. It has to be set when
the program is built and when it runs. `pith run` and `pith test` see it in
both steps:

    PITH_QUARANTINE=1 pith run prog.pith

At run time, the last release of a struct (optional and result shells
included), list, map, set, bytes, byte buffer, closure, string or channel
releases what the value owns and keeps the block. The block is filled with
0xde bytes, its magic word becomes a freed magic that names the kind, and it
is never reused, so the pools stay empty. A later retain, release or runtime
operation on the handle stops the program with exit status 1:

    pith quarantine: use after free of struct
    pith quarantine: release of a freed list (a double release)

A release that finds the count already at zero, which the runtime otherwise
ignores, is reported as an over-release. A count operation or field read on
a struct whose value died while a weak reference still holds its header is
reported as well.

At build time the variable makes the compiler leave out its inline fast
paths for list, bytes, string and closure reads, so each of them calls the
runtime, which checks the handle. The compiler also tests a struct's header
before each field read or write. A build without the variable emits none of
this, and the runtime pays one predicted branch per free and per failed
magic check.

`PITH_QUARANTINE=trace` also records a backtrace at each free and prints it
next to the backtrace of the bad use (build with `PITH_KEEP_SYMBOLS=1` to
name generated functions). Nothing is freed in either mode, so memory grows
with every allocation.

The older switches still apply. `PITH_STRUCT_FREELIST=0` hands struct and
closure blocks back to the allocator so Valgrind or a sanitizer can see the
free, `PITH_CSTRING_NOFREE=1` keeps freed strings filled with `!` and reports
string over-releases, and `PITH_RC_CHECK=1` is described under "Threads and
tasks" above.
