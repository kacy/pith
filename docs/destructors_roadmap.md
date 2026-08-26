# destructors and manual cleanup

reference counting reclaims memory the moment the last count drops. it does not
close a socket, give back a registry slot, put a terminal back in cooked mode,
or roll back a transaction. those are side effects the compiler knows nothing
about, so every one of them is a call somebody has to remember to write.

`docs/limitations.md` records the sharpest case: a `std.net.tls` config the
caller builds holds a registry slot until the process exits unless the caller
closes it, and nothing in the language notices when one is not. the entry ends
by naming the missing feature — "a destructor that runs when the last reference
to a `Config` goes away, which is the same missing feature behind several
entries here."

this page is the survey behind that sentence, an assessment of how close the
emitter already is to running user cleanup, and a recommendation. it is a plan,
not a description of what ships today: nothing here is implemented except the
lint in stage 0. issue #925 tracks it.

## the survey

every type in `std` that needs a manual close, what is lost when the call is
skipped, and whether a second call is safe.

### tls — `std/net/tls.pith`

| type | closer | lost when skipped | double close |
|---|---|---|---|
| `Config` | `close()` | up to 14 module-global registry maps keyed by handle, pinned for process life; for a server config that includes the certificate pem and the private key der | safe |
| `Conn` | `close()` | the `native_conn_states` entry (keys, ivs, transcript, sequence numbers), the handshake-pending flag, the socket fd, and the `close_notify` alert | safe |
| `Listener` | `close()` | the listener-config entry and the listening socket | safe |

handles are never reissued, so the maps only grow. std's own comment on
`Config.close()` puts the cost plainly: an https client builds a config per
request, so a forgotten close is a leak per request.

### io and files — `std/io.pith`

| type | closer | lost when skipped | double close |
|---|---|---|---|
| `FileStream` | `close()` | the os fd and the runtime's file-handle entry | safe |
| `TcpStream` | `close()` | the socket fd and its reactor registration | **unsafe** |
| `Process` | `close()` | the process-handle entry, up to three pipe fds, and the child goes unreaped | safe |
| `BufferedBytesReader` / `BufferedBytesWriter` | `free()` | four threadlocal map entries each, including the cached `Bytes` | safe |
| `StringReader`, `StringBuffer`, `BytesCursor`, the five buffered *text* readers, and the three buffered writers | `close()` | the type's threadlocal registry entries — two to six map entries each, including any cached text | safe |

the last row used to read "none": those registries had no removal path at all,
so a keep-alive server on the buffered text path accumulated entries for the
life of the process, per os thread, with no call it could make to stop it.
each of the types closes now (through `io.Closer`), a closed reader reads as
end of input, and a closed writer reports nothing written rather than
re-registering itself.

### sockets, listeners and admission slots

| type | closer | lost when skipped | double close |
|---|---|---|---|
| a bare tcp fd (`std/net/tcp.pith`) | `tcp.close(fd)` | the fd | **unsafe** |
| a registered listener (`std/shutdown.pith`) | `shutdown.close_listener(fd)` | the fd | safe by construction |
| a `web` connection slot | `release_connection_slot()` | one permanent unit off the connection ceiling; the server wedges hours later with no trace | n/a |
| an http/2 admission slot | the semaphore release in `serve_h2c_slot` / `serve_h2_tls_slot` | same shape | n/a |

`shutdown.close_listener` is the only fd closer in std that is exactly-once by
construction: it removes the registration and reports whether it won, so only
the winner reaches `tcp_close`.

### http, http/2 and grpc

| type | closer | lost when skipped | double close |
|---|---|---|---|
| `http.ClientConn` | `close()` | the stream; note it does **not** free the reader and writer, so two `io` registries leak per connection | fd path unsafe |
| `http2.Connection` | `close()` | the fd or the tls conn handle | fd path unsafe |
| `http2.Client` | `close()` | the reader task, the writer task, five channels, and the transport | unguarded |
| `http2.Stream` | `close()` | the stream's event channel, its credit channel, three map entries, and one `MAX_CONCURRENT_STREAMS` slot | safe |
| `grpc.Conn`, `grpc.PoolConn` | `close()` | the whole underlying client | — |
| `grpc.ServerStream` / `ClientStream` / `BidiStream` | **only `close_send()`** | `close_send` half-closes the wire and nothing else: the underlying `http2.Stream` is never closed, so every streaming rpc retains its channels and one concurrency slot permanently | n/a |

