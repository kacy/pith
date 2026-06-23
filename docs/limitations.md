# limitations

pith is self-hosting and runs real programs, but it is not finished. this page
is the honest list of what does not work yet, so you can plan around it instead
of discovering it the hard way. it is kept current with the compiler; if you hit
something here that now works, the page is stale and a fix to it is welcome.

## language

- **named struct construction** — fields are positional only. `Point(3, 4)`
  works; `Point(x: 3, y: 4)` does not. named arguments work for function and
  method calls, just not for struct literals. adding a field is therefore a
  breaking change to every call site.
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

- the cranelift ir consumer still infers some metadata (call return kinds, field
  offsets) rather than reading it from the ir. this is the historical source of
  a few cross-module bugs and is being moved to explicit ir.
- a handful of older edge cases (cross-module float returns, cross-module map
  reads, set codegen, negative float literals like `-1.0`) were logged during
  bring-up and may or may not still reproduce; treat them as suspect until a
  test pins them down.
