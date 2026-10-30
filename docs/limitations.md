# limitations

pith is self-hosting and runs real programs, but it is not finished. this page
lists what does not work yet, or works with a catch, so you can plan around it.
open defects carry their issue number. if something here now works, the page is
stale and a fix to it is welcome.

## language

- **a bare name in a pattern is a binding**. write `Color.Red`, not `Red`, to
  match a variant. `Red => ...` compiles and matches everything. `==` on enums
  with payloads compares tags and then payloads, strings by content and nested
  enums recursively, but a struct payload compares by identity:
  `Box.Holds(P(1)) == Box.Holds(P(1))` is false.
- **range patterns take non-negative integer literals**. `0..=9 => ...` and
  `0..10 => ...` work on integer subjects, with or-patterns and guards. a
  negative bound (`-5..=-1`) and a named constant (`LO..=9`) do not parse, and a
  `Float` subject is E228. use a guard instead: `n if n < 0 => ...`.
- **`if let` and `while let` are statements**. neither works as an expression,
  and the let position takes a variant pattern or an optional binding only. a
  literal (`if let 3 = f():`) is E245.
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
- **`.value()` on an optional is unchecked**. it does not test for `none`. on a
  `none` it reads back a zero (`0` for an `Int`, `""` for a `String`) or
  crashes (a struct field read through it segfaults). check first, or use
  `if let`, `match` or `unwrap_or`.
- **some runtime errors stop the process**. `xs[i]` out of range and `m[k]` on
  a missing key print `pith runtime error: ...` and exit, except inside a `T!`
  function, where they propagate as an error. division by zero exits in either
  case. `xs.get(i)` and `m.get(k)` return an optional.
- **a local that shadows a module global takes the name for the whole
  function**. the global is unreachable from that function, except on either
  side of a `for` loop whose variable shadows it. reading the global and then
  binding a local of the same name in one function is E266. `pith lint` reports
  the shadowing as E308.