the grpc row is the second finding: `http2.Stream.close()` is not `pub`, so a
caller could not reach it through the grpc types even knowing it was needed.

### databases

| type | closer | lost when skipped | double close |
|---|---|---|---|
| `postgres.Conn`, `mysql.Conn`, `redis.Client` | `close()` | the tls conn handle and the socket | fd path unsafe |
| `postgres.Pool`, `mysql.Pool`, `redis.Pool` | `close()` | the idle connections; checked-out ones are not reached | safe |
| `db.Db` | `close()` | the pool is drained but never removed from the module-global pool list, so its slot channel is retained for process life | safe |
| `db.Tx` | `commit()` or `rollback()` | the pinned connection never returns to the pool — `max_open` permanently loses a slot — and the transaction stays open on the server | safe |
| `db.Stmt` | `close()` | the same pin, plus the server-side prepared statement | safe |
| a `db` pin registry slot | **none, by design** | the slot is marked finished and its connection cleared, but the list entry is never removed; std documents this as a later refinement | n/a |

### processes, terminals and signals

| type | closer | lost when skipped | double close |
|---|---|---|---|
| `os.process.Process` | `close()` | the handle entry, up to three pipe fds, and the child goes unreaped | safe |
| raw terminal mode | `tty.restore()` | no echo, no line editing, ctrl-c not a signal | safe |
| `term.session.Terminal` | `close()` | the alt screen, cursor and mouse/paste/focus reporting stay on, raw mode stays on, and the reader task, waiter task and event channel are never reaped | safe |
| `signal.notify()` | **no counterpart** | the process no longer dies on the signal, permanently | n/a |

raw mode is the one resource in the tree with an automatic backstop: entering it
arms an `atexit` hook that restores termios and resets the screen, so a runtime
trap still hands back a working shell. a `SIGKILL` or a segfault bypasses it.
that hook is also the shape of the answer for anything process-global — a
per-value destructor could never have helped here, because there is no value.

### concurrency, buffers and parser scopes

| type | closer | lost when skipped | double close |
|---|---|---|---|
| `concurrent.Ticker` | `stop()` | the ticker worker task runs for the life of the process | safe |
| `concurrent.Group` | `wait()` or `cancel()` | the tasks and the semaphore stay outstanding | not re-entrant |
| `concurrent.CancelToken` | `cancel()` | the parent and deadline watcher tasks and the done channel stay live | safe |
| `bytes.ByteBuffer` | `free()`, or `take_bytes()` which consumes it | a heap allocation with no refcount behind it | safe |
| `protobuf.Writer` | `free()` | the wrapped byte buffer | safe |
| `crypto.ecdh.X25519KeyPair` | `close()` | the boxed private key and its registry entry | safe |
| `json` / `toml` / `yaml` scopes | `close_scope(scope)` | every node parsed in the scope stays in nine threadlocal maps for the life of the task | safe |

### registries with no closer at all

`resilience.Limiter`, `resilience.Breaker` and `crypto.jwks.Cache` each allocate
four to six module-map entries on construction, and no function anywhere removes
them. `web.session.Store` reclaims the sessions under it — `destroy` drops one,
and a sweep expires the rest — but never the store's own handle. all four are
meant to be built once at startup, which is why nobody has hit it; a per-request
`rate_limiter()` grows four maps forever.

### does `defer` already cover the common path

for anything scope-shaped, yes. `defer` runs on every exit from its block —
fall-through, `return`, `fail` and `!` propagation alike — and it runs *before*
the emitter's reference-count cleanup, so the deferred call still sees its
locals. `errdefer` covers the roll-back-only half. the pairing is exactly the
one a destructor would give, written explicitly.

what `defer` does not cover is a resource that outlives the scope that built it:
a connection returned to a caller, a pool held in a struct field, a config
stashed in a module global. those are the cases where a destructor would earn
its keep, and they are also the cases where the compiler can say least about who
the last owner is.

### is std internally consistent

