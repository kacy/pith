# Concurrency guide

Pith's concurrency model is explicit and deliberately small.

- `spawn expr` starts work and gives you a `Task[T]`
- `await task` waits for the task and gives you `T`
- `Channel[T]()` is unbuffered
- `Channel[T](n)` is buffered
- `select:` lets you wait on channel send/recv, a timeout, or a default path
- `std.concurrent` gives you contexts, cancellation, deadlines, timers, and task/channel helpers

## Channels

Unbuffered channels are rendezvous channels. Buffered channels queue up to their capacity.

```pith
jobs := Channel[Int](1)
jobs.send(7)
value := jobs.recv()
print("{value}")
```

The basic channel surface is:

- `send(value) -> Bool`
- `recv() -> T?`
- `try_send(value) -> Bool`
- `try_recv() -> T?`
- `close() -> Bool`
- `is_closed() -> Bool`
- `len() -> Int`
- `cap() -> Int`

A send on a closed channel returns `false`. A recv on a closed and drained channel returns `none`.

`close()` does not free the channel, and it does not need to. A channel handle
is reference-counted like a string or a list, so the channel's memory (448
bytes plus 16 per buffered slot, allocated eagerly at construction) returns
when the last binding, field, element, or capture holding it is released, and
any values still queued are released with it. `PITH_PERF_STATS=1` reports
`new`, `closed`, `freed`, `retained_bytes` and `freed_bytes` on the
`channels:` line; docs/channel_ownership.md has the design.

## Select

`select` is an expression, so each arm needs to produce the same type.

```pith
picked := select:
    msg := jobs.recv() => msg.unwrap_or(0)
    timeout 50 => -1
    default => 0
```

Use `default` when you want a non-blocking probe. Use `timeout` when you want to wait for a bounded amount of time.

## Contexts

`std.concurrent` keeps cancellation and deadlines explicit.

```pith
import std.concurrent as concurrent

pair := concurrent.with_timeout(concurrent.background(), 250)
ctx := pair.0
token := pair.1
```

Available helpers:

- `background()`
- `with_cancel(parent)`
- `with_timeout(parent, ms)`
- `with_deadline(parent, at_ms)`
- `after(ms)`
- `ticker(ms)`
- `await_ctx(task, ctx)`
- `send_ctx(ch, ctx, value)`
- `recv_ctx(ch, ctx)`

A deadline that has passed is cancelled, and `is_cancelled()` says so the
moment you ask. It does not wait for a background task to get a turn, so a busy
machine cannot show you a live context whose deadline has already gone by.
`error_code()` agrees with it: `2` for a deadline, `1` for a manual cancel, and
a cancel that lands before the deadline keeps the cancel as the reason.

Closing the done channel is still an event a task performs, so
`ctx.done().recv()` wakes when the watcher closes it rather than at the instant
the deadline passes. Code that needs the deadline enforced exactly should ask
`is_cancelled()`, which every `*_ctx` helper already does.

TCP stream waits can use contexts the same way through `std.io`:

- `TcpStream.read_ctx(ctx, max_bytes)`
- `TcpStream.read_all_ctx(ctx)`
- `TcpStream.read_bytes_ctx(ctx, max_bytes)`
- `TcpStream.write_ctx(ctx, data)`
- `TcpStream.write_all_ctx(ctx, data)`
- `BufferedReader.read_ctx(ctx, max_bytes)`, from `stream.buffered_reader()`
- `BufferedReader.read_line_ctx(ctx)`
- `BufferedWriter.write_ctx(ctx, data)`, from `stream.buffered_writer()`
- `BufferedWriter.flush_ctx(ctx)`

Process stdio can use the same pattern:

- `ProcessStdout.read_ctx(ctx, max_bytes)`
- `ProcessStdout.read_all_ctx(ctx)`
- `ProcessStderr.read_ctx(ctx, max_bytes)`
- `ProcessStderr.read_all_ctx(ctx)`
- `ProcessStdin.write_ctx(ctx, data)`
- `ProcessStdin.write_all_ctx(ctx, data)`
- `BufferedReader.read_ctx(ctx, max_bytes)` and `read_line_ctx(ctx)`, from
  `stdout.buffered()` or `stderr.buffered()`

