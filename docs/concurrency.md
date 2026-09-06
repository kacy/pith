# concurrency guide

pith's concurrency model is explicit and pretty small on purpose.

- `spawn expr` starts work and gives you a `Task[T]`
- `await task` waits for the task and gives you `T`
- `Channel[T]()` is unbuffered
- `Channel[T](n)` is buffered
- `select:` lets you wait on channel send/recv, a timeout, or a default path
- `std.concurrent` gives you contexts, cancellation, deadlines, timers, and task/channel helpers

## channels

unbuffered channels are rendezvous channels. buffered channels queue up to their capacity.

```pith
jobs := Channel[Int](1)
jobs.send(7)
value := jobs.recv()
print("{value?}")
```

the basic channel surface is:

- `send(value) -> Bool`
- `recv() -> T?`
- `try_send(value) -> Bool`
- `try_recv() -> T?`
- `close() -> Bool`
- `is_closed() -> Bool`
- `len() -> Int`
- `cap() -> Int`

send on a closed channel returns `false`. recv on a closed and drained channel returns `none`.

`close()` is not a free, and it does not need to be one: a channel handle is
reference-counted like a string or a list, so the channel's memory — about 400
bytes plus 16 per buffered slot, allocated eagerly at construction — returns
when the last binding, field, element, or capture holding it is released, and
any values still queued are released with it. `PITH_PERF_STATS=1` reports
`new`, `freed`, and `use_after_zero` on the `channels:` line;
docs/channel_ownership.md has the history and the design.

## select

`select` is an expression, so each arm needs to produce the same type.

```pith
picked := select:
    msg := jobs.recv() => msg?
    timeout 50 => -1
    default => 0
```

use `default` when you want a non-blocking probe. use `timeout` when you want to wait for a bounded amount of time.

## contexts

`std.concurrent` keeps cancellation and deadlines explicit.

```pith
import std.concurrent as concurrent

pair := concurrent.with_timeout(concurrent.background(), 250)
ctx := pair.0
token := pair.1
```

available helpers:

- `background()`
- `with_cancel(parent)`
- `with_timeout(parent, ms)`
- `with_deadline(parent, at_ms)`
- `after(ms)`
- `ticker(ms)`
- `await_ctx(task, ctx)`
- `send_ctx(ch, ctx, value)`
- `recv_ctx(ch, ctx)`

a deadline that has passed is cancelled, and `is_cancelled()` says so the
moment you ask. it does not wait for a background task to get a turn, so a busy
machine cannot show you a live context whose deadline has already gone by.
`error_code()` agrees with it: `2` for a deadline, `1` for a manual cancel, and
a cancel that lands before the deadline keeps the cancel as the reason.

closing the done channel is still an event a task performs, so
`ctx.done().recv()` wakes when the watcher closes it rather than at the instant
the deadline passes. code that needs the deadline enforced exactly should ask
`is_cancelled()`, which every `*_ctx` helper already does.

tcp stream waits can use the same context story through `std.io`:

- `TcpStream.read_ctx(ctx, max_bytes)`
- `TcpStream.read_all_ctx(ctx)`
- `TcpStream.read_bytes_ctx(ctx, max_bytes)`
- `TcpStream.write_ctx(ctx, data)`
- `TcpStream.write_all_ctx(ctx, data)`
- `BufferedTcpStream.read_ctx(ctx, max_bytes)`
- `BufferedTcpStream.read_line_ctx(ctx)`
- `BufferedTcpWriter.write_ctx(ctx, data)`
- `BufferedTcpWriter.flush_ctx(ctx)`

process stdio can use the same pattern too:

- `ProcessStdout.read_ctx(ctx, max_bytes)`
- `ProcessStdout.read_all_ctx(ctx)`
- `ProcessStderr.read_ctx(ctx, max_bytes)`
- `ProcessStderr.read_all_ctx(ctx)`
- `ProcessStdin.write_ctx(ctx, data)`
- `ProcessStdin.write_all_ctx(ctx, data)`
- `BufferedProcessStdout.read_ctx(ctx, max_bytes)`
- `BufferedProcessStdout.read_line_ctx(ctx)`
- `BufferedProcessStderr.read_ctx(ctx, max_bytes)`
- `BufferedProcessStderr.read_line_ctx(ctx)`

