# Limitations

Pith is self-hosting and runs real programs, but it is not finished. This page
lists what does not work yet, or works with a catch, so you can plan around it.
Open defects carry their issue number. If something here now works, the page is
out of date, and a fix to it is welcome.

## Language

- **A variant pattern must be qualified**. Write `Color.Red`, not `Red`, or
  `colors.Color.Red` for an enum another module declares. A bare
  name in a pattern is a binding that matches every value, so one that names a
  variant of the matched enum is rejected (E228). `==` on enums
  with payloads compares tags and then payloads, strings by content and nested
  enums recursively, but a struct payload compares by identity:
  `Box.Holds(P(1)) == Box.Holds(P(1))` is false.
- **`==` is refused on lists, maps and sets**. It would compare the
  handles, so the checker reports E217 instead. Use `lists_equal` or
  `sets_equal` from `std.collections`, or compare a map's length and look up
  each key. `==` on structs still compares identity.
- **Range patterns take integer literals**. `0..=9 => ...`, `0..10 => ...`
  and `-5..=-1 => ...` work on integer subjects, with or-patterns and guards.
  A named constant (`LO..=9`) does not parse, and a `Float` subject is E228.
  Use a guard instead: `n if n < LO => ...`.
- **`if let` and `while let` are statements**. Neither works as an expression,
  and the let position takes a variant pattern or an optional binding only. A
  literal (`if let 3 = f():`) is E245. A variant pattern on an optional enum
  matches the payload (`if let Shape.Circle(r) = maybe_shape:`), but `match`
  does not unwrap that way: match the optional with a binding arm first.
- **An unannotated `none` takes its type from the first assignment**.
  `mut x := none` followed by `x = 5` makes `x` an `Int?`, and a later
  `x = "s"` is rejected. Annotate the binding when the first assignment is far
  from the declaration.
- **Where a plain value does not widen into an optional**. `3` is accepted
  wherever an `Int?` is expected (bindings, returns, arguments, fields, container
  stores and literals, variant payloads, generic parameters), with three
  exceptions:
  - An arrow-bodied lambda with a declared optional return:
    `fn(x: Int) -> Int? => x` is E200. The block form (`fn(x: Int) -> Int?:`
    with `return x`) widens.
  - Ordering operators: `o < 5` on an `Int?` is E217. `==` and `!=` do widen;
    `o == 5` is true only when `o` holds `5`.
  - A set element or a map key. See the next entry.
- **Set elements and map keys must be ints, strings or bytes**. `Set[T]` and
  `Map[K, V]` hash those three kinds only, and any other element or key type
  (an optional, `Float`, `Bool`, a struct, an enum, a tuple, a container, a
  function value) is rejected with E254. Key by a string, integer or bytes
  encoding of the value instead. Bytes keys hash and compare by content. An
  optional map *value* is allowed.
- **Some runtime errors stop the process**. `xs[i]` and `xs[i] = v` out of
  range, `m[k]` on a missing key and `.value()` on a `none` print
  `pith runtime error: ...` and exit, except inside a `T!` function, where
  they propagate as an error. Division by zero and integer overflow exit in
  either case.
  `xs.get(i)` and `m.get(k)` return an optional, and `if let`, `match` and
  `unwrap_or` handle a `none` without stopping.

  Inside an HTTP handler (std.net.http, std.web, the HTTP/2 server) or a gRPC
  method, the same errors fail only that request. The client gets a 500, or
  gRPC status `INTERNAL`, the message goes to stderr with the request's method
  and path, and the server keeps serving. What the failed handler held is not
  freed and its `defer` blocks do not run, though a `Mutex` it held is
  unlocked. An error in main or in any other task, a stack overflow and a
  failed allocation still stop the process. docs/failure_isolation.md has the
  details.
- **A nested binding cannot shadow a local of the same function**. A function
  has one slot per name, so a `:=`, a `match`, `if let` or `while let` payload,
  a select arm or a `catch` name in a nested block that reuses an enclosing
  local's name is E262. Rename the inner one. A `for` variable may shadow a
  local, since it gets a slot of its own. A closure parameter may too, and so
  may a binding in a closure body when the closure does not capture that name.
- **A local that shadows a module global takes the name for the whole
  function**. The global is unreachable from that function, except on either
  side of a `for` loop whose variable shadows it. Reading the global and then
  binding a local of the same name in one function is E266. `pith lint` reports
  the shadowing as E308.
- **A local spelled like an import alias hides the module in its scope**. With
  `import std.text as text`, a parameter, local, loop variable, payload binding
  or closure parameter named `text` is that value wherever it is in scope, as a
  receiver too: `text.len()` is the String method. The module is reachable
  again outside that scope, and a module global spelled like an alias does not
  hide it. The parser still reads one spelling by name: a call with written
  type arguments, `text.describe[Int](v)`, on a local spelled like an alias
  parses as an index followed by a call, so bind the method's receiver under
  another name first.
- **Same-named types in different modules**. Two modules can each declare a
  struct or enum, plain or generic, with the same name, and each value
  dispatches to its own module's methods. A second impl block that declares a
  method the struct already has is E263 (an interface impl may repeat an
  inherent method of the same name, as std/io does).