Context cancellation is cooperative. Cancelling a context stops the wait but
does not stop the task itself.

## Tasks

Tasks stay simple:

- `await task`
- `task.is_done()`
- `task.detach()`

`await` consumes the task. The first `await` of a handle gets the result, and any other `await` of the same handle gets a zero result (`0` for an `Int`, an empty `String`), whether it runs after the first or at the same time in another task. Both backends behave this way, so a task's result is handed out once.

`await_ctx(task, ctx)` returns `T!WaitError`. If the context is cancelled or reaches its deadline, the wait stops and returns an error. The task can keep running unless the task body is also checking a cancelled context.

If you do not plan to join a task later, call `detach()`.

A handle bound to a local is detached for you when the function leaves it unawaited: at a return, at a failure, or when the next `spawn` is stored in the same name, so an early exit that skips the await still lets the task's record go. This applies only to a local that is just awaited, polled with `is_done()` or detached. A handle that is passed on, returned, stored or copied belongs to whoever holds it, and awaiting or detaching it is up to them.

## Timers

`after(ms)` gives you a channel that fires once.

`ticker(ms)` gives you a ticker with:

- `ticker.channel()`
- `ticker.stop()`

The ticker channel uses best-effort delivery. If you stop reading from it, ticks can be dropped instead of building an unbounded backlog.

## Current boundaries

The current concurrency support is enough for:

- Fan-out and fan-in with channels
- Timeout and cancellation around task waits
- Bounded channel coordination with `select`
- Process timeout helpers through `std.os.process`
- Shared mutable state behind a `Mutex`, `AtomicInt`, or `Semaphore`
- Fan-out that fails as a unit, with `concurrent.group`

## Closing a connection another task is using

A socket or a child's pipe is an `Int` in the language, but the number is a
handle rather than the descriptor: the descriptor's number stamped with a
generation that changes every time the number is opened, the same shape a
task handle has. The runtime keeps one word per descriptor number holding
that generation, whether the handle is open, and how many calls are inside
on it. Every fd call (read, write, accept, wait, set_timeout, shutdown,
close) resolves the handle against that word first and fails with an
ordinary error when it is stale. It also holds the word for the length of
its syscall, parks included, so the number cannot be closed and handed to
another connection while the call is in flight.

That makes closing from another task safe. `tcp.close` on a handle nobody is
inside on closes the descriptor at once. On one with a call still inside, it
marks the handle dead, shuts the socket down in both directions so a call
blocked in the kernel returns, wakes every task parked on it in the reactor,
and leaves the `close(2)` itself to the last call out. The interrupted call
reports an error on both backends, whether it was a read, a write, an accept
on a listener, or a `wait_readable`, and every call after it fails the same
way. A second `close` of the same handle is a no-op. A number the kernel
reissues arrives as a different handle, so a closed `Int` kept around by
mistake can never name the connection that took its number.

The one thing a close cannot hurry is a read on a child's pipe blocked in
the kernel under `PITH_GREEN=0`. A pipe has no `shutdown`, so that read
returns when the child writes or exits, and the descriptor is closed then;
the green backend wakes it through the reactor immediately. Either way, the
handle is dead to every other call from the moment of the close.

The `tcp_shutdown` builtin is still the way to stop a loop without taking
its socket away: the accept loop or reader keeps its handle, sees its call
fail, and closes on its own way out. `std.shutdown` does this with a
registered listener and the HTTP/2 client does it with its reader task. It
is a choice about where the close lives; safety does not require it.

## Sharing between tasks

A spawned task does run alongside its parent, whether the backend gives it
an OS thread of its own or a slice of a green worker. Reference counts are
atomic, so handing a value to another task and letting both hold a count is
safe. Two tasks mutating the same collection at once is *not* safe: a list
or map is a plain buffer behind a handle, and concurrent mutation races on
that buffer.

