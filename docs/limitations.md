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
- **a bare `none` cannot start an inference chain** — `none` is accepted only
  where the target is already an optional, which is every position that has
  one: bindings, assignments, returns, arguments, struct fields, collection
  and tuple elements, and `==` / `!=`. an unannotated `x := none` is the one
  place with no target to check against; it binds a type nothing else accepts,
  so the first use of `x` reports instead. write `x: T? := none`.
- **a plain value widens into an optional in most positions, not all** — `3`
  is accepted where an `Int?` is expected, and the callee reads the `Some(3)`
  it expects. that covers bindings, assignments, returns, struct fields
  (positional, named and defaulted), collection literal elements, map index
  assignment, and the argument of a plain function, a method, a lambda and a
  function value. a collection literal in argument position takes the
  parameter's declared element type too, so `f([1, 2])` against a
  `List[Int?]` parameter widens element by element. two argument positions
  still need the value bound to a `T?` local first:
  - a builtin container store or query — `xs.push(3)` into a `List[Int?]`,
    `m.insert(k, 3)` into a `Map[K, Int?]`, `s.add(3)` into a `Set[Int?]`.
    the element type also feeds `contains`, `index_of` and `remove`, which
    compare rather than store, so widening there would answer against a
    freshly built optional and always miss.
  - an enum variant payload — `Probe.Alpha(3)` against an `Int?` payload
    still needs the value bound to an `Int?` local first. destructuring is
    fine: a match binding on an optional payload has the payload's declared
    type, so `Probe.Alpha(b0) => b0.unwrap_or(0)` reads it like any other
    optional local.

  a parameter of a *generic function* now widens like any other argument —
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
- **a collection literal does not widen into an optional element that is
  itself a container** — `{"a": [1, 2]}` against a `Map[String, List[Int]?]`
  reports, because the literal walk recurses into a container target and an
  optional is not one. this predates the widening above and applies equally to
  a binding, a struct field and an argument. bind the inner container to a
  `List[Int]?` local first. the same nesting stops one argument case: a
  literal against a `List[Int?]?` parameter has to widen its elements and its
  container at once, and reports.
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
  call site that can see the value. method dispatch is per-declaration: two
  modules can each declare a struct with the same name and a call binds
  only to the receiver's own methods (E209 otherwise, even when the other
  struct has the method). a second impl block giving the *same* declaration
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
  everywhere else. clauses attach to functions and impl methods, and an
  interface member signature parses one too — though generic interface
  members themselves are not supported yet (the member's type parameter is
  unknown in its body, E202), with either bound spelling. struct,
  interface, and impl headers still take inline bounds only, and a
  method's clause may name only the method's own type parameters, not the
  owner's.
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
  so an `Opt[String]` releases its string when the box dies, the same way a
  generic struct instance releases its fields.

## standard library

- **tls 1.2** — the client and server speak tls 1.3 and, as a fallback, tls
  1.2 (ecdhe + aead only; the four ecdhe-rsa/ecdhe-ecdsa aes-128-gcm and
  chacha20-poly1305 suites), negotiating the highest a peer supports and
  refusing anything below 1.2 — the same posture as go's crypto/tls and rustls.
  `require_tls13()` locks a config to 1.3. the 1.2 fallback does not yet do
  session resumption, renegotiation, or client-certificate auth, supports rsa (≥2048-bit) and ecdsa (p-256) server certificates, and has no
  aes-256 suites.
- **testing** — `test` blocks are discovered and run by `pith test` (with
  `--filter`), and `std/testing` adds assertions and a `with_temp_dir` fixture
  helper. parameterized cases have no support: a table-driven test is a loop you
  write yourself. the project's own suite is golden-snapshot based (see
  `tests/`).
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

- **no incremental analysis** — `pith lsp` re-checks the whole import closure
  on every change, so diagnostics on a large file trail the last keystroke by
  the closure's check time. queries answer instantly from the last snapshot
  throughout. see [docs/lsp.md](lsp.md) for the feature list and the measured
  numbers.
- **no package registry** — dependencies are local path entries in `pith.toml`.
  `pith package lock` writes a `pith.lock` and `pith package install` copies
  those paths into `.pith/packages`, but nothing fetches over the network and
  there is no hosted index.
