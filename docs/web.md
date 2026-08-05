# web

`std.web` is a small, opinionated layer over `std.net.http`. it owns two things:
routing and the accept loop. the request and response types still come from
`std.net.http`, so there is one set of builders to learn, not two.

## a server in ten lines

```pith
import std.web as web
import std.net.http as http

fn main() -> Int!:
    app := web.new().get("/", fn(req: web.Request) => http.text(200, "hello"))
    app.listen("0.0.0.0", 8080)!
    return 0
```

`web.new()` gives you an app that already answers `GET /healthz` with `200 ok`, so a
load balancer has something to poll before you have written a single route. `listen`
binds the socket and serves forever.

## routes

add routes with `get`, `post`, `put`, and `delete`. each one returns a new app, so
calls chain:

```pith
app := web.new().get("/", home).post("/users", create)
```

a chain has to stay on one line. once you have more than a couple of routes, add them
one per line instead:

```pith
mut app := web.new()
app = app.get("/", home)
app = app.get("/users/:id", show)
app = app.post("/users", create)
```

routes match in the order you add them and the first match wins, so put your most
specific paths first.

## path parameters

a segment that starts with `:` is a parameter. the router pulls it out of the request
path and hands it to the handler by name:

```pith
fn show(req: web.Request) -> http.HttpResponse:
    return http.text(200, "user " + req.param("id"))

app := web.new().get("/users/:id", show)
```

`GET /users/42` calls `show` with `req.param("id")` equal to `"42"`. a trailing slash
is ignored, so `/users/42` and `/users/42/` hit the same route.

## the request

a handler receives a `web.Request`. the common accessors are right there:

- `req.param(name)` — a path parameter, or `""`
- `req.query(name)` — a query-string value, or `""`
- `req.query_or(name, fallback)` — a query-string value, or `fallback` when it is
  missing or empty
- `req.header(name)` — a request header, or `""`
- `req.body()` — the body as text
- `req.is_json()` — true when the `Content-Type` is `application/json`
- `req.method()` and `req.path()`

when you need more, like cookies, multipart parts, or the raw bytes, reach through
`req.raw`, which is the underlying `std.net.http` request.

## request bodies & json

most json apis read a struct out of the request body. `web.parse[T]` does exactly
that: give it the request and the struct you expect, and it hands back a typed value
or a decode error. the struct's fields have to be `pub` so the decoder can fill them
in.

```pith
struct NewUser:
    pub name: String
    pub age: Int

fn create_user(req: web.Request) -> http.HttpResponse:
    if not req.is_json():
        return http.bad_request_response()
    parsed := web.parse[NewUser](req)
    if parsed.is_err:
        return http.bad_request_response()
    user := parsed.ok
    return http.json(201, user_json(user))
```

a handler returns a response rather than propagating an error, so check
`parsed.is_err` and answer with a 400 instead of using `!`. `req.is_json()` turns
away anything that is not `application/json` before you even try to decode, and a
malformed body still lands in the `is_err` branch, so both cases come back as a clean
400.

under the hood `web.parse[T]` is just `json.decode_text[T](req.body())`. reach for
`json.decode_text` directly when you already have the body text and not a request.

going the other way, build the response body with the `std.json` constructors and
send it with `http.json`:

```pith
fn user_json(user: NewUser) -> String:
    obj := json.make_object()
    json.object_set(obj, "name", json.make_string(user.name))
    json.object_set(obj, "age", json.make_int(user.age))
    return json.encode(obj)
```

`examples/web_json_api.pith` is a complete, self-checking version of this: a
`GET /users/:id` that echoes the path parameter as json and a `POST /users` that
decodes a `NewUser` from the body and echoes it back.

## the json node pool

`json.parse` does not return a tree of objects. it returns an `Int` handle into
a node pool that `std.json` keeps in per-task storage, and nothing in that pool
is freed when the handle goes out of scope. on a keep-alive connection — one
task, many requests, a body parsed per request — that adds up.

servers do not have to think about it. `http.serve_connection`, the other
`http.serve_*` entry points, and the http/2 stream handler all run your handler
inside a pool scope and reclaim it when the handler returns, so each request
starts from an empty pool. the same is true of `web.parse[T]` and every other
typed decode: the compiler brackets the lowered body and gives the nodes back
once the struct is built.