Pass data between tasks through a channel rather than sharing a
mutable collection. A channel hands over the handle, so the sender must not
keep one: send a value built in the call, a local it does not use again, or
an independent copy (`std.collections.copy_list` and friends). The checker
enforces this for a `spawn` argument and for a channel `send` or `try_send`
(E248). A value crosses when nothing else holds what it holds: it is built
from fresh parts, or it is a local that nothing else holds and that the scope
does not use again after the handoff (a use in another branch of the same
`if`, or after a `return`, does not count). A call result counts as fresh
only when the function builds it from fresh parts: one that returns its
parameter, or builds a struct around it, hands back the argument's buffer. A
module global collection passed to `spawn` or sent is refused as well, since
the program still holds it: a task reads the global directly. Writing a
global from several tasks is covered by the sharing rule (E309).

`std.bytes.ByteBuffer` and `ByteArray` count as collections here, although
their handle is an `Int`. Neither can become an interface value, which would
hide the buffer from these checks (E248); `io.memory_writer()` is the
in-memory `BytesWriter` that several tasks may share, since its bytes sit
behind a lock.

A spawn evaluates its arguments on the new task. So `spawn total(copy_list(xs))`
copies `xs` there, while this task may still be pushing to it, and it is
refused unless the scope does not use `xs` again. Make the copy first and
hand it over: `own := copy_list(xs)` then `spawn total(own)`.

A struct field that holds a string, a struct, an optional or another counted
value is one pointer, and a task that reassigns it frees the old value while
another task may be reading it. So a struct handed to a task is also refused
when it reaches such a field and some code in the program reassigns that
field after the struct is built (E248). A field no code reassigns is settled,
and every task may read it. A field in a `guard` block is written under its
lock. A number or a flag is exempt, since a race on one frees nothing. Most
shared structs need no change, because a config, a client or a connection is
built once with every field in the constructor call and keeps its changing
parts in a `guard` block. The ones to restructure set a field from a builder
after the struct is built, or write it under a lock taken by hand.

A closure captures the values it uses by handle, so a closure that runs on
another task shares what it captured with the scope that made it. The checker
follows each function value to the places it can run: a spawn's argument,
callee or receiver, a sent value, a module global, a field of a struct that
crosses to a task, a capture of a closure that crosses, the result of a
spawned call, and any parameter, field or result that flows into one of
those, across the whole program. A closure made in a generic function is
judged by the types its captures take in each specialization. A
closure that can reach another task must capture only what a spawn argument
could carry: no collection outside a `guard` block, and no struct with such a
collection or with a counted field some code reassigns (E248). A request
handler is the common case: `std.web`, `std.net.http`, `std.net.http2` and
`std.net.grpc` run a handler on the task serving each connection, so a
handler that captures a list the program keeps pushing to is refused. Keep
that state in a `guard` block, or in a `frozen` field if it is built once.
A closure that stays on the task that made it, such as one passed to `map`,
may capture anything.

When a channel is the wrong shape (a registry every request reads and a
few requests write, a counter, a cap on how many things run at once), use
a lock. The primitives below are built in: they need no import, and
they are what the standard library itself uses.

## Locks, counters, and permits

These four are language builtins, available in any Pith program without an
import. `std.log`, `std.metrics`, `std.trace`, `std.io`, `std.uuid`,
`std.net.tls`, and both database drivers are built on them.

Each of the four is a counted handle, like a channel. A copy in a binding,
a field, a list or a capture refers to the same primitive, and its memory is
freed when the last holder is released. A task started with `spawn` holds
its own count on each one it receives, so it can keep using it after the
function that created it returns.

`Mutex()` guards a section that must not run twice at once, and a `locked`
block holds it for the length of the block:

```pith
mut rooms: Map[String, Room] := {}
mut rooms_mu := Mutex()

fn join(name: String, member: String):
    locked rooms_mu:
        room := rooms.get(name).unwrap_or(Room(name, []))
        room.members.push(member)
        rooms[name] = room
```

The block takes the mutex on entry and releases it on every way out of the
block: the end of the block, `return`, `fail`, a `!` that propagates, `break`
and `continue`. The release runs after the block's own `defer`s. The mutex
must be a variable, a field, or a list element indexed by a variable or a
literal, and the block may not change it (E272). A `locked` block inside
another one on the same mutex is a deadlock and a compile error (E273).

A `guard` block ties the mutex to the data it protects, and the checker
then refuses any access to that data without the lock (E271):

