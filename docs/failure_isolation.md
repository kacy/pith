# Failure isolation

A Pith runtime error (an index out of range, a missing map key, `.value()` on
`none`, a division by zero, an integer overflow) prints
`pith runtime error: ...` and exits with status 1. Inside a `T!` or `T!String`
function the index, key and value errors propagate as errors instead. A
handler returns an `HttpResponse`, though, so in a server one bad request ends
the process and every connection it was serving.

This page is the design for phase 1 of keeping such a failure inside the
request. A runtime error inside an HTTP handler (std.net.http, std.web, the
HTTP/2 server) fails that request with a 500, and one inside a gRPC method
fails the call with `INTERNAL`. The server keeps serving. A runtime error
anywhere else (main, a plain spawned task) still exits with the same message
and status.

## Where runtime errors come from

Every error in scope ends in a Rust function in the runtime. The compiled code
never stops the process on its own.

| Error | Raised by |
|---|---|
| Division by zero, integer division overflow, `.value()` on none (and integer overflow, code 7) | `pith_runtime_error(code)` in `runtime_core.rs`, called from the slow block the codegen emits beside a checked `div`/`mod` (`emit_checked_int_div_or_mod` in `ir_consumer.rs`) and from the none check |
| Integer overflow in `+`, `-`, `*`, a narrow-type op, an integer conversion or a shift count | The trap instruction the codegen emits on the overflow path, which the SIGILL handler in `overflow_trap.rs` turns into `pith_runtime_error(7)` |
| Integer overflow in `abs` and `Float.to_int`, and `to_int` on NaN | `pith_abs` and `pith_float_to_int` in `runtime_core.rs` |
| `xs[i]` and `xs[i] = v` out of range | `strict_index_out_of_bounds` and `strict_store_out_of_bounds` in `collections/list.rs`. The codegen inlines the in-bounds case (`inline_list_get_value_strict`) and calls `pith_list_get_value_strict` and the strict stores only on the slow path |
| `m[k]` on a missing key, and the fused `m[k] += n` | `strict_miss` in `collections/map.rs` |
| `s[i]` and `b[i]` out of range | `pith_cstring_char_at_strict` in `string_list.rs`, `pith_bytes_get_strict` in `bytes.rs` (the latter also inlined, `inline_bytes_get_strict`) |
| `substring` cutting a character in half | `report_split` in `string_list.rs` |

The other `exit(1)` sites in the runtime stay as they are: a failed
allocation, an invalid layout, a call the compiler could not resolve (codes
3, 4 and 5), an invalid or freed handle, a type mismatch between a collection
and the operation the compiler emitted for it, and the quarantine report.
Those are compiler bugs or memory corruption, and continuing past them is not
safe.

## How handlers run

- **HTTP/1.1** (std.net.http, and std.web, which serves through it): one task
  per connection. `serve_requests` loops over the connection's requests, and
  each one reaches the handler through `run_handler`, which also scopes the
  JSON node pool. `serve_one_ctx` uses the same `run_handler`.
- **HTTP/2** (`std/net/http2/server.pith`): a reader task and a writer task per
  connection and one task per stream. `handle_stream` assembles the request
  and calls the handler. A streaming server calls a `fn(ServerStream) -> Int`
  in `handle_streaming_stream` instead.
- **gRPC** (`std/net/grpc.pith`) runs on the HTTP/2 server. A unary or buffered
  call is an HTTP handler (`grpc_serve_handler`, `grpc_serve_body_handler`)
  that calls the user's dispatch. A streaming call is an HTTP/2 stream
  handler (`grpc_serve_stream_handler`).

Every task is a corosensei 0.2 coroutine on a pooled 1 MiB stack (`green/`).
`run_task` resumes it on whichever worker claims it, so a task that parks can
continue on another thread. Preemption is cooperative: a safepoint at each
loop back edge calls `pith_green_maybe_yield` when the monitor asks, which
parks the task like any other block. A task's threadlocals live in a map in
its sync block, reached through `CURRENT_TLS`, which `run_task` installs for
each resume.

So the boundary is one call of a handler function, inside a task that goes on
to serve more requests.

## Candidates

### (a) A catch frame on the same stack

Run the handler call through a small assembly routine that pushes the
callee-saved registers, records the stack pointer in a frame linked into a
per-task chain, and calls the handler. A runtime error that finds a frame
stores its message there, sets the stack pointer back and runs the routine's
epilogue, so the routine returns to its caller as if the handler had returned.
A runtime error that finds no frame exits as before.

