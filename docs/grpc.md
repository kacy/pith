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
runtime. it takes a handler for each *unary* rpc, in proto order. a request for
an unknown method comes back as `UNIMPLEMENTED`, and a request that fails to
decode comes back as `INVALID_ARGUMENT`. neither is code you write.

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

## status codes

the full set lives in `std.net.grpc` as `grpc.GRPC_OK`, `GRPC_NOT_FOUND`,
`GRPC_INVALID_ARGUMENT`, `GRPC_UNIMPLEMENTED`, `GRPC_INTERNAL`, and the rest.
`grpc.status_name(code)` gives the canonical name. raise them from a handler with
`fail grpc.GrpcError(status: ..., message: ...)`.

## what isn't here yet

- **the server is unary.** the generated server covers unary rpcs; streaming an
  rpc back from a server isn't wired up. the *client* is further along: it can
  consume all four shapes (unary, server-, client-, and bidi-streaming) against
  any grpc server, and the generated client stubs cover them. but `serve_<Svc>`
  takes a handler only per unary rpc, and an all-streaming service gets no server.
- **protogen doesn't cover** `oneof`, `map<k,v>`, 32-bit `float` (use `double`),
  the well-known types (Timestamp, Any, …), or proto2. it stops with a clear
  error naming the feature.
- **deadlines, metadata, and compression** are minimal.
- **observability**: the client opens a trace span and records red metrics per
  call automatically; the server side is your code, so instrument it yourself.