```pith
guard rooms_mu:
    mut rooms: Map[String, Room] := {}

fn join(name: String, member: String):
    locked rooms_mu:
        add_member(name, member)

fn add_member(name: String, member: String) holding rooms_mu:
    room := rooms.get(name).unwrap_or(Room(name, []))
    room.members.push(member)
    rooms[name] = room
```

The block declares `rooms_mu` as a `Mutex` and `rooms` as an ordinary module
global, private to the module. A function marked `holding rooms_mu` may touch
`rooms`, and every call to it must hold the mutex. In a struct, `guard pick:`
adds a `pick` mutex field after the others, defaulting to a new one, so a
constructor call leaves it out; the fields under it are reached through
`locked p.pick:`, or `locked self.pick:` in a method.

In a program that spawns tasks, a `mut` global that is written after module
init and holds a collection, a struct or another counted value (a string, an
optional, a function value) must be in a `guard` block or `threadlocal`, and
the compiler refuses one that is neither (E309). A global whose struct fields
some code reassigns is refused the same way, `mut` or not, and so is a global
collection, `mut` or not, that some code changes in place through any name:
its own, a local it was copied into, or a handle a function returns. A global
that is only read needs no lock.

A value read inside `locked` stays there: storing a guarded collection in
a variable declared outside the block, returning it, or capturing it in a
closure is refused (E276), since the handle would reach the buffer after the
unlock. So is storing it into a container that outlives the block: a push,
an insert or a field store into a list, map or struct declared outside the
block, a parameter or a global, whether the value is stored whole or inside
a tuple, a struct or an optional. A call is judged by what its callee does,
followed through generic functions and interface methods: passing the value
to a function that stores it into another argument outliving the block, or
lets it escape, is refused, and so is passing it to a function value, which
may keep it. A call that only reads its argument (`len`, a loop over it,
indexing, a function that keeps nothing) stays allowed. So is a guarded
struct with a counted field that some code reassigns.
A name bound inside the block to a guarded value counts as the guarded value
for the rest of the block: a local (`list := self.idle[key]`), a loop
variable over it, an `if let` binding of it, the result of `get` or `unwrap`
on it, the result of a function that returns its argument, and any value
built around it. Moving it out and giving the field a new value, as a drain
does, is allowed, as is reading an element and removing it in the same block,
and so is storing it back under the same guard.

The other way round, a value stored into a guarded collection must not stay
reachable outside the lock (E276). It is built in the call, or a local the
scope does not use again after the store. A pool's `release(conn)` keeps what
it is handed, so its callers hand over a connection they are done with; one
that keeps using it is refused. Data that is built once and only read after
belongs in a `frozen` field (see docs/ownership.md), which any task reads with
no lock.

`guard shard_mu[8]:` declares a list of eight mutexes, for state split into
shards. Holding any element of the list reaches the data under it, since the
checker does not track which index is which.

`rooms_mu.lock()` and `rooms_mu.unlock()` are still there for a lock that
has to be released partway through or held across a call boundary. `locked`
is a keyword only at the head of a block, so a variable named `locked` still
works.

`examples/room_registry.pith` works through this: a registry several
tasks read and write, a counter, a capped flush, and a group over the rooms.

Hold a mutex for the shortest span that keeps the data consistent, and never
across a channel receive or a socket read: a task parked inside a lock
holds it for as long as it is parked. `pith lint` reports a call that parks
while a mutex is held (E310), two mutexes taken in opposite orders in
different places (E311), and a call that takes a mutex the caller already
holds (E312). There is no reentrancy: locking a
mutex a task already holds deadlocks it.

`AtomicInt(n)` is a single integer several tasks can touch without a lock:

- `counter.load()`
- `counter.store(n)`
- `counter.fetch_add(delta)` atomically adds `delta` and returns the previous value
- `counter.fetch_sub(delta)` atomically subtracts `delta` and returns the previous value
- `counter.compare_set(expected, new)` sets and returns `true` only if the
  value was `expected`, which is how you elect exactly one winner among racing
  tasks. `std.concurrent` uses it so a deadline and a manual cancel settle on
  one reason rather than both writing.

