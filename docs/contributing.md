# Contributing

## Minimum setup

- Install Rust/Cargo
- Install a C toolchain (`cc` / `$CC`, for example GCC or Clang)
- Work from the repo root
- Prefer the self-hosted compiler for feature work
- Keep the Cranelift backend and self-hosted frontend healthy together

## Development loop

The smallest useful validation loop is:

```
cargo test -p pith-cli
./self-host/pith_main check <file>
make run-examples-self
make run-regressions-self
make bootstrap
```

If `./self-host/pith_main` does not exist yet, build it first:

```
make self-host
```

The recommended smoke loop for this repo is:

```
cargo build --release
./target/release/pith run examples/hello.pith
make self-host
./self-host/pith_main check examples/hello.pith
make run-examples-self
make run-regressions-self
make bootstrap
```

## Change discipline

1. Read the relevant subsystem before editing
2. Keep changes behavior-preserving unless the task explicitly changes semantics
3. Add or preserve stable error codes for new diagnostics
4. Validate the bootstrap and self-hosted paths for compiler changes, and
   regenerate the tracked seed when you change what the compiler emits
5. Prefer small helpers and explicit ownership over long inline flows

## Example style

- Prefer interpolation for direct value printing: `print("count: {items.len()}")`
- Use `std.fmt` when you need a reusable template, literal braces, or common collection display helpers
- Use `std.collections` helpers like `map_list`, `filter_list`, `fold_list`, and `count_by` for straightforward list transforms
- Use `std.io.string_buffer()` for incremental text assembly in loops or builders
- Avoid long chains of `"..." + value.to_string()` in user-facing examples unless interpolation would be awkward
- Inside a `test` block, use the built-in `assert(...)` / `assert_eq(...)`. A failing `std.testing` check also fails the test, but the built-ins read better (see [docs/testing.md](testing.md))
- Prefer `std.os.process.command(...)` for child processes; use `std.io` when you specifically need lower-level stream types
- Remember that collections are shared handles; reach for `std.collections.copy_list(...)`, `copy_map(...)`, or `copy_set(...)` when an example wants an independent top-level container
- Prefer typed results like `T!E` when callers need to inspect the error payload; keep bare `T!` for simpler string-error paths
- Use `catch`, `unwrap_or(...)`, and `or_else(...)` in examples when they make recovery intent clearer than manual `is_err` branching

## Code in the docs

Every ```` ```pith ```` block in README.md and docs/*.md is type checked by
`make doc-examples-check`, which CI runs. `pith doc --check file.md` checks one
file and reports each error at the Markdown file's line and column.

The check reads a block the way a person reads it, so most blocks need no
markup:

- A block with a top-level `fn main` is a program and is checked as it stands.
  In any other block the imports, functions, types and constants named in
  capitals stay at the top level, and the other lines are checked as the body
  of a function that may fail.
- A name the block never defines, such as `source`, `req` or `User`, is taken
  to come from the code around it. So are the error type of a `!` and the type
  of a `return` in a fragment's statements.
- A name used as `name.member`, where `name` is the last part of a std module
  that has that member, is imported for the check, so `markdown.to_html(s)`
  needs no import line. A block that uses only members a module no longer has
  is not caught this way; import the module in the block to check it strictly.
- An import of a module that is not in std and cannot be found, such as
  generated code, is left out, and the names it would bind are free.
- A line that starts with `...` stands for code left out, and a function
  signature with no body under it, as in a list of an API, gets an empty one.

Mark a block ```` ```pith no-check ```` when it shows an error on purpose or a
syntax that does not exist yet. Renderers take the first word of the info
string as the language, so the block is still highlighted as Pith.

## Where to work

- Native backend CLI: `cranelift/cli/src/main.rs`
- IR lowering and native code generation: `cranelift/codegen/src/`
- Self-hosted implementation: `self-host/`
- Runtime support: `cranelift/runtime/src/`
- Native TLS and higher-level protocol work: `std/net/tls.pith`, `std/net/tls13.pith`, `std/net/tls12.pith`, `std/net/http.pith`, `std/net/http2/*.pith`, `std/net/grpc.pith`, `std/net/websocket.pith`
- Language and diagnostic docs: `docs/`

## Common validation commands

```
cargo test -p pith-cli
cargo build --release
./target/release/pith run examples/hello.pith
make self-host
./self-host/pith_main run examples/hello.pith
make run-examples-self
make run-regressions-self
make bootstrap
```

## The tracked bootstrap seed

`self-host/bootstrap/ir_driver.ir` is a generated file, never written by hand.
It exists so a machine with no `ir_driver` binary can build one, which is the
only way a fresh clone gets off the ground. `pith` also rebuilds
`self-host/ir_driver` from it when the seed is newer than the binary, as it is
after a pull or checkout that moved the compiler, because the old binary still
emits the old IR. Apart from that, nothing in a working tree reads the seed, so
it goes stale quietly.

Regenerate it whenever you change anything the compiler emits from. That is
more than the emitter: `ir_driver.pith` imports the parser and the checker, so
a one-line parser edit moves it too.

```
make refresh-bootstrap-seed     # regenerate, then commit the file
make check-bootstrap-seed       # does the tracked seed match what this source emits?
make smoke-bootstrap-seed       # can a fresh clone bootstrap from it?
```

Both checks run in CI, and neither one closes the gap by itself. A branch
regenerates against its own base, so it stays self-consistent and passes even
when the trunk has moved underneath it. When several seed-touching branches
merge in a row, whichever lands last carries a seed generated against a base
that no longer exists, silently replaces the others, and leaves the trunk
unbuildable from scratch. So rebase onto the trunk before merging a change that
regenerates the seed, and merge those one at a time.

If `check-bootstrap-seed` fails on a branch where you changed nothing that
emits, do not just regenerate: the seed was already stale before you got there,
and regenerating buries someone else's problem in your diff. Rebase first.

For TLS-facing changes, add a live sanity check after the normal loop:

```
./self-host/pith_main run tests/live/test_tls_echo_live.pith
```
