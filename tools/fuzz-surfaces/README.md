# Parser surface fuzzing

`fuzz_surfaces.pith` mutates real-shaped inputs and feeds them to the std
parsers that read untrusted bytes.

| Target | Entry point |
| --- | --- |
| `http1` | `http.read_request_buffered_bytes`, then the request accessors (form, cookies, multipart, basic auth) |
| `http2` | `frames.read_frame` and every `frames.parse_*`, with HPACK on HEADERS |
| `hpack` | One `hpack.Decoder` across several header blocks |
| `h2server` | `server.serve_h2c_connection` over loopback TCP |
| `websocket` | `WebSocket.recv` over an in-memory reader (server side) |
| `yaml`, `toml`, `json` | `parse` / `parse_all` and `decode_scalars` / `decode_scalars_bytes` |
| `protobuf` | A generic walk: every field by wire type, nested messages, packed varints |
| `inflate` (plus `zlib`), `gzip`, `zstd` | The bounded decoders (`zstd` runs both the binding and `zstd_pure`) |
| `tar`, `zip` | `list` |
| `regex`, `glob` | Pattern and subject split at the first 0x01 byte |

Generated code carries no coverage instrumentation, so the fuzzer is black
box. It keeps an input when the call produced an outcome (an error message
with digits dropped, or the shape of a success) it has not seen yet, and saves
those inputs under `<out>/corpus/<target>` for later batches.

## Oracles

1. No crash: `run.sh` treats any non-zero exit as a finding and saves the
   input that was running (`<out>/cur-<target>`). With `--asan`, the binary
   links against an ASan build of the runtime.
2. Time: a call over `--time-ms` (default 1000, plus 1 ms per kB of
   input) is reported as `slow`. A call that never returns is caught by the
   driver's batch timeout and reported as `hang`.
3. Peak RSS: the peak is reset before each call (`/proc/self/clear_refs`)
   and read after it. Growth over 16 MB plus 64 times the input size is
   reported as `rss`. One input in 64 has a slice repeated up to about a
   megabyte, because amplification hides under the floor on small inputs.
4. Read budget: for `http1`, `websocket` and `h2server`, one input in 48
   is followed by a long tail (zeros, or for `http1` its last line repeated).
   A parser that reads more of the tail than one maximal message under the
   protocol's own limits is reported as `unbounded-read`.

Each batch records at most three inputs of a kind, so a bug that many inputs
hit does not bury the rest.

## Running it

```
make fuzz-surfaces-check                  # every target, 300 small inputs, fixed seed
make fuzz-surfaces FUZZ_MINUTES=30        # every target for 30 minutes, 4 at a time
tools/fuzz-surfaces/run.sh --minutes 10 --jobs 2 yaml toml
tools/fuzz-surfaces/run.sh --known DIR    # which oracle flags each DIR/<target>-*.bin
```

`fuzz-surfaces-check` fails only on a crash or a hang, so a slow shared
machine cannot turn it red. The long run reports findings and exits 0.

To run under ASan, set `PITH_ASAN_RUNTIME` to an ASan build of
`libpith_runtime.a` and `PITH_ASAN_CC` to a linker wrapper that adds rustc's
ASan runtime, then pass `--asan`:

```
RUSTC_BOOTSTRAP=1 RUSTFLAGS="-C relocation-model=pie -Zsanitizer=address -Cforce-frame-pointers=yes" \
  cargo build --release -p pith-runtime --target x86_64-unknown-linux-gnu --target-dir DIR
PITH_ASAN_RUNTIME=DIR/x86_64-unknown-linux-gnu/release/libpith_runtime.a \
  PITH_ASAN_CC=cc-asan tools/fuzz-surfaces/run.sh --asan --minutes 10
```

The RSS oracle is off under ASan, whose shadow memory and quarantine inflate
RSS. The run also sets `report_globals=0`, because the runtime reads a
string header just before its headerless static strings and ASan reports that
on nearly every input.

Findings go to `<out>/findings/<target>-<kind>-*.bin`. To reproduce one:

```
tools/fuzz-surfaces/fuzz_surfaces --target yaml --replay FILE
```

A crash whose replay does not crash again is renamed `*-unconfirmed.bin`.
