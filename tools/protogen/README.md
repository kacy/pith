# protogen

This is a proto3 to Pith code generator. It reads a `.proto` file and writes a
Pith module: a struct per message plus `encode_<Msg>` / `decode_<Msg>` functions
over `std.protobuf`.

That is what a gRPC call carries: build a message, encode it to bytes, hand the
bytes to `grpc.unary` (or a streaming call), and decode the reply.

```
pith build tools/protogen/protogen.pith
./tools/protogen/protogen schema.proto schema_gen.pith
```

## Supported

- `message` with singular and repeated fields, nested messages, and `enum`
- Scalars: int32/int64/uint32/uint64, sint32/sint64, bool, string, bytes,
  float, double, fixed32/fixed64, sfixed32/sfixed64
- Enums, as `Int` fields plus named `Enum_VALUE` constants
- Singular message fields, as Pith optionals (`Sub?`)
- Repeated message / string / bytes / int-varint / enum fields
- `oneof` groups, as a payload enum per group (see below)
- `map<k, v>` fields, as Pith `Map`s (see below)
- The common well-known types: Timestamp, Duration, Empty, and the
  wrappers (see below)
- gRPC `service` stubs, with a typed client and two servers (buffered and
  streaming) per service (see below)

A `float` field becomes a Pith `Float` (a 64-bit double). Decode widens the
32-bit wire value exactly. Encode narrows to the nearest f32 with ties to even,
the same rounding a C `(float)` cast does, so a value with no exact f32 form
(0.1, for example) round-trips to its f32-rounded double instead of to itself.

Singular message fields have presence (optionals); other singular fields follow
proto3 and skip their zero value on the wire. `decode_*` skips unknown fields, so
it stays forward compatible.

## oneof

Each `oneof` group becomes a Pith enum with one variant per member plus an
`Unset` variant, and the containing struct holds the enum:

```proto
message Payment {
  string id = 1;
  oneof method {
    string paypal_email = 2;
    CardDetails card = 3;
  }
}
```

```
pub enum PaymentMethod:
    PaymentMethodUnset
    PaypalEmail(String)
    Card(CardDetails)

pub struct Payment:
    pub id: String
    pub method: PaymentMethod
```

