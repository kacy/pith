# Signals and graceful shutdown

A server that cannot hear `SIGTERM` cannot be deployed. Every orchestrator ends
a process the same way: send `SIGTERM`, wait out a grace period, then send
`SIGKILL`. A process that ignores the first signal has its in-flight requests
severed by the second, which leaves half-written responses, streams cut
mid-exchange, and a telemetry batch that never left memory.

Two modules cover this. `std.signal` receives signals as ordinary values.
`std.shutdown` coordinates the drain that a signal triggers. The std servers are
already wired into the second one, so a server usually only touches the first
through one call.

## The whole wiring, in one call

```pith
import std.shutdown as shutdown
import std.web as web

fn main() -> Int!:
    shutdown.on_signals()!
    app := web.new().get("/", home)
    app.listen("0.0.0.0", 8080)!
    return 0
```

`on_signals()` arms `SIGTERM`, `SIGINT`, and `SIGHUP`, and requests a drain when
one arrives. `listen` then returns instead of blocking forever, and returns only
after the in-flight requests have finished. Its return value is how much work
was still unfinished when the grace period expired (`0` for a clean shutdown).

Without `on_signals()`, nothing changes: `listen` serves forever, exactly as it
did before, and `SIGTERM` keeps its default disposition of killing the process.

## What happens on a signal

1. The signal handler writes one byte to a pipe and does nothing else. A signal
   handler may only call async-signal-safe functions, so it cannot allocate,
   lock, or touch the Pith runtime.
2. The task parked in `signal.wait_forever()` wakes and calls
   `shutdown.request()`.
3. `request()` sets a process-wide flag and calls `shutdown(2)` on every
   registered listener. That stops new connections and wakes the accept loop.
4. The accept loop sees the flag, breaks, and closes its listener, which frees
   the port for the replacement instance before the drain starts. It then waits
   for the in-flight connections and any subsystem flushes to finish.
5. `listen` returns.

