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
- **match on variant payloads** — `match` lowers literals, qualified enum
  variants (`Color.Red`), bindings, wildcards, and guards. it does not yet
  destructure a variant's payload (`Circle(r) => r * r`) or match tuple
  patterns. note a bare name in a pattern is a binding, not a variant — write
  `Color.Red`, not `Red`, to match a variant.
- **or-patterns in match** — `1 | 2 | 3 => ...` is not parsed. write one arm per
  value, or fall through to a guard.
- **range patterns in match** — `0..=9 => ...` is not parsed. range syntax works
  in `for`, not in match arms.
- **`if let` / `while let`** — there is no pattern-binding unwrap. unwrap an
  optional with an explicit check: `if x != none: ... x.value()`.
- **closure capture cap** — a closure captures at most 16 variables. captures
  are heap-allocated per closure instance, so nesting and recursion are safe;
  the cap is a fixed ceiling, not a correctness issue.

## standard library

- **no regex** — there is no regular-expression engine. for structured text,
  reach for `std/text/scanner` or hand-written parsing.
- **tls server** — the client path works end to end; the server handshake is not
  implemented yet (`std/net/tls.pith` fails with a clear message).
- **testing** — `std/testing` covers assertions but not discovery, fixtures, or
  parameterized cases. the project's own suite is golden-snapshot based (see
  `tests/`).
- **http/2** — the http stack is 1.1 only.

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
  gone. the one remaining stub is `pith_string_retain`
  (`cranelift/runtime/src/string.rs`), which has no callers yet.
- a handful of edge cases logged during bring-up (cross-module float returns,
  cross-module map reads, set codegen, negative float literals like `-1.0`) were
  re-checked and all pass; they are now pinned by regression tests
  (`tests/cases/test_xmod_float.pith` and friends).