`load` then `store` is *two* operations and races between them;
`counter.store(counter.load() + 1)` can lose an increment. Use
`compare_set` in a retry loop when the new value depends on the old one.

`WaitGroup()` waits for a set of tasks without holding their handles:

- `wg.add(n)` before starting them
- `wg.done()` from each as it finishes
- `wg.wait()` to block until the count reaches zero

If you do hold the task handles, `await` each one instead. It is simpler
and gives you their results. A wait group is worth using when the tasks
are started somewhere that cannot keep the handles.

`Semaphore(n)` caps how many tasks may be inside a region at once:
`sem.acquire()` takes a permit and waits if none is free, `sem.release()`
returns one. `std.prometheus` uses one to bound concurrent scrapes, and
`std.net.http2.server` uses one to bound concurrent connections.

Use a semaphore when the caller should wait, and an `AtomicInt` in a
`compare_set` loop when it must not. Waiting is all a semaphore adds over a
counter, and there is no non-blocking way to take a permit. That is the
choice `std.web`'s accept loops make: they cap connections on a counter and
refuse past it, because a loop parked on a permit is a server that has
stopped answering, health check included.

`Mutex`, `Semaphore` and `WaitGroup` park rather than spin, so a green task
blocked on any of them frees its worker for other tasks instead of holding it.
`AtomicInt` is the exception and has no blocking operation at all. It is a
lock-free load, store and compare-and-set, which is why it is the tool for
the path that must not wait.

## Groups

A group fans work out and fails as a unit. It is the shape you want for
"do this for every room, and if any of them fails, stop":

```pith
import std.concurrent as concurrent

fn flush_all(rooms: List[Room]) -> Int!:
    g := concurrent.group(concurrent.background())
    for room in rooms:
        g.go(fn() => room.flush())
    return g.wait()!
```

`group(parent)` derives a child context that the first failure cancels, so
siblings can notice and stop early. Pass `g.ctx` into work that runs long
enough to care. `group_limited(parent, n)` does the same with at most `n`
units running at once, for when the fan-out is wider than the resource
behind it.

`wait()` returns the first error and waits for *every* task regardless, so
when it returns nothing from the group is still running. Work that never
consults the context still runs to completion: cancelling asks a task to
stop but does not kill it.

The work is `fn() -> Int!`, because a group cares which unit failed and not
what the survivors returned. Work with a value to report should write it to a
channel or a slot it owns, the same as it would without a group.

## Per-thread globals

A module global marked `threadlocal` gets a separate copy per task,
created lazily the first time that task reads it: one per OS thread on the
OS-thread backend, one per green task on the green one. Each task mutates its
own copy with no lock and no race, which suits per-task scratch state such as
a parser's cursor, a buffer, or a request-scoped counter:

```
threadlocal mut counter := 0
threadlocal mut arena: Map[Int, Node] := {}
```

It reads and writes like any other global; only the storage is
per-task. When a task finishes (or its thread, on the OS-thread backend),
the values its copies hold are released, so a server that spawns a task per
connection does not strand one set of copies per connection. Main's copies
live as long as the process. Use `threadlocal` for state that is genuinely
per-task; a value shared *across* tasks still belongs in a struct or behind
a channel.

This is what lets the std parsers and buffers be used from many tasks at
once: `std.json` and `std.toml` keep their parse arena in `threadlocal`
globals, and `std.io`'s readers/writers keep their per-instance state the
same way, so two tasks parsing or buffering concurrently each work in
their own state with no lock and no race.

Things that are still intentionally explicit or still growing:

- Task cancellation is cooperative, not forceful.
- Plain file I/O still does not have `_ctx` variants.
- Mutable collections are not thread-safe and the compiler refuses sharing
  them across tasks (`E248`, `E276`, `E309`); use channels, `frozen` fields,
  or `guard` blocks.

## The green backend

On OS threads each `spawn` runs on its own kernel thread, so every channel
send, mutex, or await that has to wait hands off through the kernel: a futex
wake and a context switch. That is fine when tasks mostly compute, but a
program built out of many small tasks that pass values to each other pays a
kernel switch on every hop.