context cancellation is cooperative. cancelling a context stops the wait, not the task itself.

## tasks

tasks stay simple:

- `await task`
- `task.is_done()`
- `task.detach()`

`await_ctx(task, ctx)` returns `T!WaitError`. if the context is cancelled or reaches its deadline, the wait stops and returns an error. the task can keep running unless the task body is also checking a cancelled context.

if you intentionally do not plan to join a task later, call `detach()`.

## timers

`after(ms)` gives you a channel that fires once.

`ticker(ms)` gives you a ticker with:

- `ticker.channel()`
- `ticker.stop()`

the ticker channel uses best-effort delivery. if you stop reading from it, ticks can be dropped instead of building unbounded backlog.

## current boundaries

the current concurrency story is strong enough for:

- fan-out and fan-in with channels
- timeout and cancellation around task waits
- bounded channel coordination with `select`
- process timeout helpers through `std.os.process`
- shared mutable state behind a `Mutex`, `AtomicInt`, or `Semaphore`
- fan-out that fails as a unit, with `concurrent.group`

## closing a connection another task is using

a socket or a child's pipe is an `Int` in the language, but the number is a
handle rather than the descriptor: the descriptor's number stamped with a
generation that changes every time the number is opened, the same shape a
task handle has. the runtime keeps one word per descriptor number holding
that generation, whether the handle is open, and how many calls are inside
on it. every fd call (read, write, accept, wait, set_timeout, shutdown,
close) resolves the handle against that word first and fails with an
ordinary error when it is stale. it also holds the word for the length of
its syscall, parks included, so the number cannot be closed and handed to
another connection while the call is in flight.

that makes closing from another task safe. `tcp.close` on a handle nobody is
inside on closes the descriptor at once. on one with a call still inside, it
marks the handle dead, shuts the socket down in both directions so a call
blocked in the kernel returns, wakes every task parked on it in the reactor,
and leaves the `close(2)` itself to the last call out. the interrupted call
reports an error on both backends, whether it was a read, a write, an accept
on a listener, or a `wait_readable`, and every call after it fails the same
way. a second `close` of the same handle is a no-op. a number the kernel
reissues arrives as a different handle, so a closed `Int` kept around by
mistake can never name the connection that took its number.

the one thing a close cannot hurry is a read on a child's pipe blocked in
the kernel under `PITH_GREEN=0`. a pipe has no `shutdown`, so that read
returns when the child writes or exits, and the descriptor is closed then;
the green backend wakes it through the reactor immediately. the handle is
dead to every other call from the moment of the close either way.

the `tcp_shutdown` builtin is still the way to stop a loop without taking
its socket away: the accept loop or reader keeps its handle, sees its call
fail, and closes on its own way out. `std.shutdown` does this with a
registered listener and the http/2 client does it with its reader task. it
is a choice about where the close lives, not a safety requirement.

## sharing between tasks

a spawned task runs for real alongside its parent, whether the backend
gives it an os thread of its own or a slice of a green worker. reference
counts are atomic, so handing a value to another task and letting both
hold a count is safe. what is *not* safe is two tasks mutating the same
collection at once — a list or map is a plain buffer behind a handle, and
concurrent mutation races on that buffer.

pass data between tasks through a channel rather than sharing a
mutable collection. a channel hands the value over instead of aliasing
it, so each task mutates its own. immutable values and independent
copies (`std.collections.copy_list` and friends) are also safe to
hand off. this rule is convention today, not yet enforced by the
checker.

when a channel is the wrong shape — a registry every request reads and a
few requests write, a counter, a cap on how many things run at once — reach
for a lock. the primitives below are built in: they need no import, and
they are what the standard library itself uses.

