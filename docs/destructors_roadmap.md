# Destructors and manual cleanup

Reference counting reclaims memory the moment the last count drops. It does not
close a socket, give back a registry slot, put a terminal back in cooked mode,
or roll back a transaction. Those are side effects the compiler knows nothing
about, so every one of them is a call somebody has to remember to write.

The sharpest case was a `std.net.tls` config: one the caller built held a
registry slot until the process exited unless the caller closed it, and
nothing in the language noticed when one was not.

This page is the survey of std's manual closers, an assessment of how close
the emitter was to running user cleanup, the recommendation made at the time,
and the staged plan that came out of it. The hook exists now: a struct can
`impl Drop`, its `drop` runs from the destructor the compiler already
attaches, and `std.net.tls.Config` is the first std type on it (the mechanism
is described in docs/ownership.md, "Destructors"). Since then most of std has
moved onto it: io readers, writers and buffers, files, processes, byte arrays,
ECDH key pairs, WebSocket sessions, TLS connections and listeners, HTTP and
HTTP/2 connections, the HTTP client's idle pool, and the database connections,
transactions and statements. The survey below records what each type lost
when a close was skipped before that; stage 2 says which types are done and
which still close by hand.

## The survey

This section lists every type in `std` that needs a manual close, what is lost
when the call is skipped, and whether a second call is safe.

### TLS: `std/net/tls.pith`

| Type | Closer | Lost when skipped | Double close |
|---|---|---|---|
| `Config` | `close()`, or its destructor | Nothing any more: `Config` implements `Drop`, so a config nobody closes gives its slots back when the last value naming it dies | Safe |
| `Conn` | `close()`, or its destructor | The `close_notify` alert only: `Conn` implements `Drop`, which closes the socket and frees the session state without network I/O | Safe |
| `Listener` | `close()`, or its destructor | Nothing any more: `Listener` implements `Drop`, releasing its listener-config binding and closing its socket | Safe |

All three TLS types now implement `Drop` (see stage 2 below). Originally,
`Conn` and `Listener` could not carry destructors because std rebuilt both from
bare handles via `conn_from_handle` and `listener_from_handle`. Removing those
helpers and holding the structs directly let both move onto `Drop` alongside
`Config`.

### I/O and files: `std/io.pith`

| Type | Closer | Lost when skipped | Double close |
|---|---|---|---|
| `FileStream` | `close()` | The OS fd and the runtime's file-handle entry | Safe |
| `TcpStream` | `close()` | The socket fd and its reactor registration | Safe |
| `Process` | `close()` | The process-handle entry, up to three pipe fds, and the child goes unreaped | Safe |
| `BufferedBytesReader` / `BufferedBytesWriter` | `free()` | Four threadlocal map entries each, including the cached `Bytes` | Safe |
| `StringReader`, `StringBuffer`, `BytesCursor`, the five buffered *text* readers, and the three buffered writers | `close()` | The type's threadlocal registry entries (one to six map entries each, including any cached text); for `StringBuffer`, also the runtime byte buffer the entry owns, which holds the accumulated text | Safe |

Each of the types in the last row closes through `io.Closer`. A closed reader
reads as end of input, and a closed writer reports nothing written instead of
re-registering itself.

### Sockets, listeners and admission slots

| Type | Closer | Lost when skipped | Double close |
|---|---|---|---|
| A bare TCP fd (`std/net/tcp.pith`) | `tcp.close(fd)` | The fd | Safe |
| A registered listener (`std/shutdown.pith`) | `shutdown.close_listener(fd)` | The fd | Safe by construction |
| A `web` connection slot | `release_connection_slot()` | One permanent unit off the connection ceiling; the server wedges hours later with no trace | n/a |
| An HTTP/2 admission slot | The semaphore release in `serve_h2c_slot` / `serve_h2_tls_slot` | Same shape | n/a |

Every fd closer is exactly-once now. The `Int` through which a socket or pipe
reaches the language is a handle carrying a generation and a user count (see
"Closing a connection another task is using" in docs/concurrency.md). So a
second `tcp_close` on the same value is a no-op instead of a `close(2)` on
whatever socket took the number in between, and a call on the closed value
fails instead of reaching that socket. `shutdown.close_listener` keeps its own
registration check so that a drain and a normal teardown agree on which of
them owns the close, but the fd itself no longer depends on it.

### HTTP, HTTP/2 and gRPC