- **Per-request cost**: the frame setup is a few stores and a call. Measured
  below at under 1 ns over a plain call in Rust, and about 3 ns over a plain
  closure call from Pith. The handler call also has to go through a closure,
  since the builtin takes a `fn() -> Int`.
- **Skipped frames**: the jump skips every frame between the error and the
  catch frame without running anything in them. Compiled Pith frames have no
  unwind information and nothing to run, so skipping them is the same as a
  `longjmp` over C frames. The Rust frames on the error path are the
  runtime's error functions, which are changed so that they own nothing that
  needs a drop when they jump (the formatted message is moved into the frame;
  the key text for a missed map lookup is passed by value). Apart from the
  cycle collector's generated tracers, which raise no errors, the runtime
  calls back into Pith in three places: a struct destructor, a threadlocal
  initializer and a task body. None holds a Rust lock across the call, so a
  failure inside a `drop` method skips frames with no destructors either. The
  struct whose destructor failed is left unfreed with its guard count.
- **Leaks**: whatever only the skipped Pith frames held a count on. See the
  bound below.
- **Mutexes**: a Pith `Mutex` the handler locked and did not unlock would stay
  locked forever, and every later request that takes it would park for good.
  A mutex has no owner, so the runtime records each lock taken inside an
  isolated call in the frame (one thread-local read and a branch on every
  `lock` and `unlock`, nothing more when no frame is active) and unlocks the
  ones still held when the call fails.
- **Preemption and migration**: the frame lives on the task's own stack and
  the saved stack pointer points into that stack, so a task that parked and
  resumed on another worker inside the handler is caught correctly. The chain
  head is a field of the task's threadlocal map, read afresh on every access,
  and an OS thread (main, the OS-thread backend) has its own head in a plain
  thread-local. The error is raised synchronously by the running task, so
  there is never a suspended frame between the error and the catch frame.

### (b) A nested coroutine per handler call

Take a stack from the pool, run the handler as a nested coroutine on it, and
on a runtime error suspend back out and drop the coroutine with corosensei's
`force_reset`.

- **Per-request cost**: about 8 ns over a plain call for a coroutine on a
  warm pooled stack that runs to completion, against under 1 ns for (a). The
  real cost is higher: the runtime would have to swap the task's stack bounds
  for the overflow handler and its sanitizer fiber on every switch, and the
  pool would hold one more stack per in-flight request.
- **Blocking**: a handler blocks all the time (reading a body, a database
  call, a channel). A blocked handler has to suspend the whole task, which
  means suspending the outer coroutine from inside the inner one. Corosensei
  does not support that; the runtime would need to forward every park through
  the nested coroutine and back, on every block site.
- **Leaks, mutexes, Rust frames**: the same as (a). `force_reset` runs no
  destructors either, so the frames on the abandoned stack leak the same
  counts and leave the same locks held. It skips the same Rust frames.
- **Memory**: one more mapped stack per in-flight request, so a server with
  10,000 slow requests reserves 10 GiB more address space and keeps the pages
  each handler touched.

It costs more per request and per block site, and a failure leaves exactly
what (a) leaves.

### (c) The others

- **Rust unwinding**: raise a panic from the error site and catch it at the
  handler boundary. This would run Rust destructors on the way, but every
  compiled Pith frame would need `.eh_frame` unwind information, which the
  object module does not write today (it would grow every binary), and a
  panic crossing an `extern "C"` runtime function aborts the process. It buys
  nothing for Pith frames, which still have nothing to run.
- **A task per request**: spawn the handler and await it, and let a runtime
  error end only that task. A spawn and an await cost a few hundred
  nanoseconds per request, and a handler's threadlocal request state
  (`request_socket`, `pending_producer`, the gRPC metadata and deadline,
  the JSON scope) belongs to the connection's task, so it would all have to
  be handed across. Abandoning the running coroutine also needs the jump in
  (a) or the switch in (b) anyway.
- **Propagate like `T!`**: make every function able to return a failure and
  check it after every call. That is a branch after every call in every
  program, for an error that almost never happens.

## Measurements

Measured on a Ryzen 9 9950X, pinned to one core, 10M calls per round, three
rounds (bench sources are not in the tree):

| Path | Cost per call |
|---|---|
| Plain Rust call | 0.7 ns |
| (a) catch frame around it | 1.6 ns |
| (a) a call that fails and is caught (jump only) | 6.6 ns |
| (b) nested coroutine on a warm pooled stack | 9.0 ns |
| (b) a call that fails, coroutine abandoned with `force_reset` | 6.2 ns |
| Pith: build a closure and call it | 17 ns |
| Pith: build a closure and `isolate_call` it | 20 ns |
| Pith: a failing `isolate_call`, message formatted and returned | 86 ns |

