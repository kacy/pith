# I/O foundation

Pith now has one shared I/O layer for the stdlib to build on, instead of each
module rolling its own transport loops.

For the higher-level string/bytes helper surface on top of that split, see
`docs/text_and_bytes.md`.

Before this work, the stdlib had a few unrelated I/O styles:
- `std.fs` leaned on whole-file helpers
- `std.net.tcp` was fd-based
- `std.os.process` was handle-based
- `std.io` was basically prompt helpers

That was fine for small modules, but buffering, line reads, copy loops and
protocol framing kept showing up in slightly different forms.

## What exists now

`std.io` is the shared layer.

The core surface is intentionally small:
- `Reader`
- `Writer`
- `Closer`
- `Flusher`

The original shared layer was string-first, but it now has an explicit bytes
side too.

Text-facing surface:
- `fn read(max_bytes: Int) -> String!`
- `fn write(data: String) -> Int!`

Bytes-facing surface:
- `fn read_bytes(max_bytes: Int) -> Bytes!`
- `fn write_bytes(data: Bytes) -> Int!`

The split is intentional: raw transport data and text are different things, so
Pith treats them as different things in the stdlib too.

The compiler also supports module-qualified import aliases cleanly, so stdlib
call sites can read like this:
- `import std.io as io`
- `import std.json as json`
- `reader := io.string_reader(data)`
- `text := io.read_all(reader)!`
- `line := buffered.read_line()!`
- `stream.close()`
- `conn := io.connect(host, port)!`
- `root := json.parse(text)`

## Shared pieces

The I/O layer includes:
- Handle-backed in-memory readers and writers for simple composition and tests
- Handle-backed bytes readers and buffers for raw data paths
- Buffered readers and writers for string, TCP, process and file streams
- Line-oriented reads on top of those buffered readers
- Concrete helpers for `read_all`, `write_all`, and copy-style flows
- Plain file text helpers built on top of the file stream path
- Bytes file and process helpers built on the same stream types

`std.fs` now exposes stream-based `open`, `create`, and `open_append` on the
same foundation.

## write is one syscall, write_all is the loop

This is the one distinction in the layer worth learning before you use it.

`write` and `write_bytes` are a single `write(2)`. A socket has a bounded send
buffer, so a buffer larger than the space left in it is written *in part*: the
kernel takes what fits and returns that count, and the rest is not queued
anywhere. A short write is normal and is not an error, but discarding the
returned count drops the tail silently, and the peer then waits for bytes that
were never written. A 1 MiB Redis `SET` used to work and a 4 MiB one used to
hang for exactly this reason.

`write_all` and `write_all_bytes` are the looping form: they resume from where
the last write stopped and only stop early when a write accepts nothing at all,
which means the reader is gone. Use them unless you have a specific reason to
handle the count yourself. The same pair exists at every level:

- FD level: `std.net.tcp`'s `write` / `write_all` / `write_all_bytes`
- Stream level: `TcpStream`, `FileStream`, `ProcessStdin`
- TLS: `Conn.write_bytes` is capped at one 16 KiB record, so it too is a partial
  write by construction; `Conn.write_all_bytes` is the loop
- The buffered writers flush through `write_all`, so they are already correct

An empty write is a success that returns 0, at every level and on every
target: a socket, a file, a child's stdin, a byte buffer. It makes no syscall,
so it cannot tell you whether the peer is still there, but it still fails on a
handle that was never opened or has been closed. A write that fails carries the
reason in its error: `tcp_write failed: Broken pipe (os error 32)` for a peer
that hung up, `file_write failed: Bad file descriptor (os error 9)` for a file
opened for reading, `file_write failed: invalid or closed file handle` for a
closed one. The `<name> failed: ` prefix names the builtin underneath. Before
#1153 an empty write came back as an error with no reason attached. The
`write_all` loops stop on a write that accepts nothing, and a non-empty write
never returns 0 without an error, so they still end when the reader is gone.

The loop has to get one subtlety right: the resume runs on **bytes**, never on
text. A send buffer fills at whatever byte offset it fills at, and that offset
can be in the middle of a multi-byte character. A `String` cannot be cut there
(slicing one at a non-boundary offset stops the process), so the text
`write_all` helpers encode once and resume through their bytes counterpart.

## A read waits forever unless you say otherwise

A read on a socket has no deadline by default. It waits for its peer for as long
as the peer takes, which is the right default for a connection you opened and
expect an answer on, and the wrong one for a connection some stranger opened.

`tcp.set_timeout(fd, ms)`, or `stream.set_timeout(ms)` on a `TcpStream`, bounds
it. A read that reaches the deadline with nothing to show for it fails, and the
error does not distinguish that from a connection that closed; both mean "no
more bytes are coming". Passing `0` (or less) clears the deadline and puts the
socket back to waiting indefinitely. The deadline covers one read call, not a
whole conversation, so a read that returns bytes leaves the next one a full
deadline again.

