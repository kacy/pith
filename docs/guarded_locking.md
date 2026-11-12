# guarded data and locked blocks

a design for checking, at compile time, that data shared between tasks is only
touched with its lock held. nothing here is implemented. this page says why the
check is needed, what the syntax would be, what the checker would enforce, what
it would cost, in what order it could land, and how the work is branched and
tested.

## why

pith makes memory safe by having the compiler place every retain and release,
and the runtime checks the rest. that covers one task. it does not cover two
tasks that reach the same list, map or struct, because a collection is one
buffer behind a handle, and two tasks writing it at once corrupt the buffer.
docs/concurrency.md says so, and E248 refuses the one case the checker can see:
a collection this scope still holds, passed straight to `spawn`.

everything else is convention. a `Mutex` is a value with `lock()` and
`unlock()` and no relation to any data. std pairs them by naming, as in
`rooms` beside `rooms_mu`, and by comments like the one in std/metrics.pith
that says `shard_mu[i]` guards entry `i` of every map list below it. the
compiler accepts all of these:

- reading or writing guarded data without taking the lock, from a function
  written later by someone who did not know the convention
- an early `return` or a `!` between `lock()` and `unlock()`, which leaves the
  lock held and parks every later caller for good
- taking a lock a task already holds, which deadlocks, since there is no
  reentrancy
- a `mut` global collection in a program that spawns, with no lock at all.
  `std.args` is one, and docs/limitations.md says it can tear with no
  diagnostic

