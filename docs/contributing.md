# contributing

## minimum setup

- install zig 0.15.2
- work from the repo root
- prefer the self-hosted compiler for feature work
- keep the zig bootstrap healthy because it is the safest refactor harness

## development loop

the smallest useful validation loop is:

```
zig build test
./self-host/forge_main check <file>
make run-examples-self
make run-regressions-self
make bootstrap
```

if `./self-host/forge_main` does not exist yet, build it first:

```
make self-host
```

recommended smoke loop for this repo:

```
zig build test
zig build run -- check examples/hello.fg
make self-host
./self-host/forge_main check examples/hello.fg
make run-examples-self
make run-regressions-self
make bootstrap
```

## change discipline

1. read the relevant subsystem before editing
2. keep changes behavior-preserving unless the task explicitly changes semantics
3. add or preserve stable error codes for new diagnostics
4. validate the bootstrap and self-hosted paths for compiler changes
5. prefer small helpers and explicit ownership over long inline flows

## example style

- prefer interpolation for direct value printing: `print("count: {items.len()}")`
- use `std.fmt` when you need a reusable template or literal braces
- avoid long chains of `"..." + value.to_string()` in user-facing examples unless interpolation would be awkward

## where to work

- CLI and bootstrap orchestration: `bootstrap/main.zig`, `bootstrap/cli/`, `bootstrap/pipeline.zig`
- bootstrap semantic logic: `bootstrap/checker.zig`
- bootstrap code generation: `bootstrap/codegen.zig`
- self-hosted implementation: `self-host/`
- runtime support: `runtime/forge_runtime.h`
- language and diagnostic docs: `docs/`

## common validation commands

```
zig fmt --check build.zig bootstrap/*.zig bootstrap/cli/*.zig
zig build test
zig build run -- check examples/hello.fg
make self-host
./self-host/forge_main run examples/hello.fg
make run-examples-self
make run-regressions-self
make bootstrap
```
