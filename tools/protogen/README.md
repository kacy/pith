# protogen

a proto3 to pith code generator. it reads a `.proto` file and writes a pith
module: a struct per message plus `encode_<Msg>` / `decode_<Msg>` functions over
`std.protobuf`. that is what a grpc call carries — build a message, encode it to
bytes, hand the bytes to `grpc.unary` (or a streaming call), and decode the reply.

```
pith build tools/protogen/protogen.pith
./tools/protogen/protogen schema.proto schema_gen.pith
```

## supported

- `message` with singular and repeated fields, nested messages, and `enum`
- scalars: int32/int64/uint32/uint64, sint32/sint64, bool, string, bytes,
  double, fixed32/fixed64, sfixed32/sfixed64
- enums, as `Int` fields plus named `Enum_VALUE` constants
- singular message fields, as pith optionals (`Sub?`)
- repeated message / string / bytes / int-varint / enum fields
- grpc `service` stubs — a typed client per service (see below)

singular message fields have presence (optionals); other singular fields follow
proto3 and skip their zero value on the wire. `decode_*` skips unknown fields, so
it stays forward compatible.

## services

each `service Foo { ... }` becomes a `FooClient` wrapping a `grpc.Conn`, with a
typed method per rpc that encodes the request and decodes the reply:

- unary `rpc Bar(Req) returns (Resp)` → `client.Bar(req: Req) -> Resp!`
- server-streaming `returns (stream Resp)` → a handle with `collect() -> List[Resp]!`
- client-streaming `(stream Req)` → a handle with `send(req)`, `close_send()`,
  `recv_response() -> Resp!`
- bidi `(stream Req) returns (stream Resp)` → a handle with `send(req)`,
  `close_send()`, `collect() -> List[Resp]!`

```
conn := grpc.dial("localhost", 50051)!
client := new_FooClient(conn)
resp := client.Bar(Req(...))!
```

the streaming handles wrap `std.net.grpc`'s `ServerStream` / `ClientStream` /
`BidiStream`; reach through `.inner` for incremental, non-collecting access.

## server

protogen also generates a server: implement one handler per rpc and hand them to
the generated `serve_Foo`. it routes each request by its method path, decodes it,
calls your handler, and frames the reply (an unknown path or a decode failure
comes back as the right grpc status). a unary rpc takes `(Req) ->
Resp!grpc.GrpcError`; a server-streaming rpc takes a handler returning the whole
reply as a list, `(Req) -> List[Resp]!grpc.GrpcError`, framed back as one
buffered stream:

```
fn bar(req: Req) -> Resp!grpc.GrpcError:
    return Resp(...)

fn tail(req: Req) -> List[Resp]!grpc.GrpcError:    # rpc Tail(Req) returns (stream Resp)
    return [...]

serve_Foo("0.0.0.0", 50051, bar, tail)!                 # plaintext http/2
serve_Foo_tls("0.0.0.0", 443, cert, key, bar, tail)!    # http/2 over tls
```

buffered means the whole reply passes through memory — right for bounded streams,
not endless ones. client-streaming and bidi rpcs are client-only for now, so
`serve_Foo` skips them and a service with only those gets no server.

## not yet supported

a clear parse error names these: `oneof`, `map<>`, 32-bit `float`, the well-known
types, proto2, and repeated `sint`/`fixed`/`double`.

## example

`sample.proto` and its generated `sample_gen.pith` are checked in;
`protogen_test.pith` round-trips every field kind and checks the encoder emits
the canonical protobuf bytes. `make protogen-check` regenerates the sample,
confirms it matches the committed output, and runs the tests.
