# HTTP apps

Pith now has a more practical HTTP layer in `std.net.http`.

The important split is:
- `HttpRequestBytes` is the main request type
- `HttpResponse` is the main response type
- Routing stays explicit in normal Pith code

That means you can write small apps without hand-rolling query parsing,
response serialization, or request building every time.

## Request helpers

Use `HttpRequestBytes` for normal server-side code:

```pith
fn app(req: HttpRequestBytes) -> HttpResponse:
    if http.match_route(req, "GET", "/users/:id") catch false:
        user_id := req.path_param("/users/:id", "id") catch ""
        tab := req.query_param("tab") catch ""
        return http.text(200, "user=" + user_id + " tab=" + tab)
    return http.not_found_response()
```

Use these helpers instead of reparsing strings by hand:
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
- `req.form()`, `req.post_form()`, `req.form_value(...)` and
  `req.post_form_value(...)` (see [forms](#forms))
- `req.basic_auth()`

`matches_path` and `path_param` follow the same rules as `std.web`'s router.
The path is split on its literal `/` characters and each segment is
percent-decoded after that, so an encoded slash (`%2F`) never splits a
segment, a literal pattern segment matches its encoded spelling, and a `+` in
a path stays a `+`. A `:name` segment matches one segment and a final
`*name` segment matches the rest of the path. `http.clean_path(path)` gives
the canonical form net/http's `ServeMux` redirects to, and
`http.path_is_clean(path)` checks for it without allocating (see
[canonical paths](web.md#canonical-paths)).

`body_json()` returns a handle into the `std.json` node pool, which lives in
per-task storage and is not freed handle by handle. The `http.serve_*` entry
points open a pool scope around each handler call and close it when the handler
returns, so a kept-alive connection does not carry one request's parse tree into
the next. That also means the handle is only good for the life of the handler:
read what you need out of it before you return, and do not stash it. See the
JSON node pool section in `docs/web.md` for scoping a parse you do yourself.

For common verb checks, prefer the small route wrappers:
- `http.get_route(...)`
- `http.post_route(...)`
- `http.put_route(...)`
- `http.delete_route(...)`
- `http.patch_route(...)`

If an upload should go straight to a writer instead of into memory, stream the
body and keep the request metadata:

```pith
out := io.memory_writer()
streamed := http.read_request_buffered_bytes_into(reader, out.buffered_chunked(4096))!

if streamed.matches_path("/upload")!:
    print(streamed.body_size.to_string())
```

`read_request_*_into(...)` gives you the parsed request metadata plus the
decoded body size. The request body itself goes straight into the writer.

## Response helpers

`HttpResponse` is a plain value with a small builder surface:

```pith
resp := http.json_value(200, payload).header("X-Trace", "abc123")
http.send_buffered(writer, resp)!
```

The common constructors are:
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

The older raw string helpers still exist for compatibility:
- `html_response(...)`
- `json_response(...)`
- `not_found()`
- `redirect(...)`

The object form is the better default for new code.

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

The cookie builder keeps the common cases out of manual header strings:

```pith
resp := (http.text(200, "ok")
    .set_cookie(http.cookie("session", "abc123").path("/").http_only()))
```

A chain that spans lines needs the parentheses: a line that starts with `.`
does not continue the line before it.

Client requests can carry cookies the same way:

```pith
req := http.get_request("example.test", 80, "/profile").cookie("session", "abc123")
```

## Multipart forms

`std.net.http` now has a small multipart surface for the common upload path.

On the request side:
- `req.is_multipart()`
- `req.multipart_boundary()`
- `req.multipart_parts()`
- `req.has_multipart_part(...)`
- `req.multipart_part(...)`
- `req.multipart_text(...)`
- `req.multipart_text_or(...)`

Parts keep their raw bytes, so file uploads do not need to decode as UTF-8:

```pith
part := req.multipart_part("avatar")!
size := part.body_bytes().len()
filename := part.filename
kind := part.header("Content-Type")
```

For client-side requests, build parts explicitly:

```pith
mut parts: List[http.MultipartPart] := []
parts.push(http.multipart_field("note", "hello"))
parts.push(http.multipart_file("blob", "blob.bin", "application/octet-stream", payload))

req := http.post_request("example.test", 80, "/upload").multipart_body(parts)!
```

If you want a stable boundary for tests, use `multipart_body_with_boundary(...)`.

## Forms

`http.parse_form(text)` reads an `application/x-www-form-urlencoded` body or a
query string the way net/http's `url.ParseQuery` does. The result maps each key
to all of its values, in order:

```pith
values := http.parse_form("tag=a&tag=b&q=x+y%21")
values["tag"]       # ["a", "b"]
values["q"][0]      # "x y!"
```

Keys and values are percent-decoded and `+` is a space. The text is split on
the raw `&` and `=` first, so an encoded `%26` stays inside its value, and a
pair with no `=` is a key with an empty value.

On a request:
- `req.post_form()` holds the body's values. Only a `POST`, `PUT` or `PATCH`
  body counts, and only an urlencoded or a `multipart/form-data` one. A
  multipart part that carries a file is not a value; read it with
  `multipart_part`.
- `req.form()` holds the body's values first and then the query string's, as
  net/http's `Request.Form` does.
- `req.form_value(name)` and `req.post_form_value(name)` return the first
  value, or `""`.

## Basic auth

`req.basic_auth()` returns the user and password from an
`Authorization: Basic` header (RFC 7617) as an `http.BasicAuth`. It fails when
the header is missing, names another scheme, or does not decode to
`user:password`. The scheme name is matched without case, and a password may
hold a `:`.

`http.basic_auth_challenge(realm)` is the `401` that asks for credentials,
with `WWW-Authenticate: Basic realm="...", charset="UTF-8"`. Compare the
password with `std.crypto.subtle`, not `==`.

## Middleware

Middleware wraps a normal handler and returns another normal handler:

```pith
fn auth(next: fn(HttpRequestBytes) -> HttpResponse, req: HttpRequestBytes) -> HttpResponse:
    if req.cookie_or("session", "") == "":
        return http.unauthorized_response()
    return next(req)

handler := http.wrap(app, auth)
```

Stack multiple wrappers by repeating `wrap(...)`.

For the common "log every request and keep a few process metrics" path, use
`http.instrument(...)` with `std.log` and `std.metrics`:

```pith
import std.log as log
import std.metrics as metrics

logger := log.root().json().with([log.str("service", "api")])
handler := http.instrument(app, logger, "http_api")

http.serve_one(reader, writer, handler)!
print(metrics.snapshot_text())
```

That records:
- Total requests
- In-flight requests
- Per-status totals
- Request duration histogram

It also emits one structured `request complete` line per request with method,
path, status, elapsed time, trace ID, and span ID.

## Serving one request

For tests, in-memory examples, or small buffered handlers:

```pith
http.serve_one(reader, writer, app)!
```

That keeps the handler shape simple:
- Input: `HttpRequestBytes`
- Output: `HttpResponse`

For fd-based servers that already accepted a client connection, use:

```pith
http.serve_fd(client_fd, handler)!
```

For one persistent client connection, use:

```pith
http.serve_connection(reader, writer, handler)!
```

Or with an already-accepted fd:

```pith
http.serve_connection_fd(client_fd, handler)!
```

If you want the connection to stay open, set it explicitly on the request or
response builder:

```pith
req := http.get_request("example.test", 80, "/events").keep_alive()
resp := http.text(200, "ok").keep_alive()
```

If a handler wants to recover from parse or query issues, map those
results into a normal response:

```pith
payload := req.body_json() catch -1
if payload < 0:
    return http.server_error_response()
```

### Request-size bounds

Every request reader bounds how much one request can make the server buffer, so
a hostile client cannot exhaust memory with an endless head or an over-large
body. The head (the request line plus the header fields) is capped at
`http.MAX_HEADER_BYTES`, and the body at `http.MAX_REQUEST_BODY`. The cap holds
across every reader: the fd path (`read_request`, `read_request_bytes`), the
buffered readers (`read_request_buffered_bytes`, the legacy string-form
`read_request_buffered`), and the TLS accept loop. A head that grows past the
cap fails even when it arrives as a single line with no line break, so no reader
accumulates an unbounded line before the total is checked. The serve loops turn
those failures into `431` / `413` responses and close the connection. A server
can set its own caps with a `ServerConfig`.

### Server configuration

`http.ServerConfig` holds what net/http's `Server` fields hold for one HTTP/1.1
server: its timeouts, its request-size limits and its keep-alive policy. The
`_with` entry points take one:

```pith
config := (http.server_config()
    .with_read_header_timeout(5000)
    .with_idle_timeout(60000)
    .with_max_body_bytes(1048576))
http.serve_connection_fd_with(client_fd, handler, config)!
http.serve_connection_tls_with(conn, handler, config)!
http.serve_connection_with(reader, writer, handler, config)!
```

| Field | `server_config()` | Meaning |
| --- | --- | --- |
| `read_header_timeout_ms` | 0 | How long the request head may take; 0 falls back to `read_timeout_ms` |
| `read_timeout_ms` | 0 | How long the head and the body together may take |
| `read_body_timeout_ms` | 0 | How long the body may take, from the end of the head, sized, chunked or streamed |
| `write_timeout_ms` | 0 | How long the response may take, from the end of the request head, so the handler counts |
| `idle_timeout_ms` | 0 | How long a keep-alive connection may wait for its next request; 0 falls back to `read_timeout_ms` |
| `max_header_bytes` | `MAX_HEADER_BYTES` | The request head, answered with `431` past it |
| `max_body_bytes` | `MAX_REQUEST_BODY` | The request body, declared or chunked, answered with `413` past it |
| `keep_alives` | true | false closes the connection after every response |
| `max_requests_per_conn` | 0 | Close after this many responses; 0 for no limit |
| `stream_request_bodies` | false | true hands the body to the handler as it arrives (see [streaming request bodies](#streaming-request-bodies)) |

A timeout of 0 means none, so `server_config()` matches what the plain entry
points (`serve_connection_fd`, `serve_connection_tls`, `serve_connection`) have
always done, and they use it. The net/http defaults are the same, and they let a
client that sends one byte at a time hold a connection open forever, so a server
facing the internet should set at least `read_header_timeout_ms`. std.web does:
see [web](web.md#timeouts-and-limits).

The timeouts are deadlines on the socket. `tcp.set_timeout` bounds each read on
its own, so a client that sends a byte just inside it keeps a read going
indefinitely. A deadline is a fixed time, so the time left shrinks with every
read. The clocks start here:

- The first request's head has `read_header_timeout_ms` from when the connection
  was handed to the server, the wait for its first byte included.
- A later request first waits up to `idle_timeout_ms` for a byte, then has
  `read_header_timeout_ms` from that byte.
- `read_timeout_ms` counts from the same point as the head's time, and covers the
  body too.
- `read_body_timeout_ms` counts from the end of the head. When `read_timeout_ms`
  is set as well, the earlier deadline applies. A handler reading a streamed body
  sees it run out as a `read_body_part` failure equal to
  `http.BODY_TIMEOUT_MESSAGE`.

A request that runs out of read time once its first byte has arrived is answered
with `408 Request Timeout` and the connection closes. A connection that runs out
of time with nothing sent, the first request's or an idle keep-alive's, is closed
with no answer, as net/http does. A response that runs out of write time is cut
off, and the connection closes. The deadlines are on the TCP connection, so they
hold for TLS too, and they work on both the green and the OS-thread backend.
`serve_connection_with` reads from memory, so only the limits and the keep-alive
policy apply there.

The deadlines are available on any socket as `tcp.set_read_deadline(fd, ms)` and
`tcp.set_write_deadline(fd, ms)`. A deadline holds until it is set again or
cleared with 0, and when the socket also has a `set_timeout`, a wait ends at
whichever comes first.

A handler cannot see that its client went away while it runs: the handler type
carries no `Context`, and noticing a disconnect would take a second wait on the
socket while the handler runs, which reads the next pipelined request if there
is one. A handler has no way to fail either. It returns an `HttpResponse`, so an
error becomes a response through `catch` as above. A runtime error such as an
index out of range fails that request alone: the server answers `500` and
closes the connection, as net/http does after a panic, prints the error to
stderr with the method and path, and goes on serving. The failed handler's
`defer` blocks do not run and what it held is not freed; a `Mutex` it held is
unlocked (docs/failure_isolation.md).

### What the server adds to a response

The serve loops (`serve_connection`, `serve_connection_fd`, `serve_connection_tls`, `serve_one`, `serve_fd`) frame a handler's response the way RFC 9110 and RFC 9112 ask:

- Every response carries a `Date` in IMF-fixdate form, formatted at most once a second per connection task. A `Date` the handler set is sent in its place. `http.http_date(seconds)` formats a Unix time the same way, for `Last-Modified` or `Expires`.
- The answer to `HEAD` has the headers the handler's `GET` answer would have, including its `Content-Length`, and no body. A handler that answers `HEAD` without building the body can set `Content-Length` itself.
- `1xx`, `204` and `304` responses never carry a body. `1xx` and `204` carry no `Content-Length` or `Transfer-Encoding`, and a `304` keeps a `Content-Length` only when the handler set one.
- In every other response `Content-Length` is the length of the body sent. A different value the handler set is replaced, and a `Transfer-Encoding` it set is dropped, so the two never go out together.
- An HTTP/1.1 connection stays open unless the request or the handler lists `close` in `Connection`. Then the response says `Connection: close` and the connection closes. A response on a connection that stays open carries no `Connection` field. An HTTP/1.0 connection closes unless the request asks for `keep-alive`, and when it does the response echoes `Connection: keep-alive`.
- A request with `Expect: 100-continue` and a body gets `HTTP/1.1 100 Continue` just before the server reads the body. The server reads the body before the handler runs, so the interim response goes out even when the handler ignores the body. Any other expectation gets `417`, and the connection closes without the body being read. Expectations on HTTP/1.0 requests are ignored.
- A status with no standard reason phrase is sent with an empty one.

The writer-level streaming helpers (`send_sized_response_head`, `send_chunked_response_head` and the helpers built on them) never see the request. They add no `Date` and cannot drop the body of a `HEAD` answer, so a caller streaming an answer to `HEAD` sends the head and skips the body itself. They do follow the status rules above for the framing fields.

## Streaming responses

For larger bodies, you can stream the response instead of building one big
buffer first.

For known sizes, send the head once and then stream the body bytes:

```pith
http.send_sized_response_head(writer, http.response(200).content_type("text/plain"), size)!
http.send_stream_bytes(writer, chunk1)!
http.send_stream_bytes(writer, chunk2)!
```

For open-ended bodies, use chunked transfer encoding:

```pith
http.send_chunked_response_head(writer, http.response(200).content_type("text/plain").keep_alive())!
http.send_chunked_response_text(writer, "hello ")!
http.send_chunked_response_text(writer, "world")!
http.finish_chunked_response(writer)!
```

That gives you a small first-party path for generated exports and event-style
responses without buffering the whole payload in memory.

For file-backed responses, use the path helpers instead of reading the whole
file into memory first:

```pith
http.send_static_path(writer, http.response(200).keep_alive(), "public/app.js")!
http.send_download_path(writer, http.response(200), "build/report.json", "report.json")!
```

`send_static_path(...)` guesses a content type from the file extension.
`send_download_path(...)` does the same thing and adds a normal attachment
`Content-Disposition` header.

If you want the client side of that flow, stream a response body straight into
an output file:

```pith
req := http.get_request("example.test", 80, "/artifact")
saved := req.send_to_file("artifact.bin")!
print(saved.body_size.to_string())
```

There are matching connection-level helpers too:
- `ClientConn.send_to_file(...)`
- `TlsClientConn.send_to_file(...)`

The built-in content type guesser covers the common web assets:
- `.html`, `.htm`
- `.txt`
- `.json`
- `.css`
- `.js`, `.mjs`
- `.svg`
- `.png`, `.jpg`, `.jpeg`, `.gif`, `.webp`
- `.wasm`

Everything else falls back to `application/octet-stream`.

For server-sent events, use `std.net.sse` on top of that same chunked path:

```pith
import std.net.sse as sse

sse.start(writer)!
sse.send(writer, sse.named_event("tick", "hello"))!
sse.send_retry(writer, 1500)!
sse.keep_alive(writer)!
```

That sets the usual `text/event-stream` headers and writes correctly framed SSE
events instead of making each handler rebuild the wire format.

If your payload is already a Pith JSON value, use the JSON helpers:

```pith
sse.send_named_json(writer, "tick", payload)!
```

On the client side, you can parse an event-stream body back into frames:

```pith
items := sse.parse_all(resp.body_text()!)!
payload := items[0].json_data()!
```

On the request side, `std.net.sse` also gives you the small header helpers:
- `sse.accepts_stream(req)`
- `sse.last_event_id(req)`

On the client side, you can also stream the decoded response body into a
writer:

```pith
out := io.memory_writer()
meta := http.get_request("example.com", 80, "/dump").no_redirects().send_into(out.buffered_chunked(4096))!

print(meta.status.to_string())
print(out.bytes().len().to_string())
```

`send_into(...)` and `read_response_*_into(...)` return response metadata plus
the streamed body size. The body itself goes straight into the writer.

`send_into(...)` follows redirects the same way `send()` does.

## Streaming request bodies

The server normally reads a request's whole body before the handler runs, up
to `max_body_bytes`. With `with_stream_request_bodies(true)`, an HTTP/1.1
handler runs as soon as the head is in, with an empty `body`, and reads the
body itself with `http.read_body_part(max)`. Each call returns up to `max`
bytes as soon as any arrive, sized or chunked, and an empty `Bytes` at the end:

```pith
fn save_body(path: String) -> Int!:
    out := fs.create(path)!
    defer out.close()
    mut total := 0
    while true:
        part := http.read_body_part(65536)!
        if part.is_empty():
            return total
        total = total + out.write_all_bytes(part)!
    return total

fn upload(req: http.HttpRequestBytes) -> http.HttpResponse:
    if save_body("upload.bin").is_err:
        return http.bad_request_response()
    return http.no_content()

config := http.server_config().with_stream_request_bodies(true)
```

`max_body_bytes` does not apply, since the handler decides how much it keeps.
A body the handler leaves unread is drained after it returns, up to 256 KiB,
so the next request on the connection is read from the right place. A body
with more than that left, or one that failed partway, closes the connection
after the response. An `Expect: 100-continue` request still gets its interim
response before the handler runs.

HTTP/2 requests are read whole as before, and `read_body_part` returns empty
for them, so a handler that serves both should read `req.body` when the first
part comes back empty.

## Taking over a connection

A handler can take the raw HTTP/1.1 connection, to copy bytes between it and
an upstream for a WebSocket or any other upgrade:

```pith
fn upgrade(req: http.HttpRequestBytes) -> http.HttpResponse:
    return http.take_connection(relay)

fn relay(r: io.BytesReader, w: io.BytesWriter) -> Int!:
    w.write_bytes(bytes.from_string_utf8("HTTP/1.1 101 Switching Protocols\r\n\r\n"))!
    while true:
        got := r.read_bytes(4096)!
        if got.is_empty():
            return 0
        w.write_bytes(got)!
    return 0
```

The server sends nothing for that request. It clears the connection's read and
write deadlines and calls the function on the same task with the connection's
two halves, an `io.BytesReader` and an `io.BytesWriter`. `read_bytes(n)`
returns up to `n` bytes as soon as any arrive and an empty `Bytes` at the end
of input, and `write_bytes(data)` sends all of `data`. The function writes
whatever answer the protocol needs, such as a `101`, and the connection closes
when it returns. It works the same over TLS.

Use the reader only on the task the function runs on, because the
connection's buffered reader may already hold bytes that arrived behind the
request head. The writer goes straight to the socket or the TLS connection, so
a task the function spawns, such as one copying the other direction, can use
it.

HTTP/2 has no upgrade. There, and wherever a middleware replaced the handler's
response, the `501` that `take_connection` returns is sent instead.
`examples/gateway/tunnel.pith` uses this to proxy WebSockets.

To serve a WebSocket session yourself, return
`websocket.take_connection(req, session)` from `std.net.websocket`. It checks
that the request is an upgrade (a `400` if not), writes the handshake over the
taken connection and runs `session` with a `websocket.WebSocket`.
`examples/http_websocket_app.pith` does this from a std.web route.

## Serving files and content

`http.serve_file(req, path)` answers a request with a file, and
`http.serve_content(req, name, modified, content)` with bytes already in
memory. They are the counterparts of net/http's `ServeFile` and
`ServeContent`, and they return an `HttpResponse`:

```pith
fn download(req: HttpRequestBytes) -> HttpResponse:
    return http.serve_file(req, "build/report.pdf")
```

Both send:
- `Content-Type` from the name's extension. When the table above does not
  know the extension, the type is sniffed from the first 512 bytes with
  `http.detect_content_type`.
- `Last-Modified` and a strong `ETag` built from the modification time and
  the size. `serve_content` takes the modification time in Unix seconds, and
  `0` sends neither.
- `Accept-Ranges: bytes`

They also evaluate the conditional headers in the order RFC 9110 13.2.2 gives:
1. `If-Match` that names no current tag, or failing that an
   `If-Unmodified-Since` older than the modification time, gets `412`.
2. `If-None-Match` that names the tag gets `304` on `GET` and `HEAD` and
   `412` otherwise. The comparison is weak, so `W/"..."` matches.
3. Without `If-None-Match`, an `If-Modified-Since` no older than the
   modification time gets `304`.

A `304` carries the `ETag` and no body.

A `GET` with `Range` (RFC 9110 14) gets `206`. One range is sent as is with
`Content-Range`, several as a `multipart/byteranges` body, and a range the
content cannot satisfy gets `416` with `Content-Range: bytes */size`.
`If-Range` with the current strong tag or the exact modification date allows
the range. Anything else sends the whole content. A `Range` header is
ignored, and the whole content sent, when it is malformed, uses another unit,
asks for more than 32 ranges, or adds up to more than the content. `HEAD`
ignores `Range` and gets the headers of the full answer with no body.

`serve_file` reads only the bytes it sends, so a `304`, a `HEAD` or a small
range does not read the whole file. A path that is not a regular file gets
`404`, and one with a NUL byte `400`.

`http.serve_dir(req, root, name)` serves `name` relative to `root`, as
net/http's `FileServer` does. `name` is already percent-decoded, such as a
`*path` route parameter. A `..` segment gets `404`, so nothing outside `root`
is read, and a directory gets its `index.html` or `404`. Directories are never
listed. `web.App.static` is built on it.

`http.detect_content_type(bytes)` is the sniffer on its own, in the spirit of
net/http's `DetectContentType`. It knows HTML, XML, JSON, PDF, PNG, JPEG, GIF,
WebP, ZIP, gzip and Wasm by their signatures, calls anything else with no
binary bytes in its first 512 `text/plain; charset=utf-8`, and the rest
`application/octet-stream`. It is never applied to a handler's response on
its own: a Pith handler always sets a `Content-Type`.

`http.parse_http_date(text)` reads the three HTTP date forms (RFC 9110
5.6.7) into Unix seconds, and `fs.modified(path)` gives a file's modification
time in the same unit.

## Client helpers

The client side now uses the same explicit shape:

```pith
mut req := http.request("POST", "example.com", 80, "/items")
req = req.header("X-Test", "yes")
req = req.json_body(payload)
raw := req.to_bytes()!
```

For real requests:

```pith
resp := http.get_request("127.0.0.1", 8080, "/health").accept(http.MIME_JSON).send()!
print(resp.status_code().to_string())
print(resp.body_text()!)
```

For transport control, keep it on the request builder:

```pith
resp := (http.get_https_request("example.com", "/docs")
    .with_timeout(1500)
    .follow_redirects(3)
    .send())!
```

`follow_redirects(...)` handles:
- Relative `Location` headers
- Absolute `http://` and `https://` redirects
- Method rewrite to `GET` for `301` / `302` / `303` when needed
- Auth and cookie header stripping when the redirect changes origin

For client-side keep-alive, send requests on an already-open connection:

```pith
conn := http.connect_client_with_timeout("127.0.0.1", 8080, 1500)!

req1 := http.get_request("127.0.0.1", 8080, "/one").keep_alive()
resp1 := conn.send(req1)!

req2 := http.get_request("127.0.0.1", 8080, "/two").close_connection()
resp2 := conn.send(req2)!
conn.close()
```

The same shape works for TLS with `connect_tls_client(...)`.

That keeps request construction, response parsing, and body decoding in one
module instead of splitting the work across app code.

For common write-heavy client paths, prefer the request helpers:

```pith
req := (http.post_request("example.com", 80, "/items")
    .query_param("mode", "fast path")
    .bearer("token")
    .json_body(payload))
```

## The pooled client

`http.client()` returns an `http.Client`, the counterpart of net/http's
`Client` and `Transport`. The request helpers above open one connection per
request; a `Client` keeps connections open and reuses them.

```pith
c := (http.client()
    .with_timeout(5000)
    .with_response_header_timeout(2000)
    .with_max_idle_conns_per_host(4))

resp := c.get("http://127.0.0.1:8080/health")!
created := c.send(http.new_request("POST", "http://127.0.0.1:8080/items")!.json_body(payload))!
```

`http.new_request(method, url)` builds a `ClientRequest` from an absolute
`http://` or `https://` URL. Every builder method on `ClientRequest` works on
it. `Client` has `send`, `send_into`, `send_to_file`, `open`, `get`, `head` and
`post`.

### Settings

Each `with_*` method returns a new `Client` that shares the idle connections
of the one it came from.

| Method | Default | What it does |
| --- | --- | --- |
| `with_timeout(ms)` | 0 (none) | Limit on the whole request, redirects and body included |
| `with_dial_timeout(ms)` | 0 (none) | Limit on one connect |
| `with_response_header_timeout(ms)` | 0 (none) | Limit on the wait for the response head after the request is written |
| `with_max_idle_conns_per_host(n)` | 2 | Idle connections kept per origin |
| `with_idle_conn_timeout(ms)` | 90000 | How long an idle connection is kept |
| `with_max_redirects(n)` | 10 | Redirects followed before the request fails; 0 returns the redirect |
| `with_keep_alives(on)` | true | false sends `Connection: close` and reuses nothing |
| `with_compression(on)` | true | Ask for gzip and decode it |
| `with_expect_continue_timeout(ms)` | 1000 | Wait for `100 Continue` before sending the body |
| `with_proxy(url)` | None | Send every request through this HTTP proxy |
| `with_env_proxy(on)` | false | Take the proxy from `HTTP_PROXY`, `HTTPS_PROXY` and `NO_PROXY` |
| `with_tls_config(config)` | Default roots | TLS config for HTTPS origins (released on drop when the last holder, including the client, goes away; only call `close()` once no client uses it) |
| `with_max_response_header_bytes(n)` | `http.MAX_RESPONSE_HEADER_BYTES` (1 MiB) | The most a response head may hold |
| `with_max_response_body_bytes(n)` | `http.MAX_RESPONSE_BODY` (64 MiB) | The most a buffered response body may hold, after gzip decoding |

A request's own `with_timeout(ms)` replaces the client's `timeout_ms` for
that request. A timeout fails with `http: client timeout exceeded`, or with
`http: timeout awaiting response headers` when the header timeout ran out
first. The timeouts are deadlines on the socket, so a server that sends one
byte at a time still runs out of time.

The runtime's connect has no deadline of its own. With a dial timeout, the
connect runs in a separate task and the client stops waiting for it when the
timeout passes. If the connect completes later, that task closes the
connection.

### Connection reuse

A connection goes back to the pool once its response body has been read,
unless either side asked to close it or the body ran to the end of the
connection. Before an idle connection is reused, the client checks that it
has not idled past `idle_conn_timeout_ms` and that nothing is waiting to be
read on it. If bytes are waiting, the server has closed the connection, and
the client dials a new one.

The server can still close a connection just as a request goes out on it. If
that happens before any of the response arrives, the request is sent once
more on a new connection when it is safe to replay: `GET`, `HEAD`, `OPTIONS`
and `TRACE`, or any request that carries an `Idempotency-Key` or
`X-Idempotency-Key` header, as in net/http.

`close_idle_connections()` closes every idle connection, and
`idle_connections()` counts them.

### Responses

The client checks the response head as strictly as the server checks a
request: a malformed status line, a malformed header field, conflicting
`Content-Length` values, `Transfer-Encoding` together with `Content-Length`,
and transfer codings other than `chunked` all fail the request. The body is
framed as RFC 9112 6.3 describes. The answers to `HEAD` and the `1xx`, `204`
and `304` responses have no body. Otherwise the body is chunked, has a
`Content-Length`, or runs until the server closes the connection. Interim
`1xx` responses before the final one are skipped. Trailer fields of a chunked
body are returned in the response's `trailers`.

A buffered response is bounded the way the server bounds a request. The head
may hold `http.MAX_RESPONSE_HEADER_BYTES` (1 MiB, net/http's default) and the
body `http.MAX_RESPONSE_BODY` (64 MiB), counted after gzip decoding. A
`Content-Length` past the limit fails the request before any of the body is
read. A chunked body, or one that runs until the server closes, fails as soon
as it passes the limit. Both fail with `http: response body exceeds the
maximum`. `with_max_response_body_bytes(n)` sets the body limit for a client,
and the same method on a `ClientRequest` sets it for one request; that is also
how the one-shot helpers and `ClientConn` accept a larger body. A body
streamed with `send_into` or `send_to_file` is written out as it arrives and
has no limit.

When compression is on, a request that names no `Accept-Encoding` asks for
gzip, and a gzip body is decoded before it is returned. The decoded response
has no `Content-Encoding` or `Content-Length`. Streamed bodies (`send_into`,
`send_to_file`), `HEAD` requests and requests with a `Range` header are not
compressed.

### Redirects

The client follows `301`, `302`, `303`, `307` and `308` as net/http does.
`307` and `308` resend the method and the body. `301`, `302` and `303` turn
any method other than `GET` and `HEAD` into `GET` and drop the body. A
relative `Location` is resolved against the current URL. A redirect to
another scheme, host or port drops `Authorization` and `Cookie`. net/http
keeps them when only the port changes, or when the new host is a subdomain
of the old one. A redirect without a `Location` is returned as the response.
Past `max_redirects`, the request fails with
`http: stopped after 10 redirects`.

### Request bodies

A body goes out with a `Content-Length`. A `POST`, `PUT` or `PATCH` with an
empty body sends `Content-Length: 0`; other methods with no body send no
length. Naming `Transfer-Encoding: chunked` on the request sends the body
chunked instead.

`req.expect_continue()` adds `Expect: 100-continue`. The client then sends
the head, waits up to `expect_continue_timeout_ms` for `100 Continue`, and
then sends the body. If the server answers with a final status first, that
answer is returned, the body is never sent, and the connection is closed.

### Proxies

`with_proxy("http://proxy:3128")` sends every request through that proxy.
A plain HTTP request goes to the proxy with the full URL as its target. An
HTTPS request opens a `CONNECT` tunnel through the proxy, and TLS to the
origin runs inside the tunnel. Only plain HTTP proxies are supported here
(standalone SOCKS5 dialing lives in `std.net.socks5`). Credentials in the
proxy URL (`http://user:pass@proxy:3128`) or an explicit `Proxy-Authorization`
header on the request are sent to the proxy on plain requests and `CONNECT`
tunnels, and stripped from the tunneled origin request so they never leak to
the target server.

`with_env_proxy(true)` reads `HTTP_PROXY` for HTTP URLs and `HTTPS_PROXY`
for HTTPS URLs (or their lowercase forms) on each request, with net/http's
rules for `NO_PROXY`:

- `localhost` and loopback addresses always go direct.
- `*` sends everything direct.
- `example.com` matches that host and its subdomains, and `.example.com`
  matches only its subdomains.
- An entry with a port, such as `example.com:8080`, matches only that port.

CIDR ranges in `NO_PROXY` are not supported.

### Not covered

Compared with net/http, the client has no cookie jar, no HTTP/2, no
`CheckRedirect` hook, no limit on connections per host beyond the idle limit,
no SOCKS or HTTPS proxies (see `std.net.socks5` for standalone SOCKS5), and no
streaming of a gzip body. `client.open(req)` streams a response body
incrementally via `OpenResponse` (without following redirects or decoding
gzip); `send` reads the body in full up to the body limit, and `send_into` /
`send_to_file` stream it straight into a writer or file.
