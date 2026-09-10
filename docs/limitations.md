# limitations

pith is self-hosting and runs real programs, but it is not finished. this page
is a plain list of what does not work yet, so you can plan around it instead
of discovering it the hard way. it is kept current with the compiler; if you hit
something here that now works, the page is stale and a fix to it is welcome.

## language

- **match pattern gaps** — variant payloads construct and destructure
  (`Shape.Circle(2.0)`, `Shape.Circle(r) => r * r`, with literal sub-patterns,
  guards, and payload bindings inside or-patterns like
  `Circle(r) | Square(r)`, tuple patterns with bindings, wildcards, and
  literal elements, and `none` patterns on optional subjects — a bare
  binding arm unwraps the some case, mirroring `if let`). note a bare name in a pattern is a binding, not a variant — write
  `Color.Red`, not `Red`, to match a variant. equality (`==`) on
  payload-carrying enums compares structurally: tags, then payloads, with
  string payloads by content and nested enums recursively; struct payloads
  compare by identity.
- **a bare `none` takes its type from the first assignment** — `none` is
  accepted wherever the target is already an optional, which is every position
  that has one: bindings, assignments, returns, arguments, struct fields,
  collection and tuple elements, and `==` / `!=`. an unannotated `mut x := none`
  has no target to check against, so it takes one from the first concrete
  assignment: `x = 5` makes it `Int?`, and a later assignment of another type
  is rejected against that. an annotation still says it up front and is clearer
  when the first assignment is far away.
