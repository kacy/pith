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

things that are still intentionally explicit or still growing:

- task cancellation is cooperative, not forceful
- plain file io still does not have `_ctx` variants
