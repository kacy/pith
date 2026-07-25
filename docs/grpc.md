# grpc

pith speaks grpc end to end, in pure pith: no C, no tokio, no rustls. you write a
`.proto`, run one codegen step, and get typed messages, a typed client, and a
typed server. http/2, tls 1.3, hpack, and flow control are all handled underneath.

there's one idea worth holding onto: **codegen owns the wire; you own the
handlers.** protogen turns a `.proto` into messages, their encoders and decoders,
a client, and a server router. all you write is the business logic, one typed
handler per rpc, plus one call to start serving.

the whole example below runs as `examples/grpc_catalog.pith` (server and client
in one process); `pith run examples/grpc_catalog.pith` to see it round-trip.

## the proto

nothing pith-specific. plain proto3. a small product catalog:

```proto
syntax = "proto3";
package catalog;

enum Category {
  CATEGORY_UNKNOWN = 0;
  ELECTRONICS = 1;
  BOOKS = 2;
  GROCERY = 3;
}

message Product {
  string sku = 1;
  string name = 2;
  double price = 3;
  Category category = 4;
  repeated string tags = 5;
  bool in_stock = 6;
}

message GetProductRequest { string sku = 1; }

message SearchRequest {
  string query = 1;
  int32 max_results = 2;
}

message SearchResponse {
  repeated Product products = 1;
  int32 total = 2;
}

service Catalog {
  rpc GetProduct(GetProductRequest) returns (Product);
  rpc Search(SearchRequest) returns (SearchResponse);
}
```

## generate

```
./tools/protogen/protogen catalog.proto catalog_gen.pith
```

`catalog_gen.pith` is generated — don't edit it. import it like any module.

## what you get

**enums** become named `Int` constants:

```pith
pub Category_ELECTRONICS := 1
```

**each message** becomes a struct plus a codec pair. proto types map to pith
types the obvious way: `int32` → `Int`, `double` → `Float`, `repeated T` →
`List[T]`, a nested message field → an optional, `bytes` → `Bytes`.

```pith
pub struct Product:
    pub sku: String
    pub name: String
    pub price: Float
    pub category: Int
    pub tags: List[String]
    pub in_stock: Bool

pub fn encode_Product(m: Product) -> Bytes
pub fn decode_Product(data: Bytes) -> Product!ProtoError
```

**the service** becomes a typed client and a typed server. those wrap the codecs,
so you rarely call `encode_`/`decode_` yourself.

## the server

implement one handler per rpc, `(Req) -> Resp!grpc.GrpcError`, and hand them to
the generated `serve_Catalog`. the generated router decodes each request,
dispatches by method path, and encodes the reply. you never touch a byte:

```pith
import catalog_gen as cat
import std.net.grpc as grpc

# a tiny in-memory catalog, seeded at startup.
mut store: Map[String, cat.Product] := {}

fn get_product(req: cat.GetProductRequest) -> cat.Product!grpc.GrpcError:
    if not store.contains_key(req.sku):
        fail grpc.GrpcError(status: grpc.GRPC_NOT_FOUND, message: "no product " + req.sku)
    return store[req.sku]

fn search(req: cat.SearchRequest) -> cat.SearchResponse!grpc.GrpcError:
    if req.query.len() == 0:
        fail grpc.GrpcError(status: grpc.GRPC_INVALID_ARGUMENT, message: "query is required")
    mut hits: List[cat.Product] := []
    for sku in store.keys():
        p := store[sku]
        if p.name.contains(req.query):
            hits.push(p)
    return cat.SearchResponse(products: hits, total: hits.len())

fn main():
    seed()
    cat.serve_Catalog("0.0.0.0", 50051, get_product, search)!
```

`serve_Catalog` blocks and serves connections concurrently on the green-thread
runtime. it takes a handler per rpc, in proto order. a request for an unknown
method comes back as `UNIMPLEMENTED`, and a request that fails to decode comes
back as `INVALID_ARGUMENT`. neither is code you write.

a streaming rpc changes the handler's shape, not its nature: a streamed side
becomes a `List`. server-streaming returns one, client-streaming receives one,
bidi does both:

```pith
# rpc ListProducts(SearchRequest) returns (stream Product)
fn list_products(req: cat.SearchRequest) -> List[cat.Product]!grpc.GrpcError:
    return matching_products(req.query)

# rpc BatchAdd(stream Product) returns (SearchResponse)
fn batch_add(reqs: List[cat.Product]) -> cat.SearchResponse!grpc.GrpcError:
    ...
```

the generated router frames each element as one stream message; the client reads
them back one at a time (or `collect()`s them). generated streams are *buffered*
— a handler receives the complete request list and returns the complete reply
list, so the whole stream passes through memory. that fits bounded streams
(search results, batch writes, event replay), not endless ones. for interactive
or endless streams, write the handler by hand against `grpc.serve_stream`
instead, which serves incrementally (see "what isn't here yet").