- **same-named types in different modules**. two modules can each declare a
  struct, plain or generic, with the same name, and each value dispatches to its
  own module's methods. a second impl block that declares a method the struct
  already has is E263 (an interface impl may repeat an inherent method of the
  same name, as std/io does). three shapes still resolve by the bare name:
  - a generic struct and a plain struct with the same name in two modules: a
    method call on either runs the body from whichever module is imported last,
    with no diagnostic. give them distinct names.
  - a generic function whose signature names a generic its own module declares
    (`iter.map_iter` returning `MapIter`) resolves that name in the caller, so a
    caller that declares its own `MapIter` gets a compile error.
  - two modules that each declare the same generic enum fail to compile
    (`unknown load source` from the backend).
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
- **a generic function in another module, called from inside a generic body,
  can fail to resolve at startup** (#1095). simple shapes work. a non-generic
  wrapper in the calling module is the workaround.
- **`spawn` with a builtin call in an argument fails to compile** (#1082).
  `spawn f(bit_and(x, y))` fails with `unknown load source`. bind the argument
  to a local first.
- **destructor limits**. a generic struct cannot implement `Drop` (E268), and a
  `drop` that lets `self` escape leaves a dangling reference rather than being
  refused. see docs/ownership.md.

## standard library

- **tls 1.2 is a fallback**. the client and server speak tls 1.3 and fall back
  to 1.2 with ecdhe and aead suites only, refusing anything older.
  `require_tls13()` locks a config to 1.3. the 1.2 path does client
  certificates but no session resumption or renegotiation, and its client
  ignores the `certificate_types` of a CertificateRequest (#1134).
- **most resource types close by hand**. `std.net.tls.Config` is the only std
  type that implements `Drop`. files, processes, connections, listeners,
  websocket sessions, grpc streams, buffered readers and writers and database
  handles still need `close()`; `pith lint` flags a local one that is never
  closed (E307). docs/destructors_roadmap.md tracks the rest.
- **testing has no benchmarks**. `pith test` runs `test` blocks with
  `--filter`, `--tag`, `--exclude-tag` and `--json`, and `std/testing` adds
  assertions, fixtures and parameterized cases, but there is no benchmark
  runner.
- **json decode targets**. `json.decode[T]` and `json.decode_text[T]` fill a
  struct whose public fields are `Int`, `String`, `Bool`, `Float`, optionals
  of those, nested structs, `List`s and `Map[String, _]`s, nested to any depth.
  refused with E219: a map with a non-`String` key, and a list or map whose
  element is an optional (`List[Int?]`). reach those documents through
  `json.parse` and the node accessors. documents nested deeper than 128 levels
  are refused.
- **toml, yaml and config decode targets are narrower**. `toml.decode`,
  `yaml.decode` and `config.decode` refuse `Float`, `List` and `Map` fields
  (E219). read those through the parsed document instead.
- **plaintext http/2 needs its own listener**. `web.listen_tls` negotiates h2
  or http/1.1 over alpn. without tls there is no negotiation, so plaintext
  http/2 means calling `listen_h2c`. `std.net.http` itself is http/1.1.
- **regex is deliberately small**. `std.regex` covers literals, `.`, classes,
  `\d \w \s`, greedy `* + ?`, alternation, capturing groups and `^ $`. matching
  is a pike vm, linear in the input. lazy quantifiers and lookaround are
  compile errors, but counted repeats and backreferences are not: `a{2}`
  matches the literal text `a{2}` and `(a)\1` matches `a1`. matching works on
  bytes, so `.` consumes one byte, not one character; a match never ends
  inside a multi-byte character.
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
- **`std.args` has no lock**. set it up and parse from one task. calling the
  setup functions from a spawned task can tear its state with no diagnostic.

## tooling

- **the language server caches one import closure**. an edit to the open
  document re-checks that document alone, but an edit to an imported module
  re-checks the whole closure, and switching between two open documents misses
  the cache on each switch. references on a common name in a large closure can
  fail to answer (#1071). see docs/lsp.md.
- **a declaration or a statement points at its last token**. an expression,
  type or pattern diagnostic points at where it starts, but one about a
  function or a statement names the line of its last token: a duplicate method
  is reported on the last line of its body. go-to-definition on a local lands
  on the end of its initializer (#1158).
- **`pith lint` checks only the first file it is given** (#1161). run it once
  per file.
- **`pith build` fails under a directory whose name contains `.pith`** (#1166).
  the object path replaces every `.pith` in the path, so `foo.pith/main.pith`
  fails with `Error writing object`.
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
- **known leaks**. each of these strands memory on every occurrence:
  - a discarded `catch`, `select` or `match` expression releases only a string
    result (#1091).
  - a `T!` stored in a struct field or a list element (#1167), and a result
    local forwarded straight into another call's argument (#1168).
  - a fresh `String` result passed straight to `assert_eq` (#1118).
  - returning a `match` expression with a fresh string arm (#1151), and a match
    on a result local with a tuple payload (#1150).
  - some `json.decode` shapes: a present optional `String` field (#1144), a
    nested struct on the node-pool path (#1142), and a decode that fails
    partway (#1145).
  - about 1.6 kb per tls connection (#1079).
- **closing a channel can race a send**. a send that passes its closed check
  as another task closes the channel can enqueue a value nobody reads, and the
  send reports success. a channel costs about 416 bytes plus 16 bytes per
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
- **preemption costs a check per loop back-edge**. compute-bound tasks are
  preempted at safe-points so a spinning loop cannot starve the tasks that
  share its worker. building with `PITH_GREEN_PREEMPT=0` removes the check,
  and with it the guarantee. the preemption request flag is process-global, so
  every running task takes a slow path when any one task overruns (#1089).
- **a blocked pipe read under `PITH_GREEN=0` ignores a close**. closing a
  child's pipe from another task fails every later call on the handle, but a
  read already blocked in the kernel returns only when the child writes or
  exits. the green backend wakes it at once.
