# limitations

pith is self-hosting and runs real programs, but it is not finished. this page
lists what does not work yet, or works with a catch, so you can plan around it.
open defects carry their issue number. if something here now works, the page is
stale and a fix to it is welcome.

## language

- **a variant pattern must be qualified**. write `Color.Red`, not `Red`. a bare
  name in a pattern is a binding that matches every value, so one that names a
  variant of the matched enum is rejected (E228). `==` on enums
  with payloads compares tags and then payloads, strings by content and nested
  enums recursively, but a struct payload compares by identity:
  `Box.Holds(P(1)) == Box.Holds(P(1))` is false.
- **`==` is refused on lists, maps and sets**. it would compare the
  handles, so the checker reports E217 instead. use `lists_equal` or
  `sets_equal` from `std.collections`, or compare a map's length and look up
  each key. `==` on structs still compares identity.
- **range patterns take integer literals**. `0..=9 => ...`, `0..10 => ...`
  and `-5..=-1 => ...` work on integer subjects, with or-patterns and guards.
  a named constant (`LO..=9`) does not parse, and a `Float` subject is E228.
  use a guard instead: `n if n < LO => ...`.
- **`if let` and `while let` are statements**. neither works as an expression,
  and the let position takes a variant pattern or an optional binding only. a
  literal (`if let 3 = f():`) is E245. a variant pattern on an optional enum
  matches the payload (`if let Shape.Circle(r) = maybe_shape:`), but `match`
  does not unwrap that way: match the optional with a binding arm first.
- **an unannotated `none` takes its type from the first assignment**.
  `mut x := none` followed by `x = 5` makes `x` an `Int?`, and a later
  `x = "s"` is rejected. annotate the binding when the first assignment is far
  from the declaration.
- **where a plain value does not widen into an optional**. `3` is accepted
  wherever an `Int?` is expected (bindings, returns, arguments, fields, container
  stores and literals, variant payloads, generic parameters), with three
  exceptions:
  - an arrow-bodied lambda with a declared optional return:
    `fn(x: Int) -> Int? => x` is E200. the block form (`fn(x: Int) -> Int?:`
    with `return x`) widens.
  - ordering operators: `o < 5` on an `Int?` is E217. `==` and `!=` do widen;
    `o == 5` is true only when `o` holds `5`.
  - a set element or a map key. see the next entry.
- **set elements and map keys must be ints, strings or bytes**. `Set[T]` and
  `Map[K, V]` hash those three flavors only, and any other element or key type
  (an optional, `Float`, `Bool`, a struct, an enum, a tuple, a container, a
  function value) is rejected with E254. key by a string, integer or bytes
  encoding of the value instead. bytes keys hash and compare by content. an
  optional map *value* is fine.
- **some runtime errors stop the process**. `xs[i]` and `xs[i] = v` out of
  range, `m[k]` on a missing key and `.value()` on a `none` print
  `pith runtime error: ...` and exit, except inside a `T!` function, where
  they propagate as an error. division by zero and integer overflow exit in
  either case.
  `xs.get(i)` and `m.get(k)` return an optional, and `if let`, `match` and
  `unwrap_or` handle a `none` without stopping.

  inside an http handler (std.net.http, std.web, the http/2 server) or a grpc
  method, the same errors fail only that request: the client gets a 500, or
  grpc status `INTERNAL`, the message goes to stderr with the request's method
  and path, and the server keeps serving. what the failed handler held is not
  freed and its `defer` blocks do not run, though a `Mutex` it held is
  unlocked. an error in main or in any other task, a stack overflow and a
  failed allocation still stop the process. docs/failure_isolation.md has the
  details.
- **a nested binding cannot shadow a local of the same function**. a function
  has one slot per name, so a `:=`, a `match`, `if let` or `while let` payload,
  a select arm or a `catch` name in a nested block that reuses an enclosing
  local's name is E262. rename the inner one. a `for` variable may shadow a
  local, since it gets a slot of its own. a closure parameter may too, and so
  may a binding in a closure body when the closure does not capture that name.
- **a local that shadows a module global takes the name for the whole
  function**. the global is unreachable from that function, except on either
  side of a `for` loop whose variable shadows it. reading the global and then
  binding a local of the same name in one function is E266. `pith lint` reports
  the shadowing as E308.
- **a local spelled like an import alias hides the module in its scope**. with
  `import std.text as text`, a parameter, local, loop variable, payload binding
  or closure parameter named `text` is that value wherever it is in scope, as a
  receiver too: `text.len()` is the String method. the module is reachable
  again outside that scope, and a module global spelled like an alias does not
  hide it. the parser still reads one spelling by name: a call with written
  type arguments, `text.describe[Int](v)`, on a local spelled like an alias
  parses as an index followed by a call, so bind the method's receiver under
  another name first.