what is left is a long-lived task that calls `json.parse` on its own, outside a
request. bracket it yourself:

```pith
scope := json.open_scope()
defer json.close_scope(scope)
root := json.parse(text)
name := json.object_get_string(root, "name")
```

a scope reclaims everything parsed since it opened, so scopes nest but they have
to close innermost-first — closing an outer scope also closes anything opened
inside it. copy the values you want out before the scope closes; strings and
numbers pulled out of the tree are independent, but a handle is not. reading a
handle after its scope closed reports `invalid` rather than another document's
value, because node ids are never reused.

`std.toml` has the same pool and the same `open_scope`/`close_scope`, for
configuration that gets reloaded rather than read once at startup.

## responses

`std.web` does not invent its own response type. you return an `http.HttpResponse`,
built with the same helpers you would use anywhere:

```pith
http.text(200, "ok")
http.json(200, body)
http.html(200, "<h1>hi</h1>")
http.not_found_response()
```

a request that matches no route gets `http.not_found_response()` automatically.

## middleware

middleware wraps every matched route. a middleware is a function that takes `next`
— the rest of the chain — and the request, and returns a response:

```pith
fn logging(next: fn(web.Request) -> http.HttpResponse, req: web.Request) -> http.HttpResponse:
    print(req.method() + " " + req.path())
    return next(req)
```

register it with `use_mw`, which returns a new app just like the route builders:

```pith
app := web.new().use_mw(logging).get("/", home)
```

because a middleware receives `next`, it decides what happens around it. run code
before calling `next` to inspect or rewrite the request; run code after to touch the
response; or skip `next` entirely to answer on your own — an auth check that rejects a
request never has to reach the handler.

order is the thing to keep straight. the first middleware you register runs
outermost, so it sees the request first and the response last:

```pith
app := web.new().use_mw(logging).use_mw(auth).get("/", home)
```

here `logging` wraps `auth` wraps `home`. a request goes `logging` → `auth` → `home`,
and the response comes back the other way. middleware wraps every route, including the
default `GET /healthz`.

write middleware as a named function, as above. `examples/web_middleware.pith` is a
complete, self-checking example: two middleware bracket the response body with markers
so the nesting is visible in the output.

two middleware come built: `web.rate_limit(limiter)` answers 429 when a shared
token bucket runs dry, and `web.circuit(breaker)` answers 503 while a circuit
breaker is open, counting the handler's 5xx responses as failures. both take
their state from `std.resilience` — see [docs/resilience.md](resilience.md).

## route groups

`use_mw` wraps every route, but a guard usually belongs to some routes and
not others: a service splits into a public handful — login, health, metrics —
and a protected rest. `group` binds a middleware to a path prefix, so the
split is spelled at one site instead of as an allowlist inside the guard:

```pith
app := web.new()
    .post("/login", handle_login)
    .group("/api", require_token)
    .get("/api/me", handle_me)
```

`/login` and `/healthz` stay open; everything under `/api` runs through
`require_token`, inside whatever app-wide middleware is registered. the
prefix covers itself and anything below it — `/api` scopes `/api` and
`/api/me`, not `/apiary`.

a guard usually learns something the handler wants — who the caller is,
which tenant, what role. `with_value` attaches it to the request that flows
down the chain, and `value` reads it back:

```pith
fn require_token(next: fn(web.Request) -> http.HttpResponse, req: web.Request) -> http.HttpResponse:
    session := jwt.verify_hs256(req.raw.bearer_token() catch "", secret, expected) catch:
        return http.unauthorized_response()
    return next(req.with_value("user", session.subject()))

fn handle_me(req: web.Request) -> http.HttpResponse:
    return http.response(200).json_body(json.of("user", req.value("user")))
```

`examples/web_auth.pith` runs this exact shape end to end — login, group
guard, and the requests a bad token gets back.

## sessions

`std.web.session` keeps the state a request carries about who is on the other
end of it. the browser holds an id and nothing else; everything real lives on
the server, so revoking a session is a delete rather than a hope that a token
expires soon.

```pith
import std.web.session as session

fn main() -> Int!:
    store := session.store(bytes.from_string_utf8(env("SESSION_SECRET")))!
    app := web.new().use_mw(session.middleware(store)).post("/login", log_in)
    app.listen("0.0.0.0", 8080)!
    return 0
```

