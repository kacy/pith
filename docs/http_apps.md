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
- `req.form()`, `req.post_form()`, `req.form_value(...)` and
  `req.post_form_value(...)` (see [forms](#forms))
- `req.basic_auth()`

`matches_path` and `path_param` follow the same rules as `std.web`'s router.
the path is split on its literal `/` characters and each segment is
percent-decoded after that, so an encoded slash (`%2F`) never splits a
segment, a literal pattern segment matches its encoded spelling, and a `+` in
a path stays a `+`. a `:name` segment matches one segment and a final
`*name` segment matches the rest of the path. `http.clean_path(path)` gives
the canonical form net/http's `ServeMux` redirects to, and
`http.path_is_clean(path)` checks for it without allocating (see
[canonical paths](web.md#canonical-paths)).

`body_json()` returns a handle into the `std.json` node pool, which lives in
per-task storage and is not freed handle by handle. the `http.serve_*` entry
points open a pool scope around each handler call and close it when the handler
returns, so a kept-alive connection does not carry one request's parse tree into
the next. that also means the handle is only good for the life of the handler:
read what you need out of it before you return, and do not stash it. see the
json node pool section in `docs/web.md` for scoping a parse you do yourself.

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

## forms

`http.parse_form(text)` reads an `application/x-www-form-urlencoded` body or a
query string the way net/http's `url.ParseQuery` does. the result maps each key
to all of its values, in order:

```pith
values := http.parse_form("tag=a&tag=b&q=x+y%21")
values["tag"]       # ["a", "b"]
values["q"][0]      # "x y!"
```

keys and values are percent-decoded and `+` is a space. the text is split on
the raw `&` and `=` first, so an encoded `%26` stays inside its value, and a
pair with no `=` is a key with an empty value.

on a request:
- `req.post_form()` holds the body's values. only a `POST`, `PUT` or `PATCH`
  body counts, and only an urlencoded or a `multipart/form-data` one. a
  multipart part that carries a file is not a value; read it with
  `multipart_part`
- `req.form()` holds the body's values first and then the query string's, as
  net/http's `Request.Form` does
- `req.form_value(name)` and `req.post_form_value(name)` return the first
  value, or `""`

## basic auth

`req.basic_auth()` returns the user and password from an
`Authorization: Basic` header (rfc 7617) as an `http.BasicAuth`. it fails when
the header is missing, names another scheme, or does not decode to
`user:password`. the scheme name is matched without case, and a password may
hold a `:`.

`http.basic_auth_challenge(realm)` is the `401` that asks for credentials,
with `WWW-Authenticate: Basic realm="...", charset="UTF-8"`. compare the
password with `std.crypto.subtle`, not `==`.

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

### request-size bounds

every request reader bounds how much one request can make the server buffer, so
a hostile client cannot exhaust memory with an endless head or an over-large
body. the head — the request line plus the header fields — is capped at
`http.MAX_HEADER_BYTES`, and the body at `http.MAX_REQUEST_BODY`. the cap holds
across every reader: the fd path (`read_request`, `read_request_bytes`), the
buffered readers (`read_request_buffered_bytes`, the legacy string-form
`read_request_buffered`), and the tls accept loop. a head that grows past the
cap fails even when it arrives as a single line with no line break, so no reader
accumulates an unbounded line before the total is checked. the serve loops turn
those failures into `431` / `413` responses and close the connection. a server
can set its own caps with a `ServerConfig`.

### server configuration

`http.ServerConfig` holds what net/http's `Server` fields hold for one http/1.1
server: its timeouts, its request-size limits and its keep-alive policy. the
`_with` entry points take one:

```pith
config := http.server_config()
    .with_read_header_timeout(5000)
    .with_idle_timeout(60000)
    .with_max_body_bytes(1048576)
http.serve_connection_fd_with(client_fd, handler, config)!
http.serve_connection_tls_with(conn, handler, config)!
http.serve_connection_with(reader, writer, handler, config)!
```

| field | `server_config()` | meaning |
| --- | --- | --- |
| `read_header_timeout_ms` | 0 | how long the request head may take; 0 falls back to `read_timeout_ms` |
| `read_timeout_ms` | 0 | how long the head and the body together may take |
| `write_timeout_ms` | 0 | how long the response may take, from the end of the request head, so the handler counts |
| `idle_timeout_ms` | 0 | how long a keep-alive connection may wait for its next request; 0 falls back to `read_timeout_ms` |
| `max_header_bytes` | `MAX_HEADER_BYTES` | the request head, answered with `431` past it |
| `max_body_bytes` | `MAX_REQUEST_BODY` | the request body, declared or chunked, answered with `413` past it |
| `keep_alives` | true | false closes the connection after every response |
| `max_requests_per_conn` | 0 | close after this many responses; 0 for no limit |

a timeout of 0 means none, so `server_config()` matches what the plain entry
points (`serve_connection_fd`, `serve_connection_tls`, `serve_connection`) have
always done, and they use it. net/http has the same defaults, and they let a
client that sends one byte at a time hold a connection open forever, so a server
facing the internet should set at least `read_header_timeout_ms`. std.web does:
see [web](web.md#timeouts-and-limits).

the timeouts are deadlines on the socket. `tcp.set_timeout` bounds each read on
its own, so a client that sends a byte just inside it keeps a read going
indefinitely. a deadline is a fixed time, so the time left shrinks with every
read. the clocks start here:

- the first request's head has `read_header_timeout_ms` from when the connection
  was handed to the server, the wait for its first byte included.
- a later request first waits up to `idle_timeout_ms` for a byte, then has
  `read_header_timeout_ms` from that byte.
- `read_timeout_ms` counts from the same point as the head's time, and covers the
  body too.

a request that runs out of read time once its first byte has arrived is answered
with `408 Request Timeout` and the connection closes. a connection that runs out
of time with nothing sent, the first request's or an idle keep-alive's, is closed
with no answer, as net/http does. a response that runs out of write time is cut
off, and the connection closes. the deadlines are on the tcp connection, so they
hold for tls too, and they work on both the green and the os-thread backend.
`serve_connection_with` reads from memory, so only the limits and the keep-alive
policy apply there.

the deadlines are available on any socket as `tcp.set_read_deadline(fd, ms)` and
`tcp.set_write_deadline(fd, ms)`. a deadline holds until it is set again or
cleared with 0, and when the socket also has a `set_timeout`, a wait ends at
whichever comes first.

a handler cannot see that its client went away while it runs: the handler type
carries no `Context`, and noticing a disconnect would take a second wait on the
socket while the handler runs, which reads the next pipelined request if there
is one. a handler has no way to fail either. it returns an `HttpResponse`, so an
error becomes a response through `catch` as above, and a runtime error such as
an index out of range stops the whole process, which net/http would turn into a
`500` and a closed connection.

### what the server adds to a response

the serve loops (`serve_connection`, `serve_connection_fd`, `serve_connection_tls`, `serve_one`, `serve_fd`) frame a handler's response the way rfc 9110 and rfc 9112 ask:

- every response carries a `Date` in IMF-fixdate form, formatted at most once a second per connection task. a `Date` the handler set is sent in its place. `http.http_date(seconds)` formats a unix time the same way, for `Last-Modified` or `Expires`.
- the answer to `HEAD` has the headers the handler's `GET` answer would have, including its `Content-Length`, and no body. a handler that answers `HEAD` without building the body can set `Content-Length` itself.
- `1xx`, `204` and `304` responses never carry a body. `1xx` and `204` carry no `Content-Length` or `Transfer-Encoding`, and a `304` keeps a `Content-Length` only when the handler set one.
- in every other response `Content-Length` is the length of the body sent. a different value the handler set is replaced, and a `Transfer-Encoding` it set is dropped, so the two never go out together.
- an http/1.1 connection stays open unless the request or the handler lists `close` in `Connection`. then the response says `Connection: close` and the connection closes. a response on a connection that stays open carries no `Connection` field. an http/1.0 connection closes unless the request asks for `keep-alive`, and when it does the response echoes `Connection: keep-alive`.
- a request with `Expect: 100-continue` and a body gets `HTTP/1.1 100 Continue` just before the server reads the body. the server reads the body before the handler runs, so the interim response goes out even when the handler ignores the body. any other expectation gets `417`, and the connection closes without the body being read. expectations on http/1.0 requests are ignored.
- a status with no standard reason phrase is sent with an empty one.

the writer-level streaming helpers (`send_sized_response_head`, `send_chunked_response_head` and the helpers built on them) never see the request. they add no `Date` and cannot drop the body of a `HEAD` answer, so a caller streaming an answer to `HEAD` sends the head and skips the body itself. they do follow the status rules above for the framing fields.

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

## serving files and content

`http.serve_file(req, path)` answers a request with a file, and
`http.serve_content(req, name, modified, content)` with bytes already in
memory. they are the counterparts of net/http's `ServeFile` and
`ServeContent`, and they return an `HttpResponse`:

```pith
fn download(req: HttpRequestBytes) -> HttpResponse:
    return http.serve_file(req, "build/report.pdf")
```

both send:
- `Content-Type` from the name's extension. when the table above does not
  know the extension, the type is sniffed from the first 512 bytes with
  `http.detect_content_type`
- `Last-Modified` and a strong `ETag` built from the modification time and
  the size. `serve_content` takes the modification time in unix seconds, and
  `0` sends neither
- `Accept-Ranges: bytes`

and they evaluate the conditional headers in the order rfc 9110 13.2.2 gives:
1. `If-Match` that names no current tag, or failing that an
   `If-Unmodified-Since` older than the modification time, gets `412`
2. `If-None-Match` that names the tag gets `304` on `GET` and `HEAD` and
   `412` otherwise. the comparison is weak, so `W/"..."` matches
3. without `If-None-Match`, an `If-Modified-Since` no older than the
   modification time gets `304`

a `304` carries the `ETag` and no body.

a `GET` with `Range` (rfc 9110 14) gets `206`. one range is sent as is with
`Content-Range`, several as a `multipart/byteranges` body, and a range the
content cannot satisfy gets `416` with `Content-Range: bytes */size`.
`If-Range` with the current strong tag or the exact modification date allows
the range. anything else sends the whole content. a `Range` header is
ignored, and the whole content sent, when it is malformed, uses another unit,
asks for more than 32 ranges, or adds up to more than the content. `HEAD`
ignores `Range` and gets the headers of the full answer with no body.

`serve_file` reads only the bytes it sends, so a `304`, a `HEAD` or a small
range does not read the whole file. a path that is not a regular file gets
`404`, and one with a NUL byte `400`.

`http.serve_dir(req, root, name)` serves `name` relative to `root`, as
net/http's `FileServer` does. `name` is already percent-decoded, such as a
`*path` route parameter. a `..` segment gets `404`, so nothing outside `root`
is read, and a directory gets its `index.html` or `404`. directories are never
listed. `web.App.static` is built on it.

`http.detect_content_type(bytes)` is the sniffer on its own, in the spirit of
net/http's `DetectContentType`. it knows html, xml, json, pdf, png, jpeg, gif,
webp, zip, gzip and wasm by their signatures, calls anything else with no
binary bytes in its first 512 `text/plain; charset=utf-8`, and the rest
`application/octet-stream`. it is never applied to a handler's response on
its own: a pith handler always sets a `Content-Type`.

`http.parse_http_date(text)` reads the three http date forms (rfc 9110
5.6.7) into unix seconds, and `fs.modified(path)` gives a file's modification
time in the same unit.

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

## the pooled client

`http.client()` returns an `http.Client`, the counterpart of net/http's
`Client` and `Transport`. the request helpers above open one connection per
request; a `Client` keeps connections open and reuses them.

```pith
c := http.client()
    .with_timeout(5000)
    .with_response_header_timeout(2000)
    .with_max_idle_conns_per_host(4)

resp := c.get("http://127.0.0.1:8080/health")!
created := c.send(http.new_request("POST", "http://127.0.0.1:8080/items")!.json_body(payload))!
```

`http.new_request(method, url)` builds a `ClientRequest` from an absolute
`http://` or `https://` url. every builder method on `ClientRequest` works on
it. `Client` has `send`, `send_into`, `send_to_file`, `get`, `head` and
`post`.

### settings

each `with_*` method returns a new `Client` that shares the idle connections
of the one it came from.

| method | default | what it does |
| --- | --- | --- |
| `with_timeout(ms)` | 0 (none) | limit on the whole request, redirects and body included |
| `with_dial_timeout(ms)` | 0 (none) | limit on one connect |
| `with_response_header_timeout(ms)` | 0 (none) | limit on the wait for the response head after the request is written |
| `with_max_idle_conns_per_host(n)` | 2 | idle connections kept per origin |
| `with_idle_conn_timeout(ms)` | 90000 | how long an idle connection is kept |
| `with_max_redirects(n)` | 10 | redirects followed before the request fails; 0 returns the redirect |
| `with_keep_alives(on)` | true | false sends `Connection: close` and reuses nothing |
| `with_compression(on)` | true | ask for gzip and decode it |
| `with_expect_continue_timeout(ms)` | 1000 | wait for `100 Continue` before sending the body |
| `with_proxy(url)` | none | send every request through this http proxy |
| `with_env_proxy(on)` | false | take the proxy from `HTTP_PROXY`, `HTTPS_PROXY` and `NO_PROXY` |
| `with_tls_config(config)` | default roots | tls config for https origins; the caller closes it |

a request's own `with_timeout(ms)` replaces the client's `timeout_ms` for
that request. a timeout fails with `http: client timeout exceeded`, or with
`http: timeout awaiting response headers` when the header timeout ran out
first. the timeouts are deadlines on the socket, so a server that sends one
byte at a time still runs out of time.

the runtime's connect has no deadline of its own. with a dial timeout, the
connect runs in a separate task and the client stops waiting for it when the
timeout passes. if the connect completes later, that task closes the
connection.

### connection reuse

a connection goes back to the pool once its response body has been read,
unless either side asked to close it or the body ran to the end of the
connection. before an idle connection is reused, the client checks that it
has not idled past `idle_conn_timeout_ms` and that nothing is waiting to be
read on it. if bytes are waiting, the server has closed the connection, and
the client dials a new one.

the server can still close a connection just as a request goes out on it. if
that happens before any of the response arrives, the request is sent once
more on a new connection when it is safe to replay: `GET`, `HEAD`, `OPTIONS`
and `TRACE`, or any request that carries an `Idempotency-Key` or
`X-Idempotency-Key` header, as in net/http.

`close_idle_connections()` closes every idle connection, and
`idle_connections()` counts them.

### responses

the client checks the response head as strictly as the server checks a
request: a malformed status line, a malformed header field, conflicting
`Content-Length` values, `Transfer-Encoding` together with `Content-Length`,
and transfer codings other than `chunked` all fail the request. the body is
framed as rfc 9112 6.3 describes. the answers to `HEAD` and the `1xx`, `204`
and `304` responses have no body. otherwise the body is chunked, has a
`Content-Length`, or runs until the server closes the connection. interim
`1xx` responses before the final one are skipped. trailer fields of a chunked
body are returned in the response's `trailers`.

when compression is on, a request that names no `Accept-Encoding` asks for
gzip, and a gzip body is decoded before it is returned. the decoded response
has no `Content-Encoding` or `Content-Length`. streamed bodies (`send_into`,
`send_to_file`), `HEAD` requests and requests with a `Range` header are not
compressed.

### redirects

the client follows `301`, `302`, `303`, `307` and `308` as net/http does.
`307` and `308` resend the method and the body. `301`, `302` and `303` turn
any method other than `GET` and `HEAD` into `GET` and drop the body. a
relative `Location` is resolved against the current url. a redirect to
another scheme, host or port drops `Authorization` and `Cookie`. net/http
keeps them when only the port changes, or when the new host is a subdomain
of the old one. a redirect without a `Location` is returned as the response.
past `max_redirects`, the request fails with
`http: stopped after 10 redirects`.

### request bodies

a body goes out with a `Content-Length`. a `POST`, `PUT` or `PATCH` with an
empty body sends `Content-Length: 0`; other methods with no body send no
length. naming `Transfer-Encoding: chunked` on the request sends the body
chunked instead.

`req.expect_continue()` adds `Expect: 100-continue`. the client then sends
the head, waits up to `expect_continue_timeout_ms` for `100 Continue`, and
then sends the body. if the server answers with a final status first, that
answer is returned, the body is never sent, and the connection is closed.

### proxies

`with_proxy("http://proxy:3128")` sends every request through that proxy.
a plain http request goes to the proxy with the full url as its target. an
https request opens a `CONNECT` tunnel through the proxy, and tls to the
origin runs inside the tunnel. only plain http proxies are supported, and
the client does not send `Proxy-Authorization`.

`with_env_proxy(true)` reads `HTTP_PROXY` for http urls and `HTTPS_PROXY`
for https urls (or their lowercase forms) on each request, with net/http's
rules for `NO_PROXY`:

- `localhost` and loopback addresses always go direct.
- `*` sends everything direct.
- `example.com` matches that host and its subdomains, and `.example.com`
  matches only its subdomains.
- an entry with a port, such as `example.com:8080`, matches only that port.

cidr ranges in `NO_PROXY` are not supported.

### not covered

compared with net/http, the client has no cookie jar, no http/2, no
`CheckRedirect` hook, no limit on connections per host beyond the idle limit,
no `Proxy-Authorization`, no socks or https proxies, and no streaming of a
gzip body. a response body is read in full before `send` returns, or written
in full to the writer for `send_into`, so the client has no body stream that
the caller closes.