- **same-named types in different modules**. two modules can each declare a
  struct or enum, plain or generic, with the same name, and each value
  dispatches to its own module's methods. a second impl block that declares a
  method the struct already has is E263 (an interface impl may repeat an
  inherent method of the same name, as std/io does).
- **interface values have no downcasts or equality**. a value of an
  interface type cannot be tested for or converted back to its concrete type,
  and `==` on two of them compares identity. an interface with type
  parameters or associated types, or with a method that has type parameters,
  cannot be a value type (E316). a generic struct can implement an interface
  for bounds using its bare name (`impl Iterator for FilterIter:`, not
  `impl Shape for Box[T]`), but cannot yet convert to an interface value. see
  docs/interface_values.md.
- **method syntax never reaches a free function**. there is no ufcs. a
  primitive receiver resolves builtins only, so `(1.5).to_int()` is the
  `Float` builtin even when a free `fn to_int` exists.
- **where clauses attach to functions and methods only**. struct, interface
  and impl headers take inline bounds (`[T: Display]`), and a method's `where`
  clause may name only the method's own type parameters, not the owner's
  (E264).
- **an index into a field of closures can read as type arguments**.
  `t.items[MAX](y)`, with `MAX` a screaming-case constant or a type-shaped name,
  parses as a generic method call, and the checker refuses it with E209. group
  the index: `(t.items[MAX])(y)`. see docs/generics.md.
- **generic enum gaps**. `Chain[Int].Link(...)` does not work as a
  constructor; let an annotated binding or the payload supply the instance. a
  generic function does not infer `T` from a `Chain[T]` argument
  (`head_of(pair)` is E222). a payload-free variant bound with no annotation
  (`b := Opt.Nothing`) is E262.
- **a generic body is checked only at the types the program uses**. each
  specialization is type checked against its concrete types, so a body that is
  wrong for some type its bound admits is reported only when a caller reaches
  that type. `pith check` names the specialization (`... in bad[String]`).
- **destructor limits**. a garbage cycle that runs through a `Drop` struct is
  not collected by the cycle collector: it leaks. see docs/ownership.md.

## standard library

- **tls 1.2 is a fallback**. the client and server speak tls 1.3 and fall back
  to 1.2 with ecdhe and aead suites only, refusing anything older.
  `require_tls13()` locks a config to 1.3. the 1.2 path does client
  certificates but no session resumption or renegotiation.
- **a destructor is a backstop for `close()`**. files, processes, io readers,
  writers and buffers, tls connections and listeners, http and http/2
  connections, http/2 and grpc streams, websocket sessions and database
  connections, transactions and statements release what they hold when their
  last value dies. a destructor does no network i/o, so a tls connection
  dropped without `close()` sends no close_notify, a dropped websocket sends no
  close frame, a buffered writer dropped without `flush()` loses what it held,
  and a dropped `std.db` transaction is rolled back by closing its connection.
  a dropped stream that is still open queues its reset only when the
  connection's send queue has room. call `close()` where the peer must see a
  clean end. a few types still close by hand: `http2.Client` and
  `term.session.Terminal` (whose own tasks keep them alive), `TcpStream` (a
  view over a socket another value owns), parser scopes
  (`open_scope`/`close_scope`), and `concurrent.Ticker`/`Group`/`CancelToken`.
  `pith lint` flags a local of a type without a destructor that is never closed
  (E307). docs/destructors_roadmap.md tracks the rest.
- **testing has no benchmarks**. `pith test` runs `test` blocks with
  `--filter`, `--tag`, `--exclude-tag` and `--json`, and `std/testing` adds
  assertions, fixtures and parameterized cases, but there is no benchmark
  runner.
- **json decode targets**. `json.decode[T]` and `json.decode_text[T]` fill a
  struct whose public fields are `Int`, `String`, `Bool`, `Float`, optionals
  of those, nested structs, `List`s and `Map[String, _]`s, nested to any depth.
  refused with E219: a map with a non-`String` key, and a list or map whose
  element is an optional (`List[Int?]`). reach those documents through
  `json.parse` and the node accessors. documents whose arrays and objects nest
  128 levels deep or more are refused.
- **toml, yaml and config decode targets are narrower**. `toml.decode`,
  `yaml.decode` and `config.decode` refuse `Float`, `List` and `Map` fields
  (E219). read those through the parsed document instead.
- **plaintext http/2 needs its own listener**. `web.listen_tls` negotiates h2
  or http/1.1 over alpn. without tls there is no negotiation, so plaintext
  http/2 means calling `listen_h2c`. `std.net.http` itself is http/1.1.
- **regex is deliberately small**. `std.regex` covers literals, `.`, classes,
  `\d \w \s` and their negations, greedy `* + ?`, counted repeats `{n}`,
  `{n,}` and `{n,m}` up to 1000, alternation, capturing groups, `(?:...)`
  groups and `^ $`. matching is a pike vm, linear in the input, and
  `find_all` and `replace_all` are linear in the input too. compile refuses
  lazy and possessive quantifiers, backreferences, lookaround, atomic and
  named groups, inline flags, `\b`, posix classes and other escaped letters,
  with an error that names the feature. a `{` that does not start a counted
  repeat is a literal, as in go: `a{,2}` matches the text `a{,2}`. iteration
  skips an empty match right after a match, as go does. matching works on
  bytes, so `.` consumes one byte, not one character. a match never ends
  inside a multi-byte character; a pattern whose matches keep doing so is
  rejected at each start and can make iteration quadratic.