## locks, counters, and permits

these four are language builtins, available in any pith program without an
import. `std.log`, `std.metrics`, `std.trace`, `std.io`, `std.uuid`,
`std.net.tls`, and both database drivers are built on them.

`Mutex()` guards a section that must not run twice at once:

```pith
mut rooms: Map[String, Room] := {}
mut rooms_mu := Mutex()

fn join(name: String, member: String):
    rooms_mu.lock()
    room := rooms.get(name).unwrap_or(Room(name, []))
    room.members.push(member)
    rooms[name] = room
    rooms_mu.unlock()
```

`examples/room_registry.pith` is this worked through: a registry several
tasks read and write, a counter, a capped flush, and a group over the rooms.

hold it for the shortest span that keeps the data consistent, and never
across a channel receive or a socket read — a task parked inside a lock
holds it for as long as it is parked. there is no reentrancy: locking a
mutex a task already holds deadlocks it.

`AtomicInt(n)` is a single integer several tasks can touch without a lock:

- `counter.load()`
- `counter.store(n)`
- `counter.compare_set(expected, new)` — sets and returns `true` only if the
  value was `expected`, which is how you elect exactly one winner among racing
  tasks. `std.concurrent` uses it so a deadline and a manual cancel settle on
  one reason rather than both writing.

note that `load` then `store` is *two* operations and races between them;
`counter.store(counter.load() + 1)` can lose an increment. use
`compare_set` in a retry loop when the new value depends on the old one.

`WaitGroup()` waits for a set of tasks without holding their handles:

- `wg.add(n)` before starting them
- `wg.done()` from each as it finishes
- `wg.wait()` to block until the count reaches zero

if you do hold the task handles, `await` each one instead — it is simpler
and gives you their results. a wait group earns its place when the tasks
are started somewhere that cannot keep the handles.

`Semaphore(n)` caps how many tasks may be inside a region at once:
`sem.acquire()` takes a permit and waits if none is free, `sem.release()`
returns one. `std.prometheus` uses one to bound concurrent scrapes, and
`std.net.http2.server` uses one to bound concurrent connections.

reach for a semaphore when the caller should wait, and for an `AtomicInt` in a
`compare_set` loop when it must not — waiting is the whole of what a semaphore
adds over a counter, and there is no non-blocking way to take a permit. that is
the choice `std.web`'s accept loops make: they cap connections on a counter and
refuse past it, because a loop parked on a permit is a server that has stopped
answering, health check included.

`Mutex`, `Semaphore` and `WaitGroup` park rather than spin, so a green task
blocked on any of them frees its worker for other tasks instead of holding it.
`AtomicInt` is the exception and has no blocking operation at all — it is a
lock-free load, store and compare-and-set, which is exactly why it is the tool
for the path that must not wait.

## groups

a group fans work out and fails as a unit. it is the shape you want for
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
siblings can notice and stop early — pass `g.ctx` into work that runs long
enough to care. `group_limited(parent, n)` does the same with at most `n`
units running at once, for when the fan-out is wider than the resource
behind it.

`wait()` returns the first error and waits for *every* task regardless, so
when it returns nothing from the group is still running. work that never
consults the context still runs to completion: cancelling asks a task to
stop, it does not kill it.

the work is `fn() -> Int!` — a group cares which unit failed, not what the
survivors returned. work with a value to report should write it to a
channel or a slot it owns, the same as it would without a group.

## per-thread globals

a module global marked `threadlocal` gets a separate copy per task,
created lazily the first time that task reads it — one per os thread on the
os-thread backend, one per green task on the green one. each task mutates its
own copy with no lock and no race, which is exactly what you want for
per-task scratch state — a parser's cursor, a buffer, a request-scoped
counter:

```
threadlocal mut counter := 0
threadlocal mut arena: Map[Int, Node] := {}
```

