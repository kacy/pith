# gRPC

Pith speaks gRPC end to end, in pure Pith, with no C, no tokio and no rustls. You
write a `.proto`, run one codegen step, and get typed messages, a typed client,
and a typed server. HTTP/2, TLS (1.2 and 1.3), HPACK, and flow control are all
handled underneath.

Codegen owns the wire, and you own the handlers. The protogen tool turns a
`.proto` into messages, their encoders and decoders, a client, and a server
router. All you write is the business logic, one typed handler per RPC, plus
one call to start serving.

The whole example below runs as `examples/grpc_catalog.pith` (server and client
in one process). Run `pith run examples/grpc_catalog.pith` to see it round-trip.

## The proto

The proto is plain proto3, with nothing Pith-specific. Here is a small product
catalog:

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

## Generate

```
./tools/protogen/protogen catalog.proto catalog_gen.pith
```

`catalog_gen.pith` is generated, so don't edit it. Import it like any module.

## What you get

Enums become named `Int` constants:

```pith
pub Category_ELECTRONICS := 1
```

Each message becomes a struct plus a codec pair. Proto types map to Pith types
the obvious way: `int32` → `Int`, `double` → `Float`, `repeated T` →
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

The service becomes a typed client and a typed server. Those wrap the codecs,
so you rarely call `encode_`/`decode_` yourself.

## The server

Implement one handler per RPC, `(Req) -> Resp!grpc.GrpcError`, and hand them to
the generated `serve_Catalog`. The generated router decodes each request,
dispatches by method path, and encodes the reply, so you never touch a byte:

```pith
import catalog_gen as cat
import std.algo as algo
import std.net.grpc as grpc

# seeded() builds a few products (see the example). the map is built once at
# startup and never written after, so handler tasks read it without a lock.
store := seeded()

fn get_product(req: cat.GetProductRequest) -> cat.Product!grpc.GrpcError:
    if let product = store.get(req.sku):
        return product
    fail grpc.GrpcError(status: grpc.GRPC_NOT_FOUND, message: "no product {req.sku}")

fn search(req: cat.SearchRequest) -> cat.SearchResponse!grpc.GrpcError:
    if req.query == "":
        fail grpc.GrpcError(status: grpc.GRPC_INVALID_ARGUMENT, message: "query is required")
    mut hits: List[cat.Product] := []
    for sku in store:
        if store[sku].name.contains(req.query):
            hits.push(store[sku])
    sorted := algo.sort_by_key(hits, fn(p: cat.Product) => p.name)
    mut shown := sorted
    if req.max_results > 0 and sorted.len() > req.max_results:
        shown = sorted.slice(0, req.max_results)
    return cat.SearchResponse(products: shown, total: sorted.len())

fn main() -> Int!:
    cat.serve_Catalog("0.0.0.0", 50051, get_product, search)!
    return 0
```

`serve_Catalog` blocks and serves connections concurrently on the green-thread
runtime. It takes a handler per RPC, in proto order. A request for an unknown
method comes back as `UNIMPLEMENTED`, and a request that fails to decode comes
back as `INVALID_ARGUMENT`. You write no code for either.

In a streaming RPC, each streamed side of the handler becomes a `List`. A
server-streaming handler returns one, a client-streaming handler receives one,
and a bidi handler does both:

```pith
# rpc ListProducts(SearchRequest) returns (stream Product)
fn list_products(req: cat.SearchRequest) -> List[cat.Product]!grpc.GrpcError:
    return matching_products(req.query)

# rpc BatchAdd(stream Product) returns (SearchResponse)
fn batch_add(reqs: List[cat.Product]) -> cat.SearchResponse!grpc.GrpcError:
    ...
```

The generated router frames each element as one stream message, and the client
reads them back one at a time (or `collect()`s them). Generated streams under
`serve_Catalog` are *buffered*: a handler receives the complete request list
and returns the complete reply list, so the whole stream passes through memory.
That suits bounded streams (search results, batch writes, event replay), but
not endless ones.

### Streaming, incrementally

For interactive or endless streams, protogen also generates an incremental
server: `serve_Catalog_streaming` (and `_tls`). One listener serves the whole
service. Unary handlers keep the `(Req) -> Resp` shape above, while each
streaming RPC's handler works through a typed `<Svc><Rpc>ServerCall` message by
message. Nothing is buffered end to end, so a bidi handler can answer message 1
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