unevenly, and the unevenness tracks the leaks. counting `defer` and `errdefer`
per file: `tls13` 65, `http` 30, `tls` 29, `http2/server` 18, `tls12` 17,
`websocket` 10, `db/postgres` 4, `db/mysql` 4 — against `io` 3 in 3265 lines,
`os/process` 2, `fs` 2, `grpc` 2, `redis` 1, `postgres` 1, `mysql` 1. the three
modules this survey found real leaks in — `io`, `grpc`, `websocket` — are near
the bottom of that list.

the two rules std does hold to are worth keeping: whoever builds a resource
closes it, and a closer backed by a map removal is idempotent. the split on the
second one is clean. anything that reaches `tcp_close(fd)` is not idempotent,
because the call closes unconditionally and the kernel may have reissued the
number; everything backed by a map lookup is safe to call twice.

## what the machinery already does

pith already has more destructor machinery than the surface suggests.

a heap struct carries a 32-byte header — magic, strong count, weak count, a dead
bitfield, size, and a **destructor function pointer** at offset 24. the pointer
is written at construction by `pith_struct_set_dtor`, and exactly two places in
the runtime call it: `pith_struct_release`, on the transition to zero strong
references, and `cycle_struct_run_dtor`, in the cycle collector's teardown pass.
both live in `cranelift/runtime/src/runtime_core.rs`.

the emitter generates the bodies those pointers name — `__dtor_<Struct>`,
`__dtor_<Enum>` as a tag switch, `__opt_dtor_<kind>` for an optional that owns
its payload, `__tuple_dtor_<sig>`, and a per-instance `__dtor_<sym>` for a
generic enum instantiation. each is an ordinary `fn(i64) -> void`. the attach
sites are `ir_emit_attach_dtor` and its siblings in
`self-host/ir_emitter_core.pith`, `self-host/ir_optionals.pith` and
`self-host/ir_struct_result_helpers.pith`.

so the runtime hook a destructor needs already exists, is already per-object,
and is already called from both the arc path and the collector path. adding a
user-visible hook does not need new runtime plumbing.

## what the machinery does not do yet

five things, and they are the reason this page recommends what it does.

**the predicate answers "does this hold a counted field", not "does this need
cleanup".** `ir_struct_needs_dtor` in `self-host/ir_struct_registry.pith` walks
a struct's fields and returns true only when one of them is reference counted.
every resource type in the survey above is `struct X: pub handle: Int`. not one
of them gets a destructor emitted, and not one of them gets
`pith_struct_set_dtor` called at construction. the predicate is also what feeds
the cycle collector's child walk — an object with a null destructor slot is a
leaf to the collector — so widening it moves more than it looks like it moves.

**a handle wrapper is not the resource.** `Config` is one `Int`. std builds a
second `Config` box over the same registry handle in `native_server_handshake`,
and a second `Conn` box the same way. a destructor that closed the handle when
its box died would close a live config out from under the box that still names
it. the same shape recurs across roughly 45 handle-wrapper structs in std. this
is not a compiler gap; it is an api shape, and no destructor design fixes it
from the compiler side.