- **a plain value widens into an optional wherever one is expected, except a
  map key or a set element** — `3` is accepted where an `Int?` is expected, and
  whatever reads it back gets the `Some(3)` it expects. that covers bindings,
  assignments, returns, struct fields (positional, named and defaulted),
  collection literal elements, map index assignment, the argument of a plain
  function, a method, a lambda and a function value, a builtin container's
  store (`xs.push(3)` into a `List[Int?]`, `xs.insert(0, 3)`, `m.insert(k, 3)`
  into a `Map[K, Int?]`), a list's searches (`xs.contains(3)`,
  `xs.index_of(3)`) and an enum variant payload (`Probe.Alpha(3)` against
  an `Int?` payload). destructuring is unchanged: a match binding on an
  optional payload has the payload's declared type, so `Probe.Alpha(b0) =>
  b0.unwrap_or(0)` reads it like any other optional local. a collection
  literal takes the declared element type too, so `f([1, 2])` against a
  `List[Int?]` parameter widens element by element, an optional element that
  is itself a container takes the literal through the container and wraps what
  it built (`{"a": [1, 2]}` against a `Map[String, List[Int]?]`), and a
  `List[Int?]?` target widens the elements and the container at once.

  a map key and a set element take no widening because the types that would
  need it no longer exist: `Set[T?]` and `Map[T?, V]` are rejected with
  E254, and so is every other unhashable element or key type — a container,
  a struct, an enum, a tuple, a function value, `Float`, `Bool`. a set and a
  map hash int, string and bytes flavors only, and none of those values is
  any of them — the containers used to fall back to a flavor anyway and read
  the value's word as a c-string, so distinct values collapsed into one
  entry and `contains` answered true for anything. a compile error replaced
  that silent data loss; use a `List[T?]` or key by a string, integer or
  bytes encoding instead. an optional map value stays legal, since only the
  key is hashed. a list compares an element the way `==` compares it, which
  is why its searches widen.

  a parameter of a *generic function* widens like any other argument —
  `pick(x, 3)` against `fn pick[T](a: T, b: Int?)` builds `Some(3)`, in both
  the inferred and the explicit `pick[String](x, 3)` forms. (this had been
  deliberately held back until a specialization took a parameter's
  optional-ness from the declaration rather than the call site; that lowering
  bug is fixed, and `pick(x, none)` through the same specialization still
  answers "none".)
- **`==` and `!=` widen between `T?` and `T`** — `o == 5` where `o` is an
  `Int?` compares by `(is_some, value)`: it is true only when `o` holds `5`,
  and `none` never equals a plain value. both orders work, and a `String?`
  compares by content. ordering operators (`<`, `>`, `<=`, `>=`) do not
  widen — unwrap first, because there is no sensible order between `none`
  and a value.
- **a binding that shadows a module global takes the name for the whole
  function** — a local, a parameter or a `match` / `if let` payload spelled
  like one of the module's globals reads its own storage, and the global is
  unreachable from inside that function. a `for` variable is the exception:
  its storage lasts the loop, so the global is still reachable on either side
  of it. reading the global and then binding a local of the same name in one
  function asks for both and is E266 rather than a guess. `pith lint` mentions
  the shadowing itself as E308.
- **range patterns are integer-only** — `0..=9 => ...` and `0..10 => ...`
  work in match arms (and combine with or-patterns and guards), but only for
  integer subjects and non-negative literal bounds.
- **`if let` / `while let` are statement-only** — `if let Shape.Circle(r) = s:`
  destructures a variant and `if let v = maybe():` unwraps an optional, but
  neither form works as an expression, and literal patterns are not allowed
  in the let position.
- **closure captures are unlimited** — the first 16 live inline in the
  closure allocation and any beyond that spill to a heap extension, so a
  closure capturing 17 or 40 variables computes the same answer as one
  capturing 3. this used to be a silent cap: the runtime ignored stores
  past slot 16 and answered 0 for reads, so the 17th capture read back as
  zero with no diagnostic anywhere. captures are heap-allocated per
  closure instance, so nesting and recursion are safe. (multi-line closure
  bodies — `fn(x):` with an indented block — work and infer their return
  type from their return statements.)
- **duplicate method names across impl blocks are rejected** — a struct's
  methods can be declared in more than one impl block, including a block in
  a different module than the struct, and every method resolves from any
  call site that can see the value. method dispatch is per-declaration.
  two modules can each declare a struct with the same name, plain or
  generic: a module-qualified constructor binds to that module's own
  declaration, shared method names dispatch to the receiver's own body,
  and a method the receiver's struct does not declare is E209 even when
  the other struct has it. import order changes none of that, and an
  interface impl on each module's own same-named generic dispatches per
  module too. three narrower clashes are still keyed by the bare name,
  and all three fail at compile time rather than quietly running the
  wrong body. a generic function whose signature names a generic its own
  module declares resolves that name in the calling module, so
  `iter.map_iter(...)` yields the caller's `MapIter` when the caller
  declares one too. two modules that each declare the same generic enum
  do not compile. a generic struct sharing its name with a plain struct
  in another module resolves to the plain one. give those three shapes
  distinct names.
  a second impl block giving the *same* declaration
  the *same* method name used to overwrite the first silently, with the
  winning body decided by module order — that is now E263. one deliberate
  exception: an interface impl may re-declare a method the inherent impl
  also has (`impl StringReader: fn read` alongside `impl Reader for
  StringReader: fn read`) — that pair is std/io's conformance idiom and
  stays legal in either order; a second declaration of the same KIND is
  what errors. a free function sharing a name with a builtin method on a
  primitive receiver does not capture the method either: `(1.5).to_int()`
  is the Float builtin even with a `fn to_int(text, fallback)` in the
  build, and the free function answers only plain calls. method syntax
  never reaches a free function (there is no ufcs) — a primitive receiver
  resolves builtins, everything else resolves methods.
- **interface depth** — interfaces support method signatures, default
  methods (a member with a body that implementors inherit unless they
  override it), associated types (`type Item` on the interface, bound
  per impl with `type Item = Int`), single and multiple bounds, and
  generic interfaces. an associated type resolves both inside the impl's
  own methods and in generic `T.Item` position — a
  `fn f[T: Container](c: T) -> T.Item` returns the right type for each
  concrete `T`. an impl that omits an abstract (non-default) interface
  method is rejected at the impl block (E235). bounds have two spellings
  that mean the same thing: inline (`[T: Display + Hash]`) and a `where`
  clause after the signature (`fn f[T, U](t: T, u: U) -> Int where T:
  Display + Hash, U: Ord:`) — a clause naming something that is not a
  declared type parameter is E264, reported at the offending name. `where`
  is a contextual keyword, so it stays usable as an ordinary name
  everywhere else. clauses attach to free functions and to methods —
  inherent, interface-impl, and interface members in both the abstract and
  the default spelling. struct, interface, and impl headers still take
  inline bounds only, and a method's clause may name only the method's own
  type parameters, not the owner's.
- **a method may carry type parameters of its own** — `fn describe[T](v: T)
  -> T` inside an impl block behaves like a free generic function: the
  argument types at a call site fix the parameters, and each distinct set
  gets its own specialization. inference reaches through a container
  parameter (`List[T]`), the return type may be the parameter or a shape
  built from it (`T`, `List[T]`, `T?`), a Result return carries through, a
  method may declare more than one parameter, and bounds work in both
  spellings (`[T: Label]` and a `where` clause). interface members take
  type parameters too, in the abstract and the default form, and a call
  across a module boundary specializes the same way. the type arguments can
  be written at the call, `x.describe[Int](v)`, which is how a parameter
  that appears in no argument is fixed (`x.blank[Int]()`; the bare call is
  E222). the parser reads the bracket after `.name` as type arguments only
  when `(` follows it, its contents spell types, its first word starts with
  an uppercase letter or is `fn`, and the receiver is not an import alias, so
  `t.items[0](y)` stays an index into a field and `json.decode[Row](s)` a
  module call; the shapes it misreads are a type-shaped index into a field
  of closures, a SCREAMING_CASE constant or an enum variant,
  `t.items[MAX](y)`, which the checker refuses with the repair named:
  group the index, `(t.items[MAX])(y)` (docs/generics.md). a generic owner works too: `impl Box[T]:
  fn map[U](f: fn(T) -> U) -> Box[U]` is specialized once per combination
  of the receiver's instance arguments and the method's own, and its body is
  checked per combination. the method's parameter names must differ from
  the owner's (E265). a generic method behaves as a free generic function
  does: its body is typed against the concrete types before it is emitted
  and releases its locals like a concrete body (docs/generics.md).
- **generic enums construct, infer, and match like any other enum** — a
  constructor with a payload argument infers its instance (`x :=
  Opt.Some(5)` is an `Opt[Int]`), an annotated binding supplies the
  instance to a payload-free variant (`b: Opt[Int] := Opt.Nothing`), and a
  match resolves the bare pattern name against the subject's instance, with
  exhaustiveness checked. a payload-free constructor bound with no
  annotation has nothing to infer from and is rejected (E262) rather than
  left silently untyped. two gaps remain: the explicit form
  `Chain[Int].Link(...)` does not parse as a variant constructor, and a
  call like `head_of(pair)` does not infer `T` from a `Chain[Int]`
  argument. self-referential generic types themselves work —
  `struct Node[T]: next: Node[T]?`, `Link(T, Chain[T]?)`, and mutually
  recursive pairs all instantiate (the instance registers before its fields
  resolve, so the reference finds a floor). a generic enum erases to a
  single IR struct, but ownership follows the instance: a construction site
  attaches a destructor built from the instance's concrete payload kinds,
  and that destructor releases each variant's payload under that variant's
  own tag, so an `Opt[String]` releases its string when the box dies no
  matter where the payload-carrying variant sits in the declaration. a
  generic struct instance gets the same treatment from its concrete field
  kinds, whichever field the type argument made releasable.
- **a lambda may declare a return type** — `fn(x: Int) -> Int:` and
  `fn(x: Int) -> Int => x` both parse, and the declared type is enforced: each
  return is held against it by the same check a named function's returns go
  through. without one a lambda still infers from its body, and two returns
  that disagree are an error, so an annotation is also how a lambda returns
  `Int?` from a body that returns an `Int` on one path and `none` on another.
  the built-in assertions work inside a closure body too: they are lowered by
  name at the call site, so the body has no variable to capture, and a local
  of the same name still shadows them.
- **a generic body the program never instantiates is never checked** — each
  specialization is type checked against its concrete types and keyed by them
  (`first[Int?]` and `first[String?]` are two bodies, docs/generics.md), so a
  fault in a body shows only at the types the program uses; a body valid at
  `Int` but not at every type its bound admits is not reported until a caller
  reaches the failing type. `pith check` reports what it does find with the
  specialization named (`... in first[String]`).

  a generic body is typed against each set of concrete types just
  before it is emitted (docs/generics.md) and tracks its locals like a
  concrete body: the escapes an earlier version of this entry listed as
  leaking one count per call — a local stored into a struct, a map, a set, a
  list, a module global, a lambda or a callee's argument; `return
  Holder(items: items)`; `return shout(loud)`; `fail name`; a generic struct
  built by its bare name — release the way their concrete twins do.
  `leak_generic_body_escapes`, `leak_generic_optional_local` and
  `leak_generic_body_instance` pin that in `make leak-check`, and
  `test_generic_body_escape_release` and `test_generic_bare_construction_dtor`
  read each escaped value back after its frame is gone.

## standard library

- **tls 1.2** — the client and server speak tls 1.3 and, as a fallback, tls
  1.2 (ecdhe + aead only; six suites — ecdhe-rsa/ecdhe-ecdsa in aes-128-gcm,
  aes-256-gcm and chacha20-poly1305), negotiating the highest a peer supports
  and refusing anything below 1.2 — the same posture as go's crypto/tls and
  rustls. `require_tls13()` locks a config to 1.3. the 1.2 fallback supports
  rsa (≥2048-bit) and ecdsa (p-256) certificates, for the server's own and for
  a client's: client-certificate auth works on 1.2 with the same config surface
  1.3 uses (`with_client_certificate`, `request_client_ca_file`,
  `require_client_ca_file`), and the verified identity reaches an application
  through the same `ConnectionState` fields. the fallback still does not do
  session resumption or renegotiation.
- **testing** — `test` blocks are discovered and run by `pith test` (with
  `--filter`), and `std/testing` adds assertions, a `with_temp_dir` fixture
  helper, and `each` for parameterized cases: a labeled row reports as its own
  result and is reachable on its own as `--filter "test name / row label"`. a
  test carries tags — `test "name" [slow, database]:` — which `--tag` and
  `--exclude-tag` select and exclude, and `--json` reports the run as one json
  record per result for ci. benchmarks are still missing. the project's own
  suite is golden-snapshot based (see `tests/`).
- **plaintext http/2 needs an explicit listener** — over tls, `web.listen_tls`
  offers alpn `["h2", "http/1.1"]` and serves whichever the client picks. there
  is no such negotiation without tls, so plaintext http/2 means calling
  `listen_h2c` directly. `std.net.http` itself stays http/1.1.
- **regex is deliberately small** — `std.regex` covers literals, `.`,
  classes, `\d \w \s` escapes, `* + ?` (greedy), alternation, capturing
  groups, and `^ $` anchors. it does not support `{n,m}` counts, lazy
  quantifiers, backreferences, or lookaround. matching is a pike vm, so
  time is linear in the input for any pattern. it also matches **bytes,
  not characters**: `.` and a class each consume one byte, so `.` on its
  own does not match a two-byte character, while `..` and `[^,]+` match
  one whole. a span that would end inside a character is reported as no
  match at that position rather than cut through it, so non-ascii input
  is safe to run patterns over — it just answers in bytes. ascii input
  is unaffected, since every offset in it is a character boundary.
- **gzip compresses with fixed huffman only** — `std.compress.gzip`
  reads any deflate stream (multi-member files included) and writes
  real compression (greedy lz77 over fixed huffman, stored-block
  fallback for incompressible data; system gunzip reads its output,
  and zlib routes through the same engine). dynamic huffman trees on
  the write side would shave a few more percent and are the one
  remaining refinement.

## tooling

- **analysis caches the import closure, not the module** — `pith lsp` keeps
  the checked import closure between analyses, so an edit to the open document
  re-checks that document alone: a keystroke in a 53-module closure reaches
  diagnostics in about 60 ms where it took 1.1 to 1.5 s, and the residual is
  copying the cached checker state back in. an edit to a module misses and
  re-checks the whole closure, because the checker's tables are shared arenas
  that cannot be cut at a module boundary; the cache also holds one entry file,
  so switching between two open documents misses on each switch. an edit that
  changes a module's interface re-analyzes every open document that imports
  it; a body edit re-analyzes nothing else. memory is flat across analyses:
  the growth of about 12 mb an analysis was a string loop that never released
  the byte it bound, and a 50-analysis session on a 54-module closure sits at
  104 mb. see [docs/lsp.md](lsp.md) for the feature list, the measured phase
  split, the cache's figures and what a per-module cache would still take.
- **no package registry** — dependencies are local path entries in `pith.toml`.
  `pith package lock` writes a `pith.lock` and `pith package install` copies
  those paths into `.pith/packages`, but nothing fetches over the network and
  there is no hosted index.
- **no debugger** — runtime stack traces are thin and there is no stepping.
- **a declaration or a statement points at its last token** — an expression,
  a type and a pattern each carry the position of the token they start at, so
  an error about a call, a binary expression, an index or a method chain puts
  its caret on the expression rather than on the token that closed it. a
  declaration and a statement still carry the position of their last token, so
  a warning about a function names the line its body ends on rather than the
  line its `fn` is written on, and the language server finds a declaration's
  own line by scanning the source for the declaring keyword instead of reading
  the node.

## backend

these are internal and do not usually surface in source, but they shape the
correctness story:

- the ir is self-describing — calls carry an explicit return kind and field
  loads carry their type — and the cranelift consumer reads that metadata rather
  than guessing. the older inference path that caused a few cross-module bugs is
  gone. a struct reached only through another module's return type takes its
  kind from the checker's registry of declared types rather than the caller's
  own module map, and `make validate-ir-contract-only` checks the emitted
  contract over every corpus program in ci (docs/ir-contract.md).
- memory reclamation is compiler-emitted reference counting (see the readme's
  memory section). closures are reference counted and freed like other heap
  values. the one structural gap that remains: strong reference cycles leak
  when nothing marks the back edge — but every cycle shape now has a weak
  escape hatch. a struct graph breaks its own cycle with a `weak` field, a
  local holds a struct weakly with `weak name := expr`, and a closure that
  captures a weak binding holds its target weakly too, so a callback stored
  on the object it reads from reclaims with the object (see
  docs/ownership.md). `pith lint` reports a strong cycle between a module's
  structs as E306, at the field that can break it. an unmarked strong cycle
  still leaks by default,
  bounded by design — the discipline never produces a dangling pointer in
  exchange. for cycles nobody marked there is now an experimental
  trial-deletion collector behind `PITH_CYCLE_GC=1` (off by default; see
  docs/ownership.md for what it reclaims and what stays uncollectable),
  with `std.concurrent.gc_collect()` to force a pass. (removing an element from a
  container, returning early on an error path, and indexed reads of
  `List[Struct]` were all listed here once; each was fixed and each is now
  pinned flat by the leak-growth gate, measured at two round counts.)
- a fresh optional written straight into an argument is released by the
  caller as soon as the call returns, whatever the callee is (see
  docs/ownership.md). that covers a call-produced optional (`f(maybe(i))`),
  a bare `none` (`f(none)`), a plain value widened into an optional
  parameter (`f(3)`, `f(Point(1))`) and an awaited one (`f(await t)`),
  handed to a plain function, a method (`obj.take(maybe(i))`), a function
  in another module (`mod.f(maybe(i))`), a method on a generic receiver, a
  closure, or a callee that extracts or keeps the value. the release does
  not read the callee's body. an optional parameter is a borrow, and every
  way a callee can keep the shell or take its payload retains, so the
  caller's count is the only one it has to drop. an earlier version walked
  the callee's body and released through the payload cascade when the walk
  passed. that left every callee it could not read leaking about 64 bytes
  per call, and it dropped a payload two shells shared: `show(forward(v))`
  emptied `v`, and `show(xs.get(0))` freed the list's element.
  `test_optional_arg_callee_spellings` reads back every keep and extract
  spelling under valgrind.
- a fresh result written straight into an argument (`f(r())` against
  `fn f(x: Int!)`, `obj.take(r())`, `mod.f(r())`, `f(await t)`) is released
  by the caller as soon as the call returns on the same terms: a result
  parameter is a borrow, and every way a callee can keep the box or take
  its payload retains (see docs/ownership.md). a result box has no
  destructor, so the release drops a heap payload only when the caller's
  count is the box's last, which it reads from the runtime rather than
  from the callee's body; a box the callee kept (`store(h, r())`,
  `show(forward(r()))`) survives with its payload on the keeper's count.
  `test_result_arg_callee_spellings` reads back every keep and extract
  spelling under valgrind. before this the box and its payload leaked on
  every such call.
- a heap local inside a *generic function body* follows the same release
  rules as one in a concrete body: the body is typed against each set of
  concrete types before it is emitted, so escapes, stores and returns are
  classified from real types. what a *generic instance* holds is released by
  a destructor built from the instance's concrete kinds whether the site
  names them (`Wrap[String](...)`), the checker inferred them, or the
  instance was built by bare base name inside another generic body. the
  compiler-section entry above lists what remains.
- a collection literal whose element type is an optional (`List[Int?]`,
  `Map[String, Int?]`) owns the optionals it holds, the same as a container
  built by pushing into it: the literal's wrapped elements are tagged like
  any other heap element and released with the container. an optional over a
  string owns that string as well, wherever the `Some` is built — a list or
  map literal, an index assignment, a binding, a struct field, a widened
  argument — so a `List[String?]` of built strings reclaims them with the
  container, and a payload the wrap borrowed keeps its owner's count intact.
  a bare `none` written straight into a container store (`xs.push(none)`,
  `m[k] = none`) is a shell the caller built, so the store takes that count
  instead of adding one of its own, the same as a widened plain value.
- a value dropped at statement position is released on the ownership of the
  expression that produced it, not on the shape of the statement. a result
  box, an optional shell, a string, `bytes`, a struct, a tuple, a closure, a
  channel and a bare `List`/`Map`/`Set` all release. a user function transfers
  its result out, and the builtin producers hand back either a freshly built
  value (`keys()`, `values()`, `split()`, the slice/sort copies) or a count
  taken over the call itself (`get_default`, which the call site retains;
  `take`, which the map relinquishes), so the count the discard drops was the
  statement's. a void builtin method hands its receiver back so chains work,
  so the register a discarded chain leaves behind is the receiver's. the
  classification reads the expression at the root of the chain: `mk(i).reverse()`
  drops the list `mk` minted, and `xs.reverse()` leaves a live local's count
  alone. `!` and `await` arrive by paths of their own and are classified where
  they land — `!` frees the box it opened and hands the payload on with the
  box's count, `await` hands over the count the task produced. the payload of
  a discarded result box goes only when this frame's count is the box's last,
  read through `pith_struct_strong_count` by the same per-signature helper the
  argument-position release uses. before that, `forward(r)` at statement
  position, with `r` a live local holding the same box, emptied `r`.
  what still strands: a discarded `catch`, `select` or `match` releases only a
  string, because the other kinds normalize their arms by paths of their own.
  and the count-gated drop trades an over-release for a leak. a local the
  cascade cannot take — any local handed to a callee that keeps or returns it
  — releases the shell alone at cleanup, so a payload two owners hold is
  reclaimed only if the last release reads the count. the argument release
  reads it; that cleanup does not.
- the fresh-shell family is closed, inside generic bodies as well: a user
  function's returned `Some` owns its payload through `__opt_dtor_<kind>`,
  a tuple payload and an inner optional shell included, an optional handed
  to a deeper optional (`x: Int?` into an `Int??`) gets its outer shell at
  every widening site, and the transferring runtime getters (`env_opt` behind `os.get_env`, the
  string `get`, a channel receive) have the matching destructor attached at
  the call site, so every extraction spelling is flat on call subjects and
  bound locals alike. the borrowing getters (`m.get(k)`, `xs.first()`) keep
  destructor-less shells on purpose — their payload count stays with the
  container.
- a `Set` element and a `Map` key hash int, string and bytes flavors only.
  any other element or key type used to fall into a flavor anyway and read
  its value's word as if it were that flavor: distinct optionals and lists
  collapsed into one entry through their allocation headers, a struct
  compared raw memory bytes, a plain enum stored nothing at all, and a float
  crashed the process. every such type is rejected at the checker now (E254,
  issues #920 and #955). the bytes flavor is the content-hashing one: a
  `Set[Bytes]` or a `Map[Bytes, V]` keeps its own copy of each key's content
  and finds an entry by content, so a key built from a string and the same
  bytes assembled in a buffer are one entry, and the container never holds a
  count on the caller's bytes object. its `for` and `keys()` hand out fresh
  bytes objects owned by the list they come in, as the string flavor's do.
  a tuple of hashable elements or a struct of hashable fields is a different
  design, not one more flavor: the runtime would have to hash and compare a
  shell field by field with each field's own rule (a string field by
  content, an int field by value, a nested bytes field by content), the
  emitter would have to describe that shape to the constructor the way it
  describes a closure's captured slots, and the stored copy would have to
  carry a count on every counted field. the bytes flavor shares none of that
  machinery with them.
- most std resource types must still be closed by hand. a struct can
  implement `Drop` now (docs/ownership.md, "destructors"): its `drop` runs
  from the destructor the compiler attaches, when the last value naming it
  goes away, and `std.net.tls.Config` is the first std type on it — a config
  built and never closed gives its registry slot back on its own. the rest of
  the survey in docs/destructors_roadmap.md — the buffered text readers and
  writers, websocket sessions, gRPC streams, tls connections and listeners,
  files, processes and the database handles — still close by hand. for most
  of them the reason is the same: std builds a second wrapper over the same
  handle somewhere (`conn_from_handle`, `listener_from_handle`, a struct
  rebuilt from a stored `Int`), and a destructor on such a type would close a
  live resource out from under the other box. each moves over once it is made
  single-owner, which is the roadmap's stage 2 and is std work rather than
  compiler work; the buffered io types have no such rebuild and are simply
  next in line. three
  compiler-side gaps remain as well: a generic struct cannot implement `Drop`
  (its destructor is generated per instance with no place for the call), the
  bounded leaks listed under docs/ownership.md are now a missed `drop` as well
  as a missed free, and a `drop` that lets `self` escape dangles rather than
  being refused.
- a channel is a counted heap value like a list or a struct. a handle in a
  local, a struct field, a container or a `spawn` capture carries a count, and
  the last release frees the channel outright, draining and releasing any
  values nobody received, so neither a channel that was closed with values in
  its ring nor one that was never closed outlives its last holder. a server
  opening one channel per request no longer grows (`leak_channel_lifecycle`
  and `leak_channel_create_drop` pin it). `Channel[T](n)` still allocates a
  fixed part of about 416 bytes plus an eager ring of 16 bytes per slot,
  rounded up to a power of two, for as long as it lives. what remains: a send
  that passes its closed check just as a concurrent close drains the channel
  can enqueue a value nobody will read; the send reports success and the value
  is unobservable. that is a property of close rather than of reclamation, and
  the same interleaving strands the value in the ring without it. the
  os-thread backend pays part of the reclamation cost in channel throughput
  that the green default does not; the guard that carried most of it is gone,
  and the measurements are in docs/channel_ownership.md. run with
  `PITH_PERF_STATS=1` to see
  `channels: new=N closed=C freed=F retained_bytes=B freed_bytes=D`. the
  lifetime analysis is in docs/channel_ownership.md (issues #960 and #984).
- a handful of edge cases logged during bring-up (cross-module float returns,
  cross-module map reads, set codegen, negative float literals like `-1.0`) were
  re-checked and all pass; they are now pinned by regression tests
  (`tests/cases/test_xmod_float.pith` and friends).
- `os.set_env` and `os.unset_env` no longer write to libc's environment.
  calling `setenv` once a process has more than one thread is not safe: it
  reallocates and compacts the `char **` that `getenv` walks, under a lock
  `getenv` does not take. the runtime always has readers it does not control —
  `getaddrinfo` on the dns pool consults `RES_OPTIONS` and friends, `execvp` in
  a spawning child reads `PATH` — and rust's internal env lock covers only
  rust-side accesses. so the two calls record their effect in an overlay the
  runtime owns. `env.get` and `os.get_env` read the overlay first and the
  process environment second, and a child process gets the overlay merged into
  its environment at spawn, before any per-command `env` override, so an
  override the caller named still wins. only names the program itself wrote are
  answered from the overlay, so a variable something outside the runtime
  changed is still read from libc. the cost is on the write side: a c library
  reading the real environment inside this process does not see a variable the
  program set, whenever it was set. `RES_OPTIONS` set from pith no longer
  reaches the resolver, for instance; it has to be in the environment the
  process starts with.
- closing an fd-backed handle from another task is safe, but a pipe read
  blocked in the kernel holds the number. a socket or a child's pipe reaches
  the language as a handle that carries a generation and a user count rather
  than as its raw descriptor number (see "closing a connection another task
  is using" in docs/concurrency.md), so a call on a closed handle fails with
  an error and the number is not reused while a call is inside on it. the one
  case that cannot be woken is a read on a child's pipe blocked in the kernel
  under `PITH_GREEN=0`: a pipe has no `shutdown`, so the read returns only
  when the child writes or exits, and the descriptor is closed then. the
  green backend, the default, wakes it at once through the reactor. the
  handle is dead to every other call from the moment of the close either way.

## the green backend, now the default on linux

as of 2026-07-27 the green backend is what a spawned task runs on when you
build for linux; `PITH_GREEN=0` switches back to one os thread per task, and on
macos and the bsds os threads are still the default with `PITH_GREEN=1` as the
opt-in. green beats the os-thread backend on every shape this repo measures: spawn by
~30x at comparable memory, and the channel fan-out benchmark by several times.
on fan-out it beats zig and trails go and rust at the default worker count; the
current numbers and their caveats are in docs/performance.md. the whole regression
corpus, over 600 cases at both worker counts, produces byte-identical output to
the recorded goldens (`make verify-green-corpus`, run in ci). what follows is
what the new default still costs you, not a list of things blocking it.

the structural cost is that a green worker runs many tasks, so a call with no
yield point holds all of them rather than only the task making it. sockets go
through the epoll reactor, dns and file i/o go to pools of blocking threads
while the caller parks, and child processes park on the reactor too: pipe reads
because a pipe is pollable, `wait` because linux gives out a `pidfd` that
reports the exit. none of those stall anyone any more.

what is left is the cheap end of `host_fs`, meaning `exists`, `size`,
`rename`, `mkdir` and removing a single file, which still runs on the worker,
because each is one cached kernel lookup that costs about what handing it to
another thread would. on a slow network mount that reasoning does not hold and
a task doing one of them holds its worker until it returns. making that
decision adaptive rather than fixed is the open work.

(`process.output` and the calls built on it — `run`, `text`, `output_checked`,
`run_shell`, `output_shell`, plus `exec` and `exec_output` — used to be on
this list for holding a worker while a child ran to completion; they now hand
the whole spawn-drain-wait to a process pool of their own and the caller
parks, the same shape dns and file i/o use. `sleep` used to be on it too:
`time.delay` mapped to a blocking sleep, so a sleeping task held its worker
and an idle `select`, probing in a one-millisecond sleep loop, could pin a
whole worker to no work at all. a green task's sleep now registers a timer on
the reactor's deadline heap — the same sweep that times out socket waits — and
parks the way a socket read does, which carries `select`'s idle probing, the
`concurrent.after`/`ticker` workers, and a context's deadline watcher along
with it.)

the calling task pays for that. a file call made from inside a task now costs a
thread handoff it did not before, so a task reading a small cached file in a
loop runs roughly three times slower than it used to while everything else on
its worker runs sooner. a short on-CPU wait before the park keeps the common
case from paying for two thread wakeups. calls from `main` are unaffected, since
main is not a green task and takes the direct path.

preemption is a build-time opt-in for the same reason it always was. safe-points
are only emitted under `PITH_GREEN_PREEMPT=1`, so a compute-only task that never
touches a channel or socket holds its worker until it finishes, where the kernel
gives os threads that for free. turning safe-points on costs ~0% on real work
(the event-ledger and std-pipeline benchmarks are within noise) and ~6% on a
degenerate 200-million-iteration arithmetic loop, so the flag is cheap, but a
build that will never run green should not pay for a check that cannot fire.

the reactor being linux-only is why the default is linux-only. it is epoll and
eventfd; elsewhere the fallback has no reactor and a green task waiting on a
socket blocks its worker outright. green stays available on those platforms and
stays correct, but it would be a regression as a default until there is a kqueue
sibling.

placement is left to luck. a task pins to the first worker that runs it, so
whether two tasks that talk to each other land together is chance, and the
fan-out benchmark is bimodal because of it: ~46 ms pinned to one worker with
`PITH_GREEN_WORKERS=1`, ~60 ms when the pipeline happens to share a worker
anyway, ~96-100 ms on the runs that split, which are the common case.
cross-worker wakes are the whole remaining
gap to go on coordination-heavy work.

the obvious fix is to move a parked task to whichever worker keeps waking it,
and it does work: prototyped, it took the fan-out from a bimodal ~120 ms median
to a flat ~42 ms, ahead of go's ~69, with cross-worker wakes dropping from
~100k to single digits. the first attempt was also unsound, and the reason is
worth recording so nobody spends the same day rediscovering it. the problem
was not the coroutine stack, which migrates fine; it was that the runtime's
own rust frames cached the thread-local base across a suspension, so a
coroutine resumed on another thread read the previous thread's `CURRENT_TASK`
and `CURRENT_WORKER`. that was observed directly — one os thread reporting two
different values of a variable written once at startup — and it silently
dropped channel messages. the mechanism is the tls model the runtime archive
was compiled with: a library function fetches its thread's tls base once and
adds offsets to it for every access in the frame, so any frame that reads a
thread-local both before and after a park reads the second one stale. that
hazard is now closed. the workspace is compiled for the executable the runtime
always ends up in (`-C relocation-model=pie`, in `.cargo/config.toml`), which
makes every plain thread-local access one `%fs:`-relative instruction with no
base for a frame to keep; the few cells whose address has to exist as a value
(the lazily initialized pool handle, the `RefCell` maps behind `threadlocal`
globals off the green backend and the collector's mutator slot) are touched
only inside functions the optimizer may not inline. `make check-tls-barriers`
audits the built archive for both, and `cargo test` drives a forced
cross-thread resume against the shipped archive to keep it that way. the move
itself is in, behind `PITH_GREEN_MIGRATE=1`: a worker that wakes a task parked
elsewhere takes it, which turns the fan-out's bimodal ~93 ms into a flat
~43 ms at the default worker count (`docs/performance.md` has the table). it
is off by default because the same rule pulls parallel work onto one worker
when it synchronizes: eight compute tasks reporting to a collector run 259 ms
on two workers without the flag and 337 ms with it, since a pinned task is
never stolen and nothing spreads it back out. the default waits on an idle
worker being allowed to take a ready task off a peer's queue. without the
flag a task still pins to the first worker that runs it.
`examples/grpc_chat` and `examples/grpc_reflect` are the two programs
sensitive enough to catch a placement change going wrong; run them first.

one caveat on the numbers themselves: every comparison in docs/performance.md
and bench/README.md was measured on the same 2-core box. "green wins everywhere"
is true there and unverified on wider hardware.

both ownership issues that were outstanding here are fixed. a bare `T!` or
`T?` local passed as a call argument is borrowed by the callee, and the
caller's cleanup stays the payload's owner (`leak_optional_shell_arg` and
`leak_result_arg_borrow` pin it). the double-extraction use-after-free is
fixed. `unwrap_or` and `.value()` both retain the payload they extract, so the
result carries its own count — a bind transfers it, an argument position
releases it after the call, an owned receiver is released after the member read
— and the subject's cleanup cascade stays the sole owner of the count its shell
keeps. the retain covers a parameter, field, element or fresh subject, and any
local whose uses the cascade walk can prove safe.

the exception to that retain is narrower than it was. it exists for a local
whose cleanup releases nothing but the shell, where a retained count would
have no release. it used to cover every local the cascade walk rejected, and
that swept in four shells which are not the payload's last owner. a shell built
by widening a plain value into an optional (`o: List[Int]? := [1, 2]`, and the
same wrap inside a literal or a field) carries a destructor that drops the
payload with it. a borrowed shell (`e := rows[0]`, `e := h.opt`) leaves the
container or struct behind it doing the releasing. a shell a borrowing runtime
getter built (`e := m.get(k)`, `e := xs.first()`) is the same story one step
further out: the shell itself is fresh, so nothing about that bind looks
borrowed, but the count on what it points at never left the container. and a
shell an extraction handed out of a doubly-wrapped optional
(`e := outer.value()`, where the payload is itself a `T?`) arrives as one
retained count on a shell its owner still holds, which goes on releasing what
it wraps through its own destructor. extracting from any of these handed back a
handle its owner then freed, which showed up first on container payloads, where
the freed handle came back from the registry as somebody else's list. all four
retain now. what still transfers is what the exception was written for: a local
holding a dtor-less shell a callee built and returned, whose uses the walk
cannot prove safe (one also sent to a channel, captured by a closure, or handed
to a callee the walk cannot read). extracting such a local twice is still
unsound — which is why both extraction spellings are on the cascade walk's
whitelist, so a local that only ever extracts keeps its cascade and takes a
fresh count each time rather than handing the same one out twice.

sweeping std's shared globals for the same class of bug turned up one thing
worth repairing and two that are questions of design rather than repairs, so
the second pair is recorded here instead of decided.

the repair was `std.metrics`, which used to be correct under concurrency and
not scale: one mutex covered all twelve registries, so every counter increment,
gauge set and histogram observation in the process serialized on it, and a
metric written once per request is the normal case. it now writes without a
lock. an instrument — the value `counter()`, `gauge()` or `histogram()` hands
back — holds the atomic cells of its own series rather than a name to look up,
so `inc`, `add` and `observe` compare-and-set those cells, `set` stores into
one, and none of them goes near the registry. two tasks bumping the same
counter contend for a cache line instead of for a mutex, and a task that loses
a compare-and-set retries where it used to park. the registry directory is
still guarded, because a map cannot be read while another thread inserts into
it, but it is sharded over eight mutexes and only a lookup by name touches it,
which is the path `std.net.http` walks once per label set per request.

measured on the same 2-core box with `bench/metrics_contention`, every writer on
one series, medians of 25 interleaved trials a cell, at one, two, four and eight
tasks. a counter went from 7.8M, 5.6M, 5.5M and 4.2M increments a second to
16.7M, 6.8M, 6.9M and 6.9M. a gauge set went from 10.3M, 7.7M, 7.3M and 5.0M to
33.3M, 14.8M, 14.8M and 14.8M. a histogram observation went from 381k, 285k,
276k and 275k to 5.3M, 1.3M, 1.3M and 1.3M, most of that last one because the
buckets hold per-boundary counts now, so an observation moves one cell instead
of walking ten. the whole request-shaped call with the lookup paid every time,
`counter(name).labels(pairs).inc()`, went from 90k, 66k, 69k and 66k to 90k,
93k, 92k and 90k.

the aggregate stops falling as writers are added, which was the complaint. what
is left is the step from one writer to two, and that is the cache line rather
than a lock: it is there for the counter and the histogram, whose writes are
compare-and-set retry loops, and gone for the gauge, whose write is a single
store.

two ways out were measured and dropped. striping a counter over several cells
and summing them at scrape time buys nothing here, because pith cannot pad an
`AtomicInt` to a cache line: two separately allocated cells written by two tasks
ran within 8% of one cell written by both, which is the same line either way. a
lock per series loses on the case that matters: the series every request writes
is one series, and its writers queue on its lock exactly as they queued on the
global one.

what it costs is the process-wide snapshot. a scrape takes every shard lock in
index order, copies the directory and lets them go, then reads the cells
unlocked, so two series read microseconds apart need not describe the same
instant. one histogram is still coherent, and coherent by construction rather
than by exclusion: the exposition adds the per-boundary counts up as it walks
the boundaries, so the cumulative numbers it prints are non-decreasing however
stale each term is, and the `+Inf` bucket and the `_count` line are the same
running total rather than two reads that have to agree. the order that does
matter is that an observation publishes its sum, min and max before the bucket
that makes it visible; `std/metrics.pith` has the scrape-during-writes test that
fails if that is reversed, and `tests/cases/test_metrics_registry` pins the
exposition byte for byte.

the edge left on it is `reset()`. it clears the registry and leaves behind the
cells instruments are already holding, so an instrument taken before a reset
goes on writing to a series nothing can scrape. take a fresh one after.

a `std.net.tls` config closes itself when the last value naming it goes away:
`Config` implements `Drop`, so a program that builds its own config for
`dial_with_config` and forgets `close()` no longer holds a registry slot until
it exits. what the destructor does not decide is *when*. it runs when the last
holder lets go, which for a config a listener is still handshaking on is later
than the caller may think and never earlier than is safe (the listener and each
handshake hold a count), so a server that wants its certificate released at a
particular point in its shutdown still closes by hand, and the E307 lint no
longer reports a config that is built and left to the destructor. the other
std types in the same position are listed under the backend section above and
in [docs/destructors_roadmap.md](destructors_roadmap.md).

the root bundle cache that made per-request configs cheap is capped at eight
distinct bundles and never evicts. a process that trusts more than eight
different ca bundles keeps working and keeps re-parsing the ones past the cap.
that is the right failure for the shape of the problem — programs trust one
bundle, or two — but it is a fixed number chosen rather than derived, and a
bundle that stops being used is never reclaimed.

`std.args` is safe by convention, with nothing enforcing the convention. its ten
globals have no lock, which is fine given the parser's shape: init, then the
add_flag / add_option / add_positional calls, then parse, all from main, after
which everything left only reads. no path in the module mutates from the query
side, so the state really is fixed once parse returns. nothing stops a program
from calling the setup half from a spawned task, though, and there would be no
diagnostic if it did — just a torn string. a lock here is cheap. whether a cli
argument parser should carry one is the part worth an opinion.
