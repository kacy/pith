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

most json apis read a struct out of the request body. `json.decode_text[T]` does
exactly that: give it the body text and the struct you expect, and it hands back a
typed value or a decode error. the struct's fields have to be `pub` so the decoder
can fill them in.

```pith
import std.json as json

struct NewUser:
    pub name: String
    pub age: Int

fn create_user(req: web.Request) -> http.HttpResponse:
    if not req.is_json():
        return http.bad_request_response()
    parsed := json.decode_text[NewUser](req.body())
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
PITH_GREEN=1 ./your_server
```

on the green runtime those per-connection tasks are green threads, so a server can
carry many more connections than it has os threads.

## a runnable example

`examples/web_hello.pith` is a complete, self-checking server: it defines a couple of
routes, spawns the server, makes a few requests against itself, and prints the
replies. `examples/web_observability.pith` does the same and then scrapes `/metrics` to
show the request counter the framework kept on its own. `bench/web_hello.pith` is the
same idea as a real blocking server, wired up for `bench/http_bench.sh`.