a handler only ever takes the request, so the store it reads and writes is
closed over — build it once in `main` and capture it in the handlers, the way
`examples/web_session.pith` does.

```pith
store.set(req, "user", "u-7")
store.get(req, "user")                  # "" when there is no session
store.get_or(req, "theme", "dark")
store.has(req, "user")
store.remove(req, "theme")              # one key; the session stays
store.keys(req)
store.active(req)
store.rotate(req)                       # a new id, same contents
store.destroy(req)                      # the session ends here
```

`store.count()` and `store.sweep()` are the housekeeping: how many sessions
are live, and dropping the expired ones now instead of waiting for traffic to
do it.

### the cookie

the cookie carries the id and a mac over it, and nothing else. a session
cookie that carries data is a session cookie whose data the browser can be
talked into replaying. the id is 32 bytes from the os random source
(`std.crypto.random`), base64url encoded; the mac is hmac-sha256 under the
store's secret and is compared with `std.crypto.subtle`. the mac buys nothing
against guessing — 256 bits already handles that — but a forged or truncated
cookie is thrown out on arithmetic before it becomes a store lookup, so a
flood of made-up cookies cannot be used to probe the store.

it is `HttpOnly`, `Secure` and `SameSite=Lax`, at `Path=/`, with a `Max-Age`
of the store's ttl. the builder moves each of those:

- `.ttl(seconds)` — the idle timeout, one day by default. a request that
  arrives with a live session slides the window forward
- `.cookie_name(name)` — defaults to `pith_session`
- `.path(value)`, `.same_site(value)`
- `.insecure_over_http()` — drops the `Secure` flag, for local development,
  and is the only way to drop it

the secret must be at least 32 bytes and belongs in the environment, not the
source. `store()` refuses anything shorter. changing it invalidates every
cookie already issued, which is the blunt way to sign everybody out.

### rotate, and why a login needs it

call `store.rotate(req)` at the top of a successful login, before writing who
the caller is. without it, an attacker plants a session id in a victim's
browser — a crafted link, an injected cookie — waits for them to sign in, and
finds the id they already hold has become an authenticated one. rotating
retires that id at the exact moment it would become worth having, and carries
the session's contents across to a fresh one.

### what one process holds

the store is this process's memory behind one mutex, shared by every serving
task — green or os thread, one store, as many tasks as you like. what it is
not is shared between processes. two instances behind a load balancer each
hold their own sessions, so a request that lands on the other one finds a
signature it accepts and an id it has never heard of, and treats the caller as
signed out. run one instance, pin sessions to it with a sticky load balancer,
or put the state somewhere both can reach.

a visitor who never signs in never occupies a row: an id is minted per request
but only written to the store when a handler actually stores something, and a
request that reads nothing and writes nothing gets no `Set-Cookie` back. the
store holds at most 100,000 sessions and refuses to grow past that rather than
running the process out of memory; expired sessions are swept in the
background of ordinary traffic.

## cors

a browser will not let page javascript on one origin read a response from
another unless the server says it may. `std.web.cors` is the middleware that
says so, and the preflight handler that answers the `OPTIONS` request the
browser sends first.

```pith
import std.web.cors as cors

policy := cors.origins(["https://app.example.com"]).max_age(600)
app := web.new().use_mw(cors.middleware(policy)).get("/api/items", list_items)
```

there are three constructors, and which one you pick is the whole security
decision:

- `cors.any_origin()` — answers `Access-Control-Allow-Origin: *`. the policy
  for a public, read-only api. no credentials.
- `cors.origins(list)` — an explicit allowlist, compared whole: scheme, host
  and port. `https://app.example.com` does not match a subdomain of itself,
  a different port, or the same host over http. no credentials.
- `cors.credentialed_origins(list)` — the same allowlist, and the browser may
  attach the user's cookies. every origin on the list can act as the signed-in
  user, so keep it short.

the classic cors hole is to reflect whatever `Origin` arrived back in
`Access-Control-Allow-Origin` and send `Access-Control-Allow-Credentials:
true` next to it, which tells every site on the internet that it may read this
one's responses with the user's session attached. that cannot be written here:
credentials come only from `credentialed_origins`, which has no wildcard, and
`any_origin` has no credential switch. they are separate constructors rather
than two builder methods so there is no way to combine them by accident. an
entry of `"*"` in a credentialed list is compared literally, so it matches an
origin spelled exactly `*`, which no browser sends — it allows nothing.