it reads and writes like any other global; only the storage is
per-task. what a copy holds is not released when its owner exits, so keep the
set of `threadlocal` globals small and the values scratch-sized. use it
for state that is genuinely per-task; a value shared *across* tasks still
belongs in a struct or behind a channel.

this is what lets the std parsers and buffers be used from many tasks at
once: `std.json` and `std.toml` keep their parse arena in `threadlocal`
globals, and `std.io`'s readers/writers keep their per-instance state the
same way, so two tasks parsing or buffering concurrently each work in
their own state with no lock and no race.

things that are still intentionally explicit or still growing:

- task cancellation is cooperative, not forceful
- plain file io still does not have `_ctx` variants
- sharing a mutable collection across tasks is a data race (above);
  use channels

## the green backend

on os threads each `spawn` runs on its own kernel thread, so every channel
send, mutex, or await that has to wait hands off through the kernel — a futex
wake and a context switch. that is fine when tasks mostly compute, but a
program built out of many small tasks that pass values to each other pays a
kernel switch on every hop.

the green backend is the answer to that, and on linux it is what you get
unless you ask otherwise. tasks become coroutines that run on a small pool of
worker threads (`available_parallelism()` by default, `PITH_GREEN_WORKERS=n`
to pin the count). channel, mutex, waitgroup, semaphore, and await operations
that would block yield to the scheduler instead of parking their worker, so a
handoff between two tasks on the same worker is a userspace switch with no
kernel involved. socket i/o goes through an epoll reactor: a read or write
that would block parks the task on the reactor and frees the worker to run
something else, so the whole net stack — raw tcp, tls, http/2, grpc — yields
the same way without any code of its own. sleeping parks there too: a
`time.delay` from inside a task registers a timer on the reactor's deadline
heap instead of blocking the worker, so a task sleeping out a backoff — or a
`select` idling between probes — costs nothing but its own time. that reactor
is also why the default is linux-only; see "which backend to use".

where a task runs is decided once. a task pins to the worker that first runs it
and stays there — a suspended coroutine may hold live pith stack, so moving it
between threads is not safe. that makes the placement a pair of communicating
tasks happens to get a lasting property of the program, and the two cases cost
very differently: colocated, each handoff is the userspace switch above; split
across workers, each one is a park and a futex wake. a two-task ping-pong
measured ~0.2us per round colocated and ~19us split.

workers therefore look for work for a short while before parking, so a handoff
already in flight is caught without a kernel round trip. that spin is only paid
by a worker that ran something recently: once a worker has idled long enough to
back its park timeout off it parks directly, and an idle pool stays quiet. the
spin narrows the split case rather than removing it — a pipeline that lands
split is still the slower arrangement, and `PITH_GREEN_WORKERS=1` is worth
trying for a workload that is one request/response chain rather than genuine
fan-out.

a read deadline survives that translation. `tcp.set_timeout` stores the deadline
on the socket itself, and the reactor wait is bounded by it: a read that reaches
the deadline with nothing to show for it fails, which is exactly what happens on
os threads, where the kernel enforces the same stored value on the blocking read.
that is what stops a client that connects and never speaks from holding a task
forever, and it is what every server-side bound in the stdlib rests on —
`std.web`'s connection timeout, the http/2 server's, the tls handshake deadline,
`std.prometheus`'s scrape timeout. a socket with no deadline set, which is most
of them, waits indefinitely as it always has, and a child process's pipe has no
such deadline to set at all.

name resolution and file i/o get there by a different route. `getaddrinfo` is
synchronous and there is nothing to poll, and a regular file is always reported
ready by epoll however slow the disk behind it is, so neither can yield to the
reactor. instead the call goes to a small pool of ordinary blocking threads
(four at most, started on demand, one pool for lookups and one for files) and
the task parks until a pool thread answers. the worker stays free for the whole
call, so a dial no longer has a blocking step sitting in front of its
non-blocking one, and a task that writes a log line no longer stops every other
task on its worker while it does.