**generic instances are not covered.** a generic struct built by its base name —
`Holder("x")` rather than `Holder[String]("x")` — takes an early return in
`ir_attach_generic_construction_dtor` that registers no destructor at all
(issue #917, measured at about 35 bytes per instance). a generic enum instance's
destructor tests tag zero regardless of what its signature says, so it releases
nothing unless the payload variant happens to be declared first (issue #918).
and a generic function body's specialization empties the emitter's
reference-counted-local list before emitting exit cleanup, so no scope-exit
release runs inside one at all. a `Guard[T]` or a `Pool[T]` is exactly what
someone writes when they want a resource wrapper, and it is exactly the shape
all three gaps land on. today those gaps cost bytes. under a destructor hook
they would cost an unclosed socket, silently.

**the emitter's documented bounded leaks would change meaning.** a `T!` or `T?`
local passed as an argument, a tuple-typed register the emitter cannot tie back
to a literal, an ok value consumed by `catch` — the emitter deliberately does
not release these, erring toward a leak rather than a double free. under arc
that is a byte count. under a destructor hook it becomes "your cleanup did not
run", and a feature added to make cleanup reliable would have made it less so.

**there is no type identity at a release site.** the header carries a
destructor pointer and nothing else identifying. interfaces are fully
monomorphized — there is no vtable anywhere in the tree, and the collector
already uses the destructor pointer itself as a stand-in type key. so a `Drop`
interface could not be dispatched dynamically from a release site; it would have
to be resolved at construction and baked into the pointer, which is what the
existing machinery does and is a workable design, but it makes `Drop` a
compile-time marker rather than a runtime trait.

one more, smaller: the destructor is called with the raw pointer while the
strong count is already zero. a user `fn drop(self)` compiled as an ordinary
method would track `self` as a counted local and re-enter `pith_struct_release`
at zero. the runtime's over-release guard catches that without crashing, but the
body would have to be emitted with `self` excluded from the cleanup list.

## the options

### a. a `Drop` interface the compiler calls on last release

the familiar shape. a type implements `Drop`, and the compiler calls its `drop`
method from the destructor it already emits.

the argument for it: the runtime hook exists, the call sites exist, and the fail
path comes free — arc release already runs on `fail` and `!` propagation, so
cleanup on the error path needs no new machinery.

the argument against it is everything in the section above. the predicate has to
widen, which perturbs the collector. every std resource type has to become
single-owner before it can carry one, because today two boxes routinely name one
handle.
generic instances silently would not run it. the emitter's bounded leaks turn
into missed cleanup. and reentrancy has two live hazards — the self-release at
count zero, and a `drop` body that runs during a stop-the-world collection, on a
background thread, in an order the collector chooses, under a teardown guard
that assumes nothing resurrects. running a user's socket write there is a
different execution context than the one they wrote it for.

ordering against `defer` is a further cost: today there is one cleanup mechanism
and it runs at a point users can point at. two mechanisms means a rule about
which runs first, and a user who has to know it.

### b. `defer` as the convention, plus a lint

keep the explicit call, and make the compiler notice when it is missing. this is
what the tls limitation actually asks for: the mechanism is not missing, the
diagnostic is.

this costs nothing at runtime, changes nothing in the ownership model, adds no
semantics on the fail path, and raises no reentrancy question. `defer` and
`errdefer` already run before arc cleanup and already cover the scope-shaped
cases, which is most of them. a lint is per-module and warning-only, so a false
positive costs a line of noise rather than a use-after-free.

what it does not do is see across a function boundary. a resource handed off,
stored in a field, or returned is out of reach, and the lint has to stay silent
there or it will be wrong. so it catches the shape in the limitations entry and
not much more, and being a convention it is only as strong as the reader.

### c. a linear or must-use type

let the checker refuse to drop a resource on the floor: a value of a linear type
must be consumed exactly once, by a closer or by a call that takes ownership.

this is the only one of the four that is a guarantee rather than a warning, and
the only one that reaches the cross-function cases b cannot.

it is also the largest: there is no must-use machinery in the checker at all. it
needs consuming-method annotations to be sound — `ByteBuffer.take_bytes()`
consumes its receiver while `Config.with_alpn()` returns it, and nothing in the
source distinguishes them. it needs the wrapper
struct to be linear while the `Int` inside it stays freely copyable, which is a
hole a user can walk through by accident. and it adds a second ownership
discipline to a language whose current model is one sentence long: every heap
value is counted, borrow is the default. that simplicity is load-bearing.

### d. a scoped block that closes what it opens

this one fell out of the survey rather than the brief. a `with`-style block
binds a resource for a scope and calls its closer on every exit — the same
rewrite `defer` already uses, spelled as one construct instead of two lines:

```pith
with config := tls.client_config()!:
    ...
```

it is a compile-time rewrite over machinery that already works, it has no
relationship to reference counting, so none of the five gaps above touch it, and
it is unambiguous about who closes and when. the existing `io.Closer` interface
is already the conformance point it would need.

it is also sugar. it covers exactly the cases `defer` already covers, buying
ergonomics and a name rather than a new guarantee, and it costs new syntax to do
it. worth having after the diagnostic, not instead of it.

## the recommendation

**take option b now: keep `defer` as the convention and add the missing
diagnostic. do not build a `Drop` hook against the current machinery.**

the reasoning is not that `Drop` is the wrong feature. it is that a `Drop` hook
layered on today's destructor path would not run for the types people write when
they want a resource wrapper. a generic wrapper — `Guard[T]`, `Pool[T]`,
`Handle[T]` — hits all three of the generic gaps at once, and the failure is
silent: no diagnostic, no leak-gate signal, just a socket that stays open.
shipping a cleanup feature whose first serious use case is the one it silently
skips is worse than not shipping it.

the identity problem is the second half. std's resource types are int handles
wrapped in freely-copied value structs, and std itself builds a second wrapper
over a live handle in at least two places. per-box cleanup against that api is a
use-after-close, not a leak. that has to be fixed in std before any automatic
destructor can be correct, and fixing it is an api change across roughly 45
types — a larger and more disruptive piece of work than the compiler half.

option c loses on cost and on the ownership model. it is the only real
guarantee, and it would be the right answer for a language designing this in
from the start, but retrofitting linearity onto handle wrappers whose payload is
a copyable `Int` gives a guarantee with a hole in it, at the price of doubling
the ownership rules a user has to hold.

option d loses only on ordering. it belongs on the plan, just after the
diagnostic rather than before it: it makes the well-written case shorter, while
the diagnostic makes the badly-written case visible.

## staged plan

### stage 0 — a lint for a resource that is built and abandoned — done

E307, in `self-host/linter.pith`. it reports a locally-built resource that is
never closed and never handed off. deliberately narrow: the binding's
initializer must be a call to a known resource constructor, reached through `!`
and any chain of builder methods, and the name must be used *only* as the
receiver of methods that do not close it. any other use — passed as an argument,
returned, assigned, stored, captured, read as a field — silences the rule,
because the resource may be closed anywhere the lint cannot see. a false
positive on a resource handed off elsewhere is worse than no lint.

`ByteBuffer.take_bytes()` is the shape that proves the caution is needed: it
consumes its receiver, so a rule that only looked for `close()` would report a
correct program. the constructor list is the three `std.net.tls` config builders
and nothing else, and every addition to it has to be audited the same way.

extending it is cheap and is where the next increment of value is. the obvious
next entries are the parser scopes, whose `open_scope` / `close_scope` pairing
is unambiguous and whose types have no consuming method.

effort: small — one lint pass, one error code, three smoke cases in the makefile.

### stage 1 — repair the destructor machinery

fix #917 (a generic struct instance emits no destructor), #918 (a generic enum
instance's destructor tests the wrong tag), and the generic-function-body
specialization that skips scope-exit cleanup entirely. these are measured leaks
today and worth fixing on their own; they are also the floor under anything
automatic. until all three are closed, no destructor hook can claim to run.

effort: medium. one emitter area, leak-gate cases for each, and each needs its
regression pinned at both generic-enum declaration orders.

### stage 2 — make std's resource types single-owner

stop reconstructing a wrapper struct from a raw handle, so that one live value
names one live resource. audit all roughly 45 handle wrappers. close the three
leaks this survey found on the way through: the `io` buffered text and string
registries with no removal path, `websocket`'s six threadlocal maps with no
removes, and grpc's streams never closing their underlying http/2 stream.

effort: large, and api-visible. this is the real prerequisite, and it is std
work rather than compiler work.

### stage 3 — a scoped `with` block over `io.Closer`

sugar over the `defer` rewrite that already works, using the interface std
already declares. reachable independently of stages 1 and 2, which is the point:
it improves the common case without waiting on either.

effort: medium — parser, checker conformance check, one emitter rewrite, docs
and goldens.

### stage 4 — a `Drop` marker, only if 1 through 3 land

resolved at construction and baked into the existing per-object destructor
pointer, the way tracers already are. widen `ir_struct_needs_dtor`, splice the
user call at the head of the generated destructor body, emit that body with
`self` excluded from the cleanup list, and keep the tracer registration in step.
decide explicitly what happens when the cycle collector runs one, and consider
refusing to run a user `drop` from a collection sweep rather than running it in
an order and on a thread the user did not choose.

effort: large, and gated on all three stages above. this page's position is that
it should not start until they are done.