the builder narrows the rest:

- `.methods(names)` — what a preflight may ask for. defaults to `GET`, `HEAD`,
  `POST`, `PUT`, `PATCH`, `DELETE`, `OPTIONS`
- `.headers(names)` — which request headers a preflight may ask for. defaults
  to `Content-Type` and `Authorization`
- `.expose(names)` — which response headers page javascript is allowed to read
- `.max_age(seconds)` — how long the browser may cache the preflight answer

register it outermost, before any guard that can answer 401, so a rejected
request still carries the headers that let the browser read the rejection —
otherwise a plain 401 shows up in the console as a cors error and sends
whoever is debugging it looking in the wrong place.

what happens to a request from an origin that is not allowed depends on which
one it is. a preflight is refused with 403: it exists only to ask permission,
and the answer is no. a real request still runs — the server has no standing
to refuse it, and the same request without an `Origin` header would be served
— but the response carries no `Access-Control-Allow-Origin`, so the browser
withholds the body from the page. that is the same-origin policy working as
designed.

`examples/web_cors.pith` runs the whole thing: a real request from an allowed
origin and from somewhere else, the preflight for each, and the headers that
come back.

### OPTIONS

`use_mw` middleware only wraps a route the router matched, and a preflight
arrives as `OPTIONS` on a path registered for `GET` or `POST` — so before cors
existed those requests fell straight through to a 404 with no middleware in
sight. `std.web` now answers an `OPTIONS` request to any path some route
claims with `204` and an `Allow` header listing that path's methods, and runs
it through the middleware chain like any other request. cors middleware
intercepts the preflight ones; the rest get the honest `Allow` answer. an
`OPTIONS` to a path no route claims is still a 404.

## observability

a server built with `web.new()` is observable out of the box. every route is wrapped
in a built-in middleware that records metrics and opens a trace span, and `web.new()`
serves a `/metrics` endpoint — none of which you write. this built-in middleware sits
outside any middleware you add, so it measures the whole request.

### metrics

for every request the middleware records three series, labeled by method and route
pattern. the label is the pattern (`/users/:id`), not the concrete path, so a busy
route stays one series instead of thousands:

- `http_server_requests_total{method,route,status}` — a request counter
- `http_server_requests_in_flight{method,route}` — a gauge of in-flight requests
- `http_server_duration_ms{method,route}` — a request-duration histogram

these live in memory and are always on: they need no setup and cost a map update per
request. `web.new()` also registers `GET /metrics`, which renders every `std.metrics`
series as prometheus text, so a scraper reads them straight off your server:

```pith
app := web.new().get("/users/:id", show)
# GET /metrics now returns the prometheus exposition, including
# http_server_requests_total{method="GET",route="/users/:id",status="200"} 12
```

`examples/web_observability.pith` runs the whole loop: it makes a few requests to a
route, then reads the request count back out of `/metrics` — with no instrumentation
code in the handler.

### traces

the same middleware opens a server span per request, tagged with the method, path, and
status. it reads an inbound `traceparent` header, so a request that arrives already in
a trace joins it. until you turn tracing on the span is a cheap no-op: it takes no ids
and records nothing, so an app without an exporter pays nothing for it.

to export spans, initialize `std.obs` before you start serving. `obs.init()` reads the
standard `OTEL_*` environment variables and, when `OTEL_EXPORTER_OTLP_ENDPOINT` is set,
turns tracing on and starts the OTLP exporter:

```pith
import std.obs as obs

fn main() -> Int!:
    obs.init()
    web.new().get("/", home).listen("0.0.0.0", 8080)!
    return 0
```

spans then flow to your collector; `docs/telemetry.md` covers the exporter and the
sampler in full.

### access logs

access logging is opt-in rather than on by default. metrics and traces carry the
observable-by-default story quietly, and a log line per request would clutter an
example or a benchmark, so you add it when you want it:

```pith
app := web.new().use_mw(web.access_log).get("/", home)
```

`web.access_log` writes one structured line per request to `std.log` (which goes to
stderr) with the method, route, path, status, and elapsed time.

### opting out

observability adds a little work to every request, so for a benchmark or a bare-bones
server, build the app with `web.bare()` instead of `web.new()`. a bare app has only the
default `/healthz` route: no observability middleware and no `/metrics`.

