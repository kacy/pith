# http apps

pith now has a more practical http layer in `std.net.http`.

the important split is:
- `HttpRequestBytes` is the main request type
- `HttpResponse` is the main response type
- routing stays explicit in normal pith code

that means you can write small apps without hand-rolling query parsing,
response serialization, or request building every time.

## request helpers

use `HttpRequestBytes` for normal server-side code:

```pith
fn app(req: HttpRequestBytes) -> HttpResponse:
    if http.match_route(req, "GET", "/users/:id") catch false:
        user_id := req.path_param("/users/:id", "id") catch ""
        tab := req.query_param("tab") catch ""
        return http.text(200, "user=" + user_id + " tab=" + tab)
    return http.not_found_response()
```

use these helpers instead of reparsing strings by hand:
- `req.query_param(...)`
- `req.query_param_or(...)`
- `req.has_query_param(...)`
- `req.cookie(...)`
- `req.cookie_or(...)`
- `req.has_cookie(...)`
- `req.cookies()`
- `req.path_parts()`
- `req.matches_path(...)`
- `req.path_param(...)`
- `req.path_param_or(...)`
- `req.body_json()`
- `req.body_json_or(...)`

for common verb checks, prefer the small route wrappers:
- `http.get_route(...)`
- `http.post_route(...)`
- `http.put_route(...)`
- `http.delete_route(...)`
- `http.patch_route(...)`

if an upload should go straight to a writer instead of into memory, stream the
body and keep the request metadata:

```pith
out := bytes.buffer()
streamed := http.read_request_buffered_bytes_into(reader, out.buffered_chunked(4096))!

if streamed.matches_path("/upload")!:
    print(streamed.body_size.to_string())
```

`read_request_*_into(...)` gives you the parsed request metadata plus the
decoded body size. the request body itself goes straight into the writer.

## response helpers

`HttpResponse` is a plain value with a small builder surface:

```pith
resp := http.json_value(200, payload).header("X-Trace", "abc123")
http.send_buffered(writer, resp)!
```

the common constructors are:
- `http.response(status)`
- `http.text(status, body)`
- `http.html(status, body)`
- `http.json(status, body)`
- `http.json_value(status, handle)`
- `http.bad_request_response()`
- `http.unauthorized_response()`
- `http.forbidden_response()`
- `http.no_content()`
- `http.not_found_response()`
- `http.redirect_response(url)`

the older raw string helpers still exist for compatibility:
- `html_response(...)`
- `json_response(...)`
- `not_found()`
- `redirect(...)`

but the object form is the better default for new code.

`HttpResponse` also has a few small helpers for the common inspection and
builder cases:
- `resp.is_success()`
- `resp.is_client_error()`
- `resp.is_server_error()`
- `resp.set_cookie(http.cookie(...))`
- `resp.clear_cookie(...)`
- `resp.cookies()`
- `resp.text_body(...)`
- `resp.bytes_body(...)`
- `resp.json_body(...)`
- `resp.content_type(...)`

the cookie builder keeps the common cases out of manual header strings:

```pith
resp := http.text(200, "ok")
    .set_cookie(http.cookie("session", "abc123").path("/").http_only())
```

client requests can carry cookies the same way:

```pith
req := http.get_request("example.test", 80, "/profile").cookie("session", "abc123")
```

## multipart forms

`std.net.http` now has a small multipart surface for the common upload path.

on the request side:
- `req.is_multipart()`
- `req.multipart_boundary()`
- `req.multipart_parts()`
- `req.has_multipart_part(...)`
- `req.multipart_part(...)`
- `req.multipart_text(...)`
- `req.multipart_text_or(...)`

parts keep their raw bytes, so file uploads do not need to decode as utf-8:

```pith
part := req.multipart_part("avatar")!
size := part.body_bytes().len()
filename := part.filename
kind := part.header("Content-Type")
```

for client-side requests, build parts explicitly:

```pith
mut parts: List[http.MultipartPart] := []
parts.push(http.multipart_field("note", "hello"))
parts.push(http.multipart_file("blob", "blob.bin", "application/octet-stream", payload))

req := http.post_request("example.test", 80, "/upload").multipart_body(parts)!
```

if you want a stable boundary for tests, use `multipart_body_with_boundary(...)`.

## middleware

middleware wraps a normal handler and returns another normal handler:

```pith
fn auth(next: fn(HttpRequestBytes) -> HttpResponse, req: HttpRequestBytes) -> HttpResponse:
    if req.cookie_or("session", "") == "":
        return http.unauthorized_response()
    return next(req)

handler := http.wrap(app, auth)
```

stack multiple wrappers by repeating `wrap(...)`.

for the common "log every request and keep a few process metrics" path, use
`http.instrument(...)` with `std.log` and `std.metrics`:

```pith
import std.log as log
import std.metrics as metrics

logger := log.root().json().with([log.str("service", "api")])
handler := http.instrument(app, logger, "http_api")

http.serve_one(reader, writer, handler)!
print(metrics.snapshot_text())
```

that records:
- total requests
- in-flight requests
- per-status totals
- request duration histogram

and it emits one structured `request complete` line per request with method,
path, status, elapsed time, trace id, and span id.

## serving one request

for tests, in-memory examples, or small buffered handlers:

```pith
http.serve_one(reader, writer, app)!
```

that keeps the handler shape simple:
- input: `HttpRequestBytes`
- output: `HttpResponse`

