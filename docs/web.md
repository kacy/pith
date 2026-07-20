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