one thing to watch: the accept loop is concurrent, so if a handler *mutates*
shared state (a seeded read-only store is fine), guard it with a `Mutex`.

## the client

the generated client is a thin typed wrapper over a connection:

```pith
import catalog_gen as cat
import std.net.grpc as grpc

fn main():
    conn := grpc.dial_h2c("127.0.0.1", 50051)!     # plaintext http/2
    client := cat.new_CatalogClient(conn)

    product := client.GetProduct(cat.GetProductRequest(sku: "sku-1"))!
    print(product.name + " $" + product.price.to_string())

    results := client.Search(cat.SearchRequest(query: "usb", max_results: 10))!
    print(results.total.to_string() + " matches")

    conn.close()
```

a call that returns a non-OK status fails with a `grpc.GrpcError` carrying the
status code and message — the same `NOT_FOUND` your handler raised arrives here as
`err.status == grpc.GRPC_NOT_FOUND`.

## tls

serve over tls with a cert and key; dial with a tls config that offers alpn `h2`:

```pith
grpc.serve_Catalog_tls("0.0.0.0", 443, "server.crt", "server.key", get_product, search)!
```
```pith
cfg := tls.client_config_with_ca_file("ca.crt")!.with_alpn(["h2"])
conn := grpc.dial_with_config("catalog.internal", 443, cfg)!
```

use `dial_with_config` when the server's cert is signed by a private ca; plain
`grpc.dial` trusts the system roots.

## deadlines and metadata

a client attaches metadata — custom request headers, as a flat `[k1, v1, ...]`
list — and a deadline to a unary call:

```pith
reply := conn.unary_with_headers("/pkg.Svc/Method", req, ["x-tenant", "acme"])!
reply := conn.unary_with_deadline("/pkg.Svc/Method", req, 1500)!   # 1.5s
reply := conn.unary_with_headers_and_deadline("/pkg.Svc/Method", req, md, 1500)!
```

a deadline does two things: the call sends the canonical `grpc-timeout` request
header (`1500m`; whole seconds past eight digits of millis) so the server can
stop working once the time is up, and the client itself gives up after the
timeout, failing with `GRPC_DEADLINE_EXCEEDED`. the streaming openers have
metadata variants too (`server_stream_with_headers`, `client_stream_with_headers`,
`bidi_stream_with_headers`) but no deadline enforcement — cap a stream's
lifetime yourself.

on the server, a dispatch reads the calling request's context back:

```pith
fn dispatch(path: String, request: Bytes) -> Bytes!grpc.GrpcError:
    tenant := grpc.incoming_metadata()["x-tenant"]
    remaining := grpc.incoming_deadline_ms() - time.mono_millis()  # 0 = none
    ...
```

`incoming_metadata()` is the request's custom headers, lowercase-keyed, minus
the transport and reserved `grpc-*` headers. `incoming_deadline_ms()` is the
absolute deadline as a `time.mono_millis()` timestamp, `0` when the client sent
none. a request whose deadline is already spent when it reaches the dispatch is
answered `DEADLINE_EXCEEDED` without the handler running at all; past that
point enforcement is the handler's job — check the deadline between steps of
long work. on the `serve_stream` path the live `GrpcStream` carries the same
context as `metadata()` and `deadline_ms()`.

this is initial (request) metadata only. trailing metadata — custom trailers
beyond `grpc-status`/`grpc-message` — is not supported in either direction.

## status codes

the full set lives in `std.net.grpc` as `grpc.GRPC_OK`, `GRPC_NOT_FOUND`,
`GRPC_INVALID_ARGUMENT`, `GRPC_UNIMPLEMENTED`, `GRPC_INTERNAL`, and the rest.
`grpc.status_name(code)` gives the canonical name. raise them from a handler with
`fail grpc.GrpcError(status: ..., message: ...)`.

## what isn't here yet

- **generated streams are buffered.** a streamed side of a generated stub
  passes through memory whole: the handler gets the complete request `List` and
  returns the complete reply `List`, so no interleaving on a bidi stream. for
  interactive or endless streams, drop down to `grpc.serve_stream`: a
  hand-written dispatch gets a live `GrpcStream` and can `recv_message()` and
  `send_message()` incrementally — reply to message 1 before the client sends
  message 2 — ending with `finish_ok()` or `finish(status, message)`. protogen
  doesn't generate streaming-shaped stubs yet, so serve_stream means routing
  and encoding by hand.
- **protogen doesn't cover** 32-bit `float` (use `double`), the well-known types
  (Timestamp, Any, …), or proto2. it stops with a clear error naming the
  feature. `oneof` (a payload enum per group) and `map<k,v>` (a pith `Map`,
  string or integer keys) are supported.
- **message compression and trailing metadata** are not supported; deadlines
  are enforced at the edges (client timeout, server expiry-on-arrival), not by
  cancelling a handler mid-flight.
- **observability**: the client opens a trace span and records red metrics per
  call automatically; the server side is your code, so instrument it yourself.
