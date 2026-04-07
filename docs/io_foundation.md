# io foundation

this branch is about giving forge one io vocabulary that the rest of the stdlib
can share.

right now the stdlib has a few different styles:
- `std.fs` is whole-file and path-based
- `std.net.tcp` is fd-based
- `std.os.process` is handle-based
- `std.io` is basically just stdin prompts

that works for small modules, but it doesn't scale into a great standard
library. every protocol or format layer ends up reinventing the same loops.

the branch has moved past the raw plumbing now. `std.net.http`, `std.fs`,
`std.csv`, `std.toml`, and `std.json` all share pieces of the same io path.

buffered file readers and writers now sit on top of `FileStream`, so chunked
and line-oriented file protocols can reuse the same shape as the tcp and
process adapters.

`std.csv` now uses those buffered file helpers directly for chunked reads and
writes instead of dropping back to whole-file string helpers.

`std.toml` can now come through the same imported-module path and parse config
files through buffered file reads instead of staying stuck behind builtin-only
special handling.

`std.json` is on that path too: imported calls can resolve through the module
path, and file-backed parse/save helpers can use the shared buffered file layer
instead of falling back to one-off whole-file plumbing.

## the target

we want something closer to the best parts of go's `io` package:
- tiny interfaces
- reusable helpers
- transports and formats layered on the same contracts
- easy testing with in-memory adapters

we should not copy go blindly, though. forge has different constraints:
- result types are explicit
- interfaces are best used as compile-time bounds today
- a lot of runtime io is string-based right now, not bytes-based
- plain structs are value types, so mutable adapters need care

## phase one shape

phase one is intentionally small:
- `Reader`
- `Writer`
- `Closer`
- `Flusher`
- `read_all`
- `write_all`
- `copy`
- handle-backed in-memory adapters for testing and composition

the first cut stays string-oriented:
- `fn read(max_bytes: Int) -> String!`
- `fn write(data: String) -> Int!`

that matches the runtime we already have. it also lets us start unifying code
in tcp, process, http, and future stdlib modules right away.

## why the in-memory adapters are handle-backed

forge structs are value types today. that means a helper like `copy(reader,
writer)` can't rely on mutating a caller-owned struct field and having that
mutation show up back at the call site.

for phase one, the practical workaround is simple: keep the adapter state in
module-level tables and pass around tiny wrapper structs that only hold a
handle. that gives us stable, observable state without waiting on reference
parameters or a bigger ownership model.

it's not the final shape forever, but it's a solid bridge.

## what makes this worth doing

once the core exists, a lot of stdlib work gets cheaper:
- http parsing and body handling can share read helpers
- websocket framing can sit on reader/writer contracts
- process pipelines can reuse copy loops
- future file streaming can plug into the same interfaces
- tests get easier because adapters are cheap to fake

the big win is consistency. new stdlib modules stop inventing one-off io loops.

## staged path

### milestone 1: done

land the core interfaces, handle-backed adapters, and concrete helpers in
`std.io`.

the concrete helpers matter because they let us prove the state model now,
without depending on every cross-module interface dispatch edge being perfect
yet.

### milestone 2: mostly done

add generic interface-driven helpers once that path is hardened in real code,
then wrap the current runtime surfaces:
- tcp connections
- process stdout/stderr/stdin

tcp and process wrappers are both in now, including buffered process output.
the remaining gap in this milestone is mostly polish: the interface surface is
real, but the native path still is not trustworthy enough for a heavier
generic-dispatch cleanup in the implementation.

the same rule still applies to deeper transport tests. if a full spawned tcp
roundtrip is shaky in the example path, the io branch should not hide that.
land wrapper-level progress here, then fix the runtime path directly and expand
the transport tests afterward.

### milestone 3: done

add real file-handle streaming to the runtime and move `std.fs` beyond
whole-file helpers.

that layer is in place. open file handles sit on the same reader and writer
surface as tcp and process streams, and `std.fs` exposes streaming
open/create/append helpers on top of it instead of stopping at whole-file
reads and writes.

### milestone 4: mostly done

add higher-level layers:
- buffered reader/writer
- line reader
- scanner-style helpers
- framed protocol helpers

buffered readers and writers now exist for string, tcp, process, and file.
line-oriented reads are in too. the remaining work here is the optional stuff:
scanner-style helpers, framed protocol helpers, and any nicer protocol-facing
layers that still pay for themselves after more real consumers land.

### milestone 5: mostly done

move higher stdlib modules onto the shared layer so the design proves itself in
real code, not just in toy examples.

http was the first real target here because it needs exactly the kind of loops
that get messy fast when every module rolls its own framing. that path is in
now. the request reader, response writer, and client fetch path can share the
same buffered tcp helpers instead of open-coding socket loops in multiple
places.

csv is in too, now using buffered file reads and writes directly. toml and json
both have file-backed helpers on the same foundation.

the main consumer work that still feels worth doing is:
- keep cleaning up builtin-only shortcuts that still bypass the module path
- fix the `forge run` / `forge_main run` wrapper weirdness that sometimes drops
  child stdout even when the built binaries are correct

`std.log` is off the special-case path now too. imported log calls can come
through the module, and the module can mirror logs into a file sink through the
shared file-stream helpers when a caller opts into it.

## the long-term version

the long-term version should be bytes-first and protocol-friendly. but the best
way to get there is not to wait for the perfect runtime surface. it's to land a
small useful core now, then extend it in place with real users.
