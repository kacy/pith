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
- `req.header(name)` — a request header, or `""`
- `req.body()` — the body as text
- `req.method()` and `req.path()`

when you need more, like cookies, multipart parts, or the raw bytes, reach through
`req.raw`, which is the underlying `std.net.http` request.

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
replies. `bench/web_hello.pith` is the same idea as a real blocking server, wired up
for `bench/http_bench.sh`.