The green backend addresses that, and on Linux it is what you get
unless you ask otherwise. Tasks become coroutines that run on a small pool of
worker threads (`available_parallelism()` by default, `PITH_GREEN_WORKERS=n`
to pin the count). Channel, mutex, waitgroup, semaphore, and await operations
that would block yield to the scheduler instead of parking their worker, so a
handoff between two tasks on the same worker is a userspace switch with no
kernel involved. Socket I/O goes through epoll, with one poller per worker: a
read or write that would block registers the socket with the poller of the
worker it runs on, parks the task, and frees the worker to run something else,
so the whole net stack (raw TCP, TLS, HTTP/2, gRPC) yields the same way
without any code of its own. A worker that runs out of work parks in its own
`epoll_wait`, so a socket becoming ready wakes the worker that owns the task
directly, with no reactor thread in between. Sleeping parks there too: a
`time.delay` from inside a task registers a timer on its worker's deadline
heap instead of blocking the worker, so a task sleeping out a backoff, or a
`select` idling between probes, costs nothing but its own time. Epoll is also
why the default is Linux-only; see "Which backend to use".

Where a task runs matters because the two cases cost very differently. When
two tasks that talk to each other share a worker, each handoff is the
userspace switch above; split across workers, each one is a park and a futex
wake. A two-task ping-pong measured ~0.2us per round colocated and ~19us
split.

Workers therefore look for work for a short while before parking, so a handoff
already in flight is caught without a kernel round trip. Only a worker that
ran something recently pays for that spin: once a worker has idled long enough
to back its park timeout off it parks directly, and an idle pool stays quiet.

Two rules decide placement. A worker that wakes a task parked on another
worker takes it, so the woken task resumes where its waker is, and a pair that
talk to each other converge onto one worker after their first exchange. A mutex
unlock is the exception and leaves the task it wakes where it was, because that
task only wants the lock. A task
that never waits on anyone is never moved. That rule alone would pull parallel
work onto one worker whenever it synchronizes, so there is a second one: when
a worker has woken tasks queued and has stayed inside one task for 20us, an
idle worker takes the oldest queued task and becomes its owner. The first rule
packs a pipeline onto one worker, and the second spreads work back out when
that worker is busy with something long, as when eight compute tasks report to
one collector between chunks (`bench/cpu_parallel_sync`).

On 16 workers (2026-10-28, medians of 7), the channel fan-out benchmark went
from a median of 97 ms, landing anywhere between 70 and 130 ms, to a flat
38 ms, the same as on one worker. The ping-pong went from either 400 ms or
2.2-2.9 s, depending on where its two tasks happened to start, to a steady
400 ms, and `cpu_parallel_sync` went from 921 to 658 ms. `PITH_GREEN_MIGRATE=0` turns the
first rule off, so a task stays on the first worker that runs it. Moving a
task is safe because the runtime is built so that a suspended task carries
nothing tied to the thread it parked on (`make check-tls-barriers` checks this
on every build).

A read deadline survives that translation. `tcp.set_timeout` stores the deadline
on the socket itself, and the reactor wait is bounded by it: a read that reaches
the deadline with nothing to show for it fails, which is exactly what happens on
OS threads, where the kernel enforces the same stored value on the blocking read.
That is what stops a client that connects and never speaks from holding a task
forever, and every server-side bound in the stdlib rests on it:
`std.web`'s connection timeout, the HTTP/2 server's, the TLS handshake deadline,
and `std.prometheus`'s scrape timeout. A socket with no deadline set, which is
most of them, waits indefinitely as it always has, and a child process's pipe
has no such deadline to set at all.

Name resolution and file I/O get there by a different route. `getaddrinfo` is
synchronous and there is nothing to poll, and a regular file is always reported
ready by epoll however slow the disk behind it is, so neither can yield to the
reactor. Instead the call goes to a small pool of ordinary blocking threads
(four at most, started on demand, one pool for lookups and one for files) and
the task parks until a pool thread answers. The worker stays free for the whole
call, so a dial no longer has a blocking step sitting in front of its
non-blocking one, and a task that writes a log line no longer stops every other
task on its worker while it does.

