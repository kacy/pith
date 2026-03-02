# forge

a programming language where any coding agent is immediately productive.

no panics, no null, no data races. automatic memory management via ARC with
compile-time cycle prevention. result types everywhere. designed so that AI
coding agents can read the errors, apply fixes, and iterate — fast.

**status:** early bootstrap. the compiler is being written in zig and will
self-host once the language is expressive enough.

## building

requires [zig 0.15.2](https://ziglang.org/download/).

```
zig build          # compile
zig build run      # compile and run
zig build test     # run tests
```

or with make:

```
make build
make test
make run
```

## license

MIT