- **Interface values have no downcasts or equality**. A value of an
  interface type cannot be tested for or converted back to its concrete type,
  and `==` on two of them compares identity. An interface with type
  parameters or associated types, or with a method that has type parameters,
  cannot be a value type (E316). A generic struct can implement an interface
  for bounds using its bare name (`impl Iterator for FilterIter:`, not
  `impl Shape for Box[T]`), but cannot yet convert to an interface value. See
  docs/interface_values.md.
- **Method syntax never reaches a free function**. There is no UFCS. A
  primitive receiver resolves builtins only, so `(1.5).to_int()` is the
  `Float` builtin even when a free `fn to_int` exists.
- **Where clauses attach to functions and methods only**. Struct, interface
  and impl headers take inline bounds (`[T: Display]`), and a method's `where`
  clause may name only the method's own type parameters, not the owner's
  (E264).
- **An index into a field of closures can read as type arguments**.
  `t.items[MAX](y)`, with `MAX` a screaming-case constant or a type-shaped name,
  parses as a generic method call, and the checker refuses it with E209. Group
  the index: `(t.items[MAX])(y)`. See docs/generics.md.
- **Generic enum gaps**. `Chain[Int].Link(...)` does not work as a
  constructor. The instance comes from the payload, or from the typed target
  the value is stored into: an annotated binding, a return, an argument, an
  assignment, a field or a list element. A generic function does not infer `T`
  from a `Chain[T]` argument (`head_of(pair)` is E222). A payload-free variant
  bound with no annotation (`b := Opt.Nothing`) is E262.
- **A generic body is checked only at the types the program uses**. Each
  specialization is type checked against its concrete types, so a body that is
  wrong for some type its bound admits is reported only when a caller reaches
  that type. `pith check` names the specialization (`... in bad[String]`).
- **Destructor limits**. The cycle collector does not collect a garbage cycle
  that runs through a `Drop` struct, so that cycle leaks. See docs/ownership.md.

## Standard library

- **TLS 1.2 is a fallback**. The client and server speak TLS 1.3 and fall back
  to 1.2 with ECDHE and AEAD suites only, refusing anything older.
  `require_tls13()` locks a config to 1.3. The 1.2 path does client
  certificates but no session resumption or renegotiation.
- **A destructor is a backstop for `close()`**. Files, processes, I/O readers,
  writers and buffers, TLS connections and listeners, HTTP and HTTP/2
  connections, HTTP/2 and gRPC streams, WebSocket sessions and database
  connections, transactions and statements release what they hold when their
  last value dies. A destructor does no network I/O, so a TLS connection
  dropped without `close()` sends no close_notify, a dropped WebSocket sends no
  close frame, a buffered writer dropped without `flush()` loses what it held,
  and a dropped `std.db` transaction is rolled back by closing its connection.
  A dropped stream that is still open queues its reset only when the
  connection's send queue has room. Call `close()` where the peer must see a
  clean end. A few types still close by hand: `http2.Client` and
  `term.session.Terminal` (whose own tasks keep them alive), `TcpStream` (a
  view over a socket another value owns), parser scopes
  (`open_scope`/`close_scope`), and `concurrent.Ticker`/`Group`/`CancelToken`.
  `pith lint` flags a local of a type without a destructor that is never closed
  (E307). docs/destructors_roadmap.md tracks the rest.
- **Testing has no benchmarks**. `pith test` runs `test` blocks with
  `--filter`, `--tag`, `--exclude-tag` and `--json`, and `std/testing` adds
  assertions, fixtures and parameterized cases, but there is no benchmark
  runner.
- **JSON decode targets**. `json.decode[T]` and `json.decode_text[T]` fill a
  struct whose public fields are `Int`, `String`, `Bool`, `Float`, optionals
  of those, nested structs, `List`s and `Map[String, _]`s, nested to any depth.
  Two shapes are refused with E219: a map with a non-`String` key, and a list
  or map whose element is an optional (`List[Int?]`). Reach those documents
  through `json.parse` and the node accessors. Documents whose arrays and
  objects nest 128 levels deep or more are refused.
- **TOML, YAML and config decode targets are narrower**. `toml.decode`,
  `yaml.decode` and `config.decode` refuse `Float`, `List` and `Map` fields
  (E219). Read those through the parsed document instead.
- **Plaintext HTTP/2 needs its own listener**. `web.listen_tls` negotiates h2
  or HTTP/1.1 over ALPN. Without TLS there is no negotiation, so plaintext
  HTTP/2 means calling `listen_h2c`. `std.net.http` itself is HTTP/1.1.