Build a value with the generated constructor functions
(`Payment_paypal_email("a@b")`, `Payment_card(CardDetails(...))`,
`Payment_method_unset()`) instead of naming the variants directly.
Cross-module enum-variant construction is shaky, and the constructors
sidestep it (std.sql's `Value` does the same). Reading is a plain `match` on
the field, which works fine across modules.

A set member always writes, even at its zero value; `Unset` writes nothing. On
decode the members are ordinary fields of the message, and the last one seen
wins, per proto3.

## Maps

A `map<k, v>` field becomes a Pith `Map`. String keys stay `String`; the
integer key types (`int32`/`int64`/`uint32`/`uint64`/`sint32`/`sint64`) become
`Int`. Values can be any supported scalar, string, bytes, enum (as `Int`), or
message type:

```proto
message Inventory {
  map<string, int32> counts = 1;
  map<string, Bin> bins = 2;
  map<int64, string> names = 3;
}
```

```
pub struct Inventory:
    pub counts: Map[String, Int]
    pub bins: Map[String, Bin]
    pub names: Map[Int, String]
```

On the wire, each entry is a little message (key = 1, value = 2) written
length-delimited on the map's field number, per proto3. An empty map writes
nothing. The encoder always writes both key and value, even at their zero
values; the decoder accepts entries that omit either (they mean the zero key
or zero value) and merges duplicates last-one-wins.

`bool` and `fixed*`/`sfixed*` keys are a parse error, because a Pith `Map`
keys by `String` or `Int` only. A map value that is itself a map, which proto3
forbids anyway, is also a parse error. Map iteration order is not fixed, so
two encodes of the same multi-entry map may order entries differently (both
decode the same).

## Well-known types

Importing a supported google/protobuf file enables its types. Each type the
file actually uses is synthesized into the generated module as a plain
message: a struct plus `encode_`/`decode_` functions, named by its simple
name (`Timestamp`, `Duration`, `Empty`, `Int32Value`, ...). Nothing is
generated for an imported type that is never used. The supported files are:

- `google/protobuf/timestamp.proto`: `Timestamp` (seconds, nanos)
- `google/protobuf/duration.proto`: `Duration` (seconds, nanos)
- `google/protobuf/empty.proto`: `Empty`
- `google/protobuf/wrappers.proto`: `DoubleValue`, `FloatValue`,
  `Int64Value`, `UInt64Value`, `Int32Value`, `UInt32Value`, `BoolValue`,
  `StringValue`, `BytesValue` (each a single `value` field)

Refer to them by their qualified names (`google.protobuf.Timestamp`). As
message fields they generate as optionals, so a wrapper like
`google.protobuf.Int32Value` gives the usual optional-int presence pattern:

```proto
import "google/protobuf/timestamp.proto";
import "google/protobuf/wrappers.proto";

message Event {
  string name = 1;
  google.protobuf.Timestamp at = 2;       // -> pub at: Timestamp?
  google.protobuf.Int32Value priority = 3; // -> pub priority: Int32Value?
}
```

Declaring your own message (or enum) with one of these names while also
importing it is an error, because the two would be indistinguishable in the
generated module. Any other google/protobuf import (`any.proto`,
`struct.proto`, `field_mask.proto`, ...) is a clear error naming the file.
Imports of your own files are ignored, since protogen generates from a
single file.

## Services

Each `service Foo { ... }` becomes a `FooClient` wrapping a `grpc.Conn`, with a
typed method per rpc that encodes the request and decodes the reply:

- Unary `rpc Bar(Req) returns (Resp)` → `client.Bar(req: Req) -> Resp!`
- Server-streaming `returns (stream Resp)` → a handle with `recv() -> FooBarReply!`
  and `collect() -> List[Resp]!`
- Client-streaming `(stream Req)` → a handle with `send(req)`, `close_send()`,
  `recv_response() -> Resp!`
- Bidi `(stream Req) returns (stream Resp)` → a handle with `send(req)`,
  `close_send()`, `recv() -> FooBarReply!`, `collect() -> List[Resp]!`

```
conn := grpc.dial("localhost", 50051)!
client := new_FooClient(conn)
resp := client.Bar(Req(...))!
```

`recv()` reads one response at a time. `FooBarReply` has `done` (the server
finished the stream with OK; `message` is then all-default) and `message`, the
decoded `Resp`. A non-OK status or a reset fails, and so does a reply that does
not decode (`INTERNAL`). The streaming handles wrap `std.net.grpc`'s
`ServerStream`, `ClientStream` and `BidiStream`, which stay reachable through
`.inner`.

## Server

The protogen tool also generates a server: implement one handler per rpc and
hand them to the generated `serve_Foo`. It routes each request by its method
path, decodes it, calls your handler, and frames the reply (an unknown path or
a decode failure comes back as the right gRPC status). The handler's shape
follows the rpc, and a streamed side becomes a `List`:

- Unary `rpc Bar(Req) returns (Resp)` → `fn(Req) -> Resp!`
- Server-streaming `returns (stream Resp)` → `fn(Req) -> List[Resp]!`
- Client-streaming `(stream Req)` → `fn(List[Req]) -> Resp!`
- Bidi → `fn(List[Req]) -> List[Resp]!`

```
fn bar(req: Req) -> Resp!grpc.GrpcError:
    return Resp(...)

fn tail(req: Req) -> List[Resp]!grpc.GrpcError:    # rpc Tail(Req) returns (stream Resp)
    return [...]

serve_Foo("0.0.0.0", 50051, bar, tail)!                 # plaintext http/2
serve_Foo_tls("0.0.0.0", 443, cert, key, bar, tail)!    # http/2 over tls
```

Streams are buffered end to end: a handler receives the complete request list
and returns the complete reply list, so the whole stream passes through memory.
That suits bounded streams. Endless or interactive streams need the streaming
server below.

## Streaming server

`serve_Foo_streaming` (and `serve_Foo_streaming_tls`) serve every rpc on one
incremental listener over `grpc.serve_stream`, so nothing is buffered end to
end. Unary handlers keep the `(Req) -> Resp!` shape above; each streaming rpc
gets a typed `Foo<Rpc>ServerCall` and its handler drives the stream itself:

- Unary `rpc Bar(Req) returns (Resp)` → `fn(Req) -> Resp!` (same as buffered)
- Server-streaming `rpc Tail(Req) returns (stream Resp)` →
  `fn(Req, FooTailServerCall) -> Int!`: the router decodes the one request,
  and the call sends
- Client-streaming `rpc Ingest(stream Req) returns (Resp)` →
  `fn(FooIngestServerCall) -> Int!`: the handler calls recv, then sends one
  reply
- Bidi → `fn(FooBidiServerCall) -> Int!`: recv and send interleave freely

A `ServerCall` carries `send(reply: Resp) -> Int` (framed and sent
immediately), `finish(status, message)` / `finish_ok()`, `metadata()`,
`deadline_ms()`, and `closing()`, which is true once a graceful shutdown has
begun. A handler that streams indefinitely can use it to end its own stream
instead of having it closed with `UNAVAILABLE` partway through the grace
period (see [docs/signals.md](../../docs/signals.md)). Client-streaming and
bidi calls add `recv() -> Foo<Rpc>Recv!`, which returns a small struct with
`done` (the client half-closed) and the decoded `message`. A message that
fails to decode fails recv with `INVALID_ARGUMENT`.

```
fn tail(req: Req, call: FooTailServerCall) -> Int!grpc.GrpcError:
    for r in produce(req):
        call.send(r)
    return 0            # returning without finishing ends the rpc with OK

serve_Foo_streaming("0.0.0.0", 50051, bar, tail)!
```

Failing the handler ends the rpc with that error's status (a compact
trailers-only response when nothing was sent yet). `examples/grpc_reflect.pith`
runs all four shapes through one streaming listener.

## Not yet supported

A clear error names these: `bool`/`fixed` map keys, repeated
`bool`/`sint`/`fixed`/`float`/`double`, and the well-known types beyond the list
above (`Any`, `Struct`/`Value`/`ListValue`, `FieldMask`, ...). The `syntax`
header is not enforced, and proto2-only constructs (such as `required` fields)
are not supported.

## Example

`sample.proto` and its generated `sample_gen.pith` are checked in;
`protogen_test.pith` round-trips every field kind and checks the encoder emits
the canonical protobuf bytes. `make protogen-check` regenerates the sample,
confirms it matches the committed output, and runs the tests.
