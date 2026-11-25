# Guarded data and locked blocks

This is a design for checking, at compile time, that data shared between tasks
is only touched with its lock held. Status: implemented (see [as built](#as-built)
below; the sections before it are the original design proposal as written).
This page says why the check was needed, what the syntax would be, what the
checker would enforce, what it would cost, in what order it could land, and how
the work was branched and tested.

## Why

Pith makes memory safe by having the compiler place every retain and release,
and the runtime checks the rest. That covers one task. It does not cover two
tasks that reach the same list, map or struct, because a collection is one
buffer behind a handle, and two tasks writing it at once corrupt the buffer.
The page docs/concurrency.md says so, and E248 refuses the one case the checker
can see: a collection this scope still holds, passed straight to `spawn`.

Everything else is convention. A `Mutex` is a value with `lock()` and
`unlock()` and no relation to any data. The std code pairs them by naming, as
in `rooms` beside `rooms_mu`, and by comments like the one in std/metrics.pith
that says `shard_mu[i]` guards entry `i` of every map list below it. The
compiler accepts all of these:

- Reading or writing guarded data without taking the lock, from a function
  written later by someone who did not know the convention
- An early `return` or a `!` between `lock()` and `unlock()`, which leaves the
  lock held and parks every later caller for good
- Taking a lock a task already holds, which deadlocks, since there is no
  reentrancy
- A `mut` global collection in a program that spawns, with no lock at all.
  `std.args` is one, and docs/limitations.md says it can tear with no
  diagnostic

The std code has 382 `lock()` and `unlock()` calls in 24 files, every one paired
by hand. The 2026-10-31 audit found a wake race in the runtime (fixed in #1270),
and ThreadSanitizer still reports an unlocked read in the HTTP/2 client. The
checks below would not have found the runtime bug, which is Rust code, but they
turn the whole class of "forgot the lock" and "forgot the unlock" bugs in Pith
code into compile errors.

Rust prevents these with its ownership rules: data behind a `Mutex<T>` can
only be reached through the lock, and the `Send` and `Sync` traits stop a plain
collection from crossing a thread at all. The design here aims at that
guarantee without the ownership model that delivers it in Rust: existing Pith
code keeps its shape and gains annotations, and the checker does the pairing
that comments do today.

## The design

The design adds three things to the language and one rule. `guard` declares
data and the mutex that protects it, `locked` holds the mutex for a block, and
`holding` marks a function that runs with the mutex already held.

### `guard`

A `guard` block declares a mutex and the data it protects:

```pith
guard jwks_mu:
    mut cached_document: Map[Int, String] := {}
    mut cached_at: Map[Int, Int] := {}
    mut parsed_keys: Map[Int, KeySet] := {}
```

The block declares `jwks_mu` as a `Mutex` and the three globals inside it. The
names belong to the enclosing scope, as if they were written at module level,
so the rest of the module reads `cached_document` as before. The block exists
to group them, which matches how Pith already groups a struct's fields and an
`impl`'s methods.

The block works at module level and inside a struct, where it declares a
`Mutex` field and the fields it protects:

```pith
struct PoolConn:
    conns: List[Conn]
    guard pick:
        next: Int
```

`PoolConn` has three fields: `conns`, `pick` and `next`. A struct literal does
not set `pick`, since the struct creates it unlocked.

The mutex is always declared by its block, so a guard cannot name a mutex from
another module or a field of another struct. `jwks_mu.lock()` still works on
it, as on any `Mutex`.

Reading or writing a guarded name outside a block that holds its mutex is a
compile error:

```pith
fn peek(handle: Int) -> String?:
    return cached_document.get(handle)
    # error[E271]: cached_document is guarded by jwks_mu, which this function does not hold
    #   fix: wrap the access in `locked jwks_mu:`, or mark the function `holding jwks_mu`
```

A read counts as an access. A read of a map while another task inserts into it
is as unsafe as the write, so there is no read exemption.

For sharded state, `guard name[n]:` declares a `List[Mutex]` of `n` mutexes
(see "Sharded locks" below).

### `locked`

This is a block that holds a mutex for its length and releases it on every
exit:

```pith
fn store_document(handle: Int, text: String):
    locked jwks_mu:
        cached_document.insert(handle, text)
        cached_at.insert(handle, time.unix())
        parsed_keys.remove(handle)
```

The block is an ordinary block. `return`, `break`, `continue`, a `!` that
propagates and falling off the end all unlock. That is the same shape as
`defer`, which the emitter already lowers statically at each exit, loops
included (`ir_defer_loop_base_stack` in self-host/ir_emitter_core.pith), so
`locked m:` lowers to `m.lock()`, the body, and `m.unlock()` on each exit
edge. No runtime machinery is added.

Inside the block the checker treats the mutex as held. A nested `locked` on a
mutex the block already holds is a compile error, since the runtime would
deadlock there.

`lock()` and `unlock()` stay. A `locked` block cannot express "unlock early,
do slow work, lock again", which `Cache.shared_keys` in std/crypto/jwks.pith
does around a fetch, and some code wants the lock held across a call boundary.
Those keep the calls. The checker does not try to follow a hand-written
`lock()`: guarded data under one is still an error, and the fix is a `locked`
block around the accesses, or a `holding` helper for the part that needs the
lock.

### `holding`

A function that expects to be called with a lock held says so:

```pith
fn evict(handle: Int) holding jwks_mu:
    cached_document.remove(handle)
    cached_at.remove(handle)
```

Inside its body the mutex counts as held. Every call to it must be inside a
`locked` block for that mutex, or inside another function `holding` it.
Calling it anywhere else is a compile error. For a method, the mutex may be a
field: `fn bump() holding self.pick:`.

### The sharing rule

The annotations only protect data someone annotated. The sharing rule makes
people annotate: a `mut` module global whose type is a list, map, set or struct, in a
program that contains `spawn`, must be one of

- Declared inside a `guard` block
- `threadlocal`
- Never assigned or mutated after module init (the checker already knows
  which globals a function writes, from `ir_record_storage_keepers`)

Anything else is an error. It lands as a lint first (see "Stages"), because
every program with an unguarded global collection and a `spawn` trips it, and
some of those are single-task programs that spawn once for something
unrelated. The lint names the global and the spawn.

Struct fields are out of scope for the rule in this design. A struct that
crosses to a task through `spawn` is already E248 if it is a collection, and a
struct holding collections was a known E248 gap, since closed (see "As
built").

## The examples, before and after

### Module state: std/crypto/jwks.pith

Before:

```pith
mut jwks_mu := Mutex()
mut cached_document: Map[Int, String] := {}
mut cached_at: Map[Int, Int] := {}
mut parsed_keys: Map[Int, KeySet] := {}

fn store_document(handle: Int, text: String):
    jwks_mu.lock()
    cached_document.insert(handle, text)
    cached_at.insert(handle, time.unix())
    parsed_keys.remove(handle)
    jwks_mu.unlock()

impl Cache:
    fn shared_keys() -> KeySet!:
        jwks_mu.lock()
        if parsed_keys.contains_key(self.handle) and time.unix() - cached_at[self.handle] < self.ttl_v:
            held := parsed_keys[self.handle]
            jwks_mu.unlock()
            return held
        jwks_mu.unlock()
        text := self.document(false)!
        set := parse(text)!
        jwks_mu.lock()
        if cached_document.contains_key(self.handle) and cached_document[self.handle] == text:
            parsed_keys.insert(self.handle, set)
        jwks_mu.unlock()
        return set
```

After:

```pith
guard jwks_mu:
    mut cached_document: Map[Int, String] := {}
    mut cached_at: Map[Int, Int] := {}
    mut parsed_keys: Map[Int, KeySet] := {}

fn store_document(handle: Int, text: String):
    locked jwks_mu:
        cached_document.insert(handle, text)
        cached_at.insert(handle, time.unix())
        parsed_keys.remove(handle)

impl Cache:
    fn shared_keys() -> KeySet!:
        locked jwks_mu:
            if parsed_keys.contains_key(self.handle) and time.unix() - cached_at[self.handle] < self.ttl_v:
                return parsed_keys[self.handle]
        text := self.document(false)!
        set := parse(text)!
        locked jwks_mu:
            if cached_document.contains_key(self.handle) and cached_document[self.handle] == text:
                parsed_keys.insert(self.handle, set)
        return set
```

The early return no longer carries its own unlock, the fetch happens between
two blocks exactly as it happened between two unlock and lock pairs, and any
new function in the module that touches `parsed_keys` without the lock fails
to compile.

### A struct field: std/net/grpc.pith

Before:

```pith
struct PoolConn:
    conns: List[Conn]
    pick: Mutex
    next: Int

impl PoolConn:
    fn choose() -> Int:
        self.pick.lock()
        i := self.next
        self.next = (self.next + 1) % self.conns.len()
        self.pick.unlock()
        return i
```

After:

```pith
struct PoolConn:
    conns: List[Conn]
    guard pick:
        next: Int

impl PoolConn:
    fn choose() -> Int:
        locked self.pick:
            i := self.next
            self.next = (self.next + 1) % self.conns.len()
            return i
```

### Sharded locks: std/metrics.pith

The registry keeps eight mutexes in a list, and `shard_mu[i]` guards element
`i` of several map lists. The checker cannot match an index expression at one
site to an index expression at another, so a guard over a `List[Mutex]` is
checked at the list level: any `locked shard_mu[k]:` satisfies an access to
data in the `shard_mu` block.

Before:

```pith
mut shard_mu: List[Mutex] := [Mutex(), Mutex(), Mutex(), Mutex(), Mutex(), Mutex(), Mutex(), Mutex()]
mut metric_descriptions: List[Map[String, String]] := [{}, {}, {}, {}, {}, {}, {}, {}]
```

After:

```pith
guard shard_mu[8]:
    mut metric_descriptions: List[Map[String, String]] := [{}, {}, {}, {}, {}, {}, {}, {}]

fn register_description(bare: String, help: String, unit: String):
    shard := shard_of(bare)
    locked shard_mu[shard]:
        if help != "" and not metric_descriptions[shard].contains_key(bare):
            metric_descriptions[shard].insert(bare, help)
```

This is weaker than the single-mutex case: holding shard 3 and touching
element 5 compiles. It still catches the unlocked access, which is the bug
that has happened, and the comment in std/metrics.pith that describes the
invariant stays.

## What the checker enforces

The whole check is a pass over each function body after type checking, with a
set of "held" mutexes that grows on entering a `locked` block or a `holding`
function and shrinks on leaving the block.

1. An access to a name declared in `guard m:` requires `m` in the held set.
   An access is any read or write of the name, including a method call on it
   and passing it as an argument.
2. A `locked m:` block adds `m`. `m` must be a `Mutex`, or an index into a
   `List[Mutex]`. `m` already in the held set is an error (self deadlock).
3. A call to a function declared `holding m` needs `m` in the held set.
4. A `guard` block appears only at module level or in a struct body, and
   holds only declarations: `mut` globals at module level, fields in a
   struct.
5. A `mut` global collection or struct in a program that spawns must be
   guarded, `threadlocal`, or not written after init (the sharing rule).

What the checker does not do:

- It does not know which index of a `List[Mutex]` is held.
- It does not follow a hand-written `lock()`. An access to guarded data after
  `m.lock()` and before `m.unlock()` is still an error, and the fix is the
  `locked` block.
- It does not check lock order, so two blocks that nest two mutexes in
  opposite orders can still deadlock at run time. The E311 lint reports it
  (see "As built").
- It does not check that a `locked` block holds no park. Holding a lock
  across a channel receive or a socket read compiles, as it does today. The
  E310 lint reports it (see "As built").

## What it costs

### At run time

A program pays nothing it does not pay today. `locked m:` emits the same
`pith_mutex_lock` and `pith_mutex_unlock` calls the hand-written pairs emit
today, one per exit edge, and `guard` and `holding` emit nothing beyond the
mutex the code already allocates. `defer` is lowered the same way and has no
runtime cost.

For the data that the sharing rule newly forces behind a lock, the cost is
one uncontended lock and unlock pair per access. Measured on the Ryzen 9
9950X with a 20M-iteration loop, pinned to one core, an uncontended pair
costs 8 ns (a compare-and-swap to take the lock and one swap to release it,
on a line the core already owns). The contended cost is what `lock_sweep`
measures: sixteen tasks on one mutex, 50M operations, 74 ms in the
2026-11-12 run, against 61 ms for Go. That row has read 87 ms in earlier runs.

Where this could show in a benchmark:

- `std.args` gains a lock. It is read at startup, not on a hot path.
- A program that today reads a global map from many tasks with no lock, and
  gets away with it because nothing writes after init, is not forced to lock:
  the sharing rule accepts a global that is never written after module init.
- A block that is wider than the hand-written pair it replaces holds the lock
  longer. `shared_keys` above shows the fix, which is two blocks. The lint for
  a parking call inside `locked` would catch the worst version.

The gateway and HTTP server benchmarks lock on pooled connections and
registries already, through the pairs that become blocks, so their numbers
should not move. "Testing and merging" below says how that is checked.

### At compile time

The check is one more pass over each function body, linear in its size, with a
small set of held mutexes. The checker already walks every body for E248 and
for `ir_record_storage_keepers`, so this is a few percent of check time at
most. The measured analysis times in docs/lsp.md are the numbers to compare
against after the pass exists.

### In the language

The language gains three words and one rule. `guard` and `locked` are blocks,
and `holding` is a function suffix. None of them changes what a value is or
how it is passed, and code with none of them compiles as before until the
sharing rule becomes an error.

## Stages

1. **`locked` alone.** The block, its lowering on the defer path, tests for
   each exit kind (fall through, return, break, continue, `!`), and the
   self-deadlock error. This is useful by itself: it removes every
   hand-written unlock on an early exit. The std modules migrate their pairs to
   blocks where the pair sits in one function, which is most of the 382 calls.
2. **`guard` and `holding`.** The `guard` block at module level and in
   structs, `guard name[n]:`, the held-set pass, E271 for an unguarded access
   and its fix text, and the `List[Mutex]` weakening. The std modules move every
   global and field that a comment says is guarded into a `guard` block. The
   metrics shards and the HTTP/2 connection state are the two to do first,
   since they are where the comments are longest.
3. **The sharing rule as a lint.** `pith lint` reports an unguarded `mut`
   global collection in a program that spawns, naming the global and the
   spawn. The std modules and the examples go clean.
4. **The sharing rule as an error.** At a version boundary, after a release
   with the lint on, with the lint's text as the error's fix text.

Stages 1 to 3 are the feature. Stage 4 was planned to wait for a release.
Pith has no users outside this tree yet, so it landed right after the
others (see "As built").

The count checker proposed alongside this one is independent and can run
before or after. Neither needs the other.

## Branches and pull requests

The feature is built in isolation from main until it is complete and shown to
cause no regressions, and it lands as one commit per stage.

- Each of stages 1 to 3 is one pull request, split into several small commits
  where that helps review: the compiler change with its tests, then the std
  migration.
- The pull requests form one stack. Stage 1 branches from main, and each later
  stage branches from the one before and targets it, so each diff shows one
  stage.
- Every pull request stays open until the whole feature is up. Nothing merges
  into main before then.
- Work moves to the next stage without waiting for the previous one's long CI
  jobs. Every pull request must still have green CI before it merges.
- If main moves while the stack is open, main is merged into stage 1 and
  through the stack, and the affected CI runs again.

## Testing and merging

When the last pull request is up, the tip of the stack is tested against the
main commit stage 1 branched from:

- The full regression suite: `make run-regressions`, `make
  run-regressions-self`, `make test-std-self`, `make run-examples-self-only`,
  `make check-invalid`, `make check-bootstrap-seed` and the sanitizer runs in
  tooling/sanitize.sh.
- The full benchmark suite: `bench/compare.sh` against Go and Rust,
  `bench/http_sweep.sh`, `bench/grpc/run.sh` and the gateway benchmark.
- An interleaved race of every row that moves, against the base commit, on
  the pinned setup bench/README.md describes. `lock_sweep`, the HTTP sweep and
  the gateway are the rows to watch, since they lock the most. A row that
  moves past its run-to-run range is a regression until a race says it is
  noise.
- The compile time of the self-hosted compiler and the LSP analysis times in
  docs/lsp.md, before and after.

The results go into bench/README.md with the last pull request. If they show
no regressions and every pull request has green CI, the stack merges into main
bottom up, one squash-merged commit per stage. Before each merge the next pull
request is retargeted to main, and main's own CI run on each merged commit
must be green before the next stage merges.

## As built

Stages 1 to 3 are #1324, #1325 and the sharing lint after them. The build
settled some questions this page left open and departed from it in places:

- **Error codes.** E271 is an access or a call without the lock. E272 is a
  `locked` or `holding` path that is not a path to a `Mutex`, or a `locked`
  body that changes its own path, since the unlock re-reads the path at each
  exit. E273 is a `locked` block on a path an enclosing block or the
  function's `holding` already holds. E274 is a guard block's shape. E309 is
  the sharing lint.
- **The mutex path.** `locked` and `holding` take a variable, `self`, a field
  read off one, or a list element indexed by a variable or a literal. A call
  result such as `locked pick_lock():` is refused, because the path is
  evaluated again to unlock.
- **Guard blocks lower to plain declarations.** A module block becomes
  `m := Mutex()` and the globals; a struct block adds `m: Mutex = Mutex()` as
  the struct's last field, so a constructor call leaves it out. The emitter is
  unchanged.
- **Guarded globals are private.** A `pub` global in a guard block is E274,
  since no other module can name the mutex. A sharded guard is for module
  globals only, and a generic struct cannot have a guard block.
- **`holding` on a list.** `holding shard_mu` on a `List[Mutex]` is satisfied by
  holding any element, which lets a whole-registry operation run under every
  shard: std/metrics.pith takes all eight by a recursion that nests eight
  `locked` blocks, in index order.
- **The sharing rule is an error.** E309 started as a lint and is now an
  error from `pith check`, `build`, `run` and `test`, which run it after the
  checker on a program that checked cleanly. `pith lint` still reports it.
- **"A program that spawns".** A program spawns when a function in the entry
  file holds a `spawn` or calls a function that does. A `spawn` merely present
  in an imported module does not count, so the compiler, which imports
  std.concurrent without spawning, is not flagged. Calls are resolved by
  module: a plain name to the module's own function or a `from` import,
  `alias.f` through the import, and a method through its receiver's type. A
  method on a receiver whose type does not name the declaration, such as an
  interface, counts every method of that name. The lint first matched calls
  by bare name, and that flagged bench/catalog_server.pith, which never
  spawns, because std.net.http has a function with the same name as one of
  its own.
- **What counts as a write.** An assignment rooted at the global, or a mutating
  method called on it. `sharing_lint.pith` now follows global buffers through
  aliases, locals, function arguments, and return values as well (see E309 in
  `docs/errors.md`).
- **The TLS config registry.** It was fourteen handle-keyed maps behind
  generic accessors that took the lock themselves, which the checker could not
  follow. It is now one guarded map from handle to a `ConfigRecord`. A reader
  takes the record under the lock and uses it after; a setter copies the
  record, changes the copy and stores it under one lock, so a record is never
  changed after it is stored.
- **The HTTP/2 client.** `mu` and `gate` each guard a block of `Client`'s
  fields. The stream id counter and the peer's frame size limit are atomics
  instead, because the inline path and every request read them before taking a
  lock. Guarding the rest found two reads made without the lock.
- **`Limiter` and `Breaker`.** They kept their state in atomics, but every
  access already took the struct's mutex, so the atomics bought nothing. The
  state is now plain fields in a `guard mu:` block. With 32 tasks sharing one
  limiter and one breaker, a call takes 16% less time. The std.concurrent test
  counters for the live and peak task counts moved the same way, and the
  counter the module bumps without a lock stays an atomic.
- **Parking under a lock.** E310 is a lint for a call that parks while a
  mutex is held: `await`, a `select` with no default arm, `sleep`, a channel
  `recv`, and the std file, TCP, TLS and WebSocket reads. A channel send is
  left out, because the HTTP/2 client sends its frames under `mu` to keep
  stream ids in order. The std modules, the examples and the compiler lint
  clean.
- **E248 and structs.** A struct passed to `spawn` is refused when one of
  its fields reaches a list, a map or a set outside a `guard` block. Moving
  std under the rule put the HTTP/2 HPACK encoder and decoder under
  `hpack_mu` on both connection types, std.web's routes under the app's
  mutex (a connection takes them once, as it starts), and a request's header,
  query and path maps under `fields_mu`, along with the gRPC channel's
  credentials, the stream reader's header blocks, the HTTP/2 pool's client
  list and the terminal's options and output queue. The std.obs module builds
  its export target in the spawn's arguments instead.
- **Frozen fields and handles that leave a lock.** A handle copied out of a
  `locked` block reached the buffer with no lock. E276 now refuses a guarded
  collection stored outside the block, returned from it or captured by a
  closure made in it, and allows a take that gives the field a new value.
  Read-only data needed a way out, so a struct field can be declared `frozen`:
  set at construction, never changed through a path that runs through it
  (E275), read by any task with no lock, and not a shared buffer to E248.
  The std.web router, a request's header, query and path maps, the HTTP/2
  pool's client list, the terminal's options, metrics series labels, the TLS
  parsed-roots cache entries and the gateway's gRPC targets moved from guards
  to frozen fields, which drops the per-request mutex E248 had added to
  requests. The zstd predefined tables are built at module init. In a program
  that spawns, `sharing_lint.pith` now follows frozen field buffers through
  aliases, locals, parameters, and calls too (E275).
- **E248 at channel sends.** `send` and `try_send` are checked like a spawn's
  arguments, since the receiver gets the handle. The HTTP/2 client and server
  handed stream events to a helper that sent its parameter; the helper now
  returns the inbox, and each caller builds the event in the send.
- **Lock order.** E311 is a lint for two mutexes taken in opposite orders
  anywhere in the program. It records each mutex taken while another is held,
  directly or in a function called under the lock through a call the checker
  resolved, and reports a pair taken both ways. A mutex is named by its
  module global, by its struct type and field, or by its sharded guard, so
  two values of one struct are one name and nesting them is not reported.
  The std modules, the examples and the compiler lint clean.
- **Reentry through a call.** E312 is a lint for a call that takes a mutex
  its caller holds, which E273 cannot see because no `locked` block is
  nested. A module global is followed through the whole call chain; a
  struct's mutex is matched when a method on the same receiver locks that
  field on `self`.
- **Escape through a container or a call.** E276 saw a guarded collection
  assigned, returned or captured, but not one pushed into a list declared
  outside the block, inserted into a map, or handed to a function that
  stores it. A reader of that list then raced the writers under the lock.
  The sharing pass now refuses a guarded value stored into a container that
  outlives the block, a value handed where it cannot be followed, and an
  argument whose callee's summary lets it escape or stores it into another
  argument that outlives the block. The summaries are the ones E248 already
  computes over the whole program, so generic functions and interface methods
  are followed, and a call that only reads its argument stays allowed. The std
  modules, the examples and the compiler check clean.

## Open questions

- **The words.** `guard`, `locked` and `holding` are the current choice.
  `locked` could be `with m:` or `hold m:`; it should not be `lock m:`, which
  reads as a statement that does not release.
- **Guarded locals.** This design guards globals and struct fields. A local
  shared with a spawned task is already E248 when it is a collection, so a
  `guard` block inside a function body is left out until a case needs it.
- **The sharded form.** `guard shard_mu[8]:` fixes the count at compile time,
  which matches std/metrics.pith, std/trace.pith and the TLS connection
  shards. A shard count chosen at run time would need another form.
- **`RwMutex`.** Nothing here needs a reader-writer lock, and std does not
  have one. If one is added, `guard` applies unchanged and `locked` would
  take a mode.