- **gzip compresses with fixed huffman codes only**. it reads any deflate
  stream and writes valid gzip and zlib, but without dynamic huffman trees the
  output is a few percent larger than it could be.
- **`os.set_env` does not reach c code**. `os.set_env` and `os.unset_env`
  write an overlay the runtime owns instead of calling libc's `setenv`, which is
  unsafe once threads exist. pith reads (`env.get`, `os.get_env`) and spawned
  children see the overlay, but c code inside the process does not:
  `RES_OPTIONS` set from pith never reaches the resolver. set such variables in
  the environment the process starts with.
- **`metrics.reset()` orphans live instruments**. an instrument taken before a
  reset keeps writing to a series nothing scrapes. take a fresh one after.
- **the tls root bundle cache holds eight bundles**. a process that trusts
  more than eight distinct ca bundles keeps working but re-parses the ones past
  the cap on every config, and a cached bundle is never evicted.

## tooling

- **the language server caches one import closure**. an edit to the open
  document re-checks that document alone, but an edit to an imported module
  re-checks the whole closure, and switching between two open documents misses
  the cache on each switch. references on a common name in a large closure can
  fail to answer (#1071). see docs/lsp.md.
- **a declaration or a statement points at its last token**. an expression,
  type or pattern diagnostic points at where it starts, but one about a
  function or a statement names the line of its last token: a duplicate method
  is reported on the last line of its body.
- **no package registry**. dependencies are local path entries in
  `pith.toml`. `pith package lock` and `pith package install` work on those
  paths, but nothing fetches over the network.
- **no debugger**. a runtime error prints its message without a stack trace,
  and there is no stepping.

## memory

- **an unmarked strong reference cycle leaks**. reclamation is reference
  counting, so a cycle with no `weak` edge is never freed. break it with a
  `weak` field, a `weak name := expr` local, or a closure that captures a weak
  binding (docs/ownership.md). `pith lint` reports a strong cycle between a
  module's structs as E306. an experimental cycle collector runs behind
  `PITH_CYCLE_GC=1`, off by default, with `std.concurrent.gc_collect()` to force
  a pass.
- **closing a channel can race a send**. a send that passes its closed check
  as another task closes the channel can enqueue a value nobody reads, and the
  send reports success. a channel costs about 448 bytes plus 16 bytes per
  buffered slot (rounded up to a power of two) for as long as it lives.

## concurrency

- **the green backend is the default on linux only**. spawned tasks run on an
  m:n green scheduler with an epoll reactor. `PITH_GREEN=0` switches to one os
  thread per task. macos and the bsds default to os threads because there is no
  kqueue reactor yet; `PITH_GREEN=1` opts in there, but a green task waiting
  on a socket blocks its whole worker.
- **a call with no yield point holds its worker**. sockets, dns, file reads
  and writes, child processes and sleeps all park the task. the cheap
  filesystem calls (`exists`, `size`, `rename`, `mkdir`, removing one file) run
  on the worker, so on a slow network mount they hold every task on it until
  they return. a file call from inside a task pays a thread handoff that a call
  from `main` does not.
- **a green task's stack does not grow**. it is 1 MiB unless
  `PITH_GREEN_STACK_KB` sets another size, and recursion that needs more
  stops the process with a stack overflow report.
- **preemption costs a check per loop back-edge**. compute-bound tasks are
  preempted at safe-points so a spinning loop cannot starve the tasks that
  share its worker. building with `PITH_GREEN_PREEMPT=0` removes the check,
  and with it the guarantee. the preemption request flag is process-global, so
  every running task takes a slow path when any one task overruns (#1089).
- **a call through a function value is followed to every closure**. the
  sharing rules (E248, E276) send its arguments to any closure parameter and
  take its result from any closure result, not from the one closure it
  calls, so an argument passed through a function value counts as kept where
  the checker cannot follow it. `std.net.tls`'s `config_edit` stores a copy
  of the record its edit closure changed for this reason.
- **the sharing rules are flow-insensitive within a function**. a local
  counts as used again after a handoff when its name appears later in the
  source, outside another branch of the same `if` or `match` and before any
  `return` that follows the handoff, and as changed when it is changed
  anywhere in the function. reorder the code, or read what is needed before
  the handoff.
- **a blocked pipe read under `PITH_GREEN=0` ignores a close**. closing a
  child's pipe from another task fails every later call on the handle, but a
  read already blocked in the kernel returns only when the child writes or
  exits. the green backend wakes it at once.