for fd-based servers that already accepted a client connection, use:

```pith
http.serve_fd(client_fd, handler)!
```

for one persistent client connection, use:

```pith
http.serve_connection(reader, writer, handler)!
```

or with an already-accepted fd:

```pith
http.serve_connection_fd(client_fd, handler)!
```

if you want the connection to stay open, set it explicitly on the request or
response builder:

```pith
req := http.get_request("example.test", 80, "/events").keep_alive()
resp := http.text(200, "ok").keep_alive()
```

if a handler wants to recover from parse or query issues, just map those
results into a normal response:

```pith
payload := req.body_json() catch -1
if payload < 0:
    return http.server_error_response()
```

## streaming responses

for larger bodies, you can stream the response instead of building one big
buffer first.

for known sizes, send the head once and then stream the body bytes:

```pith
http.send_sized_response_head(writer, http.response(200).content_type("text/plain"), size)!
http.send_stream_bytes(writer, chunk1)!
http.send_stream_bytes(writer, chunk2)!
```

for open-ended bodies, use chunked transfer encoding:

```pith
http.send_chunked_response_head(writer, http.response(200).content_type("text/plain").keep_alive())!
http.send_chunked_response_text(writer, "hello ")!
http.send_chunked_response_text(writer, "world")!
http.finish_chunked_response(writer)!
```

that gives you a small first-party path for generated exports and event-style
responses without buffering the whole payload in memory.

for file-backed responses, use the path helpers instead of reading the whole
file into memory first:

```pith
http.send_static_path(writer, http.response(200).keep_alive(), "public/app.js")!
http.send_download_path(writer, http.response(200), "build/report.json", "report.json")!
```

`send_static_path(...)` guesses a content type from the file extension.
`send_download_path(...)` does the same thing and adds a normal attachment
`Content-Disposition` header.

if you want the client side of that flow, stream a response body straight into
an output file:

```pith
req := http.get_request("example.test", 80, "/artifact")
saved := req.send_to_file("artifact.bin")!
print(saved.body_size.to_string())
```

there are matching connection-level helpers too:
- `ClientConn.send_to_file(...)`
- `TlsClientConn.send_to_file(...)`

the built-in content type guesser covers the common web assets:
- `.html`, `.htm`
- `.txt`
- `.json`
- `.css`
- `.js`, `.mjs`
- `.svg`
- `.png`, `.jpg`, `.jpeg`, `.gif`, `.webp`
- `.wasm`

everything else falls back to `application/octet-stream`.

for server-sent events, use `std.net.sse` on top of that same chunked path:

```pith
import std.net.sse as sse

sse.start(writer)!
sse.send(writer, sse.named_event("tick", "hello"))!
sse.send_retry(writer, 1500)!
sse.keep_alive(writer)!
```

that sets the usual `text/event-stream` headers and writes correctly framed sse
events instead of making each handler rebuild the wire format.

if your payload is already a pith json value, use the json helpers:

```pith
sse.send_named_json(writer, "tick", payload)!
```

on the client side, you can parse an event-stream body back into frames:

```pith
items := sse.parse_all(resp.body_text()!)!
payload := items[0].json_data()!
```

on the request side, `std.net.sse` also gives you the small header helpers:
- `sse.accepts_stream(req)`
- `sse.last_event_id(req)`

on the client side, you can also stream the decoded response body into a
writer:

```pith
out := bytes.buffer()
meta := http.get_request("example.com", 80, "/dump").no_redirects().send_into(out.buffered_chunked(4096))!

print(meta.status.to_string())
print(out.bytes().len().to_string())
```

`send_into(...)` and `read_response_*_into(...)` return response metadata plus
the streamed body size. the body itself goes straight into the writer.

`send_into(...)` follows redirects the same way `send()` does.

## client helpers

the client side now uses the same explicit shape:

```pith
mut req := http.request("POST", "example.com", 80, "/items")
req = req.header("X-Test", "yes")
req = req.json_body(payload)
raw := req.to_bytes()!
```

for real requests:

```pith
resp := http.get_request("127.0.0.1", 8080, "/health").accept(http.MIME_JSON).send()!
print(resp.status_code().to_string())
print(resp.body_text()!)
```

for transport control, keep it on the request builder:

```pith
resp := http.get_https_request("example.com", "/docs")
    .with_timeout(1500)
    .follow_redirects(3)
    .send()!
```

`follow_redirects(...)` handles:
- relative `Location` headers
- absolute `http://` and `https://` redirects
- method rewrite to `GET` for `301` / `302` / `303` when needed
- auth and cookie header stripping when the redirect changes origin

for client-side keep-alive, send requests on an already-open connection:

```pith
conn := http.connect_client_with_timeout("127.0.0.1", 8080, 1500)!

req1 := http.get_request("127.0.0.1", 8080, "/one").keep_alive()
resp1 := conn.send(req1)!

req2 := http.get_request("127.0.0.1", 8080, "/two").close_connection()
resp2 := conn.send(req2)!
conn.close()
```

the same shape works for tls with `connect_tls_client(...)`.

that keeps request construction, response parsing, and body decoding in one
module instead of splitting the work across app code.

for common write-heavy client paths, prefer the request helpers:

```pith
req := http.post_request("example.com", 80, "/items") \
    .query_param("mode", "fast path") \
    .bearer("token") \
    .json_body(payload)
```