A bidi handler looks like the client-streaming one, with `recv()` and `send()`
interleaved however the conversation needs. The call also carries `metadata()`,
`deadline_ms()`, and `finish(status, message)` for ending with a non-OK status
mid-stream. A handler that fails before sending anything becomes a compact
trailers-only error response. A message that fails to decode surfaces from
`recv()` as `INVALID_ARGUMENT`. A runtime error inside a handler (an index out
of range, a missing map key, a division by zero) fails that call with
`INTERNAL` and the message `internal error`. The error itself goes to the
server's stderr, and the server keeps serving (docs/failure_isolation.md).

Pick the listener by the service's needs: `serve_Catalog` when every stream is
bounded (the `List` shapes are the simplest to write), and
`serve_Catalog_streaming` when any RPC is interactive or long-lived. Both sit on
`std.net.grpc`. `grpc.serve_stream` remains the low-level API when you want to
route and encode by hand. `examples/grpc_chat.pith` does that, and
`examples/grpc_reflect.pith` is the same idea through the generated stubs.

The accept loop is concurrent, so if a handler *mutates* shared state (a seeded
read-only store is fine), guard it with a `Mutex`.

## The client

The generated client is a thin typed wrapper over a connection:

```pith
import catalog_gen as cat
import std.net.grpc as grpc

fn lookup(client: cat.CatalogClient) -> Int!grpc.GrpcError:
    product := client.GetProduct(cat.GetProductRequest(sku: "sku-1"))!
    print(product.name + " $" + product.price.to_string())

    results := client.Search(cat.SearchRequest(query: "usb", max_results: 10))!
    print(results.total.to_string() + " matches")
    return 0

fn main() -> Int!:
    conn := grpc.dial_h2c("127.0.0.1", 50051)!     # plaintext http/2
    client := cat.new_CatalogClient(conn)
    looked := lookup(client)
    if looked.is_err:
        print(grpc.status_name(looked.err.status) + ": " + looked.err.message)
    conn.close()
    return 0
```

A call that returns a non-OK status fails with a `grpc.GrpcError` carrying the
status code and message. The same `NOT_FOUND` your handler raised arrives here
as `err.status == grpc.GRPC_NOT_FOUND`. The calls sit in their own function
because `!` only propagates an error of the enclosing function's error type,
and `dial_h2c` fails with a `String`, not a `GrpcError`.

### Streaming calls

A streaming RPC's client method returns a typed `<Svc><Rpc>Call` handle. On the
request side, `send(req)` encodes and frames one message and `close_send()`
half-closes. On the response side, `recv()` reads one reply,
`recv_response()` reads the single reply of a client-streaming RPC, and
`collect()` gathers every remaining reply into a `List`. Every handle has a
`close()` that releases the stream; call it once you are done. A handle dropped
without `close()` releases the stream as well. If the RPC was still running,
the drop also resets it, provided the connection's send queue has room.

`recv()` is on the server-streaming and bidi handles. It blocks for the next
response and returns a `<Svc><Rpc>Reply`, the client-side mirror of the
server's `<Svc><Rpc>Recv`. `done` is false with the decoded `message`, or true
once the server has finished the stream with OK (`message` is then an
all-default value). A non-OK status or a reset fails with the `GrpcError`,
and a reply that does not decode fails with `INTERNAL`. A bidi client can use
`recv()` to talk turn by turn, reading each answer before it composes the next
request:

```pith
# rpc Chat(stream Note) returns (stream Note)
fn ask(chat: cat.CatalogChatCall, note: cat.Note) -> cat.Note!grpc.GrpcError:
    chat.send(note)
    got := chat.recv()!
    if got.done:
        fail grpc.GrpcError(grpc.GRPC_UNAVAILABLE, "stream ended early")
    return got.message
```

`collect()` waits for the whole stream, so use it only when the server ends the
stream on its own: a server-streaming reply, or a bidi call after
`close_send()`. `examples/grpc_reflect.pith` drives all four call shapes this
way.

## TLS

Serve over TLS with a cert and key, and dial with a TLS config that offers ALPN
`h2`:

```pith
cat.serve_Catalog_tls("0.0.0.0", 443, "server.crt", "server.key", get_product, search)!
```
```pith
cfg := tls.client_config_with_ca_file("ca.crt")!.with_alpn(["h2"])
defer cfg.close()
conn := grpc.dial_with_config("catalog.internal", 443, cfg)!
```

Use `dial_with_config` when the server's cert is signed by a private CA; plain
`grpc.dial` trusts the system roots.