child processes take neither route. a pipe is pollable where a regular file is
not, and linux hands out a `pidfd` that becomes readable exactly when a child
exits, so `Process.wait`, `ProcessStdout.read` and their siblings park on the
reactor the way a socket does. no thread is held per child either, which
matters more here than anywhere else: a wait has no bound at all, and four
`sleep 3600`s would have emptied a four-thread pool. a child that has already
finished by the time you wait for it is collected on the spot, with no park.

`process.output` — along with everything routed through it (`run`, `text`,
`output_checked`) and the two shell helpers `run_shell` and `output_shell` —
runs a child to completion inside one call while draining both of its pipes,
which the reactor cannot cover (it waits on one fd at a time). those calls go
to a small process pool of their own instead, the dns/file shape: the command
crosses to a blocking thread, the task parks, and the worker stays free for
however long the child runs. `exec` and `exec_output` take the same route.
`start` plus your own reads and `wait` still parks on the reactor directly,
with no pool thread involved.

a task waits a few microseconds on-CPU before it actually parks. a read the page
cache answers comes back faster than the two thread wakeups a park costs, so
waiting for it beats suspending, and the wait is bounded at roughly what the
call would have held the worker for had it run inline. anything slower falls
through to the park and frees the worker, which is the case the pool exists for.

nothing in your program changes. the same `spawn`, `Channel`, `Mutex`, and
`await` run on either backend, and a correct program prints the same thing
both ways. `PITH_GREEN` picks one explicitly: `1`, `on`, or `true` forces
green, `0`, `off`, or `false` forces os threads, and anything else — including
leaving it unset — takes the platform default, which is green on linux and os
threads everywhere else. the two green examples in this repo run identically
whichever way you set it:

```
pith run examples/worker_pool.pith
PITH_GREEN=0 pith run examples/worker_pool.pith
```

it helps most when tasks coordinate a lot and compute little — a fan-out of
short jobs over channels, or a network client whose reader, writer, and
worker tasks trade a request back and forth per call. on that last shape the
green backend cut internal context switches from about five per grpc call to
well under one and raised throughput on a two-core box (see the grpc section
of `docs/performance.md`). on the channel fan-out benchmark — eight tasks
trading a million messages through one bounded channel — it runs well ahead of
the os-thread backend and of zig, but behind go and rust at the default worker
count (~95 ms median against their ~71 and ~69 on the 2026-08-22 rerun — and
bimodal: 2 of 15 launches land at 59-66 ms, faster than either, when the task
placement race goes well). pinned to one worker it turns the go comparison
around at ~46 ms to ~71: the benchmark is pure handoff, which one worker does
without crossing a core. see the coordination table in
`docs/performance.md`. it does the least for tasks that are already
cpu-bound and rarely wait — those never hit the handoffs green makes cheap.

the machinery behind that: coroutine stacks are pooled and reused rather
than mapped and unmapped per task, so a fan-out of short tasks costs
allocations, not TLB shootdowns; each task's scheduling state lives in one
atomic word, so waking a task or resuming it never takes the scheduler's
slab lock; and a channel wake only signals a condvar when an os-thread
waiter is actually parked in it, because green waiters suspend their
coroutine instead and rust's condvar pays a futex syscall per notify
regardless of waiters. `PITH_GREEN_STATS=1` prints the wake breakdown,
contended-lock counts, and hot-path counters at exit if you want to see
where a workload's handoffs actually go.

it also keeps memory flat under fan-out. when a task finishes, the green
backend reclaims its slot and releases the closure it was spawned with, so a
server that spawns one task per request holds only the tasks running at once,
not one record for every request it has ever served. this holds even for a
fire-and-forget `spawn f()` whose handle you never keep: a discarded spawn is
detached at the spawn site, so its slot is reclaimed when the body finishes
rather than pinned until the process exits. a fan-out of 500k short tasks that
used to climb to ~226 mb now holds around 3 mb, flat no matter how many run in
total — see the green fan-out row in `docs/performance.md`. (an awaited task on
the os-thread backend still keeps its record until you read the result;
teaching that path the same reclamation is the next step.)