The bar was a change under 2% on `bench/http_sweep.sh`. The first version
carried the handler's response out of the closure in a threadlocal, and cost
4% to 7% of throughput at 256 and 1024 connections: a threadlocal store marks
the value shared, so the whole response, headers and body, switched to atomic
counts. Carrying it in a small struct the closure captures brought the change
under 2% at every connection count, within the run-to-run spread. The numbers
are in the pull request that added it.

## The choice

(a). It is the cheapest per request, it needs no change to the scheduler or to
any block site, and nothing it gives up is kept by (b). The code is
`cranelift/runtime/src/failure.rs`; the catch routine is about twenty
instructions each for x86_64 and aarch64. On any other target the builtin is a
plain call and every runtime error still exits.

### The builtin

`isolate_call(f: fn() -> Int) -> String` runs `f` and returns `""`, or the
message of the runtime error that stopped it. The runtime error itself prints
nothing in that case; the caller decides what to report. The std servers use
it. A program can call it too, but it makes no promise about the state the
failed call left behind (the rest of this section describes that state), so
it is a poor general `try`.

### What the client sees

- **HTTP/1.1**: `500 Internal Server Error` with `Connection: close`, and the
  connection closes after it. The handler may have left the request body half
  read, and closing is the one safe answer. A request already pipelined behind
  it gets no answer, as with any closing response.
- **HTTP/2**: a 500 on that stream. The connection and its other streams go
  on. A streaming HTTP/2 handler that fails gets its stream reset with
  `INTERNAL_ERROR`.
- **gRPC**: status 13 (`INTERNAL`) with the message `internal error`, for a
  unary call, a buffered stream and a streaming call. The runtime error's own
  text stays on the server.
- The body is the server's generic 500 page and never contains the error.

`examples/web_failure_isolation.pith` sends a std.web server one request for
each kind of runtime error and shows it still serving afterwards.

### What is logged

One line on stderr:

```
pith runtime error in GET /items/9: list index out of bounds: 9 for list of length 3 (use .get(i) for Optional access)
```

The method and path are the request's. std.web routes inside the handler, so
the pattern that matched is not known at the point of the failure; the path
is.

### What the failed handler leaves behind

- **Counts held by the skipped frames leak.** The bound is exact: every value
  that only the skipped frames (the handler and what it had called at the
  moment of the error) held a count on is never freed. That is the handler's
  live working set at that instant: the strings, lists, maps and structs it
  had built, and the request too if a handler frame took a count of its own on
  it. Values the caller still holds (the request, the router, the app) are
  freed as usual. The leak is bounded per failure and never reclaimed, so a
  handler that fails on every request leaks its working set on every request.
  `tests/leaks/leak_isolated_failure.pith` holds the machinery itself to zero:
  a failed call whose frames own nothing leaks nothing, and neither do the
  frame, the message or the record of a lock.
- **`defer` blocks in the skipped frames do not run.** A middleware that
  counts in-flight requests and decrements in a `defer`, or after the handler
  returns, keeps the failed request counted.
- **A `Mutex` the handler still held is unlocked**, newest first. The data it
  guards may be half updated, as after a Go panic that runs a deferred
  `Unlock`. A lock the handler took and returned holding is left alone, as
  any function may do that.
- **Task state goes with the connection.** The threadlocals a handler sets
  belong to the connection's task (or the HTTP/2 stream's), and the failure
  ends it: the HTTP/1.1 connection closes and the stream task finishes. The
  streamed body or connection takeover a handler set up before it failed is
  dropped, so the server does not run it.

## Not covered in phase 1

- A runtime error in main, in a task the handler spawned, or in any task that
  is not serving a request still exits with status 1.
- A stack overflow still ends the process. It is reported from a signal
  handler on the alternate stack, and resuming from there is a separate
  problem.
- The code 3, 4 and 5 errors and the invalid-handle reports above still exit.
- Code that runs after the handler returned is outside the frame: a streamed
  body's producer, a connection taker such as a WebSocket loop, and the
  server's own framing.
- A semaphore permit, a waitgroup count or a channel the handler was in the
  middle of using is not repaired. Only mutexes are.
- A mutex unlocked by another task while the failed handler's frame still
  listed it would be unlocked again. Mutexes have no owner, so the runtime
  cannot tell.
- Under ThreadSanitizer the Rust frames on the error path are never exited,
  so each caught failure leaves a few entries on the task's shadow call
  stack. AddressSanitizer is told about the jump.
- std.web's middleware does not see the failure: the 500 comes from below it,
  so an error-page middleware cannot replace it.
