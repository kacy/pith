# io foundation

forge now has one shared io layer that the stdlib can build on instead of each
module rolling its own transport loops.

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

the current runtime is still string-oriented, so the first pass is string-first
too:
- `fn read(max_bytes: Int) -> String!`
- `fn write(data: String) -> Int!`

that is enough to unify the common loops today without waiting for a bytes
layer.

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
- buffered readers and writers for string, tcp, process, and file streams
- line-oriented reads on top of those buffered readers
- concrete helpers for `read_all`, `write_all`, and copy-style flows
- plain file text helpers built on top of the file stream path

`std.fs` now exposes stream-based `open`, `create`, and `open_append` on the
same foundation.

## why the adapters are handle-backed

forge structs are value types right now. that means a tiny adapter struct cannot
just mutate internal fields and expect the caller to observe that state after it
gets passed around.

the practical bridge is to keep mutable adapter state in module-level tables and
pass around tiny structs that only hold handles into that state. it is not the
final forever shape, but it gives forge stable buffered and stateful io today
without waiting on a larger ownership model.

## stdlib consumers on the shared path

the point of this work was not to stop at toy adapters.

real stdlib consumers now use the shared layer:
- `std.net.http`
- `std.csv`
- `std.toml`
- `std.json`
- `std.log`

that matters because it proves the design under actual request parsing,
buffered body reads, file-backed parsing, incremental writes, and process/file
integration instead of only synthetic helpers.

## what this buys us

the main win is consistency.

new stdlib work can start from one io vocabulary instead of inventing new
string/socket/file loops every time. that makes a few things simpler:
- http-style request and response handling
- file-backed format parsers
- process pipelines
- tests that need cheap fake readers and writers
- future protocol layers that want buffering and line reads

## what is still open

the foundation is in place, but there is still room to grow:
- more stdlib consumers can move onto the shared path where it actually helps
- scanner-style or framed helpers may still be worth adding if real users want
  them
- a future bytes-first layer would be a better long-term shape than staying
  string-only forever
- there are still a few older builtin shortcuts worth cleaning up when they get
  in the way

## direction

the long-term version of forge io should probably be more protocol-friendly and
more bytes-first.

but the right way to get there was to land a useful shared core first, move
real stdlib code onto it, and then extend the shape from working users instead
of trying to design the perfect abstraction in advance.
