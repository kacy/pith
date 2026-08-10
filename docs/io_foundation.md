# io foundation

pith now has one shared io layer that the stdlib can build on instead of each
module rolling its own transport loops.

for the higher-level string/bytes helper surface on top of that split, see
`docs/text_and_bytes.md`.

before this work, the stdlib had a few unrelated io styles:
- `std.fs` leaned on whole-file helpers
- `std.net.tcp` was fd-based
- `std.os.process` was handle-based
- `std.io` was basically prompt helpers

that was fine for small modules, but it made buffering, line reads, copy loops,
and protocol framing show up over and over again in slightly different forms.

## what exists now

`std.io` is the shared layer.

the core surface is intentionally small:
- `Reader`
- `Writer`
- `Closer`
- `Flusher`

the original shared layer was string-first, but it now has an explicit bytes
side too.

text-facing surface:
- `fn read(max_bytes: Int) -> String!`
- `fn write(data: String) -> Int!`

bytes-facing surface:
- `fn read_bytes(max_bytes: Int) -> Bytes!`
- `fn write_bytes(data: Bytes) -> Int!`

the split is intentional: raw transport data and text are different things, so
pith now treats them as different things in the stdlib too.

the compiler now also supports module-qualified import aliases cleanly, so
stdlib call sites can read like:
- `import std.io as io`
- `import std.json as json`
- `reader := io.string_reader(data)`
- `text := io.read_all(reader)!`
- `line := buffered.read_line()!`
- `stream.close()`
- `conn := io.connect(host, port)!`
- `root := json.parse(text)`

## shared pieces

the io layer now includes:
- handle-backed in-memory readers and writers for simple composition and tests
- handle-backed bytes readers and buffers for raw data paths
- buffered readers and writers for string, tcp, process, and file streams
- line-oriented reads on top of those buffered readers
- concrete helpers for `read_all`, `write_all`, and copy-style flows
- plain file text helpers built on top of the file stream path
- bytes file and process helpers built on the same stream types

`std.fs` now exposes stream-based `open`, `create`, and `open_append` on the
same foundation.

## write is one syscall, write_all is the loop

this is the one distinction in the layer worth learning before you use it.

`write` and `write_bytes` are a single `write(2)`. a socket has a bounded send
buffer, so a buffer larger than the space left in it is written *in part*: the
kernel takes what fits and returns that count, and the rest is not queued
anywhere. a short write is normal, not an error — but discarding the returned
count drops the tail silently, and the peer then waits for bytes that were never
written. a 1 MiB redis `SET` used to work and a 4 MiB one used to hang for
exactly this reason.

`write_all` and `write_all_bytes` are the looping form: they resume from where
the last write stopped and only stop early when a write accepts nothing at all,
which means the reader is gone. use them unless you have a specific reason to
handle the count yourself. the same pair exists at every level:

- fd level: `std.net.tcp`'s `write` / `write_all` / `write_all_bytes`
- stream level: `TcpStream`, `FileStream`, `ProcessStdin`
- tls: `Conn.write_bytes` is capped at one 16 KiB record, so it too is a partial
  write by construction; `Conn.write_all_bytes` is the loop
- the buffered writers flush through `write_all`, so they are already correct

one subtlety the loop has to get right: the resume runs on **bytes**, never on
text. a send buffer fills at whatever byte offset it fills at, and that offset
can be in the middle of a multi-byte character. a `String` cannot be cut there —
slicing one at a non-boundary offset stops the process — so the text `write_all`
helpers encode once and resume through their bytes counterpart.

## a read waits forever unless you say otherwise

a read on a socket has no deadline by default. it waits for its peer for as long
as the peer takes, which is the right default for a connection you opened and
expect an answer on, and the wrong one for a connection some stranger opened.

`tcp.set_timeout(fd, ms)` — or `stream.set_timeout(ms)` on a `TcpStream` — bounds
it. a read that reaches the deadline with nothing to show for it fails, and the
error does not distinguish that from a connection that closed; both mean "no
more bytes are coming". passing `0` (or less) clears the deadline and puts the
socket back to waiting indefinitely. the deadline covers one read call rather
than a whole conversation, so a read that returns bytes leaves the next one a
full deadline again.

it holds on both concurrency backends, and the mechanism is the same one on
each: the deadline lives on the socket (`SO_RCVTIMEO`), so the os-thread backend
gets it from the kernel bounding its blocking read, and the green backend reads
the same value back and bounds its reactor wait by it. it used to hold on only
the os-thread backend, which made every read timeout in the stdlib inert in the
default configuration — a client that connected and sent nothing pinned a server
task for good.

two things it does not reach:

- **a pipe.** `SO_RCVTIMEO` is a socket option, so a child process's stdout has
  no deadline to set and a read on it waits for the child. bound a child with
  `process.run_ctx`/`output_ctx` and a context deadline instead.
- **a connect.** on the os-thread backend `connect` hands back a socket with a
  five second read deadline already on it; on the green backend it does not. set
  one explicitly on a client connection whose reads must be bounded rather than
  relying on the difference.

## why the adapters are handle-backed

pith structs are value types right now. that means a tiny adapter struct cannot
just mutate internal fields and expect the caller to observe that state after it
gets passed around.

