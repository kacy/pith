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
```

the zig runtime library is built from `cranelift/runtime-zig/` and linked as
`libforge_runtime_zig.a`.

## current scope

the first slice only covers the runtime surface needed for:

- c-string printing and comparisons
- simple string concatenation helpers
- int and bool to string conversion
- 8-byte list storage and iteration
- string-key and int-key map smoke paths
- int set smoke paths

it does not aim for stdlib or backend parity yet. if a forge program needs a
runtime symbol the zig experiment does not implement, the native link step will
fail and that is expected for now.

## abi boundary

the shared list layout constants live in `cranelift/runtime-abi/list_layout.json`.
the rust codegen path reads that manifest, and the zig runtime consumes the same
values during its build.