| Type | Closer | Lost when skipped | Double close |
|---|---|---|---|
| `http.ClientConn` | `close()` | The stream. It does **not** free the reader and writer, so two `io` registries leak per connection | Safe |
| `http2.Connection` | `close()` | The fd or the TLS conn handle | Safe |
| `http2.Client` | `close()` | The reader task, the writer task, five channels, and the transport | Unguarded |
| `http2.Stream` | `close()` | The stream's event channel, its credit channel, three map entries, and one `MAX_CONCURRENT_STREAMS` slot | Safe |
| `grpc.Conn`, `grpc.PoolConn` | `close()` | The whole underlying client | - |
| `grpc.ServerStream` / `ClientStream` / `BidiStream` | `close()` (`close_send()` only half-closes the request stream) | The underlying `http2.Stream`: its event and credit channels, three map entries, and one `MAX_CONCURRENT_STREAMS` slot, retained for the connection's life | Safe |

Closing a completed gRPC RPC sends nothing. The reset only goes on the wire
for a stream whose terminal event never arrived.

### Databases

| Type | Closer | Lost when skipped | Double close |
|---|---|---|---|
| `postgres.Conn`, `mysql.Conn`, `redis.Client` | `close()` | The TLS conn handle and the socket | Safe |
| `postgres.Pool`, `mysql.Pool`, `redis.Pool` | `close()` | The idle connections; checked-out ones are not reached | Safe |
| `db.Db` | `close()` | The pool is drained but never removed from the module-global pool list, so its slot channel is retained for process life | Safe |
| `db.Tx` | `commit()` or `rollback()` | The pinned connection never returns to the pool (`max_open` permanently loses a slot), and the transaction stays open on the server | Safe |
| `db.Stmt` | `close()` | The same pin, plus the server-side prepared statement | Safe |
| A `db` pin registry slot | **None, by design** | The slot is marked finished and its connection cleared, but the list entry is never removed; std documents this as a later refinement | n/a |

### Processes, terminals and signals

| Type | Closer | Lost when skipped | Double close |
|---|---|---|---|
| `os.process.Process` | `close()` | The handle entry, up to three pipe fds, and the child goes unreaped | Safe |
| Raw terminal mode | `tty.restore()` | No echo, no line editing, Ctrl-C not a signal | Safe |
| `term.session.Terminal` | `close()` | The alt screen, cursor and mouse/paste/focus reporting stay on, raw mode stays on, and the reader task, waiter task and event channel are never reaped | Safe |
| `signal.notify()` | **No counterpart** | The process no longer dies on the signal, permanently | n/a |

Raw mode is the one resource in the tree with an automatic backstop: entering it
arms an `atexit` hook that restores termios and resets the screen, so a runtime
trap still hands back a working shell. A `SIGKILL` or a segfault bypasses it.
That hook is also the shape of the answer for anything process-global. A
per-value destructor could never have helped here, because there is no value.

### Concurrency, buffers and parser scopes

| Type | Closer | Lost when skipped | Double close |
|---|---|---|---|
| `concurrent.Ticker` | `stop()` | The ticker worker task runs for the life of the process | Safe |
| `concurrent.Group` | `wait()` or `cancel()` | The tasks and the semaphore stay outstanding | Not re-entrant |
| `concurrent.CancelToken` | `cancel()` | The parent and deadline watcher tasks and the done channel stay live | Safe |
| `bytes.ByteBuffer` | Its destructor; `free()` and `take_bytes()` empty it early | Nothing: the destructor frees it | Safe |
| `protobuf.Writer` | Its destructor (via `ByteBuffer`); `free()` empties it early | Nothing: the wrapped `ByteBuffer`'s destructor frees it | Safe |
| `crypto.ecdh.X25519KeyPair` | `close()` | The boxed private key and its registry entry | Safe |
| `json` / `toml` / `yaml` scopes | `close_scope(scope)` | Every node parsed in the scope stays in the threadlocal node pool for the life of the task | Safe |

### Registries with no closer at all

`crypto.jwks.Cache` allocates module-map entries on construction, and no
function anywhere removes them. `web.session.Store` reclaims the sessions under
it (`destroy` drops one, and a sweep expires the rest) but never the store's
own handle. Both are meant to be built once at startup, which is why nobody has
hit it. (`resilience.Limiter` and `resilience.Breaker` used to be in this group
too, but both moved their state into `guard mu:` struct fields and are now
reclaimed automatically on drop.)