the practical bridge is to keep mutable adapter state in module-level tables and
pass around tiny structs that only hold handles into that state. it is not the
final forever shape, but it gives pith stable buffered and stateful io today
without waiting on a larger ownership model.

## the buffered text readers are one reader

`BufferedStringReader`, `BufferedTcpStream`, `BufferedFileReader`,
`BufferedProcessStdout` and `BufferedProcessStderr` are five wrapper types over
one implementation. they differ in exactly one thing — which function a refill
calls to get the next chunk — so a reader carries a source tag and there is a
single `read`, `read_line` and `read_until` rather than five copies of each.

the deadline-aware reads are the one split that is real. `read_ctx`,
`read_line_ctx` and `read_until_ctx` return a different error type, and the
context has to reach the source read itself, since waiting for a chunk is the
only part of a buffered read that can block. so those are a second
implementation, and a string reader and a file — neither of which blocks — do
not offer them.

two properties are worth knowing when reading that code, because both are easy
to lose:

- **the result is gathered, not appended.** a pith `String` is immutable, so
  growing one in a loop rebuilds it on every step and reading n bytes costs on
  the order of n squared. each read collects its pieces and joins them once.
- **the buffer has a read cursor.** consuming from the front moves an integer;
  it does not re-slice the bytes behind it. without that, taking a 60-byte line
  out of a 4 KiB buffer copies the remaining 4 KiB, and reading that buffer one
  byte at a time copies it 4096 times. the consumed prefix is dropped in one
  step when the next chunk arrives, which is also what keeps the buffer from
  growing without bound.

`read_line` returns the line without its terminator, and strips a CR before the
LF so a CRLF stream reads the same as an LF one. it tests the last **byte** for
that CR: a `String` is indexed in bytes, so slicing the last byte off a line
ending in any non-ascii character asks for the tail of a multi-byte character,
which stops the process. a line ending in "é" is ordinary text and now reads
back as ordinary text.

## a byte count is not a character boundary

every transfer here is counted in bytes, because that is what a read and a write
are. a `String` is also indexed in bytes — but it can only be **cut** where a
character starts, and cutting one anywhere else stops the process outright.
those two facts meet wherever a count decides where a string ends, and the rule
throughout `std.io` is that the count says how far to go and the boundary beside
it says where to stop.

`read(n)` reads at most n bytes and stops at the last character boundary at or
before n. a read that would come back empty because n fell inside the very first
character takes that whole character instead: an empty read is how every caller
recognises end of stream, so a read must never make no progress. `write_n` is
the same rule without the exception — it writes at most n bytes ending on a
boundary, and a count that falls inside the first character writes nothing.
`read_until`'s hold-back flush stops at a boundary too, and holds the odd byte
or two over to the next round.

the same count shows up once more from the other side. a source read of n bytes
ends wherever n lands, so the last character in a chunk can be missing its tail.
that chunk is not decodable on its own, and decoding chunk by chunk used to call
an ordinary character split across two reads a decode error — reading a file or
a child's output as text failed outright whenever one accent straddled a 4 KiB
boundary. the whole-stream reads gather bytes and decode once; the buffered
readers hold an incomplete trailing character back and decode it with the next
chunk. a stream that ends mid-character is still an error, because that is
genuinely truncated text.

use `std.text` when you want to count characters rather than bytes; `read(n)`
and `write_n` still take a byte budget, they just spend it in whole characters.

## stdlib consumers on the shared path

the point of this work was not to stop at toy adapters.

real stdlib consumers now use the shared layer:
- `std.net.http`
- `std.net.websocket`
- `std.csv`
- `std.toml`
- `std.json`
- `std.log`

and the bytes-first boundary is real too:
- `std.bytes`
- `std.encoding`
- `std.hash`

that matters because it proves the design under actual request parsing,
buffered body reads, websocket framing, file-backed parsing, incremental
writes, and process/file integration instead of only synthetic helpers.

## what this buys us

the main win is consistency.

new stdlib work can start from one io vocabulary instead of inventing new
string/socket/file loops every time. that makes a few things simpler:
- http-style request and response handling
- websocket handshake and frame reads
- file-backed format parsers
- process pipelines
- tests that need cheap fake readers and writers
- future protocol layers that want buffering and line reads

## what is still open

the foundation is in place, but there is still room to grow:
- more stdlib consumers can move onto the shared path where it actually helps
- scanner-style or framed helpers may still be worth adding if real users want
  them
- the text-heavy modules still need a longer migration from string-first helper
  paths onto the newer bytes-first transport surface where that actually helps
- there are still a few older builtin shortcuts worth cleaning up when they get
  in the way

## direction

the long-term version of pith io should be more protocol-friendly and more
bytes-first than the original string-only start.

that is already visible in a couple of places:
- `std.net.http` can preserve request bodies as `Bytes` and only decode headers
  or bodies when the caller asks for text
- `std.net.websocket` now has bytes-first handshake, frame, and session helpers,
  including `dial(...)`, `accept(...)`, `upgrade(...)`, buffered frame reads,
  and an in-memory `from_buffered(...)` path that keeps protocol tests
  deterministic

but the right way to get there was to land a useful shared core first, move
real stdlib code onto it, and then extend the shape from working users instead
of trying to design the perfect abstraction in advance.
