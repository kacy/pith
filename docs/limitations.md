# limitations

pith is self-hosting and runs real programs, but it is not finished. this page
is the honest list of what does not work yet, so you can plan around it instead
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
- **range patterns are integer-only** — `0..=9 => ...` and `0..10 => ...`
  work in match arms (and combine with or-patterns and guards), but only for
  integer subjects and non-negative literal bounds.
- **`if let` / `while let` are statement-only** — `if let Shape.Circle(r) = s:`
  destructures a variant and `if let v = maybe():` unwraps an optional, but
  neither form works as an expression, and literal patterns are not allowed
  in the let position.
- **closure capture cap** — a closure captures at most 16 variables. captures
  are heap-allocated per closure instance, so nesting and recursion are safe;
  the cap is a fixed ceiling, not a correctness issue. (multi-line closure
  bodies — `fn(x):` with an indented block — work and infer their return
  type from their return statements.)
- **function-value struct fields aren't callable through a field** —
  a function value stored in a struct field can be passed and returned,
  but `instance.field(args)` parses as a method call and is rejected
  (E209). bind it to a name first (`f := instance.field; f(args)`).
  function values held in locals, globals, list/map/tuple elements, and
  returned from calls are all directly callable.
- **interface depth** — interfaces support method signatures, default
  methods (a member with a body that implementors inherit unless they
  override it), associated types (`type Item` on the interface, bound
  per impl with `type Item = Int`), single and multiple bounds, and
  generic interfaces. an associated type resolves both inside the impl's
  own methods and in generic `T.Item` position — a
  `fn f[T: Container](c: T) -> T.Item` returns the right type for each
  concrete `T`. an impl that omits an abstract (non-default) interface
  method is rejected at the impl block (E235). `where` clauses are the
  remaining gap — bounds must be written inline (`[T: Display + Hash]`).

## standard library

- **tls 1.2** — tls is 1.3 only, client and server. there is no 1.2
  compatibility mode, so peers that cannot speak 1.3 will not connect.
- **testing** — `std/testing` covers assertions but not discovery, fixtures, or
  parameterized cases. the project's own suite is golden-snapshot based (see
  `tests/`).
- **http/2 is client-only** — `std.net.http2` is a native http/2 client
  (multiplexed streams, hpack, tls with alpn). there is no http/2 server yet,
  and `std.net.http` stays http/1.1.
- **regex is deliberately small** — `std.regex` covers literals, `.`,
  classes, `\d \w \s` escapes, `* + ?` (greedy), alternation, capturing
  groups, and `^ $` anchors. it does not support `{n,m}` counts, lazy
  quantifiers, backreferences, or lookaround. matching is a pike vm, so
  time is linear in the input for any pattern.
- **gzip compresses with fixed huffman only** — `std.compress.gzip`
  reads any deflate stream (multi-member files included) and writes
  real compression (greedy lz77 over fixed huffman, stored-block
  fallback for incompressible data; system gunzip reads its output,
  and zlib routes through the same engine). dynamic huffman trees on
  the write side would shave a few more percent and are the one
  remaining refinement.

## tooling

- **no language server** — there is no lsp; editor support is limited to syntax.
- **no package registry** — dependencies are local path entries in `pith.toml`;
  there is no fetch, lock, or hosted index yet.
- **no debugger** — runtime stack traces are thin and there is no stepping.

## backend

these are internal and do not usually surface in source, but they shape the
correctness story:

- the ir is self-describing — calls carry an explicit return kind and field
  loads carry their type — and the cranelift consumer reads that metadata rather
  than guessing. the older inference path that caused a few cross-module bugs is
  gone.
- memory reclamation is compiler-emitted reference counting (see the readme's
  memory section). closures are reference counted and freed like other heap
  values. the deliberate gaps that remain: strong reference cycles leak, but a
  struct graph can break its own cycle with a `weak` field (see
  docs/ownership.md) — the only cycle with no weak escape hatch today is a
  closure that captures a binding reaching back to it, since closure captures
  cannot yet be weak. container element removal leaks until the container dies,
  and error-path early returns skip cleanup. all are bounded-leak by design —
  the discipline never produces a dangling pointer in exchange.