The config is the caller's here, so the caller closes it (see
[who closes a config](tls.md#who-closes-a-config)). Closing it once
`dial_with_config` has returned is fine: the connection is already up and does
not read the config again. `grpc.dial` builds its own config and closes it for
you.

## Deadlines and metadata

A client attaches metadata (custom request headers, as a flat `[k1, v1, ...]`
list) and a deadline to a unary call:

```pith
reply := conn.unary_with_headers("/pkg.Svc/Method", req, ["x-tenant", "acme"])!
reply := conn.unary_with_deadline("/pkg.Svc/Method", req, 1500)!   # 1.5s
reply := conn.unary_with_headers_and_deadline("/pkg.Svc/Method", req, md, 1500)!
```

A deadline does two things. The call sends the canonical `grpc-timeout` request
header (`1500m`; whole seconds past eight digits of millis) so the server can
stop working once the time is up, and the client itself gives up after the
timeout, failing with `GRPC_DEADLINE_EXCEEDED`. The streaming openers have
metadata variants too (`server_stream_with_headers`, `client_stream_with_headers`,
`bidi_stream_with_headers`) but no deadline enforcement, so cap a stream's
lifetime yourself.

On the server, a dispatch reads the calling request's context back:

```pith
fn dispatch(path: String, request: Bytes) -> Bytes!grpc.GrpcError:
    tenant := grpc.incoming_metadata()["x-tenant"]
    remaining := grpc.incoming_deadline_ms() - time.mono_millis()  # 0 = none
    ...
```

`incoming_metadata()` is the request's custom headers, lowercase-keyed, minus
the transport and reserved `grpc-*` headers. `incoming_deadline_ms()` is the
absolute deadline as a `time.mono_millis()` timestamp, `0` when the client sent
none. A request whose deadline is already spent when it reaches the dispatch is
answered `DEADLINE_EXCEEDED` without the handler running at all. Past that
point, enforcement is the handler's job: check the deadline between steps of
long work. On the `serve_stream` path the live `GrpcStream` carries the same
context as `metadata()` and `deadline_ms()`.

This is initial (request) metadata only. Trailing metadata (custom trailers
beyond `grpc-status`/`grpc-message`) is not supported in either direction.

## Status codes

The full set lives in `std.net.grpc` as `grpc.GRPC_OK`, `GRPC_NOT_FOUND`,
`GRPC_INVALID_ARGUMENT`, `GRPC_UNIMPLEMENTED`, `GRPC_INTERNAL`, and the rest.
`grpc.status_name(code)` gives the canonical name. Raise them from a handler
with `fail grpc.GrpcError(status: ..., message: ...)`.

## Limits

The server refuses oversized input instead of trying to buffer it, and a
client channel bounds what a server can make it buffer. None of these limits
are configurable yet. They sit well above what ordinary traffic needs, so
hitting one usually means something is wrong on the wire, not that the limit is
too low.

| Limit | Value | What happens past it |
| --- | --- | --- |
| `grpc.MAX_MESSAGE_BYTES` | 4 MiB | `RESOURCE_EXHAUSTED`, refused as soon as the frame header is read |
| `grpc.MAX_STREAM_MESSAGES` | 65536 | `RESOURCE_EXHAUSTED` on a buffered request stream |
| `protobuf.MAX_NESTING_DEPTH` | 100 | The decode fails; a self-referential message cannot recurse without bound |
| `http.MAX_REQUEST_BODY` | 10 MiB | The stream is reset |
| HTTP/2 frame payload | 16 KiB, the `SETTINGS_MAX_FRAME_SIZE` the server advertises | The connection is closed with a `FRAME_SIZE_ERROR` GOAWAY, refused from the frame header before any payload is buffered |
| Unary response on a dialed channel | `grpc.MAX_MESSAGE_BYTES` plus its 5-byte prefix | `RESOURCE_EXHAUSTED`; the HTTP/2 stream is cancelled as soon as the body passes it |
| Concurrent streams per connection | 128 | `REFUSED_STREAM` |
| Concurrent connections per listener | 512 | The accept loop waits for one to end |
| Idle socket | 2 minutes | The connection is dropped |

A body that is not framed cleanly (truncated, a length prefix that runs past
what arrived, or bytes that were never framed at all) fails with
`INVALID_ARGUMENT` instead of decoding as an all-default message. A unary
request carrying more than one message fails the same way.

## Interceptors

An interceptor wraps the dispatch the way a `std.web` middleware wraps a route
handler. It takes the rest of the chain plus the call, so it can run code
before or after, or answer the call itself and never reach the dispatch:

```pith
fn log_calls(next: fn(String, Bytes) -> Bytes!grpc.GrpcError, path: String, request: Bytes) -> Bytes!grpc.GrpcError:
    print(path)
    return next(path, request)

guarded := grpc.intercept(chat_dispatch, [grpc.authorize(check_token), log_calls])
grpc.serve("0.0.0.0", 50051, guarded)!
```

`intercept` wraps any `fn(String, Bytes) -> Bytes!grpc.GrpcError` dispatch and
returns a dispatch of the same type, so it drops into `serve`, `serve_tls`,
`serve_body`, or `serve_body_tls` without changing a signature. The first
interceptor in the list runs outermost, seeing the call first and the reply
last.

Three come ready-made:

- `grpc.authorize(verify)` runs `verify` against the caller's metadata before
  any method body, and answers `UNAUTHENTICATED` when it returns false. This is
  the transport-level auth hook: one interceptor covers every RPC the server
  exposes. `grpc.bearer_token(metadata)` pulls the token out of an
  `authorization` header (matching the scheme case-insensitively, as RFC 7235
  requires), so verifying a JWT is a one-liner against `std.crypto.jwt`.
- `grpc.rate_limit(limiter)` spends a token per call and answers
  `RESOURCE_EXHAUSTED` when the bucket is dry.
- `grpc.circuit(breaker)` opens on repeated *server* faults (`INTERNAL`,
  `UNAVAILABLE`, `DATA_LOSS`, an expired deadline) and answers `UNAVAILABLE`
  while open. A caller's own error (`NOT_FOUND`, an invalid argument, a refused
  credential) does not count against it, since a client sending bad requests is
  no reason to stop serving good ones.

