# limitations

pith is self-hosting and runs real programs, but it is not finished. this page
is the honest list of what does not work yet, so you can plan around it instead
of discovering it the hard way. it is kept current with the compiler; if you hit
something here that now works, the page is stale and a fix to it is welcome.

## language

- **named struct construction for generic structs** — concrete structs support
  named fields (`Point(x: 3, y: 4)`, any order, defaults may be omitted), but
  generic structs like `Box[T]` still need positional arguments; the type
  inference doesn't reorder named fields yet.
- **match pattern gaps** — variant payloads construct and destructure
  (`Shape.Circle(2.0)`, `Shape.Circle(r) => r * r`, with literal sub-patterns,
  guards, and payload bindings inside or-patterns like
  `Circle(r) | Square(r)`, tuple patterns with bindings, wildcards, and
  literal elements, and `none` patterns on optional subjects — a bare
  binding arm unwraps the some case, mirroring `if let`). note a bare name in a pattern is a binding, not a variant — write
  `Color.Red`, not `Red`, to match a variant. equality (`==`) on
  payload-carrying enums compares structurally: tags, then payloads, with
  string payloads by content and nested enums recursively; struct payloads
  compare by identity.
- **range patterns are integer-only** — `0..=9 => ...` and `0..10 => ...`
  work in match arms (and combine with or-patterns and guards), but only for
  integer subjects and non-negative literal bounds.
- **`if let` / `while let` are statement-only** — `if let Shape.Circle(r) = s:`
  destructures a variant and `if let v = maybe():` unwraps an optional, but
  neither form works as an expression, and literal patterns are not allowed
  in the let position.
- **closure capture cap** — a closure captures at most 16 variables. captures
  are heap-allocated per closure instance, so nesting and recursion are safe;
  the cap is a fixed ceiling, not a correctness issue. (multi-line closure
  bodies — `fn(x):` with an indented block — work and infer their return
  type from their return statements.)

## standard library

- **no regex** — there is no regular-expression engine. for structured text,
  reach for `std/text/scanner` or hand-written parsing.
- **tls 1.2** — tls is 1.3 only, client and server. there is no 1.2
  compatibility mode, so peers that cannot speak 1.3 will not connect.
- **testing** — `std/testing` covers assertions but not discovery, fixtures, or
  parameterized cases. the project's own suite is golden-snapshot based (see
  `tests/`).
- **http/2** — the http stack is 1.1 only.
- **regex is deliberately small** — `std.regex` covers literals, `.`,
  classes, `\d \w \s` escapes, `* + ?` (greedy), alternation, capturing
  groups, and `^ $` anchors. it does not support `{n,m}` counts, lazy
  quantifiers, backreferences, or lookaround. matching is a pike vm, so
  time is linear in the input for any pattern.
- **gzip compresses with fixed huffman only** — `std.compress.gzip`
  reads any deflate stream (multi-member files included) and writes
  real compression (greedy lz77 over fixed huffman, stored-block
  fallback for incompressible data; system gunzip reads its output,
  and zlib routes through the same engine). dynamic huffman trees on
  the write side would shave a few more percent and are the one
  remaining refinement.

## tooling

- **no language server** — there is no lsp; editor support is limited to syntax.
- **no package registry** — dependencies are local path entries in `pith.toml`;
  there is no fetch, lock, or hosted index yet.
- **no debugger** — runtime stack traces are thin and there is no stepping.

## backend

these are internal and do not usually surface in source, but they shape the
correctness story:

- the ir is self-describing — calls carry an explicit return kind and field
  loads carry their type — and the cranelift consumer reads that metadata rather
  than guessing. the older inference path that caused a few cross-module bugs is
  gone.
- memory reclamation is compiler-emitted reference counting (see the readme's
  memory section). the deliberate gaps: closure environments never free,
  reference cycles leak, container element removal leaks until the container
  dies, and error-path early returns skip cleanup. all are bounded-leak by
  design — the discipline never produces a dangling pointer in exchange.
- a handful of edge cases logged during bring-up (cross-module float returns,
  cross-module map reads, set codegen, negative float literals like `-1.0`) were
  re-checked and all pass; they are now pinned by regression tests
  (`tests/cases/test_xmod_float.pith` and friends).