- reading an optional that is synthesized rather than stored can leak a small
  optional box per read. a `weak` field read builds a fresh optional to carry
  the liveness result; the common consumers — `!= none` / `== none` and
  `.value()` — now free that shell once the value is extracted, so the common
  idiom no longer leaks. an indexed read of a collection whose element type is a
  struct (`items[i]` where `items` is `List[Node]`) still synthesizes an optional
  that is not reclaimed. a stored optional field read does not synthesize and
  does not leak. the leak is bounded per read and never dangles; reclaiming the
  remaining synthesized-optional temporaries is a known follow-up.
- a handful of edge cases logged during bring-up (cross-module float returns,
  cross-module map reads, set codegen, negative float literals like `-1.0`) were
  re-checked and all pass; they are now pinned by regression tests
  (`tests/cases/test_xmod_float.pith` and friends).
- `os.set_env` after tasks have started is a libc-level race. glibc's `setenv`
  and `getenv` are not synchronized against each other, and the runtime's own
  pool threads read the environment behind your back — `getaddrinfo` on the
  dns pool consults `RES_OPTIONS` and friends on every lookup. rust's internal
  env lock only covers rust-side accesses, so a `set_env` concurrent with a
  dial can crash in libc. set environment variables before spawning tasks. the
  candidate fixes (snapshot the environment at startup, or make late `set_env`
  write to an overlay that child processes inherit) both change observable
  semantics, so this is documented rather than decided for now.