## a task per connection

`listen` accepts connections in a loop and hands each one to its own task with
`spawn`, so a slow client only ever holds up itself. this works the same whether you
run on os threads or on the green runtime:

```
./your_server              # green on linux, os threads elsewhere
PITH_GREEN=0 ./your_server # one os thread per connection, anywhere
```

on the green runtime those per-connection tasks are green threads, so a server can
carry many more connections than it has os threads.

whatever goes wrong on one connection stays on its task. a client that fails a
tls handshake, hangs up mid-request, or sends something unparseable costs itself
its own connection and nothing else. the loop itself only gives up when the
listener stops producing connections at all: it backs off after a failed accept
rather than spinning, and stops after a bounded run of failures with nothing
accepted in between, which is as much as it can tell from the outside. that
matters because a listener that has quietly stopped accepting still answers a
tcp health check — the socket is open, the kernel is filling the backlog, and
nobody is serving.

## http/2

the same app serves http/2 with a different call. `listen_h2c` speaks cleartext
http/2 (h2c), and `listen_tls` speaks http/2 over tls with an http/1.1 fallback.
either way the request runs through the same routing, middleware, and
observability as `listen`. only the transport changes, so nothing in your app
has to know which one it is.

```
# cleartext http/2, no certificate needed
app.listen_h2c("0.0.0.0", 8080)!
```

`listen_tls` offers alpn `["h2", "http/1.1"]` and branches per connection. a
client that negotiates `h2` is served over http/2; anything else, including a
client that sends no alpn, is served as http/1.1 over the same tls session. one
listener handles a modern http/2 client and a plain https client alike.

```
# http/2 over tls, falling back to http/1.1 for clients that ask for it
app.listen_tls("0.0.0.0", 8443, "cert.pem", "key.pem")!
```

`cert` and `key` are paths to pem files: the server certificate and its private
key. h2c needs neither, which makes it the easy choice behind a load balancer
that terminates tls. reach for `listen_tls` when the pith server terminates tls
itself.

## graceful shutdown

`listen` and `listen_tls` serve forever until a shutdown is requested. one call
wires that to `SIGTERM`:

```pith
import std.shutdown as shutdown

fn main() -> Int!:
    shutdown.on_signals()!
    web.new().get("/", home).listen("0.0.0.0", 8080)!
    return 0
```

on `SIGTERM` (or `SIGINT`/`SIGHUP`) the listener stops accepting and releases its
port, the in-flight requests are given a grace period to finish, and `listen`
returns the work that was still unfinished when that period expired — `0` for a
clean drain. a rolling deploy stops severing responses mid-flight. without the
`on_signals()` call nothing changes and `listen` blocks forever, as before.

[docs/signals.md](signals.md) covers the drain, the coordinator, and what
becomes of a stream that would otherwise never end.

## a runnable example

`examples/web_hello.pith` is a complete, self-checking server: it defines a couple of
routes, spawns the server, makes a few requests against itself, and prints the
replies. `examples/web_cors.pith` puts a cors policy in front of an api and drives it
from two origins, one allowed and one not. `examples/web_session.pith` runs the whole
life of one session: an anonymous visit, a sign-in that rotates the id, a forged
cookie, and a sign-out. `examples/web_observability.pith` does the
same and then scrapes `/metrics` to show the request counter the framework kept on its
own. `examples/web_h2.pith` serves
the same kind of app over http/2 (h2c) and drives it with a small built-in h2c client.
`bench/web_hello.pith` is the same idea as a real blocking server, wired up for
`bench/http_bench.sh`. `examples/graceful_shutdown.pith` serves, signals itself,
and prints the drain outcome.

## rendering html

`http.html(200, body)` will send whatever body you hand it, so what goes into
that body is the whole of your xss story. anything the caller controls — a
query parameter, a path parameter, a header, a stored value that arrived from a
request once — goes through `std.html` on the way in:

```pith
import std.html as html

fn greet(req: web.Request) -> http.HttpResponse:
    return http.html(200, "<h1>hello, " + html.escape(req.param("name")) + "</h1>")
```

[docs/html.md](html.md) is the full account: the five characters, why there is
no separate attribute escaper, why a url in an `href` needs `html.escape_url`
rather than `html.escape`, and the contexts — inside `<script>`, inside
`<style>`, an unquoted attribute — where escaping is not the fix.