### Does `defer` already cover the common path

For anything scope-shaped, yes. `defer` runs on every exit from its block
(fall-through, `return`, `fail` and `!` propagation alike), and it runs
*before* the emitter's reference-count cleanup, so the deferred call still sees
its locals. `errdefer` covers the roll-back-only half. The pairing is exactly
the one a destructor would give, written explicitly.

`defer` does not cover a resource that outlives the scope that built it: a
connection returned to a caller, a pool held in a struct field, a config
stashed in a module global. Those are the cases where a destructor would earn
its keep, and they are also the cases where the compiler can say least about
who the last owner is.

### Is std internally consistent

Unevenly, and the unevenness tracks the leaks. Counting `defer` and `errdefer`
per file gives `tls13` 65, `tls` 39, `http` 32, `tls12` 18, `websocket` 12,
`http2/server` 10, `db/postgres` 4 and `db/mysql` 4. Against those, `io` has 5
in 3619 lines, `os/process` 2, `fs` 2, `grpc` 2, `redis` 1, `postgres` 1 and
`mysql` 1. The three modules this survey found real leaks in (`io`, `grpc`,
`websocket`) are near the bottom of that list.

There are two rules std does hold to, and they are worth keeping: whoever
builds a resource closes it, and a closer is idempotent. `tcp_close` retires a
generation-stamped handle instead of a number, so it is idempotent too.

## What the machinery already does

Pith already has more destructor machinery than the surface suggests.

A heap struct carries a 32-byte header: magic, strong count, weak count, a dead
bitfield, size, and a **destructor function pointer** at offset 24. The pointer
is written at construction by `pith_struct_set_dtor`, and exactly two places in
the runtime call it: `pith_struct_release`, on the transition to zero strong
references, and `cycle_struct_run_dtor`, in the cycle collector's teardown pass.
Both live in `cranelift/runtime/src/runtime_core.rs`.

The emitter generates the bodies those pointers name: `__dtor_<Struct>`,
`__dtor_<Enum>` as a tag switch, `__opt_dtor_<kind>` for an optional that owns
its payload, `__tuple_dtor_<sig>`, and a per-instance `__dtor_<sym>` for a
generic enum instantiation. Each is an ordinary `fn(i64) -> void`. The attach
sites are `ir_emit_attach_dtor` and its siblings in
`self-host/ir_emitter_core.pith`, `self-host/ir_optionals.pith` and
`self-host/ir_struct_result_helpers.pith`.

So the runtime hook a destructor needs already exists, is already per-object,
and is already called from both the ARC path and the collector path. Adding a
user-visible hook does not need new runtime plumbing.

## What the machinery did not do, and what changed

There were five gaps, and they were the reason for the recommendation below.
Each is kept here with what became of it.

**The predicate answered "does this hold a counted field", not "does this need
cleanup".** `ir_struct_needs_dtor` in `self-host/ir_struct_registry.pith` walked
a struct's fields and returned true only when one of them was reference counted.
Every resource type in the survey above is `struct X: pub handle: Int`, and not
one of them got a destructor. It answers true for a `Drop` struct now, from a
registry of drop methods keyed by declaration node, and the generated body
calls the method before the field walk. The collector's child walk is
unchanged: a tracer is registered only for a struct with traceable fields, so a
`Drop` struct with none stays a leaf to the collector.

**A handle wrapper is not the resource.** `Config` is one `Int`. In the
listener's handshake, std built a second `Config` box over the same registry
handle, and it builds a second `Conn` box the same way. A destructor that
closes the handle when its box dies closes a live config out from under the box
that still names it. The same shape recurs across roughly 45 handle-wrapper
structs in std. This is a problem with the API shape, and no destructor design
fixes it from the compiler side. It is fixed per type, by audit, as each type
moves onto the hook: `Config` is done, and stage 2 below lists the rest.