Child processes take neither route. A pipe is pollable where a regular file is
not, and Linux hands out a `pidfd` that becomes readable exactly when a child
exits, so `Process.wait`, `ProcessStdout.read` and their siblings park on the
reactor the way a socket does. No thread is held per child either, which
matters more here than anywhere else: a wait has no bound at all, and four
`sleep 3600`s would have emptied a four-thread pool. A child that has already
finished by the time you wait for it is collected on the spot, with no park.

`process.output`, along with everything routed through it (`run`, `text`,
`output_checked`) and the two shell helpers `run_shell` and `output_shell`,
runs a child to completion inside one call while draining both of its pipes,
which the reactor cannot cover (it waits on one fd at a time). Those calls go
to a small process pool of their own instead, the DNS/file shape: the command
crosses to a blocking thread, the task parks, and the worker stays free for
however long the child runs. `exec` and `exec_output` take the same route.
`start` plus your own reads and `wait` still parks on the reactor directly,
with no pool thread involved.

A task waits a few microseconds on-CPU before it actually parks. A read the page
cache answers comes back faster than the two thread wakeups a park costs, so
waiting for it beats suspending, and the wait is bounded at roughly what the
call would have held the worker for had it run inline. Anything slower falls
through to the park and frees the worker, which is the case the pool exists for.

Nothing in your program changes. The same `spawn`, `Channel`, `Mutex`, and
`await` run on either backend, and a correct program prints the same thing
both ways. `PITH_GREEN` picks one explicitly: `1`, `on`, or `true` forces
green, `0`, `off`, or `false` forces OS threads, and anything else, including
leaving it unset, takes the platform default, which is green on Linux and OS
threads everywhere else. Examples like `worker_pool.pith` run identically
whichever way you set it:

```
pith run examples/worker_pool.pith
PITH_GREEN=0 pith run examples/worker_pool.pith
```

It helps most when tasks coordinate a lot and compute little: a fan-out of
short jobs over channels, or a network client whose reader, writer, and
worker tasks trade a request back and forth per call. On that last shape the
green backend cut internal context switches from about five per gRPC call to
well under one and raised throughput on a two-core box (see the gRPC section
of `docs/performance.md`). On the channel fan-out benchmark, eight tasks
trading a million messages through one bounded channel, it runs well ahead of
the OS-thread backend, and at 40 ms median it is ahead of Go (49 ms) on the
2026-11-12 suite run. See the headline table in `docs/performance.md`. It does
the least for tasks that are already CPU-bound and rarely wait, since those
never hit the handoffs green makes cheap.

The machinery behind that: coroutine stacks are pooled and reused rather
than mapped and unmapped per task, so a fan-out of short tasks costs
allocations, not TLB shootdowns; each task's scheduling state lives in one
atomic word, so waking a task or resuming it takes no scheduler lock beyond
one run queue's; and a channel wake only signals a condvar when an OS-thread
waiter is actually parked in it, because green waiters suspend their
coroutine instead and Rust's condvar pays a futex syscall per notify
regardless of waiters. `PITH_GREEN_STATS=1` prints the wake breakdown,
contended-lock counts, and hot-path counters at exit if you want to see
where a workload's handoffs actually go.

Each green task runs on a 1 MiB stack that does not grow. The size is
reserved address space, and memory backs only the pages a task touches.
Recursion that runs past the end of the stack hits its guard page, and the
runtime stops the process with `pith runtime error: stack overflow in task N`
and exit status 2. An overflow on the main thread or in an OS-thread task is
reported the same way. `PITH_GREEN_STACK_KB=n` sets the stack size of every
task in KiB, clamped to the range 64 to 1048576 (1 GiB). The runtime reads it
once, when it makes the first stack.

It also keeps memory flat under fan-out. When a task finishes, the green
backend reclaims its slot and releases the closure it was spawned with, so a
server that spawns one task per request holds only the tasks running at once,
not one record for every request it has ever served. This holds even for a
fire-and-forget `spawn f()` whose handle you never keep: a discarded spawn is
detached at the spawn site, and a handle a function leaves unawaited in a
local is detached when the function returns, so its slot is reclaimed when the
body finishes rather than pinned until the process exits. A fan-out of 500k
short tasks holds around 3 MB, flat no matter how many run in total; see the
green fan-out row in `docs/performance.md`. (The OS-thread backend reclaims
finished tasks on `await` or `detach` under the same policy.)

