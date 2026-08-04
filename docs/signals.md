# signals and graceful shutdown

a server that cannot hear `SIGTERM` cannot be deployed. every orchestrator ends
a process the same way: send `SIGTERM`, wait out a grace period, then send
`SIGKILL`. a process that ignores the first signal has its in-flight requests
severed by the second — half-written responses, streams cut mid-exchange, and a
telemetry batch that never left memory.

two modules cover this. `std.signal` receives signals as ordinary values.
`std.shutdown` coordinates the drain that a signal triggers. the std servers are
already wired into the second one, so a server usually only touches the first
through one call.

## the whole wiring, in one call

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
after the in-flight requests have finished. its return value is how much work was
still unfinished when the grace period expired — `0` for a clean shutdown.

without `on_signals()`, nothing changes: `listen` serves forever, exactly as it
did before, and `SIGTERM` keeps its default disposition of killing the process.

## what happens on a signal

1. the signal handler writes one byte to a pipe. that is all it does — a signal
   handler may only call async-signal-safe functions, so it cannot allocate,
   lock, or touch the pith runtime.
2. the task parked in `signal.wait_forever()` wakes and calls
   `shutdown.request()`.
3. `request()` sets a process-wide flag and calls `shutdown(2)` on every
   registered listener. that stops new connections and wakes the accept loop.
4. the accept loop sees the flag, breaks, closes its listener — releasing the
   port for the replacement instance before the drain, not after — and waits for
   the in-flight connections and any subsystem flushes to finish.
5. `listen` returns.

step 3 uses `shutdown(2)` rather than `close(2)` deliberately. closing an fd does
not wake a call already blocked on it, and `listen()` called straight from `main`
runs its accept on a plain thread rather than a green task, so a close would
leave it blocked indefinitely. `shutdown(2)` wakes both shapes, and because it
leaves the descriptor valid, the accept loop still closes its own listener —
nothing closes an fd another task is mid-syscall on.

## a waiting task does not hold a worker

`signal.wait(timeout_ms)` reads the pipe; when it is empty it hands the fd to the
same readiness seam every socket read uses. inside a green task that registers
with the epoll reactor and suspends the coroutine, so the worker OS thread runs
other tasks meanwhile. outside one it blocks on `poll`. neither spins.

## the sharp edge

arming a signal replaces its default disposition. after `notify_shutdown()` the
process no longer dies on `SIGTERM` — something must wait for it and act. arm
signals only where you also handle them. `shutdown.on_signals()` pairs the two in
one call so this cannot be got wrong by accident; reach for `signal.notify()`
directly only when you are writing the waiting side yourself.

## std.signal

```pith
import std.signal as signal

signal.notify([signal.SIGTERM, signal.SIGHUP])!   # arm; returns how many
sig := signal.wait_forever()                      # the next signal number
print("got " + signal.name(sig))                  # "got SIGTERM"
```

- `SIGHUP` (1), `SIGINT` (2), `SIGQUIT` (3), `SIGTERM` (15) — named constants.
- `notify(signals)` arms a list, returning how many. it fails rather than arming
  a subset: a server that believes it is listening for `SIGTERM` and is not would
  drop every request of its final deploy.
- `notify_shutdown()` arms the standard trio.
- `wait(timeout_ms)` returns the next signal number, `0` on timeout, or `-1` when
  nothing is armed. `wait_forever()` is the same with no deadline.
- `raise_self(sig)` sends a signal to this process, exactly as an orchestrator
  would. it is how a signal path is tested honestly, and how a program can
  trigger its own drain.
- `pid()` is this process's id.

signals queue, so none is lost between two waits.

## std.shutdown

the coordinator. every count is a count rather than a flag, so several servers
and several subsystems compose without knowing about each other.

| call | what it does |
| --- | --- |
| `on_signals()` | arm the trio and drain when one arrives |
| `request()` | begin a drain, from anywhere (a `/quit` route, a supervisor) |
| `requested()` | whether a drain has begun — poll this in a long-lived handler |
| `register_listener(fd)` / `close_listener(fd)` | listener registry; `close_listener` closes at most once |
| `enter()` / `leave()` | bracket one unit of in-flight work |
| `expect_flush()` / `flush_done()` | a subsystem with shutdown work of its own |
| `drain(deadline_ms)` | wait for both, returning what was left unfinished |
| `set_drain_deadline(ms)` | the grace period every std server uses |

`set_drain_deadline` is one knob rather than an argument on each of the nine
listen entry points, because the right value is a property of the deployment —
it has to fit inside the orchestrator's own grace period — not of one listener.
it defaults to 15 seconds, comfortably inside kubernetes' 30.

## which servers drain

all of them, and through the same coordinator:

- `std.web` — `listen` and `listen_tls`.
- `std.net.http2.server` — `listen_h2c`, `listen_h2_tls`, and both streaming
  twins.
- `std.net.grpc` — `serve`, `serve_tls`, `serve_body`, `serve_body_tls`,
  `serve_stream`, `serve_stream_tls`, which ride the http/2 accept loop and so
  inherit its drain. an rpc in progress is never severed by the drain itself,
  which for a streaming method means the stream runs to its trailers.

## endless streams

a drain waits for a connection to end, and a stream that never ends outlasts any
grace period. a long-lived handler should poll `shutdown.requested()` between
messages and finish its stream when it goes true, rather than relying on the
deadline to cut it off:

```pith
while not shutdown.requested():
    message := stream.recv_message()!
    stream.send_message(reply_to(message))!
stream.finish_ok()
```

## telemetry

`std.obs` registers itself as shutdown work when its exporter starts, so a drain
waits for the final span and metric export before the process exits. see
[docs/telemetry.md](telemetry.md#flushing-on-exit).

## a runnable example

`examples/graceful_shutdown.pith` does the whole dance in one process: it serves,
starts a slow request, sends itself a `SIGTERM`, shows a new connection being
refused while the earlier one is still being served, and prints the drain
outcome.
