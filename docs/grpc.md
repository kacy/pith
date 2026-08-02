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
them back one at a time (or `collect()`s them). generated streams under
`serve_Catalog` are *buffered* — a handler receives the complete request list
and returns the complete reply list, so the whole stream passes through memory.
that fits bounded streams (search results, batch writes, event replay), not
endless ones.

### streaming, incrementally

for interactive or endless streams, protogen also generates an incremental
server: `serve_Catalog_streaming` (and `_tls`). one listener serves the whole
service — unary handlers keep the `(Req) -> Resp` shape above, while each
streaming rpc's handler works a typed `<Svc><Rpc>ServerCall` message by
message. nothing is buffered end to end, so a bidi handler can answer message 1
while the client is still composing message 2:

```pith
# rpc Tail(SearchRequest) returns (stream Product) — the handler gets the
# decoded request plus a send-side call
fn tail(req: cat.SearchRequest, call: cat.CatalogTailServerCall) -> Int!grpc.GrpcError:
    for p in matching_products(req.query):
        call.send(p)                     # framed and sent immediately
    return 0                             # returning finishes with OK

# rpc Import(stream Product) returns (SearchResponse) — the handler drives
# recv itself; recv() decodes each message, done marks the half-close
fn import_products(call: cat.CatalogImportServerCall) -> Int!grpc.GrpcError:
    mut n := 0
    while true:
        got := call.recv()!
        if got.done:
            break
        store.insert(got.message.sku, got.message)
        n = n + 1
    call.send(cat.SearchResponse(products: [], total: n))
    return 0

cat.serve_Catalog_streaming("0.0.0.0", 50051, get_product, search, tail, import_products)!
```

a bidi handler looks like the client-streaming one — `recv()` and `send()`
interleave however the conversation needs. the call also carries `metadata()`,
`deadline_ms()`, and `finish(status, message)` for ending with a non-OK status
mid-stream; a handler that fails before sending anything becomes a compact
trailers-only error response. a message that fails to decode surfaces from
`recv()` as `INVALID_ARGUMENT`.

pick the listener by the service's needs: `serve_Catalog` when every stream is
bounded (the `List` shapes are the simplest to write), `serve_Catalog_streaming`
when any rpc is interactive or long-lived. underneath both sit on
`std.net.grpc`; `grpc.serve_stream` remains the low-level api when you want to
route and encode by hand — `examples/grpc_chat.pith` does that, and
`examples/grpc_reflect.pith` is the same idea through the generated stubs.

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

## limits

the server refuses oversized input rather than trying to buffer it. none of
these are configurable yet. they sit well above what ordinary traffic needs, so
hitting one usually means something is wrong on the wire, not that the limit is
too low.

| limit | value | what happens past it |
| --- | --- | --- |
| `grpc.MAX_MESSAGE_BYTES` | 4 MiB | `RESOURCE_EXHAUSTED`, refused as soon as the frame header is read |
| `grpc.MAX_STREAM_MESSAGES` | 65536 | `RESOURCE_EXHAUSTED` on a buffered request stream |
| `protobuf.MAX_NESTING_DEPTH` | 100 | the decode fails; a self-referential message cannot recurse without bound |
| `http.MAX_REQUEST_BODY` | 10 MiB | the stream is reset |
| concurrent streams per connection | 100 | `REFUSED_STREAM` |
| concurrent connections per listener | 512 | the accept loop waits for one to end |
| idle socket | 2 minutes | the connection is dropped |

a body that is not framed cleanly — truncated, a length prefix that runs past
what arrived, or bytes that were never framed at all — fails
`INVALID_ARGUMENT` rather than decoding as an all-default message. a unary
request carrying more than one message fails the same way.

## interceptors

an interceptor wraps the dispatch the way a `std.web` middleware wraps a route
handler. it takes the rest of the chain plus the call, so it can run code
before or after, or answer the call itself and never reach the dispatch:

```pith
fn log_calls(next: fn(String, Bytes) -> Bytes!grpc.GrpcError, path: String, request: Bytes) -> Bytes!grpc.GrpcError:
    print(path)
    return next(path, request)

guarded := grpc.intercept(serve_Chat, [grpc.authorize(check_token), log_calls])
grpc.serve("0.0.0.0", 50051, guarded)!
```

`intercept` returns a dispatch, so it drops into `serve`, `serve_tls`, or any
other serve form without changing a signature — a generated `serve_<Svc>` router
and a hand-written dispatch both compose the same way. the first interceptor in
the list runs outermost, seeing the call first and the reply last.

three come ready made:

- `grpc.authorize(verify)` runs `verify` against the caller's metadata before
  any method body, and answers `UNAUTHENTICATED` when it returns false. this is
  the transport-level auth hook: one interceptor covers every rpc the server
  exposes. `grpc.bearer_token(metadata)` pulls the token out of an
  `authorization` header (matching the scheme case-insensitively, as rfc 7235
  requires), so verifying a jwt is a one-liner against `std.crypto.jwt`.
- `grpc.rate_limit(limiter)` spends a token per call and answers
  `RESOURCE_EXHAUSTED` when the bucket is dry.
- `grpc.circuit(breaker)` opens on repeated *server* faults — `INTERNAL`,
  `UNAVAILABLE`, `DATA_LOSS`, an expired deadline — and answers `UNAVAILABLE`
  while open. a caller's own error (`NOT_FOUND`, an invalid argument, a refused
  credential) does not count against it: a client sending bad requests is not a
  reason to stop serving good ones.

the last two take the same `std.resilience` `Limiter` and `Breaker` that
`web.rate_limit` and `web.circuit` take, so one limiter can cap an http surface
and a grpc surface together.

`examples/grpc_interceptors.pith` runs the whole shape end to end — a service
that knows nothing about auth or rate limiting, guarded by both, called by a
client that presents its credential once.

writing your own: an interceptor that refuses a call returns
`grpc.refuse(status, message)` rather than failing directly, because a pith
lambda can neither `fail` nor use `!`. return `next(path, request)` to pass the
call along.

on the client, a credential belongs on the channel rather than on every call
site:

```pith
conn := grpc.dial_h2c("localhost", 50051)!
conn.set_credentials(["authorization", "Bearer " + token])
reply := conn.unary("/chat.Chat/Send", req)!     # carries the credential
```

per-call metadata still works through the `*_with_headers` forms and is appended
after the channel's, so a call can add to the credentials but not silently drop
them.

## what isn't here yet

- **generated streaming is server-side.** the generated *client* still collects
  a response stream (`collect()`) or reaches through `.inner` for per-message
  reads; a typed incremental client handle is not generated yet.
- **protogen doesn't cover** proto2, `bool`/`fixed` map keys, repeated
  `sint`/`fixed`/`float`/`double` fields, or the well-known types beyond
  Timestamp, Duration, Empty, and the wrappers (no Any, Struct, FieldMask, …).
  it stops with a clear error naming the feature. `oneof` (a payload enum per
  group) and `map<k,v>` (a pith `Map`, string or integer keys) are supported.
- **message compression and trailing metadata** are not supported; deadlines
  are enforced at the edges (client timeout, server expiry-on-arrival), not by
  cancelling a handler mid-flight.
- **observability**: the client opens a trace span and records red metrics per
  call automatically. the server side is your code — an interceptor is the
  place to put it, since it sees every method.
- **interceptors are server-side and unary.** the client has channel-wide
  credentials but no interceptor chain of its own, and a server interceptor
  wraps the unary dispatch; the streaming serve forms take their own dispatch
  shapes and are not wrapped by `intercept`.