- **Regex is deliberately small**. `std.regex` covers literals, `.`, classes,
  `\d \w \s` and their negations, greedy `* + ?`, counted repeats `{n}`,
  `{n,}` and `{n,m}` up to 1000, alternation, capturing groups, `(?:...)`
  groups and `^ $`. Matching uses a Pike VM and is linear in the input, and
  `find_all` and `replace_all` are linear in the input too. Compile refuses
  lazy and possessive quantifiers, backreferences, lookaround, atomic and
  named groups, inline flags, `\b`, POSIX classes and other escaped letters,
  with an error that names the feature. A `{` that does not start a counted
  repeat is a literal, as in Go: `a{,2}` matches the text `a{,2}`. Iteration
  skips an empty match right after a match, as Go does. Matching works on
  bytes, so `.` consumes one byte, not one character. A match never ends
  inside a multi-byte character. A pattern whose matches keep doing so is
  rejected at each start and can make iteration quadratic.
- **Gzip compresses with fixed Huffman codes only**. It reads any DEFLATE
  stream and writes valid gzip and zlib. Without dynamic Huffman trees, the
  output is a few percent larger than it could be.
- **`os.set_env` does not reach C code**. `os.set_env` and `os.unset_env`
  write an overlay the runtime owns instead of calling libc's `setenv`, which is
  unsafe once threads exist. Pith's reads (`env.get`, `os.get_env`) and spawned
  children see the overlay, but C code inside the process does not:
  `RES_OPTIONS` set from Pith never reaches the resolver. Set such variables in
  the environment the process starts with.
- **`metrics.reset()` orphans live instruments**. An instrument taken before a
  reset keeps writing to a series nothing scrapes. Take a fresh one after the
  reset.
- **The TLS root bundle cache holds eight bundles**. A process that trusts
  more than eight distinct CA bundles keeps working but re-parses the ones past
  the cap on every config, and a cached bundle is never evicted.

## Tooling

- **The language server caches one import closure**. An edit to the open
  document re-checks that document alone, but an edit to an imported module
  re-checks the whole closure, and switching between two open documents misses
  the cache on each switch. References on a common name in a large closure can
  fail to answer (#1071). See docs/lsp.md.
- **A declaration or a statement points at its last token**. An expression,
  type or pattern diagnostic points at where it starts, but one about a
  function or a statement names the line of its last token: a duplicate method
  is reported on the last line of its body.
- **No package registry**. Dependencies are local path entries in
  `pith.toml`. `pith package lock` and `pith package install` work on those
  paths, but nothing fetches over the network.
- **No debugger**. A runtime error prints its message without a stack trace,
  and there is no stepping.

## Memory

- **An unmarked strong reference cycle leaks**. Reclamation is reference
  counting, so a cycle with no `weak` edge is never freed. Break it with a
  `weak` field, a `weak name := expr` local, or a closure that captures a weak
  binding (docs/ownership.md). `pith lint` reports a strong cycle between a
  module's structs as E306. An experimental cycle collector runs behind
  `PITH_CYCLE_GC=1`, off by default, with `std.concurrent.gc_collect()` to force
  a pass.
- **Closing a channel can race a send**. A send that passes its closed check
  as another task closes the channel can enqueue a value nobody reads, and the
  send reports success. A channel costs about 448 bytes plus 16 bytes per
  buffered slot (rounded up to a power of two) for as long as it lives.

## Concurrency

- **The green backend is the default on Linux only**. Spawned tasks run on an
  M:N green scheduler with an epoll reactor. `PITH_GREEN=0` switches to one OS
  thread per task. macOS and the BSDs default to OS threads because there is no
  kqueue reactor yet. `PITH_GREEN=1` opts in there, but a green task waiting
  on a socket blocks its whole worker.
- **A call with no yield point holds its worker**. Sockets, DNS, file reads
  and writes, child processes and sleeps all park the task. The cheap
  filesystem calls (`exists`, `size`, `rename`, `mkdir`, removing one file) run
  on the worker, so on a slow network mount they hold every task on it until
  they return. A file call from inside a task pays a thread handoff that a call
  from `main` does not.
- **A green task's stack does not grow**. It is 1 MiB unless
  `PITH_GREEN_STACK_KB` sets another size, and recursion that needs more
  stops the process with a stack overflow report.
- **Preemption costs a check per loop back-edge**. Compute-bound tasks are
  preempted at safe-points so a spinning loop cannot starve the tasks that
  share its worker. Building with `PITH_GREEN_PREEMPT=0` removes the check,
  and with it the guarantee. The preemption request flag is process-global, so
  every running task takes a slow path when any one task overruns (#1089).
- **A call through a function value is followed to every closure**. The
  sharing rules (E248, E276) send its arguments to any closure parameter and
  take its result from any closure result, not from the one closure it
  calls, so an argument passed through a function value counts as kept where
  the checker cannot follow it. `std.net.tls`'s `config_edit` stores a copy
  of the record its edit closure changed for this reason.
- **The sharing rules are flow-insensitive within a function**. A local
  counts as used again after a handoff when its name appears later in the
  source, outside another branch of the same `if` or `match` and before any
  `return` that follows the handoff. It counts as changed when it is changed
  anywhere in the function. Reorder the code, or read what is needed before
  the handoff.
- **A blocked pipe read under `PITH_GREEN=0` ignores a close**. Closing a
  child's pipe from another task fails every later call on the handle, but a
  read already blocked in the kernel returns only when the child writes or
  exits. The green backend wakes it at once.