Step 3 uses `shutdown(2)` instead of `close(2)` on purpose. A close from
the signal task would also wake the accept loop (see "closing a connection
another task is using" in docs/concurrency.md), but `shutdown(2)` leaves the
listener open, so the accept loop still closes its own listener on its way
out and the registry stays the one place that says which of a drain and a
normal teardown owns that close.

## A waiting task does not hold a worker

`signal.wait(timeout_ms)` reads the pipe; when it is empty it hands the fd to
the same readiness seam every socket read uses. Inside a green task that
registers with the epoll reactor and suspends the coroutine, so the worker OS
thread runs other tasks meanwhile. Outside one it blocks on `poll`. Neither
spins.

## The sharp edge

Arming a signal replaces its default disposition. After `notify_shutdown()`
the process no longer dies on `SIGTERM`, so something must wait for it and
act. Arm signals only where you also handle them. `shutdown.on_signals()` pairs
the two in one call so this cannot be got wrong by accident; call
`signal.notify()` directly only when you are writing the waiting side yourself.

The queue itself is process-global: `signal.wait` drains one pipe, so exactly
one loop in a process can wait on signals. A terminal session
(`std.term.session`) runs such a loop for the resize signal, so a TUI cannot
also use `shutdown.on_signals`. Route extra signals through the session's
`Options.signals` instead, and they arrive as `Event.Signal(n)` on its event
stream. See [terminal.md](terminal.md).

## The one signal Pith disarms for you

`SIGPIPE`. Writing to a socket whose peer has gone raises it, and its default
disposition is to terminate. A server answering a client that hung up a moment
early would die silently, taking every other connection on the process with
it. The runtime sets it to ignore from the first socket onwards, and the
failing write reports an error instead, the same answer a reset already gives.

Every server runtime does this. Pith needed to do it explicitly because a Pith
binary's `main` is generated code linking the runtime as a static library, so
it never runs Rust's own startup path. A program that wants to die on a broken
pipe can arm `SIGPIPE` through `signal.notify()`, which runs later and replaces
the disposition.

## std.signal

```pith
import std.signal as signal

signal.notify([signal.SIGTERM, signal.SIGHUP])!   # arm; returns how many
sig := signal.wait_forever()                      # the next signal number
print("got " + signal.name(sig))                  # "got SIGTERM"
```

- `SIGHUP` (1), `SIGINT` (2), `SIGQUIT` (3), `SIGTERM` (15): named constants.
- `notify(signals)` arms a list, returning how many. It fails instead of arming
  a subset: a server that believes it is listening for `SIGTERM` and is not
  would drop every request of its final deploy.
- `notify_shutdown()` arms the standard trio.
- `wait(timeout_ms)` returns the next signal number, `0` on timeout, or `-1`
  when nothing is armed. `wait_forever()` is the same with no deadline.
- `raise_self(sig)` sends a signal to this process, exactly as an orchestrator
  would. It is how a signal path is tested for real, and how a program can
  trigger its own drain.
- `pid()` is this process's id.

Signals queue, so one is not lost between two waits in the ordinary case. The
handler writes a byte to a non-blocking pipe and ignores the result, which is
what keeps it async-signal-safe, so a signal arriving after that pipe has
filled is dropped rather than buffered.

## std.shutdown

This module is the coordinator. Each of its counts is a count and not a flag,
so several servers and several subsystems compose without knowing about each
other.

| Call | What it does |
| --- | --- |
| `on_signals()` | Arm the trio and drain when one arrives |
| `request()` | Begin a drain, from anywhere (a `/quit` route, a supervisor) |
| `requested()` | Whether a drain has begun; poll this in a long-lived handler |
| `register_listener(fd)` / `close_listener(fd)` | Listener registry; `close_listener` closes at most once |
| `enter()` / `leave()` | Bracket one unit of in-flight work: `enter()` on the accept loop, `leave()` from the task |
| `expect_flush()` / `flush_done()` | A subsystem with shutdown work of its own |
| `drain(deadline_ms)` | Wait for both, returning what was left unfinished |
| `set_drain_deadline(ms)` | The grace period every std server uses |
| `stream_deadline()` / `stream_deadline_passed()` | The midpoint of the period, where a stream stops being the handler's problem |

`set_drain_deadline` is one knob instead of an argument on each of the
fourteen listen entry points, because the right value is a property of the
deployment and not of one listener: it has to fit inside the orchestrator's
own grace period.
It defaults to 15 seconds, comfortably inside Kubernetes' 30.

## Which servers drain

All of them, and through the same coordinator:

- `std.web`: `listen`, `listen_tls`, and `listen_h2c` (which runs on the
  HTTP/2 accept loop below).
- `std.net.http2.server`: `listen_h2c`, `listen_h2_tls`, and both streaming
  twins.
- `std.net.grpc`: `serve`, `serve_tls`, `serve_body`, `serve_body_tls`,
  `serve_stream`, `serve_stream_tls`, which ride the HTTP/2 accept loop and so
  inherit its drain. The drain itself never severs an RPC in progress. For a
  streaming method that means the stream runs to its trailers, and for one
  that would otherwise never get there, to a `UNAVAILABLE` status instead of a
  cut connection.
- `std.prometheus`: `serve`. The scrape endpoint is a listener like any other:
  it stops accepting on a shutdown request, and a scrape that was already
  accepted gets the grace period to finish instead of being cut mid-response.

### When a connection joins the count

On the accept loop, right after the accept, and not inside the task that
serves it. Between a `spawn` and the spawned task's first instruction a
connection is invisible, and a drain landing in that window would see nothing
outstanding and return, cutting off a client that had already been accepted.
The whole promise of a graceful drain is that every accepted connection
finished, so the count starts at the accept.

That leaves only one direction to be wrong in, and it is the safe one: work
that is counted and then never runs holds its count up until the grace period
expires, and `drain()` returns it as work it abandoned. That error is bounded
and reported, where the other direction drops a connection silently. Write
your own accept loops the same way:

```pith
accepted := tcp.accept(fd)
shutdown.enter()
spawn serve(accepted.ok)   # serve() opens with `defer shutdown.leave()`
```

## Streams that never end

A drain waits for work to finish, and a stream that never finishes would
outlast any grace period. So the period is split in half, and both halves read
the same timestamp, `stream_deadline()`, so they cannot drift apart.

**The first half is the handler's.** A long-lived handler polls the flag
between messages and ends its stream on its own terms:

```pith
while not stream.closing():
    message := stream.recv_message()!
    stream.send_message(reply_to(message))
stream.finish_ok()
```

**The second half is the framework's**, and it needs nothing from the handler.
At the midpoint:

1. Every gRPC RPC still open is finished with `UNAVAILABLE` and the message
   `grpc: server is shutting down`. This runs from a watchdog instead of from
   inside a send, because a handler asleep between two ticks is not in any
   call the framework could intercept.
2. Each connection cancels the streams still on it. That wakes a handler
   parked on a receive and, unlike tearing the stream table down, leaves it a
   writer to send its own trailers through.
3. The socket is stopped with `shutdown(2)`, which unblocks the reader task. An
   HTTP/2 connection is long-lived by design: a client reading an endless
   stream, or just holding a pool entry, has no reason to hang up, and until
   the reader returns that connection never leaves the in-flight count.

As a result, a handler that does nothing special still degrades gracefully:
its peer reads a real status and retries against the replacement instance, and
the drain finishes without running out the clock.

Handlers are still expected to poll; this is a safety net.
`grpc.streams_closed_by_shutdown()` counts the RPCs the framework had to close;
anything but `0` names a handler that should be polling `closing()`.

## Telemetry

`std.obs` registers itself as shutdown work when its exporter starts, so a
drain waits for the final span and metric export before the process exits. See
[docs/telemetry.md](telemetry.md#flushing-on-exit).

## A runnable example

`examples/graceful_shutdown.pith` runs the whole sequence in one process: it
serves, starts a slow request, sends itself a `SIGTERM`, shows a new connection
being refused while the earlier one is still being served, and prints the
drain outcome.

`examples/grpc_stream_shutdown.pith` covers the streaming half: two endless
RPCs run side by side while a `SIGTERM` lands, one polling `closing()` and one
ignoring it completely, and it prints the status each peer ends up with.