The deadline holds on both concurrency backends, through the same mechanism on
each. It lives on the socket (`SO_RCVTIMEO`), so the OS-thread backend gets it
from the kernel bounding its blocking read, and the green backend reads the same
value back and bounds its reactor wait by it. It used to hold only on the
OS-thread backend, which made every read timeout in the stdlib inert in the
default configuration: a client that connected and sent nothing pinned a server
task for good.

The deadline does not reach a pipe. `SO_RCVTIMEO` is a socket option, so a
child process's stdout has no deadline to set and a read on it waits for the
child. Bound a child with `process.run_ctx`/`output_ctx` and a context deadline
instead.

Nothing sets a deadline for you: a fresh connection has no read deadline, on
either backend. `connect` hands back a socket that waits indefinitely, so a
client whose reads must be bounded sets one explicitly.

## Why the adapters are handle-backed

When this layer was written, Pith structs were value types, so a tiny adapter
struct could not mutate its own fields and expect the caller to observe that
state after it was passed around.

The bridge was to keep mutable adapter state in module-level tables and pass
around tiny structs that only hold handles into that state. Structs are shared
now (a field written through one copy is visible through every other), so the
handle tables are no longer required, but the adapters still use them.

## What a value releases when it dies

`close()` and `free()` are still how you say when a stream is done. Each of
these types also implements `Drop` (see "destructors" in docs/ownership.md),
so a value nobody closed releases what it holds when its last value dies:

- `StringReader`, `StringBuffer`, `BytesCursor`, `BufferedReader`,
  `BufferedWriter`, `BufferedBytesReader` and `BufferedBytesWriter` give back
  their registry entries, and a string buffer frees its storage. A buffered
  reader or writer releases its source or target with the entry, so a file
  under it closes then if nothing else holds the file.
- `FileStream` closes its file.
- A `Process` and its `stdin()`, `stdout()` and `stderr()` views share one
  `ProcessHandle`, so the child is closed when the last of them dies. A view
  outlives the `Process` it came from. Closing does not wait for the child or
  kill it, and a child still running is reaped without blocking once it exits.
- `std.bytes.ByteBuffer` and `ByteArray` free their storage when the last
  value naming them dies. A buffer's `free()` and `take_bytes()` hand the
  contents back early and leave an empty buffer that still works.

A destructor does no I/O. A buffered writer that dies with text in it drops the
text, as `close()` does: call `flush()` first. A file is unbuffered, so its
writes have already reached the OS.

`TcpStream` has no destructor. It is a view of a socket, and std builds one
over sockets that other code owns, so whoever opened the socket closes it.

The registry handles come from one process-wide counter. A value can die on a
task other than the one that built it, and its destructor then looks in that
task's registry, where it finds nothing and does nothing, which is also what a
`close()` on that task does. The entry it was built with stays in the first
task's registry until that task ends.

## The buffered readers and writers hold an interface value

A buffered text reader is one type, `BufferedReader`, whatever it reads from.
Its source is an interface value of type `TextSource`, which has two methods:
`read_chunk` for a plain read of raw bytes and `read_chunk_ctx` for the same
read with a context. A refill calls one of them through the interface, and
nothing else in `read`, `read_line` or `read_until` depends on the source. The
writer side is the same: `BufferedWriter` flushes into a `TextTarget`, which
has `write_all` and `write_all_ctx`. The bytes side uses the existing
interfaces: `BufferedBytesReader` reads from a `BytesReader` and
`BufferedBytesWriter` writes to a `BytesWriter`.

A new source or target needs only an impl of the interface; `buffered_reader`,
`buffered_writer`, `buffered_bytes_reader` and `buffered_bytes_writer` take
any value that implements it. Dispatching through the interface costs one
indirect call per fill or flush.

`TextSource` does not reuse `BytesReader`'s method name because one type cannot
declare the same method in two interface impls, and the streams already
implement `BytesReader`.

The deadline-aware reads and writes (`read_ctx`, `read_line_ctx`,
`read_until_ctx`, `write_ctx`, `flush_ctx`) are a second implementation,
because they return a different error type and the context has to reach the
source read itself. Every text buffered reader and writer (`BufferedReader` and
`BufferedWriter`, regardless of source or target) offers them. A string, a
string buffer and a file never block, so their `_ctx` calls check the context
once and then do the plain call.

Two properties are worth knowing when reading that code, because both are easy
to lose:

- **The result is gathered, not appended.** A Pith `String` is immutable, so
  growing one in a loop rebuilds it on every step and reading n bytes costs on
  the order of n squared. Each read collects its pieces and joins them once.