### which backend to use

on linux, the one you already have. green is faster on every shape this repo
measures, so it is the default there and you only reach for `PITH_GREEN=0` if
one of the caveats below is your workload. everywhere else the default is os
threads, and `PITH_GREEN=1` opts in.

the reason for that split is the reactor. it is epoll and eventfd, so it is
linux-only; macos and the bsds compile a stand-in with no reactor at all, and a
green task waiting on a socket there — or sleeping — blocks its worker for as
long as the wait lasts. green is still correct on those platforms and you can
turn it on, but an
i/o-heavy server on macos wants os threads until there is a kqueue sibling.

the caveats that come with the linux default all come from one fact: a green
worker runs many tasks, so anything that holds the worker holds all of them,
where an os thread would only cost the one task making the call.

file metadata is where the last of that lives. opening, reading, writing,
appending, listing a directory and removing a tree all go to the file pool now,
but `exists`, `size`, `rename`, `mkdir` and removing a single file still call
the kernel directly on the worker. each of those is one lookup that the kernel
answers out of its cache, which costs about what handing it to another thread
would; on a slow network mount it is not, and a task doing one there holds its
worker until it returns. `PITH_GREEN=0` is still the answer if that bites you.

the pool is not free either. a file call made from inside a task now costs a
thread handoff that it used to not, so a task that reads a small cached file in
a tight loop is several times slower than it was — the run finishes sooner for
everything else on that worker and later for itself. calls made from `main`,
which is not a green task, take the direct path and are unaffected.

preemption is the other thing the kernel used to hand you. os threads get it for
free; under green a compute-only task that never touches a channel, await, or
socket holds its worker until it finishes, unless the binary was built with
safe-points (`PITH_GREEN_PREEMPT=1`, below). code that coordinates already yields
on its own and never needs it.

and placement is luck. a task pins to the first worker that runs it, so whether
two tasks that talk to each other end up sharing one is chance. on the channel
fan-out benchmark that is the difference between ~46ms and ~130-170ms for the
same program. `PITH_GREEN_WORKERS=1` takes the choice away and is often the
fastest setting for a single pipeline.

there is one more reason the os-thread backend stays, which matters to this
repo rather than to your program: it is the reference green gets checked
against. `make verify-green-corpus` runs the whole regression corpus under
green and diffs it against the fixed os-thread answers, which is how a
single-worker deadline bug turned up. two implementations that have to agree is
worth keeping around past the point where one of them is faster.

the rest of the rough edges:

- a compute-bound task that loops without ever touching a channel, await, or
  socket can be preempted, but you opt in at build time. compile with
  `PITH_GREEN_PREEMPT=1` and the backend puts a safe-point at every loop
  back-edge; run that binary under green and a monitor thread makes a
  task that has held its worker past its time slice yield, so its peers on the
  same worker get to run. it is opt-in because that safe-point costs a bit on
  every loop iteration — nothing measurable on real work, about 6% on a
  degenerate arithmetic loop — and a build that never runs green would pay for
  a check that can never fire. code that uses channels and sockets already
  yields on its own and never needs this. one gap in this first version:
  a task sitting inside a long native runtime call (a large file read, say) is
  not preempted until it returns to pith code and hits the next back-edge
- fewer workers means more locality: a task pins to the first worker that runs
  it and every later wake goes back to that one worker, never the whole pool, so
  a coordinated pipeline stays put and its handoffs stay in userspace. at
  `PITH_GREEN_WORKERS=1` the whole pipeline shares one worker with no cross-thread
  wakes at all, which is often the fastest setting for a single connection even
  though it uses one core; more workers only pay off when the work genuinely
  spreads across connections or cores. on the channel fan-out benchmark this is
  the difference between ~46ms pinned and ~130-170ms when the pipeline splits
  across two workers — placement is the biggest remaining cost on coordinated
  shapes, and today it falls where first-resume luck puts it