std has 382 `lock()` and `unlock()` calls in 24 files, every one paired by
hand. the 2026-10-31 audit found a wake race in the runtime (fixed in #1270),
and thread sanitizer still reports an unlocked read in the http/2 client. the
checks below would not have found the runtime bug, which is rust code, but they
turn the whole class of "forgot the lock" and "forgot the unlock" bugs in pith
code into compile errors.

rust prevents these with its ownership rules: data behind a `Mutex<T>` can
only be reached through the lock, and the `Send` and `Sync` traits stop a plain
collection from crossing a thread at all. the design here aims at that
guarantee without the ownership model that delivers it in rust: existing pith
code keeps its shape and gains annotations, and the checker does the pairing
that comments do today.

## the design

three additions to the language and one rule. `guard` declares data and the
mutex that protects it, `locked` holds the mutex for a block, and `holding`
marks a function that runs with the mutex already held.

### `guard`

a `guard` block declares a mutex and the data it protects:

```pith
guard jwks_mu:
    mut cached_document: Map[Int, String] := {}
    mut cached_at: Map[Int, Int] := {}
    mut parsed_keys: Map[Int, KeySet] := {}
```

the block declares `jwks_mu` as a `Mutex` and the three globals inside it. the
names belong to the enclosing scope, as if they were written at module level,
so the rest of the module reads `cached_document` as before. the block exists
to group them, which matches how pith already groups a struct's fields and an
`impl`'s methods.

the block works at module level and inside a struct, where it declares a
`Mutex` field and the fields it protects:

```pith
struct PoolConn:
    conns: List[Conn]
    guard pick:
        next: Int
```

`PoolConn` has three fields: `conns`, `pick` and `next`. a struct literal does
not set `pick`, since the struct creates it unlocked.

the mutex is always declared by its block, so a guard cannot name a mutex from
another module or a field of another struct. `jwks_mu.lock()` still works on
it, as on any `Mutex`.

reading or writing a guarded name outside a block that holds its mutex is a
compile error:

```pith
fn peek(handle: Int) -> String?:
    return cached_document.get(handle)
    # error[E271]: cached_document is guarded by jwks_mu, which this function does not hold
    #   fix: wrap the access in `locked jwks_mu:`, or mark the function `holding jwks_mu`
```

a read counts as an access. a read of a map while another task inserts into it
is as unsafe as the write, so there is no read exemption.

for sharded state, `guard name[n]:` declares a `List[Mutex]` of `n` mutexes
(see "sharded locks" below).

### `locked`

a block that holds a mutex for its length and releases it on every exit:

```pith
fn store_document(handle: Int, text: String):
    locked jwks_mu:
        cached_document.insert(handle, text)
        cached_at.insert(handle, time.unix())
        parsed_keys.remove(handle)
```

the block is an ordinary block. `return`, `break`, `continue`, a `!` that
propagates and falling off the end all unlock. that is the same shape as
`defer`, which the emitter already lowers statically at each exit, loops
included (`ir_defer_loop_base_stack` in self-host/ir_emitter_core.pith), so
`locked m:` lowers to `m.lock()`, the body, and `m.unlock()` on each exit
edge. no runtime machinery is added.

inside the block the checker treats the mutex as held. a nested `locked` on a
mutex the block already holds is a compile error, since the runtime would
deadlock there.

`lock()` and `unlock()` stay. a `locked` block cannot express "unlock early,
do slow work, lock again", which `Cache.shared_keys` in std/crypto/jwks.pith
does around a fetch, and some code wants the lock held across a call boundary.
those keep the calls. the checker does not try to follow a hand-written
`lock()`: guarded data under one is still an error, and the fix is a `locked`
block around the accesses, or a `holding` helper for the part that needs the
lock.

### `holding`

a function that expects to be called with a lock held says so:

```pith
fn evict(handle: Int) holding jwks_mu:
    cached_document.remove(handle)
    cached_at.remove(handle)
```

inside its body the mutex counts as held. every call to it must be inside a
`locked` block for that mutex, or inside another function `holding` it.
calling it anywhere else is a compile error. for a method, the mutex may be a
field: `fn bump() holding self.pick:`.

### the sharing rule

the annotations only protect data someone annotated. the rule that makes
people annotate: a `mut` module global whose type is a list, map, set or
struct, in a program that contains `spawn`, must be one of

- declared inside a `guard` block
- `threadlocal`
- never assigned or mutated after module init (the checker already knows
  which globals a function writes, from `ir_record_storage_keepers`)

anything else is an error. it lands as a lint first (see "stages"), because
every program with an unguarded global collection and a `spawn` trips it, and
some of those are single-task programs that spawn once for something
unrelated. the lint names the global and the spawn.

struct fields are out of scope for the rule in this design. a struct that
crosses to a task through `spawn` is already E248 if it is a collection, and a
struct holding collections was a known E248 gap, since closed (see "as
built").

## the examples, before and after

### module state: std/crypto/jwks.pith

before:

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

after:

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

the early return no longer carries its own unlock, the fetch happens between
two blocks exactly as it happened between two unlock and lock pairs, and any
new function in the module that touches `parsed_keys` without the lock fails
to compile.

### a struct field: std/net/grpc.pith

before:

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

after:

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

### sharded locks: std/metrics.pith

the registry keeps eight mutexes in a list, and `shard_mu[i]` guards element
`i` of several map lists. the checker cannot match an index expression at one
site to an index expression at another, so a guard over a `List[Mutex]` is
checked at the list level: any `locked shard_mu[k]:` satisfies an access to
data in the `shard_mu` block.

before:

```pith
mut shard_mu: List[Mutex] := [Mutex(), Mutex(), Mutex(), Mutex(), Mutex(), Mutex(), Mutex(), Mutex()]
mut metric_descriptions: List[Map[String, String]] := [{}, {}, {}, {}, {}, {}, {}, {}]
```

after:

```pith
guard shard_mu[8]:
    mut metric_descriptions: List[Map[String, String]] := [{}, {}, {}, {}, {}, {}, {}, {}]

fn register_description(bare: String, help: String, unit: String):
    shard := shard_of(bare)
    locked shard_mu[shard]:
        if help != "" and not metric_descriptions[shard].contains_key(bare):
            metric_descriptions[shard].insert(bare, help)
```

this is weaker than the single-mutex case: holding shard 3 and touching
element 5 compiles. it still catches the unlocked access, which is the bug
that has happened, and the comment in std/metrics.pith that describes the
invariant stays.

## what the checker enforces

the whole check is a pass over each function body after type checking, with a
set of "held" mutexes that grows on entering a `locked` block or a `holding`
function and shrinks on leaving the block.

1. an access to a name declared in `guard m:` requires `m` in the held set.
   an access is any read or write of the name, including a method call on it
   and passing it as an argument.
2. a `locked m:` block adds `m`. `m` must be a `Mutex`, or an index into a
   `List[Mutex]`. `m` already in the held set is an error (self deadlock).
3. a call to a function declared `holding m` needs `m` in the held set.
4. a `guard` block appears only at module level or in a struct body, and
   holds only declarations: `mut` globals at module level, fields in a
   struct.
5. a `mut` global collection or struct in a program that spawns must be
   guarded, `threadlocal`, or not written after init (the sharing rule).

what the checker does not do:

- it does not know which index of a `List[Mutex]` is held.
- it does not follow a hand-written `lock()`. an access to guarded data after
  `m.lock()` and before `m.unlock()` is still an error, and the fix is the
  `locked` block.
- it does not check lock order, so two blocks that nest two mutexes in
  opposite orders can still deadlock at run time. the E311 lint reports it
  (see "as built").
- it does not check that a `locked` block holds no park. holding a lock
  across a channel receive or a socket read compiles, as it does today. the
  E310 lint reports it (see "as built").

## what it costs

### at run time

a program pays nothing it does not pay today. `locked m:` emits the same
`pith_mutex_lock` and `pith_mutex_unlock` calls the hand-written pairs emit
today, one per exit edge, and `guard` and `holding` emit nothing beyond the
mutex the code already allocates. `defer` is lowered the same way and has no
runtime cost.

for the data that the sharing rule newly forces behind a lock, the cost is
one uncontended lock and unlock pair per access. measured on the ryzen 9
9950x with a 20M-iteration loop, pinned to one core, an uncontended pair
costs 8 ns (a compare-and-swap to take the lock and one swap to release it,
on a line the core already owns). the contended cost is what `lock_sweep`
measures: sixteen tasks on one mutex, 50M operations, 74 ms in the
2026-11-12 run, against 61 ms for go. that row has read 87 ms in earlier runs.

where this could show in a benchmark:

- `std.args` gains a lock. it is read at startup, not on a hot path.
- a program that today reads a global map from many tasks with no lock, and
  gets away with it because nothing writes after init, is not forced to lock:
  the sharing rule accepts a global that is never written after module init.
- a block that is wider than the hand-written pair it replaces holds the lock
  longer. `shared_keys` above shows the fix, which is two blocks. the lint for
  a parking call inside `locked` would catch the worst version.

the gateway and http server benchmarks lock on pooled connections and
registries already, through the pairs that become blocks, so their numbers
should not move. "testing and merging" below says how that is checked.

### at compile time

one more pass over each function body, linear in its size, with a small set
of held mutexes. the checker already walks every body for E248 and for
`ir_record_storage_keepers`, so this is a few percent of check time at most.
docs/lsp.md has the measured analysis times to compare against after the
pass exists.

### in the language

the language gains three words and one rule. `guard` and `locked` are blocks,
and `holding` is a function suffix. none of them changes what a value is or
how it is passed, and code with none of them compiles as before until the
sharing rule becomes an error.

## stages

1. **`locked` alone.** the block, its lowering on the defer path, tests for
   each exit kind (fall through, return, break, continue, `!`), and the
   self-deadlock error. this is useful by itself: it removes every
   hand-written unlock on an early exit. std migrates its pairs to blocks
   where the pair sits in one function, which is most of the 382 calls.
2. **`guard` and `holding`.** the `guard` block at module level and in
   structs, `guard name[n]:`, the held-set pass, E271 for an unguarded access
   and its fix text, and the `List[Mutex]` weakening. std moves every global
   and field that a comment says is guarded into a `guard` block. the metrics
   shards and the http/2 connection state are the two to do first, since they
   are where the comments are longest.
3. **the sharing rule as a lint.** `pith lint` reports an unguarded `mut`
   global collection in a program that spawns, naming the global and the
   spawn. std and the examples go clean.
4. **the sharing rule as an error.** at a version boundary, after a release
   with the lint on, with the lint's text as the error's fix text.

stages 1 to 3 are the feature. stage 4 was planned to wait for a release.
pith has no users outside this tree yet, so it landed right after the
others (see "as built").

the count checker proposed alongside this one is independent and can run
before or after. neither needs the other.

## branches and pull requests

the feature is built in isolation from main until it is complete and shown to
cause no regressions, and it lands as one commit per stage.

- each of stages 1 to 3 is one pull request, split into several small commits
  where that helps review: the compiler change with its tests, then the std
  migration.
- the pull requests form one stack. stage 1 branches from main, and each later
  stage branches from the one before and targets it, so each diff shows one
  stage.
- every pull request stays open until the whole feature is up. nothing merges
  into main before then.
- work moves to the next stage without waiting for the previous one's long CI
  jobs. every pull request must still have green CI before it merges.
- if main moves while the stack is open, main is merged into stage 1 and
  through the stack, and the affected CI runs again.

## testing and merging

when the last pull request is up, the tip of the stack is tested against the
main commit stage 1 branched from:

- the full regression suite: `make run-regressions`, `make
  run-regressions-self`, `make test-std-self`, `make run-examples-self-only`,
  `make check-invalid`, `make check-bootstrap-seed` and the sanitizer runs in
  tooling/sanitize.sh.
- the full benchmark suite: `bench/compare.sh` against go and rust,
  `bench/http_sweep.sh`, `bench/grpc/run.sh` and the gateway benchmark.
- an interleaved race of every row that moves, against the base commit, on
  the pinned setup bench/README.md describes. `lock_sweep`, the http sweep and
  the gateway are the rows to watch, since they lock the most. a row that
  moves past its run-to-run range is a regression until a race says it is
  noise.
- the compile time of the self-hosted compiler and the lsp analysis times in
  docs/lsp.md, before and after.

the results go into bench/README.md with the last pull request. if they show
no regressions and every pull request has green CI, the stack merges into main
bottom up, one squash-merged commit per stage. before each merge the next pull
request is retargeted to main, and main's own CI run on each merged commit
must be green before the next stage merges.

## as built

stages 1 to 3 are #1324, #1325 and the sharing lint after them. where the
build settled a question this page left open, or departed from it:

- **error codes.** E271 is an access or a call without the lock. E272 is a
  `locked` or `holding` path that is not a path to a `Mutex`, or a `locked`
  body that changes its own path, since the unlock re-reads the path at each
  exit. E273 is a `locked` block on a path an enclosing block or the
  function's `holding` already holds. E274 is a guard block's shape. E309 is
  the sharing lint.
- **the mutex path.** `locked` and `holding` take a variable, `self`, a field
  read off one, or a list element indexed by a variable or a literal. a call
  result such as `locked pick_lock():` is refused, because the path is
  evaluated again to unlock.
- **guard blocks lower to plain declarations.** a module block becomes
  `m := Mutex()` and the globals; a struct block adds `m: Mutex = Mutex()` as
  the struct's last field, so a constructor call leaves it out. the emitter is
  unchanged.
- **guarded globals are private.** a `pub` global in a guard block is E274,
  since no other module can name the mutex. a sharded guard is for module
  globals only, and a generic struct cannot have a guard block.
- **`holding` on a list.** `holding shard_mu` on a `List[Mutex]` is satisfied by
  holding any element, which lets a whole-registry operation run under every
  shard: std/metrics.pith takes all eight by a recursion that nests eight
  `locked` blocks, in index order.
- **the sharing rule is an error.** E309 started as a lint and is now an
  error from `pith check`, `build`, `run` and `test`, which run it after the
  checker on a program that checked cleanly. `pith lint` still reports it.
- **"a program that spawns".** a program spawns when a function in the entry
  file holds a `spawn` or calls a function that does. a `spawn` merely present
  in an imported module does not count, so the compiler, which imports
  std.concurrent without spawning, is not flagged. calls are resolved by
  module: a plain name to the module's own function or a `from` import,
  `alias.f` through the import, and a method through its receiver's type. a
  method on a receiver whose type does not name the declaration, such as an
  interface, counts every method of that name. the lint first matched calls
  by bare name, and that flagged bench/catalog_server.pith, which never
  spawns, because std.net.http has a function with the same name as one of
  its own.
- **what counts as a write.** an assignment rooted at the global, or a mutating
  method called on it. a write through a function that takes the collection as
  an argument is not seen.
- **the TLS config registry.** it was fourteen handle-keyed maps behind
  generic accessors that took the lock themselves, which the checker could not
  follow. it is now one guarded map from handle to a `ConfigRecord`. a reader
  takes the record under the lock and uses it after; a setter copies the
  record, changes the copy and stores it under one lock, so a record is never
  changed after it is stored.
- **the HTTP/2 client.** `mu` and `gate` each guard a block of `Client`'s
  fields. the stream id counter and the peer's frame size limit are atomics
  instead, because the inline path and every request read them before taking a
  lock. guarding the rest found two reads made without the lock.
- **`Limiter` and `Breaker`.** they kept their state in atomics, but every
  access already took the struct's mutex, so the atomics bought nothing. the
  state is now plain fields in a `guard mu:` block. with 32 tasks sharing one
  limiter and one breaker, a call takes 16% less time. std.concurrent's test
  counters for the live and peak task counts moved the same way, and the
  counter it bumps without a lock stays an atomic.
- **parking under a lock.** E310 is a lint for a call that parks while a
  mutex is held: `await`, a `select` with no default arm, `sleep`, a channel
  `recv`, and the std file, tcp, tls and websocket reads. a channel send is
  left out, because the HTTP/2 client sends its frames under `mu` to keep
  stream ids in order. std, the examples and the compiler lint clean.
- **E248 and structs.** a struct passed to `spawn` is refused when one of
  its fields reaches a list, a map or a set outside a `guard` block. moving
  std under the rule put the http/2 hpack encoder and decoder under
  `hpack_mu` on both connection types, std.web's routes under the app's
  mutex (a connection takes them once, as it starts), and a request's header,
  query and path maps under `fields_mu`, along with the grpc channel's
  credentials, the stream reader's header blocks, the http/2 pool's client
  list and the terminal's options and output queue. std.obs builds its
  export target in the spawn's arguments instead.
- **frozen fields and handles that leave a lock.** a handle copied out of a
  `locked` block reached the buffer with no lock. E276 now refuses a guarded
  collection stored outside the block, returned from it or captured by a
  closure made in it, and allows a take that gives the field a new value.
  read-only data needed a way out, so a struct field can be declared `frozen`:
  set at construction, never changed through a path that runs through it
  (E275), read by any task with no lock, and not a shared buffer to E248.
  std.web's router, a request's header, query and path maps, the http/2
  pool's client list, the terminal's options, metrics series labels, the tls
  parsed-roots cache entries and the gateway's grpc targets moved from guards
  to frozen fields, which drops the per-request mutex E248 had added to
  requests. zstd's predefined tables are built at module init. a write through
  a copy of a frozen value, under another name, is not seen.
- **E248 at channel sends.** `send` and `try_send` are checked like a spawn's
  arguments, since the receiver gets the handle. the http/2 client and server
  handed stream events to a helper that sent its parameter; the helper now
  returns the inbox, and each caller builds the event in the send.
- **lock order.** E311 is a lint for two mutexes taken in opposite orders
  anywhere in the program. it records each mutex taken while another is held,
  directly or in a function called under the lock through a call the checker
  resolved, and reports a pair taken both ways. a mutex is named by its
  module global, by its struct type and field, or by its sharded guard, so
  two values of one struct are one name and nesting them is not reported.
  std, the examples and the compiler lint clean.
- **reentry through a call.** E312 is a lint for a call that takes a mutex
  its caller holds, which E273 cannot see because no `locked` block is
  nested. a module global is followed through the whole call chain; a
  struct's mutex is matched when a method on the same receiver locks that
  field on `self`.

## open questions

- **the words.** `guard`, `locked` and `holding` are the current choice.
  `locked` could be `with m:` or `hold m:`; it should not be `lock m:`, which
  reads as a statement that does not release.
- **guarded locals.** this design guards globals and struct fields. a local
  shared with a spawned task is already E248 when it is a collection, so a
  `guard` block inside a function body is left out until a case needs it.
- **the sharded form.** `guard shard_mu[8]:` fixes the count at compile time,
  which matches std/metrics.pith, std/trace.pith and the TLS connection
  shards. a shard count chosen at run time would need another form.
- **`RwMutex`.** nothing here needs a reader-writer lock, and std does not
  have one. if one is added, `guard` applies unchanged and `locked` would
  take a mode.
