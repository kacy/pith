# parser surface fuzzing

`fuzz_surfaces.pith` mutates real-shaped inputs and feeds them to the std
parsers that read untrusted bytes:

| target | entry point |
| --- | --- |
| `http1` | `http.read_request_buffered_bytes`, then the request accessors (form, cookies, multipart, basic auth) |
| `http2` | `frames.read_frame` and every `frames.parse_*`, with HPACK on HEADERS |
| `hpack` | one `hpack.Decoder` across several header blocks |
| `h2server` | `server.serve_h2c_connection` over loopback tcp |
| `websocket` | `WebSocket.recv` over an in-memory reader (server side) |
| `yaml`, `toml`, `json` | `parse` / `parse_all` and `decode_scalars` |
| `protobuf` | a generic walk: every field by wire type, nested messages, packed varints |
| `inflate`, `gzip`, `zstd` | the bounded decoders (`zstd` runs both the binding and `zstd_pure`) |
| `tar`, `zip` | `list` |
| `regex`, `glob` | pattern and subject split at the first 0x01 byte |

generated code carries no coverage instrumentation, so the fuzzer is black
box. it keeps an input when the call produced an outcome (an error message
with digits dropped, or the shape of a success) it has not seen yet, and saves
those inputs under `<out>/corpus/<target>` for later batches.

## oracles

1. no crash: `run.sh` treats any non-zero exit as a finding and saves the
   input that was running (`<out>/cur-<target>`). with `--asan` the binary
   links against an asan build of the runtime.
2. time: a call over `--time-ms` (default 1000, plus 1 ms per kB of
   input) is reported as `slow`. a call that never returns is caught by the
   driver's batch timeout and reported as `hang`.
3. peak rss: the peak is reset before each call (`/proc/self/clear_refs`)
   and read after it. growth over 16 MB plus 64 times the input size is
   reported as `rss`. one input in 64 has a slice repeated up to about a
   megabyte, because amplification hides under the floor on small inputs.
4. read budget: for `http1`, `websocket` and `h2server`, one input in 48
   is followed by a long tail (zeros, or for `http1` its last line repeated).
   a parser that reads more of the tail than one maximal message under the
   protocol's own limits is reported as `unbounded-read`.

each batch records at most three inputs of a kind, so a bug that many inputs
hit does not bury the rest. `hits_known_bug` skips inputs that only re-find a
hang already on file; delete its clauses as those bugs are fixed, or pass
`--no-skip`.

## running it

```
make fuzz-surfaces-check                  # every target, 400 small inputs, fixed seed
make fuzz-surfaces FUZZ_MINUTES=30        # every target for 30 minutes, 4 at a time
tools/fuzz-surfaces/run.sh --minutes 10 --jobs 2 yaml toml
tools/fuzz-surfaces/run.sh --known DIR    # which oracle flags each DIR/<target>-*.bin
```

`fuzz-surfaces-check` fails only on a crash or a hang, so a slow shared
machine cannot turn it red. the long run reports findings and exits 0.

under asan, set `PITH_ASAN_RUNTIME` to an asan build of `libpith_runtime.a`
and `PITH_ASAN_CC` to a linker wrapper that adds rustc's asan runtime, then
pass `--asan`:

```
RUSTC_BOOTSTRAP=1 RUSTFLAGS="-Zsanitizer=address -Cforce-frame-pointers=yes" \
  cargo build --release -p pith-runtime --target x86_64-unknown-linux-gnu --target-dir DIR
PITH_ASAN_RUNTIME=DIR/x86_64-unknown-linux-gnu/release/libpith_runtime.a \
  PITH_ASAN_CC=cc-asan tools/fuzz-surfaces/run.sh --asan --minutes 10
```

the rss oracle is off under asan, whose shadow memory and quarantine inflate
rss. the run also sets `report_globals=0`, because the runtime reads a
string header just before its headerless static strings and asan reports that
on nearly every input.

findings go to `<out>/findings/<target>-<kind>-*.bin`. to reproduce one:

```
tools/fuzz-surfaces/fuzz_surfaces --target yaml --replay FILE
```

a crash whose replay does not crash again is renamed `*-unconfirmed.bin`.
