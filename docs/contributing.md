# contributing

## minimum setup

- install rust/cargo
- install a C toolchain with `gcc`
- work from the repo root
- prefer the self-hosted compiler for feature work
- keep the Cranelift backend and self-hosted frontend healthy together

## development loop

the smallest useful validation loop is:

```
cargo test -p pith-cli
./self-host/pith_main check <file>
make run-examples-self
make run-regressions-self
make bootstrap
```

if `./self-host/pith_main` does not exist yet, build it first:

```
make self-host
```

recommended smoke loop for this repo:

```
cargo build --release
./target/release/pith run examples/hello.pith
make self-host
./self-host/pith_main check examples/hello.pith
make run-examples-self
make run-regressions-self
make bootstrap
```

## change discipline

1. read the relevant subsystem before editing
2. keep changes behavior-preserving unless the task explicitly changes semantics
3. add or preserve stable error codes for new diagnostics
4. validate the bootstrap and self-hosted paths for compiler changes, and
   regenerate the tracked seed when you change what the compiler emits
5. prefer small helpers and explicit ownership over long inline flows

## example style

- prefer interpolation for direct value printing: `print("count: {items.len()}")`
- use `std.fmt` when you need a reusable template, literal braces, or common collection display helpers
- use `std.collections` helpers like `map_list`, `filter_list`, `fold_list`, and `count_by` for straightforward list transforms
- use `std.io.string_buffer()` for incremental text assembly in loops or builders
- avoid long chains of `"..." + value.to_string()` in user-facing examples unless interpolation would be awkward
- inside a `test` block use the built-in `assert(...)` / `assert_eq(...)`; a failing `std.testing` check only tallies and will not fail the test (see [docs/testing.md](testing.md))
- prefer `std.os.process.command(...)` for child processes; use `std.io` when you specifically need lower-level stream types
- remember that collections are shared handles; reach for `std.collections.copy_list(...)`, `copy_map(...)`, or `copy_set(...)` when an example wants an independent top-level container
- prefer typed results like `T!E` when callers need to inspect the error payload; keep bare `T!` for simpler string-error paths
- use `catch`, `unwrap_or(...)`, and `or_else(...)` in examples when they make recovery intent clearer than manual `is_err` branching

## where to work

- native backend CLI: `cranelift/cli/src/main.rs`
- IR lowering and native code generation: `cranelift/codegen/src/`
- self-hosted implementation: `self-host/`
- runtime support: `cranelift/runtime/src/`
- native tls and higher-level protocol work: `std/net/tls.pith`, `std/net/tls13.pith`, `std/net/http.pith`, `std/net/websocket.pith`
- language and diagnostic docs: `docs/`

## common validation commands

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

## the tracked bootstrap seed

`self-host/bootstrap/ir_driver.ir` is generated, not written. it exists so a
machine with no `ir_driver` binary can build one, which is the only way a fresh
clone gets off the ground. nothing you run in a working tree reads it, because
your `ir_driver` is already there, so it goes stale quietly.

regenerate it whenever you change anything the compiler emits from. that is
more than the emitter: `ir_driver.pith` imports the parser and the checker, so
a one-line parser edit moves it too.

```
make refresh-bootstrap-seed     # regenerate, then commit the file
make check-bootstrap-seed       # does the tracked seed match what this source emits?
make smoke-bootstrap-seed       # can a fresh clone bootstrap from it?
```

both checks run in ci, and neither one closes the gap by itself. a branch
regenerates against its own base, so it stays self-consistent and passes even
when the trunk has moved underneath it. when several seed-touching branches
merge in a row, whichever lands last carries a seed generated against a base
that no longer exists, silently replaces the others, and leaves the trunk
unbuildable from scratch. so rebase onto the trunk before merging a change that
regenerates the seed, and merge those one at a time.

if `check-bootstrap-seed` fails on a branch where you changed nothing that
emits, do not just regenerate: the seed was already stale before you got there,
and regenerating buries someone else's problem in your diff. rebase first.

for tls-facing changes, add a live sanity check after the normal loop:

```
./self-host/pith_main run tests/live/test_tls_echo_live.pith
```
