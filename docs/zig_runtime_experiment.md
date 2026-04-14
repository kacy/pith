# zig runtime experiment

this is a side-by-side experiment with the native rust runtime.

the default native path still links against the rust static library. the zig
runtime is opt-in and intentionally incomplete.

## what it proves

- the native cli can choose a runtime explicitly
- the backend/runtime boundary can be shared through a small abi manifest
- a second runtime implementation can link and run real forge programs

## how to use it

build or run with the runtime flag:

```sh
./target/release/forge run --runtime zig examples/hello.fg
./target/release/forge run --runtime zig tests/cases/test_global_list.fg
./target/release/forge run --runtime zig tests/cases/test_map_basic.fg
./target/release/forge run --runtime zig tests/cases/test_map_int_key.fg
./target/release/forge run --runtime zig tests/cases/test_set_int_smoke.fg
./target/release/forge test --runtime zig tests/cases/test_test_declarations.fg
```

the zig runtime library is built from `cranelift/runtime-zig/` and linked as
`libforge_runtime_zig.a`.

## current scope

the experiment is still incomplete, but it is well past the original smoke-only
slice now. the zig runtime currently covers:

- strings, bytes, byte buffers, and the common `std.strings` helper surface
- 8-byte lists, higher-order list helpers, maps, and sets
- time, random, channels, tasks, mutexes, waitgroups, and semaphores
- file io, env helpers, process spawn/output, tcp, dns, and the http/websocket
  helper paths those examples sit on top of
- the native `run` path plus the `test` path for real `test "..."` declarations

bounded examples that now run under `--runtime zig` include:

- `examples/hello.fg`
- `examples/json_ops.fg`
- `examples/toml_ops.fg`
- `examples/url_ops.fg`
- `examples/path_ops.fg`
- `examples/concurrency.fg`
- `examples/data_pipeline.fg`
- `examples/http_api.fg`
- `examples/http_apps.fg`
- `examples/http_websocket_app.fg`
- `examples/net_echo.fg`
- `examples/tcp_echo.fg`
- `examples/websocket_echo.fg`
- `examples/websocket_chat.fg`

it still does not aim for full rust-runtime parity. if a forge program needs a
runtime symbol the zig experiment does not implement yet, the native link step
will fail explicitly and that is still expected for now.

## abi boundary

the shared list layout constants live in `cranelift/runtime-abi/list_layout.json`.
the rust codegen path reads that manifest, and the zig runtime consumes the same
values during its build.