- **no debugger** — runtime stack traces are thin and there is no stepping.
- **a diagnostic points at its construct's last token** — columns are exact
  now (a token's recorded position is where it starts, and the caret lands on
  it), but a node's position is the position of the last token that formed it,
  so an error about a whole expression points at its closing token rather than
  its first.
- **an error inside a string interpolation points at the string, not at the
  expression** — the expression in `"{f(x)}"` is parsed as its own fragment, so
  its tokens are stamped with the enclosing interpolation's position. the line
  is right; the column is the string's rather than the expression's, because
  the fragment's own columns are relative to the expression text.

## backend

these are internal and do not usually surface in source, but they shape the
correctness story:

- the ir is self-describing — calls carry an explicit return kind and field
  loads carry their type — and the cranelift consumer reads that metadata rather
  than guessing. the older inference path that caused a few cross-module bugs is
  gone.
- memory reclamation is compiler-emitted reference counting (see the readme's
  memory section). closures are reference counted and freed like other heap
  values. the one structural gap that remains: strong reference cycles leak
  when nothing marks the back edge — but every cycle shape now has a weak
  escape hatch. a struct graph breaks its own cycle with a `weak` field, a
  local holds a struct weakly with `weak name := expr`, and a closure that
  captures a weak binding holds its target weakly too, so a callback stored
  on the object it reads from reclaims with the object (see
  docs/ownership.md). an unmarked strong cycle still leaks by default,
  bounded by design — the discipline never produces a dangling pointer in
  exchange. for cycles nobody marked there is now an experimental
  trial-deletion collector behind `PITH_CYCLE_GC=1` (off by default; see
  docs/ownership.md for what it reclaims and what stays uncollectable),
  with `std.concurrent.gc_collect()` to force a pass. (removing an element from a
  container, returning early on an error path, and indexed reads of
  `List[Struct]` were all listed here once; each was fixed and each is now
  pinned flat by the leak-growth gate, measured at two round counts.)
- a fresh optional written straight into an argument is released once the
  emitter proves through the callee's body that the caller stayed the shell's
  sole owner (see docs/ownership.md). that covers a call-produced optional
  (`f(maybe(i))`), a bare `none` (`f(none)`), a plain value widened into an
  optional parameter (`f(3)`, `f(Point(1))`), and all three handed to a
  method (`obj.take(maybe(i))`). what stays on the leak side, about 64 bytes
  per call: a callee the walk cannot read — one in another module, or a
  method on a generic receiver — and a callee that extracts a heap payload
  out of the optional rather than reading it in place. binding the value
  first (`v: Int? := 3` then `f(v)`) is reclaimed normally either way.
- a collection literal whose element type is an optional (`List[Int?]`,
  `Map[String, Int?]`) does not release the optionals it holds — about 128 bytes
  per literal. building the container and pushing into it does not leak; only
  the literal form does.
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
  share a connection between tasks without coordinating its close. the
  coordination std uses wherever a task of its own reads a socket — the http/2
  client's reader, the h2 server's drain — is `tcp_shutdown` first, which ends
  that task's current and next call while the descriptor number stays
  reserved, then wait for it to stop, and only then close. closing first hands
  the number back for the next open anywhere in the process, and a reader that
  has not stopped yet reads whatever lands on it.

## the green backend, now the default on linux

as of 2026-07-27 the green backend is what a spawned task runs on when you
build for linux; `PITH_GREEN=0` switches back to one os thread per task, and on
macos and the bsds os threads are still the default with `PITH_GREEN=1` as the
opt-in. green wins every shape this repo measures: spawn is ~30x the os-thread
backend at a seventeenth of the memory, and the channel fan-out benchmark runs
2.6x faster than os threads and ahead of rust and zig. the whole regression
corpus, 380 cases at both worker counts, produces byte-identical output to the
recorded goldens (`make verify-green-corpus`, run in ci). what follows is
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

a `std.net.tls` config that the caller builds is the caller's to close, and
nothing in the language notices when one is not. the rule is now uniform inside
std — whoever builds a config closes it, and `client_config()` shares one cached
root bundle rather than handing out a copy — so an https request no longer leaks
a config per request. what is left is that the compiler cannot help: a program
that builds its own config for `dial_with_config` and forgets `close()` holds a
registry slot until it exits, with no diagnostic. the slot is small now, which
makes it a slow leak rather than a fast one, which is arguably worse. what would
actually fix it is a destructor that runs when the last reference to a `Config`
goes away, which is the same missing feature behind several entries here.

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
