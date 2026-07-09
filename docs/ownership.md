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
- the runtime counts: `cranelift/runtime/src/runtime_core.rs` and
  `arc.rs`

if you are adding a statement or call shape, decide explicitly which
of the rules above applies to every string that flows through it —
"it seemed to work" is how the dangling ones got in.