- closing an fd-backed handle races a concurrent call on the same handle. a
  task parked on a socket or pipe that another task closes is woken with an
  error (the reactor's close teardown), which is the common case and safe.
  what remains is the standard raw-fd hazard: between one task's handle
  lookup and its syscall, a close plus a new open can recycle the fd number,
  and the syscall lands on the wrong fd. present on both backends and
  inherited from fd semantics; closing it needs handles that carry liveness
  (a generation, like task handles have) rather than raw fd numbers. do not
  share a connection between tasks without coordinating its close.

## the green backend, now the default on linux

as of 2026-07-27 the green backend is what a spawned task runs on when you
build for linux; `PITH_GREEN=0` switches back to one os thread per task, and on
macos and the bsds os threads are still the default with `PITH_GREEN=1` as the
opt-in. green wins every shape this repo measures: spawn is ~30x the os-thread
backend at a seventeenth of the memory, and the channel fan-out benchmark runs
2.6x faster than os threads and ahead of rust and zig. the whole regression
corpus, 268 cases at both worker counts, produces byte-identical output to the
fixed os-thread answers (`make verify-green-corpus`, run in ci). what follows is
what the new default still costs you, not a list of things blocking it.

the structural cost is that a green worker runs many tasks, so a call with no
yield point holds all of them rather than only the task making it. sockets go
through the epoll reactor, dns and file i/o go to pools of blocking threads
while the caller parks, and child processes park on the reactor too: pipe reads
because a pipe is pollable, `wait` because linux gives out a `pidfd` that
reports the exit. none of those stall anyone any more.

two things are left. the cheap end of `host_fs`, meaning `exists`, `size`,
`rename`, `mkdir` and removing a single file, still runs on the worker, because
each is one cached kernel lookup that costs about what handing it to another
thread would. on a slow network mount that reasoning does not hold and a task
doing one of them holds its worker until it returns. making that decision
adaptive rather than fixed is the open work.

the other is `sleep`, and everything built on it. `time.delay` maps straight
to the blocking sleep, so a green task that sleeps holds its worker for the
duration — and the waiting loops built on delay hold one too: a `select` with
no ready arm sleeps a millisecond per probe until an arm fires, and the
`concurrent.after`/`ticker` workers and a context's deadline watcher do the
same. each slice is short and correctness is unaffected, but on a two-worker
host two idle selects can occupy both workers while they poll. the fix is a
real timer in the reactor — the deadline heap `netpoll` already keeps could
serve a pure-timeout wait with no fd — so a sleeping task parks the way a
socket read does. `PITH_GREEN=0` is the blunt workaround.

(`process.output` and the calls built on it — `run`, `text`, `output_checked`,
`run_shell`, `output_shell`, plus `exec` and `exec_output` — used to be on
this list for holding a worker while a child ran to completion; they now hand
the whole spawn-drain-wait to a process pool of their own and the caller
parks, the same shape dns and file i/o use.)

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
anyway, ~130-170 ms when it splits. cross-worker wakes are the whole remaining
gap to go on coordination-heavy work.

the obvious fix is to move a parked task to whichever worker keeps waking it,
and it does work: prototyped, it took the fan-out from a bimodal ~120 ms median
to a flat ~42 ms, ahead of go's ~69, with cross-worker wakes dropping from
~100k to single digits. it is also unsound, and the reason is worth recording
so nobody spends the same day rediscovering it. the problem is not the
coroutine stack, which migrates fine; it is that the compiler caches the
thread-local base in a frame across a suspension, so a coroutine resumed on
another thread reads the previous thread's `CURRENT_TASK` and `CURRENT_WORKER`.
that was observed directly — one os thread reporting two different values of a
variable written once at startup — and it silently dropped channel messages. no
source-level barrier covers it, because every thread-local read in every frame
that can span a park is exposed. so migration is gated on removing those reads
from the resumable path, which is its own project rather than a scheduler
tweak. `examples/grpc_chat` and `examples/grpc_reflect` are the two programs
sensitive enough to catch a placement change going wrong; run them first.

one caveat on the numbers themselves: every comparison in docs/performance.md
and bench/README.md was measured on the same 2-core box. "green wins everywhere"
is true there and unverified on wider hardware.

two related ownership gaps, both bounded leaks rather than unsafety, are also
outstanding: passing a bare `T!` or `T?` local as a call argument leaks its
payload (the caller-side cascade does not yet treat a call argument as the
borrow it now provably is), and extracting the same optional local twice is a
rare use-after-free that needs a second-extraction check rather than the blanket
retain that was tried and reverted for regressing the common single case.

sweeping std's shared globals for the same class of bug turned up three things
that are questions of design rather than repairs, so they are recorded here
instead of decided.

`std.metrics` is correct under concurrency and does not scale. one mutex covers
all thirteen registries, so every counter increment, gauge set and histogram
observation in the process serializes on it, and a metric written once per
request is the normal case. measured on the 2-core box, one task manages 7.4M
increments a second; two manage 3.7M between them, four 3.0M, eight 2.4M — the
aggregate falls as tasks are added, which is what a single lock looks like. the
absolute cost is still small next to a request, around 0.4 µs per increment at
eight tasks, so nothing is on fire. it is the shape that will not hold if a
process ever writes metrics faster than it serves requests. every way out costs
something. sharding by metric name spreads the contention but makes a coherent
snapshot harder to take. a lock per series adds a lookup to the hot path.
atomic counters are the obvious answer for a counter and no answer at all for a
histogram, which updates seven values as one unit.

`std.net.tls` configs are never closed, and `std.net.http` builds one per https
request. `Config.close()` exists and removes the config's entries from the
registry, but nothing in std or the examples calls it, so every https request
adds another copy of the system root bundle — 219 KB here — to a map that only
grows. two hundred configs held 44 MB that closing them released. the fix is an
ownership decision rather than a patch: either the http client closes the config
it built, which means being sure no response still refers to it, or the default
client config becomes a process-wide value built once, which changes what
`tls.client_config()` returns.

`std.args` is safe by convention, with nothing enforcing the convention. its ten
globals have no lock, which is fine given the parser's shape: init, then the
add_flag / add_option / add_positional calls, then parse, all from main, after
which everything left only reads. no path in the module mutates from the query
side, so the state really is fixed once parse returns. nothing stops a program
from calling the setup half from a spawned task, though, and there would be no
diagnostic if it did — just a torn string. a lock here is cheap. whether a cli
argument parser should carry one is the part worth an opinion.