### Which backend to use

On Linux, use the one you already have. Green is faster on every shape this
repo measures, so it is the default there and you only reach for
`PITH_GREEN=0` if one of the caveats below is your workload. Everywhere else
the default is OS threads, and `PITH_GREEN=1` opts in.

The reason for that split is the reactor. It is epoll and eventfd, so it is
Linux-only; macOS and the BSDs compile a stand-in with no reactor at all, and a
green task waiting on a socket there, or sleeping, blocks its worker for as
long as the wait lasts. Green is still correct on those platforms and you can
turn it on, but an
I/O-heavy server on macOS wants OS threads until there is a kqueue sibling.

The caveats that come with the Linux default all come from one fact: a green
worker runs many tasks, so anything that holds the worker holds all of them,
where an OS thread would only cost the one task making the call.

File metadata is where the last of that lives. Opening, reading, writing,
appending, listing a directory and removing a tree all go to the file pool now,
but `exists`, `size`, `rename`, `mkdir` and removing a single file still call
the kernel directly on the worker. Each of those is one lookup that the kernel
answers out of its cache, which costs about what handing it to another thread
would; on a slow network mount it is not, and a task doing one there holds its
worker until it returns. `PITH_GREEN=0` is still the answer if that bites you.

The pool has a cost too. A file call made from inside a task now costs a
thread handoff that it did not before, so a task that reads a small cached
file in a tight loop is several times slower than it was: the run finishes
sooner for everything else on that worker and later for itself. Calls made
from `main`, which is not a green task, take the direct path and are
unaffected.

Preemption is the other thing the kernel used to hand you, and green builds now
emit it: a compute-only task that never touches a channel, await, or socket is
descheduled at a loop back-edge instead of holding its worker until it finishes.
`PITH_GREEN_PREEMPT=0` at build time takes the check back out (below). Code that
coordinates already yields on its own and never needs any of it.

Placement follows the two rules in "The green backend" above: a woken
task moves to the worker that woke it, unless a mutex unlock woke it, and an
idle worker takes queued tasks
off a worker stuck in a long one. `PITH_GREEN_WORKERS=1` takes the choice away
entirely, and `PITH_GREEN_MIGRATE=0` keeps each task on the first worker that
runs it.

There is one more reason the OS-thread backend stays, which matters to this
repo rather than to your program: it is the reference green gets checked
against. `make verify-green-corpus` runs the whole regression corpus under
green and diffs it against the fixed OS-thread answers, which is how a
single-worker deadline bug turned up. Two implementations that have to agree
are worth keeping around past the point where one of them is faster.

The rest of the rough edges:

- A compute-bound task that loops without ever touching a channel, await, or
  socket is preempted, and you get that without asking. The backend puts a
  safe-point at every loop back-edge and a monitor thread makes a task that has
  held its worker past its time slice yield, so its peers on the same worker get
  to run. The check costs between +0.07% and +1.85% instructions on the
  benchmarks in `docs/performance.md` and about +2.5% wall on the compiler
  compiling itself; a flat arithmetic loop is the outlier and runs about twice as
  long, because six instructions of check land on a seven-instruction body and
  the call inside them costs the loop its registers. `PITH_GREEN_PREEMPT=0` at build
  time emits no check at all, which is the right setting for a build that will
  never run green or for a numeric kernel that would rather have the 50% back.
  Code that uses channels and sockets already yields on its own and never needs
  this. One gap: a task sitting inside a long native runtime call (a large file
  read, say) is not preempted until it returns to Pith code and hits the next
  back-edge.
- Fewer workers means more locality. At `PITH_GREEN_WORKERS=1` a whole
  pipeline shares one worker with no cross-thread wakes at all, which is often
  the fastest setting for a single connection even though it uses one core;
  more workers only pay off when the work spreads across connections or cores.
  Since a woken task moves to its waker's worker, a coordinated pipeline
  converges onto one worker on its own, and the channel fan-out benchmark runs
  in about the same 37 ms at 16 workers as at one.