The last two take the same `std.resilience` `Limiter` and `Breaker` that
`web.rate_limit` and `web.circuit` take, so one limiter can cap an HTTP surface
and a gRPC surface together.

`examples/grpc_interceptors.pith` runs the whole shape end to end: a service
that knows nothing about auth or rate limiting, guarded by both, called by a
client that presents its credential once.

When you write your own, an interceptor that refuses a call returns
`grpc.refuse(status, message)` instead of failing directly, because a Pith
lambda can neither `fail` nor use `!`. Return `next(path, request)` to pass the
call along.

On the client, a credential goes on the channel, so call sites do not repeat
it:

```pith
conn := grpc.dial_h2c("localhost", 50051)!
conn.set_credentials(["authorization", "Bearer " + token])
reply := conn.unary("/chat.Chat/Send", req)!     # carries the credential
```

Per-call metadata still works through the `*_with_headers` forms and is
appended after the channel's, so a call can add to the credentials but not
silently drop them.

## Graceful shutdown

Every `serve*` entry point runs on the HTTP/2 accept loop, so they all drain on
`SIGTERM` once `std.shutdown.on_signals()` has been called. The listener stops
accepting, the RPCs already in progress run to their trailers, and `serve`
returns the work left unfinished at the grace period (`0` for a clean drain).
The drain itself never severs an RPC, and every peer reads a grpc-status.

### Streams that never end

A stream that never ends would outlast any grace period, so the period is split
in half. The first half is the handler's:

```pith
while not call.closing():
    call.send(next_tick())
call.finish_ok()
```

`closing()` is on the live `GrpcStream` and on every generated typed
`ServerCall`, and goes true the moment a drain starts. A handler that polls it
ends its stream on its own terms, with its own status.

The second half is the framework's. Every RPC still open at the midpoint is
finished for it with `UNAVAILABLE` and the message `grpc: server is shutting
down`, and then the connection is wound down: a GOAWAY, a cancel for any
handler parked on a receive, and finally the socket. A handler that ignores
`closing()` entirely, even one asleep between two sends, still leaves its peer
a real grpc-status to act on instead of a truncated body. It also still lets
the drain finish instead of spending the whole period waiting on a peer that
had no reason to hang up.

Handlers should not rely on this fallback. `grpc.streams_closed_by_shutdown()`
counts the RPCs the framework had to close, and anything but `0` names a
handler to fix. See [docs/signals.md](signals.md) for the shared machinery.

## What isn't here yet

- **Unsupported proto features**: proto2, `bool`/`fixed` map keys, repeated
  `sint`/`fixed`/`float`/`double` fields, and the well-known types beyond
  Timestamp, Duration, Empty, and the wrappers (no Any, Struct, FieldMask, …).
  The generator stops with a clear error naming the feature. `oneof` (a payload
  enum per group) and `map<k,v>` (a Pith `Map`, string or integer keys) are
  supported.
- **Message compression and trailing metadata** are not supported. Deadlines
  are enforced at the edges (client timeout, server expiry-on-arrival), not by
  cancelling a handler mid-flight.
- **Observability**: the client opens a trace span and records RED metrics per
  call automatically. The server side is your code. An interceptor is the
  place to put it, since it sees every method.
- **Interceptors are server-side and unary.** The client has channel-wide
  credentials but no interceptor chain of its own, and a server interceptor
  wraps the unary dispatch. The streaming serve forms take their own dispatch
  shapes and are not wrapped by `intercept`.