**Generic instances were not covered.** A generic struct built by its base
name (`Holder("x")` instead of `Holder[String]("x")`) registered no destructor
at all (issue #917). A generic enum instance's destructor tested tag zero
regardless of its signature (issue #918). A generic function body's
specialization emitted no scope-exit release. All three are closed: a generic
body is typed against each set of concrete types before it is emitted
(docs/generics.md), and a struct built inside one gets its destructor like the
same line outside. Generic structs can also implement `Drop`: each
instantiation gets its own destructor calling the specialized `drop` checked
against that instance's type arguments (see stage 4 below).

**The emitter's documented bounded leaks change meaning.** The emitter
deliberately does not release a `T!` or `T?` local passed as an argument, a
tuple-typed register it cannot tie back to a literal, or an ok value consumed
by `catch`, erring toward a leak over a double free. Under ARC that is a byte
count. Under the hook it is also a `drop` that did not run. That is accepted,
with the list in docs/ownership.md as the record of where: the shapes are rare,
each is being closed on its own, and the alternative was to leave every
forgotten close leaking so that the rare shape would not.

**There is no type identity at a release site.** The header carries a
destructor pointer and nothing else that identifies the type. Plain struct
values carry no vtable or type tag (only interface values box a method table),
and the collector already uses the destructor pointer itself as a stand-in type
key. So `Drop` is not dispatched from a release site. It is resolved at
construction and baked into the per-declaration destructor body, which is what
the existing machinery always did. `Drop` is a compile-time marker: nothing
declares the interface, the checker recognizes the impl by name and checks its
shape (E268), and a generic bound on it would be meaningless.

One more gap was smaller. The destructor is called with the raw pointer while
the strong count is already zero, and a `drop` body that binds `self` retains
and releases it, which at zero would re-enter the release and run the
destructor again. `pith_struct_release` runs the destructor under a guard count
of one, the same guard the collector's teardown applies, so the pairs inside
the body balance and nothing inside it can reach zero. `self` is an ordinary
borrowed parameter, in no cleanup list. A `drop` that lets `self` escape is
refused at compile time (E318).

## The options

### A. A `Drop` interface the compiler calls on last release

This is the familiar shape. A type implements `Drop`, and the compiler calls
its `drop` method from the destructor it already emits.

The argument for it: the runtime hook exists, the call sites exist, and the
fail path comes free. ARC release already runs on `fail` and `!` propagation,
so cleanup on the error path needs no new machinery.

The argument against it is everything in the section above. The predicate has
to widen, which perturbs the collector. Every std resource type has to become
single-owner before it can carry one, because today two boxes routinely name
one handle. Generic instances silently would not run it. The emitter's bounded
leaks turn into missed cleanup. And reentrancy has two live hazards: the
self-release at count zero, and a `drop` body that runs during a
stop-the-world collection, on a background thread, in an order the collector
chooses, under a teardown guard that assumes nothing resurrects. Running a
user's socket write there puts it in a different execution context from the
one they wrote it for.

Ordering against `defer` is a further cost. Today there is one cleanup
mechanism, and it runs at a point users can point at. Two mechanisms mean a
rule about which runs first, and a user who has to know it.

### B. `defer` as the convention, plus a lint

Keep the explicit call, and make the compiler notice when it is missing. This
is what the TLS limitation actually asks for: the mechanism exists, and the
diagnostic is what is missing.

This costs nothing at runtime, changes nothing in the ownership model, adds no
semantics on the fail path, and raises no reentrancy question. `defer` and
`errdefer` already run before ARC cleanup and already cover the scope-shaped
cases, which is most of them. A lint is per-module and warning-only, so a false
positive costs a line of noise, not a use-after-free.

It cannot see across a function boundary. A resource handed off, stored in a
field, or returned is out of reach, and the lint has to stay silent there or it
will be wrong. So it catches the shape in the limitations entry and not much
more, and as a convention it is only as strong as the reader.

### C. A linear or must-use type

Let the checker refuse to drop a resource on the floor: a value of a linear type
must be consumed exactly once, by a closer or by a call that takes ownership.

This is the only one of the four that is a guarantee instead of a warning, and
the only one that reaches the cross-function cases B cannot.

It is also the largest: there is no must-use machinery in the checker at all.
It needs consuming-method annotations to be sound. `ByteBuffer.take_bytes()`
consumes its receiver while `Config.with_alpn()` returns it, and nothing in the
source distinguishes them. It needs the wrapper struct to be linear while the
`Int` inside it stays freely copyable, which is a hole a user can walk through
by accident. And it adds a second ownership discipline to a language whose
current model is one sentence long: every heap value is counted, and borrow is
the default. That simplicity is load-bearing.

### D. A scoped block that closes what it opens

This option came out of the survey, not the brief. A `with`-style block binds
a resource for a scope and calls its closer on every exit. It is the same
rewrite `defer` already uses, spelled as one construct instead of two lines:

```pith no-check
with config := tls.client_config()!:
    ...
```

It is a compile-time rewrite over machinery that already works. It has no
relationship to reference counting, so none of the five gaps above touch it,
and it is unambiguous about who closes and when. The existing `io.Closer`
interface is already the conformance point it would need.

It is also sugar. It covers exactly the cases `defer` already covers, buying
ergonomics and a name but no new guarantee, and it costs new syntax. It is
worth having after the diagnostic, not instead of it.

## The recommendation, as made at the time

**Take option B now: keep `defer` as the convention and add the missing
diagnostic. Do not build a `Drop` hook against the current machinery.**

That was the position when the machinery had the five gaps above. Stage 1
closed the generic gaps and the runtime guard closed the reentrancy one. That
left the identity problem (std's own second boxes over a live handle) as the
real prerequisite, and that one is per type. So the hook was built, and each
type moves onto it as its audit is done. The reasoning below is kept because it
decided the order.

`Drop` itself was not the problem. A `Drop` hook layered on today's destructor
path would not run for the types people write when they want a resource
wrapper. A generic wrapper (`Guard[T]`, `Pool[T]`, `Handle[T]`) hits all three
of the generic gaps at once, and the failure is silent: no diagnostic, no
leak-gate signal, just a socket that stays open. Shipping a cleanup feature
whose first serious use case is the one it silently skips is worse than not
shipping it.

The identity problem is the second half. The resource types in std are int
handles wrapped in freely copied value structs, and std itself builds a second
wrapper over a live handle in at least two places. Per-box cleanup against that
API is a use-after-close, not a leak. That has to be fixed in std before any
automatic destructor can be correct, and fixing it is an API change across
roughly 45 types, a larger and more disruptive piece of work than the compiler
half.

Option C loses on cost and on the ownership model. It is the only real
guarantee, and it would be the right answer for a language designing this in
from the start. Retrofitting linearity onto handle wrappers whose payload is a
copyable `Int` gives a guarantee with a hole in it, at the price of doubling the
ownership rules a user has to hold.

Option D loses only on ordering. It belongs on the plan, just after the
diagnostic: it makes the well-written case shorter, while the diagnostic makes
the badly-written case visible.

## Staged plan

### Stage 0: a lint for a resource that is built and abandoned (done)

E307, in `self-host/linter.pith`, reports a locally-built resource that is
never closed and never handed off. Its constructor list holds the TLS config
builders and the JSON, TOML and YAML `open_scope()`. A constructor whose result
type implements `Drop` is skipped, so the TLS builders stay quiet and a type
moved onto the destructor goes quiet without an edit to the list. The rule is
deliberately narrow: the binding's initializer must be a call to a known
resource constructor, reached through `!` and any chain of builder methods, and
the name must be used *only* as the receiver of methods that do not close it.
Any other use (passed as an argument, returned, assigned, stored, captured,
read as a field) silences the rule, because the resource may be closed anywhere
the lint cannot see. A false positive on a resource handed off elsewhere is
worse than no lint.

`ByteBuffer.take_bytes()` is the shape that proves the caution is needed: it
consumes its receiver, so a rule that only looked for `close()` would report a
correct program. Every addition to the constructor list has to be audited the
same way.

Extending it is cheap when a new non-`Drop` resource needs tracking (the parser
scopes' `open_scope` / `close_scope` pairing is already included above).

Effort: small (one lint pass, one error code, three smoke cases in the
Makefile).

### Stage 1: repair the destructor machinery (done)

#917 (a generic struct instance emitted no destructor), #918 (a generic enum
instance's destructor tested the wrong tag), and the generic-function-body
specialization that skipped scope-exit cleanup are closed. A generic body is
typed per instantiation before it is emitted, and docs/ownership.md records
the shapes that pin it.

### Stage 2: make std's resource types single-owner (mostly done)

Status: done for `Config`; the io readers, writers, buffers and cursors, files
and processes (through a shared `io.ProcessHandle` that pipe views keep
alive), `ByteArray` and `X25519KeyPair`; WebSocket sessions (no close frame on
drop); `tls.Conn` and `tls.Listener` (`conn_from_handle` and
`listener_from_handle` were removed, and a dropped conn sends no close_notify);
`http.ClientConn`, `http2.Connection`, the h2 server connection and the HTTP
client's idle pool; `http2.Stream`, and with it the three gRPC stream types
(stream ids are never reused on a connection, so a drop of a stream the
client already forgot finds nothing, and a dropped open stream queues its
reset only when the send queue has room); `postgres.Conn`, `mysql.Conn`,
`redis.Client`, and `std.db`'s `Tx` and `Stmt`, which own their pin, and
`Db`, which holds its pool (the module-global pool and pin registries are
gone). The io registry handles come from one process-wide counter, so a drop
on another task finds nothing. `ByteBuffer` frees its storage on drop:
`take_bytes()` leaves an empty buffer behind instead of freeing it, so the
destructor has nothing to record. `unix.UnixListener` and `unix.UnixStream`
close their sockets on drop too. Still closed by hand: `http2.Client`, which
its reader and writer tasks hold; `term.session.Terminal`, whose tasks hold it
and whose close writes to the TTY; and `TcpStream`, which is a view of a socket
another value owns. The text below is the plan as it was written.

Stop reconstructing a wrapper struct from a raw handle, so that one live value
names one live resource, then put the type on the hook. `std.net.tls.Config`
is done: the listener registry holds the config instead of its handle, the
hook result carries `Config?` instead of a box over handle 0, and
`impl Drop for Config` calls `close()`. The three registry leaks this survey
found are closed (issue #925): the `io` buffered text and string registries,
`websocket`'s six threadlocal maps, and gRPC's streams never closing their
underlying HTTP/2 stream. Each of those types deregisters through a `close()`
now, which is the half a destructor needs first.

The rest, in the order they are worth doing, each with what its audit has to
settle:

- The `io` buffered text readers and writers, `StringReader`, `StringBuffer`
  and `BytesCursor`: threadlocal registries, no public rebuild-from-handle,
  so the audit is small. The one question is a value whose last holder dies
  on a different OS thread from the one that built it. The destructor then
  finds no entry, which is the same no-op an explicit `close()` on that
  thread already is.
- `websocket` sessions and gRPC streams: single-owner from the caller's
  side already. The audit is whether the server-side code rebuilds either
  from a stored handle.
- `tls.Conn`, `tls.Listener`, `http.ClientConn`, `http2.Connection` and the
  database connections: each is rebuilt from a bare handle somewhere
  (`conn_from_handle`, `listener_from_handle`, the drivers' stored `Int`
  fields), and each closes a socket. These need the rebuild sites replaced
  with a held value first, and a close that records whether it won.
- The types with no closer at all (`resilience.Limiter`, `Breaker`,
  `crypto.jwks.Cache`, `web.session.Store`): built once at startup, so a
  destructor buys little. They get a closer when something builds them per
  request.

Effort: large in total and API-visible, done a type at a time. This is std
work, not compiler work.

### Stage 3: a scoped `with` block over `io.Closer`

This is sugar over the `defer` rewrite that already works, using the interface
std already declares. It is deliberately independent of stages 1 and 2, so it
improves the common case without waiting on either.

Effort: medium (parser, checker conformance check, one emitter rewrite, docs
and goldens).

### Stage 4: a `Drop` marker (done for the compiler, ahead of stage 3)

The marker is resolved at construction and baked into the existing per-object
destructor pointer, the way tracers already are. `ir_struct_needs_dtor`
answers true for a `Drop` struct, the generated destructor calls the method at
its head, the runtime runs the body under a guard count, and the tracer
registration is unchanged because the method adds no edges. The checker
refuses the shapes the destructor cannot call (E268): an enum, an impl outside
the declaring module, or a `drop` with parameters or a return value.

The collector question was settled conservatively. A garbage cycle that runs
through a `Drop` struct is left alone and leaks, because running a `drop`
inside the sweep pass could reach another cycle member through a strong field
and keep it past the free (`keep_user_drops` in `cycle.rs`). A `Drop` struct
outside any cycle, reached only through a cycle member's field, dies by an
ordinary release when that member is freed, and its `drop` runs then (see
docs/ownership.md, "Destructors").

A generic struct is on the hook. Each instance gets its own destructor, keyed
by its field kinds and its specialized drop symbol, which calls the drop
checked against that instance's type arguments. The checker walks a generic
drop against every instance built and applies E318 after the first. What is
left on the compiler side is the bounded leaks above. A struct literal that
sets a private field outside its module is refused (E251), so a second value
cannot be built over a live handle.
