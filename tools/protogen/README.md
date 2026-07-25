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
  float, double, fixed32/fixed64, sfixed32/sfixed64
- enums, as `Int` fields plus named `Enum_VALUE` constants
- singular message fields, as pith optionals (`Sub?`)
- repeated message / string / bytes / int-varint / enum fields
- `oneof` groups, as a payload enum per group (see below)
- `map<k, v>` fields, as pith `Map`s (see below)
- grpc `service` stubs — a typed client per service (see below)

singular message fields have presence (optionals); other singular fields follow
proto3 and skip their zero value on the wire. `decode_*` skips unknown fields, so
it stays forward compatible.

a `float` field becomes a pith `Float` (a 64-bit double). decode widens the
32-bit wire value exactly; encode narrows to the nearest f32 with ties to
even, the same rounding a c `(float)` cast does — so a value with no exact
f32 form (0.1, say) round-trips to its f32-rounded double, not to itself.

## oneof

each `oneof` group becomes a pith enum with one variant per member plus an
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

build a value with the generated constructor functions — `Payment_paypal_email("a@b")`,
`Payment_card(CardDetails(...))`, `Payment_method_unset()` — rather than naming
the variants directly; cross-module enum-variant construction is shaky, and the
constructors sidestep it (std.sql's `Value` does the same). reading is a plain
`match` on the field, which works fine across modules.

a set member always writes, even at its zero value; `Unset` writes nothing. on
decode the members are ordinary fields of the message, and the last one seen
wins, per proto3.

## maps

a `map<k, v>` field becomes a pith `Map`. string keys stay `String`; the
integer key types (`int32`/`int64`/`uint32`/`uint64`/`sint32`/`sint64`) become
`Int`. values can be any supported scalar, string, bytes, enum (as `Int`), or
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

on the wire each entry is a little message (key = 1, value = 2) written
length-delimited on the map's field number, per proto3. an empty map writes
nothing. the encoder always writes both key and value, even at their zero
values; the decoder accepts entries that omit either (they mean the zero key
or zero value) and merges duplicates last-one-wins.

`bool` and `fixed*`/`sfixed*` keys are a parse error — a pith `Map` keys by
`String` or `Int` only. so is a map value that is itself a map, which proto3
forbids anyway. map iteration order is not fixed, so two encodes of the same
multi-entry map may order entries differently (both decode the same).

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
comes back as the right grpc status). the handler's shape follows the rpc — a
streamed side becomes a `List`:

- unary `rpc Bar(Req) returns (Resp)` → `fn(Req) -> Resp!`
- server-streaming `returns (stream Resp)` → `fn(Req) -> List[Resp]!`
- client-streaming `(stream Req)` → `fn(List[Req]) -> Resp!`
- bidi → `fn(List[Req]) -> List[Resp]!`

```
fn bar(req: Req) -> Resp!grpc.GrpcError:
    return Resp(...)

fn tail(req: Req) -> List[Resp]!grpc.GrpcError:    # rpc Tail(Req) returns (stream Resp)
    return [...]

serve_Foo("0.0.0.0", 50051, bar, tail)!                 # plaintext http/2
serve_Foo_tls("0.0.0.0", 443, cert, key, bar, tail)!    # http/2 over tls
```

streams are buffered end to end: a handler receives the complete request list
and returns the complete reply list, so the whole stream passes through memory —
right for bounded streams, not endless or interactive ones.

## not yet supported

a clear error names these: `bool`/`fixed` map keys, the well-known types,
proto2, and repeated `sint`/`fixed`/`float`/`double`.

## example

`sample.proto` and its generated `sample_gen.pith` are checked in;
`protogen_test.pith` round-trips every field kind and checks the encoder emits
the canonical protobuf bytes. `make protogen-check` regenerates the sample,
confirms it matches the committed output, and runs the tests.