- **The buffer has a read cursor.** Consuming from the front moves an integer
  and does not re-slice the bytes behind it. Without that, taking a 60-byte line
  out of a 4 KiB buffer copies the remaining 4 KiB, and reading that buffer one
  byte at a time copies it 4096 times. The consumed prefix is dropped in one
  step when the next chunk arrives, which is also what keeps the buffer from
  growing without bound.

`read_line` returns the line without its terminator, and strips a CR before the
LF so a CRLF stream reads the same as an LF one. It tests the last **byte** for
that CR: a `String` is indexed in bytes, so slicing the last byte off a line
ending in any non-ASCII character asks for the tail of a multi-byte character,
which stops the process. A line ending in "é" is ordinary text and now reads
back as ordinary text.

At the end of input `read_line` returns `""`, the same value as an empty line.
`read_line_opt` returns `String?!` instead: the line, or `none` once the
input is used up. A last line with no terminator still comes back before the
`none`. `read_line_opt_ctx` is the deadline-aware form.

```pith
while true:
    line := reader.read_line_opt()!
    if line == none:
        break
    handle(line.value())
```

## Closing a child's stdin

`ProcessStdin` implements `Closer`. `close()` closes the write end of the pipe,
so a child that reads to the end of its input, like `sort` or `wc`, sees end of
file and can finish. A write after the close fails, and a second close does
nothing. A `BufferedWriter` over the pipe holds text until it flushes, so
flush it before closing the stdin:

```pith
child := process.command("sort").start()!
writer := io.buffered_writer(child.stdin(), 0)
writer.write("pear\napple\n")!
writer.flush()!
child.stdin().close()
sorted := child.stdout().read_all()!
```

## A byte count is not a character boundary

Every transfer here is counted in bytes, because that is what a read and a write
are. A `String` is also indexed in bytes, but it can only be **cut** where a
character starts, and cutting one anywhere else stops the process outright.
Those two facts meet wherever a count decides where a string ends, and the rule
throughout `std.io` is that the count says how far to go and the boundary beside
it says where to stop.

`read(n)` reads at most n bytes and stops at the last character boundary at or
before n. A read that would come back empty because n fell inside the very first
character takes that whole character instead: an empty read is how every caller
recognises end of stream, so a read must never make no progress. `write_n`
follows the same rule without the exception: it writes at most n bytes ending on
a boundary, and a count that falls inside the first character writes nothing.
`read_until`'s hold-back flush stops at a boundary too, and holds the odd byte
or two over to the next round.

The same count shows up once more from the other side. A source read of n bytes
ends wherever n lands, so the last character in a chunk can be missing its tail.
That chunk is not decodable on its own, and decoding chunk by chunk used to call
an ordinary character split across two reads a decode error, so reading a file
or a child's output as text failed outright whenever one accent straddled a
4 KiB boundary. The whole-stream reads gather bytes and decode once; the
buffered readers hold an incomplete trailing character back and decode it with
the next chunk. A stream that ends mid-character is still an error, because that
text is truncated.

Use `std.text` when you want to count characters instead of bytes. `read(n)`
and `write_n` still take a byte budget; they spend it in whole characters.

## Stdlib consumers on the shared path

Beyond the first toy adapters, real stdlib consumers now use the shared layer:
- `std.net.http`
- `std.net.websocket`
- `std.toml`
- `std.json`
- `std.log`

These modules sit on the bytes-first boundary:
- `std.bytes`
- `std.encoding`
- `std.hash`

These uses prove the design under actual request parsing, buffered body reads,
WebSocket framing, file-backed parsing, incremental writes, and process/file
integration, beyond synthetic helpers.

## What this buys us

The main gain is consistency. New stdlib work can start from one I/O vocabulary
instead of inventing new string/socket/file loops every time. That makes a few
things simpler:
- HTTP-style request and response handling
- WebSocket handshake and frame reads
- File-backed format parsers
- Process pipelines
- Tests that need cheap fake readers and writers
- Future protocol layers that want buffering and line reads

## What is still open

The foundation is in place, and some work remains:
- More stdlib consumers can move onto the shared path where it helps
- Scanner-style or framed helpers may still be worth adding if real users want
  them
- The text-heavy modules still need a longer migration from string-first helper
  paths onto the newer bytes-first transport surface where that helps
- A few older builtin shortcuts are worth cleaning up when they get in the way

## Direction

The long-term version of Pith I/O should be more protocol-friendly and more
bytes-first than the original string-only start.

That is already visible in two places:
- `std.net.http` can preserve request bodies as `Bytes` and only decode headers
  or bodies when the caller asks for text
- `std.net.websocket` now has bytes-first handshake, frame, and session helpers,
  including `dial(...)`, `accept(...)`, `upgrade(...)`, buffered frame reads,
  and an in-memory `from_buffered(...)` path that keeps protocol tests
  deterministic

The right way to get there was to land a useful shared core first, move real
stdlib code onto it, and then extend the shape from working users instead of
trying to design the perfect abstraction in advance.
