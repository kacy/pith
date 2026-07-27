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
print(value?)
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

## sharing between tasks

`spawn` runs on a real os thread. reference counts are atomic, so
handing a value to another task and letting both hold a count is safe.
what is *not* safe is two tasks mutating the same collection at once —
a list or map is a plain buffer behind a handle, and concurrent
mutation races on that buffer.

pass data between tasks through a channel rather than sharing a
mutable collection. a channel hands the value over instead of aliasing
it, so each task mutates its own. immutable values and independent
copies (`std.collections.copy_list` and friends) are also safe to
hand off. this rule is convention today, not yet enforced by the
checker.

## per-thread globals

a module global marked `threadlocal` gets a separate copy per os thread,
created lazily the first time a thread reads it. each task mutates its
own copy with no lock and no race, which is exactly what you want for
per-task scratch state — a parser's cursor, a buffer, a request-scoped
counter:

```
threadlocal mut counter := 0
threadlocal mut arena: Map[Int, Node] := {}
```

it reads and writes like any other global; only the storage is
per-thread. a copy is not reclaimed when its thread exits, so keep the
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

## the green backend (experimental)

by default each `spawn` runs on its own os thread, so every channel send,
mutex, or await that has to wait hands off through the kernel — a futex wake
and a context switch. that is fine when tasks mostly compute, but a program
built out of many small tasks that pass values to each other pays a kernel
switch on every hop.

`PITH_GREEN=1` switches on a second backend. tasks become coroutines that run
on a small pool of worker threads (`available_parallelism()` by default,
`PITH_GREEN_WORKERS=n` to pin the count). channel, mutex, waitgroup,
semaphore, and await operations that would block yield to the scheduler
instead of parking their worker, so a handoff between two tasks on the same
worker is a userspace switch with no kernel involved. socket i/o goes through
an epoll reactor: a read or write that would block parks the task on the
reactor and frees the worker to run something else, so the whole net stack —
raw tcp, tls, http/2, grpc — yields the same way without any code of its own.

name resolution gets there by a different route. `getaddrinfo` is synchronous
and there is nothing to poll, so instead of yielding to the reactor the lookup
goes to a small pool of ordinary blocking threads (four at most, started on
demand) and the task parks until one of them answers. the worker stays free for
the whole lookup, so a dial no longer has a blocking step sitting in front of
its non-blocking one.

nothing in your program changes. the same `spawn`, `Channel`, `Mutex`, and
`await` run on either backend, and a correct program prints the same thing
both ways. the two green examples in this repo run identically with the flag
on or off:

```
pith run examples/worker_pool.pith
PITH_GREEN=1 pith run examples/worker_pool.pith
```

it helps most when tasks coordinate a lot and compute little — a fan-out of
short jobs over channels, or a network client whose reader, writer, and
worker tasks trade a request back and forth per call. on that last shape the
green backend cut internal context switches from about five per grpc call to
well under one and raised throughput on a two-core box (see the grpc section
of `docs/performance.md`). on the channel fan-out benchmark — eight tasks
trading a million messages through one bounded channel — it runs 2.6x faster
than the os-thread backend and ahead of rust and zig, at 2.3x go (~46ms
against go's ~75 when pinned to one worker; see `bench/README.md`). it does
the least for tasks that are already cpu-bound and rarely wait — those never
hit the handoffs green makes cheap.

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
not one record for every request it has ever served. a fan-out of 500k short
tasks that used to climb to ~226 mb now holds around 3 mb, flat no matter how
many run in total — see the green fan-out row in `docs/performance.md`. (the default
os-thread backend still keeps a record per task it has run; teaching it the
same reclamation is the next step.)

### which backend to use

green is faster on every shape this repo measures, so the short answer is to
use it on linux for anything that coordinates a lot of tasks. the longer
answer is why the os-thread backend is still here, because it is not just
waiting to be deleted.

a blocking call on a green worker stalls the worker, not only the task making
it. the epoll reactor covers sockets and the resolver pool covers dns, so a
client dial yields the whole way through, but nothing else does: file reads and
writes and any slow native call hold the thread they run on, and every task
pinned to that worker waits behind them. on os threads that same call costs one
task, because the task is a thread and the kernel just runs someone else.

the reactor is also linux-only. it is epoll and eventfd; macos and the bsds
compile a stand-in with no reactor at all, so a green task waiting on a socket
there blocks its worker for as long as the wait lasts. green is still correct
on those platforms, but an i/o-heavy server on macos wants os threads. and
preemption, which the kernel gives os threads for free, is a build-time opt-in
under green (see `PITH_GREEN_PREEMPT` below).

there is one more reason, which matters to this repo rather than to your
program: os threads are the reference green gets checked against.
`make verify-green-corpus` runs the whole regression corpus under green and
diffs it against what the os-thread backend produces, which is how a
single-worker deadline bug turned up. two implementations that have to agree
is worth keeping around past the point where one of them is faster.

it is off by default and still experimental. the known rough edges:

- a compute-bound task that loops without ever touching a channel, await, or
  socket can now be preempted, but you opt in at build time. compile with
  `PITH_GREEN_PREEMPT=1` and the backend puts a safe-point at every loop
  back-edge; run that binary under `PITH_GREEN=1` and a monitor thread makes a
  task that has held its worker past its time slice yield, so its peers on the
  same worker get to run. it is opt-in because that safe-point costs a bit on
  every loop iteration (nothing you would notice on real work, but real on a
  tight arithmetic loop), and the default build is almost always run os-thread,
  so it plants no check and pays nothing. code that uses channels and sockets
  already yields on its own and never needs this. one gap in this first version:
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
