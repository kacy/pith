# Web

`std.web` is a small, opinionated layer over `std.net.http`. It owns two things:
routing and the accept loop. The request and response types still come from
`std.net.http`, so there is only one set of builders to learn.

## A server in ten lines

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

## Routes

Add routes with `get`, `post`, `put`, `patch`, `delete`, and `head`, or with
`handle(method, pattern, handler)` for any other method. Each one returns a new
app, so calls chain:

```pith
app := web.new().get("/", home).post("/users", create)
```

A line that starts with `.` does not continue the line before it, so a chain stays
on one line unless it is wrapped in parentheses. Once you have more than a couple of
routes, it is often clearer to add them one per line:

```pith
mut app := web.new()
app = app.get("/", home)
app = app.get("/users/:id", show)
app = app.post("/users", create)
```

Routes match in the order you add them and the first match wins, so put your most
specific paths first.

## Path parameters

A segment that starts with `:` is a parameter. The router pulls it out of the request
path and hands it to the handler by name:

```pith
fn show(req: web.Request) -> http.HttpResponse:
    return http.text(200, "user " + req.param("id"))

app := web.new().get("/users/:id", show)
```

`GET /users/42` calls `show` with `req.param("id")` equal to `"42"`. A trailing slash
is ignored, so `/users/42` and `/users/42/` hit the same route. In net/http, `/dir`
redirects to `/dir/` only when a subtree pattern is registered. This router
has no subtree patterns, so both spellings reach the route and neither is
redirected.

A last segment that starts with `*` matches the rest of the path, including
nothing at all. `/files/*path` matches `/files`, `/files/a.txt` and
`/files/docs/a.txt`, and `req.param("path")` is `""`, `"a.txt"` and
`"docs/a.txt"`.

### Percent-encoding

The router splits the path on its literal `/` characters first and decodes
each segment after that. So:

- A literal pattern segment matches the decoded segment: `/café` matches a
  request for `/caf%C3%A9`, and `/api` matches `/%61pi`.
- An encoded slash stays inside its segment. `/files/:name` matches
  `/files/a%2Fb` with `name` equal to `"a/b"`, and `/files/:dir/:name` does
  not match it.
- A parameter is decoded the way a path is decoded, so `%20` is a space and
  `+` stays a `+`. Only a query string or a form body uses `+` for a space.
- A `*` parameter is the remaining segments, each decoded and joined with
  `/`, so an encoded slash in it looks like any other slash. A handler that
  uses one as a file name has to check it, as `http.serve_dir` does.

Group prefixes are compared the same way, so `/%61pi/me` is inside
`group("/api", guard)`.

### Canonical paths

A request path that is not in canonical form gets a `301` to the one that is,
with the query string kept. This is what net/http's `ServeMux` does, and it
happens before any route or middleware sees the request:

- Repeated slashes collapse: `/a//b` goes to `/a/b`
- `.` segments are dropped: `/a/./b` goes to `/a/b`
- `..` segments remove the segment before them: `/a/b/../c` goes to `/a/c`.
  A `..` at the root stays at the root, so `/../etc/passwd` goes to
  `/etc/passwd` and a path never climbs above `/`
- An encoded dot counts as a dot, so `/a/%2e%2e/b` goes to `/b`
- A trailing slash is kept

`http.clean_path(path)` computes the canonical form and
`http.path_is_clean(path)` checks for it. The check reads the path in place
and allocates nothing, so a request whose path is already clean costs one
scan. Because repeated slashes collapse, a `Location` never starts with `//`,
which a browser would read as a different host.

## The request

A handler receives a `web.Request`. The common accessors are:

- `req.param(name)`: a path parameter, or `""`
- `req.query(name)`: a query-string value, or `""`
- `req.query_or(name, fallback)`: a query-string value, or `fallback` when it is
  missing or empty
- `req.header(name)`: a request header, or `""`
- `req.body()`: the body as text
- `req.is_json()`: true when the `Content-Type` is `application/json`
- `req.method()` and `req.path()`
- `req.peer_address()` and `req.peer_ip()`: the client's address, as `ip:port` or
  just the IP. This is the peer of the connection, so behind a proxy it is the
  proxy, and it is `""` in a task the handler spawned

When you need more, like cookies, multipart parts, or the raw bytes, reach through
`req.raw`, which is the underlying `std.net.http` request.

## Request bodies & JSON

Most JSON APIs read a struct out of the request body. `web.parse[T]` does that:
give it the request and the struct you expect, and it hands back a typed value
or a decode error. The struct's fields have to be `pub` so the decoder can fill them
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

A handler returns a response instead of propagating an error, so check
`parsed.is_err` and answer with a 400 instead of using `!`. `req.is_json()` turns
away anything that is not `application/json` before you try to decode, and a
malformed body still lands in the `is_err` branch, so both cases come back as a clean
400. The error's message says which it was: `invalid json at byte 0` for a body
that is not JSON, with the byte the parser stopped at, and `missing json field: age`
for an object that leaves a field out.

Under the hood, `web.parse[T]` is just `json.decode_text[T](req.body())`. Use
`json.decode_text` directly when you already have the body text and not a request.

A target's fields may be `Int`, `String`, `Bool`, `Float`, an optional of those
(`Float?` reads as `none` when the key is absent), or another struct with the
same kinds of fields, which is filled in place. A `Float` field takes any JSON
number: `1.5`, `-2.5e-3`, and an integer such as `3`, which widens to `3.0`. An
`Int` field takes only an integer, so `1.5` under one is a decode error. `NaN`
and `Infinity` are not JSON and are refused, as is an exponent past the range
of a 64-bit float.

```pith
struct Reading:
    pub sensor: String
    pub value: Float
    pub delta: Float?

fn record(req: web.Request) -> http.HttpResponse:
    parsed := web.parse[Reading](req)
    if parsed.is_err:
        return http.bad_request_response()
    r := parsed.ok
    return http.text(200, "{r.sensor}: {r.value}")
```

A field may also be a `List` of any of those scalars or of another struct, so
an array in the document decodes in the same call. The filler builds the list
as it reads the array, and builds each element struct with it. A wrong element
is reported by its position (`expected int element: sizes[2]`,
`items[1]: missing string field: sku`):

```pith
struct Item:
    pub sku: String
    pub qty: Int

struct Order:
    pub id: Int
    pub tags: List[String]
    pub items: List[Item]

fn place(req: web.Request) -> http.HttpResponse:
    parsed := web.parse[Order](req)
    if parsed.is_err:
        return http.bad_request_response()
    order := parsed.ok
    return http.text(200, "{order.id}: {order.items.len()} items")
```

A field may also be a `Map[String, _]`, for an object whose keys are data
rather than field names. Lists and maps nest: the value of a map or the
element of a list may be a scalar, a struct, another list or another map. A
map's members are inserted as they are read, and a repeated key keeps its last
value. A wrong value is reported by its path (`expected int element:
scores.bob`, `variants.xl: missing int field: qty`, `expected int element:
grid[1][3]`):

```pith
struct Variant:
    pub qty: Int
    pub price: Float

struct Product:
    pub sku: String
    pub scores: Map[String, Int]
    pub variants: Map[String, Variant]
    pub grid: List[List[Int]]
    pub labels: Map[String, List[String]]

fn stock(req: web.Request) -> http.HttpResponse:
    parsed := web.parse[Product](req)
    if parsed.is_err:
        return http.bad_request_response()
    p := parsed.ok
    mut total := 0
    for size in p.variants.keys():
        total = total + p.variants[size].qty
    return http.text(200, "{p.sku}: {total} in stock")
```

The checker refuses a `Map` whose key is not a `String`, and a `List` or a `Map`
whose element is an optional (`docs/limitations.md`). Decode such a
document through `json.parse` and the node accessors instead. A document
whose arrays and objects nest 128 levels deep or more is refused, as
`json.parse` refuses it; 127 levels is the deepest that decodes.

Going the other way, build the response body with the `std.json` constructors and
send it with `http.json`:

```pith
fn user_json(user: NewUser) -> String:
    obj := json.make_object()
    json.object_set(obj, "name", json.make_string(user.name))
    json.object_set(obj, "age", json.make_int(user.age))
    return json.encode(obj)
```

`examples/web_json_api.pith` is a complete, self-checking version of this: a
small book catalog with routes to list, read, create and delete. `POST /books`
decodes a `NewBook` from the body and answers 400, 415 or 422 when the body is
not JSON, not sent as JSON, or not a valid book.

## The JSON node pool

`json.parse` does not return a tree of objects. It returns an `Int` handle into
a node pool that `std.json` keeps in per-task storage, and nothing in that pool
is freed when the handle goes out of scope. On a keep-alive connection (one
task, many requests, a body parsed per request), that adds up.

Servers do not have to manage this. `http.serve_connection`, the other
`http.serve_*` entry points, and the HTTP/2 stream handler all run your handler
inside a pool scope and reclaim it when the handler returns, so each request
starts from an empty pool. The same is true of `web.parse[T]` and every other
typed decode: the compiler brackets the lowered body and gives the nodes back
once the struct is built.

That leaves a long-lived task that calls `json.parse` on its own, outside a
request. Bracket it yourself:

```pith
scope := json.open_scope()
defer json.close_scope(scope)
root := json.parse(text)
name := json.object_get_string(root, "name")
```

A scope reclaims everything parsed since it opened, so scopes nest, but they have
to close innermost-first: closing an outer scope also closes anything opened
inside it. Copy the values you want out before the scope closes. Strings and
numbers pulled out of the tree are independent, but a handle is not. Reading a
handle after its scope closed reports `invalid` instead of another document's
value, because node IDs are never reused.

`std.toml` has the same pool and the same `open_scope`/`close_scope`, for
configuration that gets reloaded instead of read once at startup.

## Responses

`std.web` has no response type of its own. You return an `http.HttpResponse`,
built with the same helpers you would use anywhere:

```pith
http.text(200, "ok")
http.json(200, body)
http.html(200, "<h1>hi</h1>")
http.not_found_response()
```

A request that matches no route gets `http.not_found_response()` automatically.
A request for a path some route serves, under a method none of those routes
accepts, gets `405 Method Not Allowed` with an `Allow` header that lists the
methods the path does accept. Neither answer runs middleware.

### HEAD

A `GET` route answers `HEAD` as well, as net/http's `ServeMux` does. The
router runs the `GET` handler and the server sends the headers without the
body, including the `Content-Length` the body would have had. A `HEAD` route
registered for the same path wins over the `GET` one, for a handler that can
answer without building the body. `Allow` lists `HEAD` wherever it lists
`GET`.

### Streaming a response

A handler can send its body as it is produced, which suits server-sent events and
large downloads. Return `http.stream_response` with the head and a function that
writes the pieces to the `io.BytesWriter` it is given:

```pith
fn ticks(out: io.BytesWriter) -> Int!:
    for i in 0..3:
        out.write_bytes(bytes.from_string_utf8("tick {i}\n"))!
    return 0

fn feed(req: web.Request) -> http.HttpResponse:
    return http.stream_response(http.response(200).content_type("text/plain"), ticks)
```

The head passes through the middleware like any response, and the function runs
after it returns, on the same task. HTTP/1.1 sends chunks unless the head has a
`Content-Length`, and HTTP/2 sends DATA frames. A function that fails, or a client
that went away, ends the response early, and the client can tell it was cut short.

The client side of this is `client.open(request)`, which returns once the head
arrives. `read()` hands over the body as it comes and is empty at the end, and
`close()` returns the connection to the pool:

```pith
stream := client.open(http.request("GET", "localhost", 8080, "/feed"))!
defer stream.close()
while true:
    piece := stream.read()!
    if piece.is_empty():
        break
    use(piece)
```

### WebSockets

A route upgrades to a WebSocket session by returning
`websocket.take_connection` with the request and the function that runs the
session:

```pith
import std.net.websocket as websocket

fn echo(ws: websocket.WebSocket) -> Int!:
    while true:
        text := ws.recv_text() catch:
            break
        ws.send_text("echo: {text}")!
    return 0

fn upgrade(req: web.Request) -> http.HttpResponse:
    return websocket.take_connection(req.raw, echo)

app := web.new().get("/ws", upgrade)
```

Once the handler returns, the server writes the `101` handshake and calls the
session function on the connection's task, over the reader and writer
`http.take_connection` hands over (see docs/http_apps.md). The connection
closes when the function returns. A request that is not a WebSocket upgrade
gets a `400` instead. `websocket.accept_io(req, reader, writer)` is the same
upgrade for code that already holds an `io.BytesReader` and an
`io.BytesWriter`.

A session can be shared between tasks. Any task may call `send`,
`send_text`, `ping` or `close` on it, and each message goes out under a lock
the session holds, so two tasks sending at once never mix their frames.
Receiving stays with one task at a time: a message can span several frames,
and two tasks reading would split them. A chat room keeps its members'
sessions in a `guard` block and sends each line on all of them from the task
that read it, as `examples/websocket_chat.pith` does. A session made with
`websocket.from_buffered` writes through a buffered writer, which belongs to
the task that made it, so use that session on that task only.

## Middleware

Middleware wraps every matched route. A middleware is a function that takes `next`
(the rest of the chain) and the request, and returns a response:

```pith
fn logging(next: fn(web.Request) -> http.HttpResponse, req: web.Request) -> http.HttpResponse:
    print(req.method() + " " + req.path())
    return next(req)
```

Register it with `use_mw`, which returns a new app just like the route builders:

```pith
app := web.new().use_mw(logging).get("/", home)
```

Because a middleware receives `next`, it decides what happens around it. Run code
before calling `next` to inspect or rewrite the request, run code after to touch the
response, or skip `next` entirely to answer on your own. An auth check that rejects a
request never has to reach the handler.

Keep the order straight. The first middleware you register runs
outermost, so it sees the request first and the response last:

```pith
app := web.new().use_mw(logging).use_mw(auth).get("/", home)
```

Here `logging` wraps `auth`, which wraps `home`. A request goes `logging` → `auth` → `home`,
and the response comes back the other way. Middleware wraps every route, including the
default `GET /healthz`.

Write middleware as a named function, as above. `examples/web_middleware.pith` is a
complete, self-checking example: an app-wide middleware adds a header to every
response, including the 401 that an API key check bound to `/admin` returns.

Two middleware are built in. `web.rate_limit(limiter)` answers 429 when a shared
token bucket runs dry, and `web.circuit(breaker)` answers 503 while a circuit
breaker is open, counting the handler's 5xx responses as failures. Both take
their state from `std.resilience`; see [docs/resilience.md](resilience.md).

## Route groups

`use_mw` wraps every route, but a guard usually belongs to some routes and
not others. A service splits into a public handful (login, health, metrics)
and a protected rest. `group` binds a middleware to a path prefix, so the
split is spelled at one site instead of as an allowlist inside the guard:

```pith
app := (web.new()
    .post("/login", handle_login)
    .group("/api", require_token)
    .get("/api/me", handle_me))
```

`/login` and `/healthz` stay open. Everything under `/api` runs through
`require_token`, inside whatever app-wide middleware is registered. The
prefix covers itself and anything below it: `/api` scopes `/api` and
`/api/me`, but not `/apiary`.

A guard usually learns something the handler wants: who the caller is,
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

`examples/web_auth.pith` runs this exact shape end to end: login, group
guard, and the requests a bad token gets back.

## Sessions

`std.web.session` keeps the state a request carries about who is on the other
end of it. The browser holds an ID and nothing else. Everything real lives on
the server, so revoking a session is a delete instead of a hope that a token
expires soon.

```pith
import std.web.session as session

fn main() -> Int!:
    store := session.store(bytes.from_string_utf8(env("SESSION_SECRET")))!
    app := web.new().use_mw(session.middleware(store)).post("/login", log_in)
    app.listen("0.0.0.0", 8080)!
    return 0
```

A handler only ever takes the request, so the store it reads and writes is
closed over. Build it once in `main` and capture it in the handlers, the way
`examples/web_session.pith` does.

```pith
store.set(req, "user", "u-7")             # false when the store is full
store.set_provisional(req, "note", "x")  # see provisional sessions below
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

### The cookie

The cookie carries the ID and a MAC over it, and nothing else. A session
cookie that carries data is a session cookie whose data the browser can be
talked into replaying. The ID is 32 bytes from the OS random source
(`std.crypto.random`), base64url encoded. The MAC is HMAC-SHA256 under the
store's secret and is compared with `std.crypto.subtle`. The MAC adds nothing
against guessing, since 256 bits already handles that, but a forged or truncated
cookie is thrown out on arithmetic before it becomes a store lookup, so a
flood of made-up cookies cannot be used to probe the store.

It is `HttpOnly`, `Secure` and `SameSite=Lax`, at `Path=/`, with a `Max-Age`
of the store's TTL. The builder moves each of those:

- `.ttl(seconds)`: the idle timeout, one day by default. A request that
  arrives with a live session slides the window forward
- `.cookie_name(name)`: defaults to `pith_session`
- `.path(value)`, `.same_site(value)`
- `.insecure_over_http()`: drops the `Secure` flag, for local development,
  and is the only way to drop it

The secret must be at least 32 bytes and belongs in the environment, not in the
source. `store()` refuses anything shorter. Changing it invalidates every
cookie already issued, which is the blunt way to sign everybody out.

### Rotate, and why a login needs it

Call `store.rotate(req)` at the top of a successful login, before writing who
the caller is. Without it, an attacker plants a session ID in a victim's
browser (a crafted link, an injected cookie), waits for them to sign in, and
finds the ID they already hold has become an authenticated one. Rotating
retires that ID at the exact moment it would become worth having, and carries
the session's contents across to a fresh one.

### What one process holds

The store is this process's memory behind one mutex, shared by every serving
task: green or OS thread, one store, as many tasks as you like. It is not
shared between processes. Two instances behind a load balancer each
hold their own sessions, so a request that lands on the other one finds a
signature it accepts and an ID it has never heard of, and treats the caller as
signed out. Run one instance, pin sessions to it with a sticky load balancer,
or put the state somewhere both can reach.

An ID is minted per request but only written to the store when a handler
stores something, so a request that reads nothing and writes nothing occupies
no row and gets no `Set-Cookie` back. The store holds at most 100,000 sessions
and refuses to grow past that instead of running the process out of memory.
Expired sessions are swept in the background of ordinary traffic.

### Provisional sessions

A login form's CSRF token has to live in a session before anybody has signed
in. `store.set_provisional(req, key, value)` writes a value like that. A
session that only provisional writes have touched is provisional: it lives at
most `session.PROVISIONAL_TTL` (an hour) instead of the store's TTL, and when
the store is full the oldest provisional session is evicted to make room for
a new one. A `set` or `rotate` turns it into a full session, and full sessions
are never evicted. `csrf` stores its token this way, so a flood of cookieless
requests for the login page only churns through provisional sessions, and
sign-in keeps working.

Eviction takes the oldest provisional session first. Least-recently-used
eviction would be the wrong choice here: a flood is made of the newest
sessions, so it would evict the idle sessions of real users first and sign
them out. Evicting the oldest provisional session costs a real visitor at most
a CSRF token, and reloading the form gets a new one.

`set`, `set_provisional` and `rotate` return false when the store is full of
full sessions and they could not write, and `guard.sign_in` returns false
with them. Answer that with a 503 instead of a page that assumes the write
happened.

## Signing in

`std.web.auth` is the binding between a session and an identity: the three
moments of signing in, guarding, and signing out.

```pith
import std.web.auth as auth

guard := auth.guard(store)

app = app.use_mw(session.middleware(store))
app = app.post("/login", log_in)
app = app.group("/app", auth.require(guard))
app = app.get("/app/me", me)
```

```pith
fn log_in(req: web.Request) -> http.HttpResponse:
    if not password.verify(submitted, stored_hash):
        return http.unauthorized_response()
    if not guard.sign_in(req, "u-7"):
        return http.text(503, "try again shortly")
    return http.text(200, "welcome")

fn me(req: web.Request) -> http.HttpResponse:
    return http.text(200, "hello " + guard.user(req))
```

- `guard.sign_in(req, subject)`: rotate the session ID, then record who the
  caller is. This is what a successful password check ends with. It returns
  false when the session store is full
- `guard.sign_out(req)`: end the session on the server
- `guard.user(req)`: the subject, or `""`
- `guard.signed_in(req)`: whether anybody is
- `auth.require(guard)`: the middleware. It must sit inside the session
  middleware, since what it reads is the session

`require` also attaches the subject to the request, so a handler that already
says `req.value("user")` (the shape the token guard above uses) works
unchanged when it moves onto sessions.

An unauthenticated caller gets a 401. `guard.redirect_to("/login")` changes
that to a 303 at the login page, which is what a browser app wants and what a
`fetch()` caller cannot do anything useful with. Leave it alone for a JSON
API.

`sign_in` rotates before it writes, which is the session fixation defence
spelled once in the one place every login goes through. Having a session is
not the same as being signed in: an anonymous visitor with a shopping cart has
one, and the guard turns them away all the same.

### What it deliberately is not

It is not a user database and it does not check passwords. Where the
credentials live, what a subject means, and how a password is checked belong
to the application, and `std.crypto.password` already does the last one
properly; see [docs/auth.md](auth.md). An API that took a password here
would have to take a user lookup with it, and would be wrong for every
application that keeps its users somewhere other than where it guessed.

Put a rate limiter in front of the login route. The Argon2id cost makes each
attempt expensive, and the limiter bounds how many are made:

```pith
app = app.group("/login", web.rate_limit(resilience.rate_limiter(5, 10)))
```

This limiter is one bucket for every caller. It bounds the total cost of
login attempts, but one client sending as fast as it can spends the whole
budget, and everyone else's sign-in gets a 429 until the bucket refills.
`web.rate_limit` has no per-client mode. To limit each client separately,
keep a limiter per `req.peer_ip()` in your own middleware and bound how many
it holds, or put a proxy that limits by address in front of the app.

`examples/web_login.pith` runs the whole stack end to end: the login page
with its CSRF token, a wrong password, a right one, the rotation, the
protected page, three ways of faking the cookie, and a sign-out.

## CSRF

A browser attaches a site's cookies to a request whatever page caused it,
which is what lets another site put a form on its own page, point it at
yours, and have it arrive fully authenticated. `std.web.csrf` demands
something on every state-changing request that a cross-site page has no way
to read: a token that lives in the session and has to be sent back.

```pith
import std.web.csrf as csrf

protection := csrf.guard(store)
app = app.use_mw(session.middleware(store))
app = app.use_mw(csrf.middleware(protection))   # inside the session
```

Register it inside the session middleware, since the token it compares
against lives in the session.

`GET`, `HEAD`, `OPTIONS` and `TRACE` pass through untouched. Everything else
must present the token, in the `X-CSRF-Token` header or in a `csrf_token`
form field (both a urlencoded and a multipart body are read), and gets a 403
if it does not. The comparison goes through `std.crypto.subtle`, so it looks
at every byte whether or not the first one matched.

A handler that renders a form asks for the token:

```pith
fn render_form(req: web.Request) -> http.HttpResponse:
    return http.html(200, "<input type=hidden name=csrf_token value=" + protection.token(req) + ">")
```

`protection.token(req)` returns the session's token, minting one (and with
it the session) if there is none yet. Asking twice returns the same token,
and it survives `store.rotate(req)`, so a login in the middle of a form's
life does not strand it. The middleware then mirrors the token into a
readable cookie on the way out, so a fetch-based front end can read it from
`document.cookie` and put it in the header without a round trip.

`.header_name(n)`, `.field_name(n)` and `.cookie_name(n)` rename the three
places, for a front end that expects other spellings.

### Why the mirror cookie is not the scheme

Handing out a readable cookie and comparing it against the header is the
"double submit" pattern, and on its own it leans on the attacker not being
able to write a cookie for your domain, which a sibling subdomain, or
anything else that gets to set one, can do. Here the cookie is a copy for
convenience and the session is the authority. The comparison is always
against what the store holds, so a cookie the attacker planted alongside a
matching header matches nothing.

`SameSite=Lax` on the session cookie, the default in `std.web.session`,
turns away most of this on its own: a browser that honors it leaves the
cookie off a POST, a fetch, or a form that another site starts. The token
covers what `Lax` does not:

- A sibling subdomain. `SameSite` compares sites, and `evil.example.com` is
  the same site as `app.example.com`, so a page on any subdomain you do not
  control gets the cookie attached.
- A `GET` that changes state. `Lax` sends the cookie on a top-level
  navigation from another site, and the middleware lets `GET` through, so
  keep every change behind `POST`, `PUT`, `PATCH` or `DELETE`.
- A browser that ignores `SameSite`, and a session deliberately set to
  `SameSite=None`.
- Chrome's grace period for a cookie set with no `SameSite` attribute: for
  its first two minutes, a cross-site top-level POST still carries it.
  `std.web.session` always sets the attribute, so this applies only to a
  cookie written some other way.

A urlencoded body larger than 64 KiB is not scanned for the token, because
splitting a ten megabyte body on `&` to find one field is work an attacker
can ask for repeatedly. A form that big should send the token in the header,
which costs nothing to read.

`examples/web_csrf.pith` runs both halves: the legitimate post with the token in
a header and in a form field, and then the cross-site version (session
cookie attached, no token), plus a token borrowed from another session and a
near miss.

## CORS

A browser will not let page JavaScript on one origin read a response from
another unless the server says it may. `std.web.cors` is the middleware that
says so, and the preflight handler that answers the `OPTIONS` request the
browser sends first.

```pith
import std.web.cors as cors

policy := cors.origins(["https://app.example.com"]).max_age(600)
app := web.new().use_mw(cors.middleware(policy)).get("/api/items", list_items)
```

There are three constructors, and which one you pick is the whole security
decision:

- `cors.any_origin()`: answers `Access-Control-Allow-Origin: *`. The policy
  for a public, read-only API. No credentials.
- `cors.origins(list)`: an explicit allowlist, compared whole: scheme, host
  and port. `https://app.example.com` does not match a subdomain of itself,
  a different port, or the same host over HTTP. No credentials.
- `cors.credentialed_origins(list)`: the same allowlist, and the browser may
  attach the user's cookies. Every origin on the list can act as the signed-in
  user, so keep it short.

The classic CORS hole is to reflect whatever `Origin` arrived back in
`Access-Control-Allow-Origin` and send `Access-Control-Allow-Credentials:
true` next to it, which tells every site on the internet that it may read this
one's responses with the user's session attached. That cannot be written here.
Credentials come only from `credentialed_origins`, which has no wildcard, and
`any_origin` has no credential switch. They are separate constructors instead
of two builder methods, so there is no way to combine them by accident. An
entry of `"*"` in a credentialed list is compared literally, so it matches an
origin spelled exactly `*`. No browser sends that origin, so the entry allows
nothing.

The builder narrows the rest:

- `.methods(names)`: what a preflight may ask for. Defaults to `GET`, `HEAD`,
  `POST`, `PUT`, `PATCH`, `DELETE`, `OPTIONS`
- `.headers(names)`: which request headers a preflight may ask for. Defaults
  to `Content-Type` and `Authorization`
- `.expose(names)`: which response headers page JavaScript is allowed to read
- `.max_age(seconds)`: how long the browser may cache the preflight answer

Register it outermost, before any guard that can answer 401, so a rejected
request still carries the headers that let the browser read the rejection.
Otherwise a plain 401 shows up in the console as a CORS error and sends
whoever is debugging it looking in the wrong place.

What happens to a request from an origin that is not allowed depends on which
kind it is. A preflight is refused with 403: it exists only to ask permission,
and the answer is no. A real request still runs: the server has no standing
to refuse it, and the same request without an `Origin` header would be
served. But the response carries no `Access-Control-Allow-Origin`, so the
browser withholds the body from the page. That is the same-origin policy
working as designed.

`examples/web_cors.pith` runs the whole thing: a real request from an allowed
origin and from somewhere else, the preflight for each, and the headers that
come back.

### OPTIONS

`use_mw` middleware only wraps a route the router matched, and a preflight
arrives as `OPTIONS` on a path registered for `GET` or `POST`, so before the
CORS middleware existed those requests fell straight through to a 404 with no
middleware in sight. `std.web` now answers an `OPTIONS` request to any path some route
claims with `204` and an `Allow` header listing that path's methods, and runs
it through the middleware chain like any other request. CORS middleware
intercepts the preflight ones, and the rest get the ordinary `Allow` answer. An
`OPTIONS` to a path no route claims is still a 404.

`OPTIONS *` asks about the server instead of a path (RFC 9110 9.3.7).
`std.web` answers it with `200`, an empty body and an `Allow` header that
lists every method the app's routes use, again through the app-wide middleware.

## Static files

`static(prefix, root)` serves the files under a directory, as net/http's
`FileServer` does:

```pith
app := web.new().static("/assets", "public")
```

`GET /assets/app.js` answers with `public/app.js`. The route is an ordinary
`GET` on `/assets/*path`, so it also answers `HEAD` and runs inside the
middleware. Each answer comes from `http.serve_dir`, which adds:

- `Content-Type` from the extension, or sniffed from the first 512 bytes
  when the extension is unknown or missing (see `http.detect_content_type`)
- `Last-Modified` from the file's modification time, and a strong `ETag`
  built from the modification time and the size
- `304` for `If-None-Match` and `If-Modified-Since`, and `412` for
  `If-Match` and `If-Unmodified-Since`, evaluated in the order of RFC 9110
  13.2.2
- `Accept-Ranges: bytes` and range requests. One range gets `206` with
  `Content-Range`, several get `206` with a `multipart/byteranges` body, and
  a range no byte of the file satisfies gets `416` with
  `Content-Range: bytes */size`. `If-Range` is honored. `HEAD` ignores
  `Range`, as RFC 9110 14.2 requires
- `index.html` for a directory, or `404` when there is none. Directories are
  never listed

A `Range` header the server cannot honor is ignored and the whole file
is sent, which RFC 9110 allows. That covers a malformed range, a unit other
than `bytes`, more than 32 ranges, and ranges that add up to more than the
file.

Nothing outside `root` is ever read. A `..` segment, plain or encoded, is
redirected to the canonical path before the route runs (see [canonical
paths](#canonical-paths)). A `..` that reaches the handler through an encoded
slash, such as `..%2Fsecret`, gets `404`, and a NUL byte gets `400`. Symbolic
links under `root` are followed, as they are in net/http.

## Markdown pages

`std.web.pages` serves a directory of Markdown files through a
`std.template` layout, with front matter, a table of contents and heading
IDs:

```pith
import std.web.pages as pages

# layout is a compiled template, such as template.load("layout.html")
app := web.new().get("/docs/*path", pages.handler("docs", layout))
```

It keeps the rules of static files: nothing outside the directory is read, a
symbolic link is followed, and the validators and conditional requests come
from `http.serve_content`. Only `.md` files are ever opened. The layout writes
the body with `<%md content %>`, so it needs no `<%raw %>`.
[docs/markdown.md](markdown.md#pages-in-stdweb) covers the layout's context
and the rest.

## Forms and basic auth

`req.form_value(name)` reads one form field. It looks in an
`application/x-www-form-urlencoded` or `multipart/form-data` body first and
then in the query string, as net/http's `FormValue` does. `req.raw.form()`
returns every value of every field, and `req.raw.post_form()` only the body's.
A body counts only on `POST`, `PUT` and `PATCH`. See
[HTTP apps](http_apps.md#forms) for the parsing rules.

`req.raw.basic_auth()` reads an `Authorization: Basic` header, and
`http.basic_auth_challenge(realm)` is the `401` to send when it fails:

```pith
fn admin(req: web.Request) -> http.HttpResponse:
    creds := req.raw.basic_auth() catch:
        return http.basic_auth_challenge("admin")
    if not subtle.equal(bytes.from_string_utf8(creds.password), expected):
        return http.basic_auth_challenge("admin")
    return http.text(200, "hello " + creds.user)
```

## Request body limits

The server reads a request body before the handler runs and refuses one
longer than the app's `ServerConfig.max_body_bytes` with `413` (see
[timeouts and limits](#timeouts-and-limits)). That is the limit that bounds
memory. `web.max_body(limit)` is a middleware that narrows it for some
routes, the way net/http's `MaxBytesReader` wraps one handler:

```pith
app := web.new().group("/avatars", web.max_body(262144)).post("/avatars", upload)
```

A request to those routes with a longer body gets `413 Content Too Large`.

## Observability

A server built with `web.new()` is observable out of the box. Every route is wrapped
in a built-in middleware that records metrics and opens a trace span, and `web.new()`
serves a `/metrics` endpoint. You write none of this. The built-in middleware sits
outside any middleware you add, so it measures the whole request.

### Metrics

For every request the middleware records three series, labeled by method and route
pattern. The label is the pattern (`/users/:id`), not the concrete path, so a busy
route stays one series instead of thousands:

- `http_server_requests_total{method,route,status}`: a request counter
- `http_server_requests_in_flight{method,route}`: a gauge of in-flight requests
- `http_server_duration_ms{method,route}`: a request-duration histogram

The accept loop records two more, unlabeled, about connections instead of
requests: `http_server_connections` and
`http_server_connections_refused_total`. Those come from `listen` itself, so a
`web.bare()` app has them even though it has no request middleware. [How many at
once](#how-many-at-once) covers what they mean.

These live in memory and are always on: they need no setup and cost a map update per
request. `web.new()` also registers `GET /metrics`, which renders every `std.metrics`
series as Prometheus text, so a scraper reads them straight off your server:

```pith
app := web.new().get("/users/:id", show)
# GET /metrics now returns the prometheus exposition, including
# http_server_requests_total{method="GET",route="/users/:id",status="200"} 12
```

`examples/web_observability.pith` runs the whole loop: it makes a few requests to a
route, then reads the request count back out of `/metrics`, with no instrumentation
code in the handler.

### Traces

The same middleware opens a server span per request, tagged with the method, path, and
status. It reads an inbound `traceparent` header, so a request that arrives already in
a trace joins it. Until you turn tracing on, the span is a cheap no-op: it takes no IDs
and records nothing, so an app without an exporter pays nothing for it.

To export spans, initialize `std.obs` before you start serving. `obs.init()` reads the
standard `OTEL_*` environment variables and, when `OTEL_EXPORTER_OTLP_ENDPOINT` is set,
turns tracing on and starts the OTLP exporter:

```pith
import std.obs as obs

fn main() -> Int!:
    obs.init()
    web.new().get("/", home).listen("0.0.0.0", 8080)!
    return 0
```

Spans then flow to your collector. `docs/telemetry.md` covers the exporter and the
sampler in full.

### Access logs

Access logging is opt-in. Metrics and traces already make a server observable by
default without printing anything, and a log line per request would clutter an
example or a benchmark, so you add it when you want it:

```pith
app := web.new().use_mw(web.access_log).get("/", home)
```

`web.access_log` writes one structured line per request to `std.log` (which goes to
stderr) with the method, route, path, status, and elapsed time.

### Opting out

Observability adds a little work to every request, so for a benchmark or a bare-bones
server, build the app with `web.bare()` instead of `web.new()`. A bare app has only the
default `/healthz` route: no observability middleware and no `/metrics`.

## A task per connection

`listen` accepts connections in a loop and hands each one to its own task with
`spawn`, so a slow client only ever holds up itself. This works the same whether you
run on OS threads or on the green runtime:

```
./your_server              # green on linux, os threads elsewhere
PITH_GREEN=0 ./your_server # one os thread per connection, anywhere
```

On the green runtime those per-connection tasks are green threads, so a server can
carry many more connections than it has OS threads.

Whatever goes wrong on one connection stays on its task. A client that fails a
TLS handshake, hangs up mid-request, or sends something unparseable costs itself
its own connection and nothing else. The loop itself only gives up when the
listener stops producing connections at all. It backs off after a failed accept
instead of spinning, and stops after a bounded run of failures with nothing
accepted in between, which is as much as it can tell from the outside. That
matters because a listener that has silently stopped accepting still answers a
TCP health check: the socket is open, the kernel is filling the backlog, and
nobody is serving.

### How many at once

`listen` and `listen_tls` serve at most 512 connections at a time, between them.
A task costs a stack and its buffers, so a loop that spawns one per accept with
no ceiling lets a client trade cheap sockets for expensive server memory: open
them faster than they complete and the server runs out.

Past the cap, the loop keeps accepting and refuses immediately with `503 Service
Unavailable` and `Retry-After: 1`, written by the accept loop itself, with no task
spawned. Over TLS the socket is closed instead, before the handshake: saying 503
in HTTP would mean completing the key exchange first, which is the expensive work
being refused.

The 503 is best-effort. Closing a socket that still holds unread data resets it
instead of finishing it cleanly, and a client that gets a reset drops what it had
buffered. So a peer that had already sent its request (most of them, when
a cap is firing) sees a connection error instead of the status line. Draining the
request first would fix that, but even a bounded read would hold the
accept loop for up to its deadline against every silent peer, which breaks the
property that the accept loop never waits. So treat the refusal metric as the
signal and the 503 as a courtesy to the clients that happen to be waiting.

This deliberately differs from the HTTP/2 server, which waits for a
connection to end before accepting another. The difference is the protocol. An
HTTP/2 connection multiplexes, so requests keep arriving on connections that are
already open and a loop that stops accepting still serves at full rate. HTTP/1.1
has no such decoupling: a request needs a connection, so a loop that stops
accepting stops answering *everything*, including the load balancer's probe and
your `/healthz`. An orchestrator then kills the process for being unresponsive at
exactly the moment it is healthy and full, and the replacement comes up into the
same load. Refusing keeps the server reachable, and a load balancer that gets an
immediate answer (a 503 or a reset) sheds to another instance instead of
waiting on a socket that will never reply.

`listen_h2c` is not part of this budget. It delegates to `std.net.http2.server`
and is bounded by that module's own cap of 512.

Two metrics make the cap visible. Both are recorded by the accept loop instead
of the request middleware, so a `web.bare()` app has them too:

- `http_server_connections`: a gauge of connections being served right now
- `http_server_connections_refused_total`: a counter of connections turned away

A refusal counter that is climbing is the cap shedding load, which is what it is
for. A gauge pinned at 512 with no requests flowing is the other case: slots held
by connections that are not going anywhere. The timeouts below reclaim such a
slot within a few seconds, so a pinned gauge means clients are opening
connections faster than that. The server keeps answering every new
connection either way, which it would not if the loop had blocked.

### Timeouts and limits

An app carries an `http.ServerConfig` (see
[HTTP apps](http_apps.md#server-configuration)) that sets the time limits,
request-size limits and keep-alive policy of every HTTP/1.1 connection `listen`
and `listen_tls` serve. `web.server_config()` is the default:

| Field | Default | What it bounds |
| --- | --- | --- |
| `read_header_timeout_ms` | 5000 | The whole request head, from its first byte |
| `idle_timeout_ms` | 5000 | A keep-alive connection's wait for its next request |
| `read_timeout_ms` | off | The head and the body together |
| `read_body_timeout_ms` | 60000 | The body, from the end of the request head |
| `write_timeout_ms` | off | The response, from the end of the request head |
| `max_header_bytes` | `http.MAX_HEADER_BYTES` (64 KB) | The request head |
| `max_body_bytes` | `http.MAX_REQUEST_BODY` (10 MB) | The request body |
| `keep_alives` | true | Whether a connection serves more than one request |
| `max_requests_per_conn` | 0, no limit | How many requests one connection serves |
| `max_connections` | 0, which means 512 | How many connections `listen` and `listen_tls` serve at once, counted across every such listener in the process |

Past `max_connections`, a new connection is refused with a 503.

By default net/http turns every timeout off, and a server built that way can be
held open by a client that sends one byte of its head every few seconds. The
read-header timeout here is a deadline on the socket, so the head has 5 s in
total however the bytes are spaced. The body has 60 s in total from the end of
the head, whether it is sized, chunked, or streamed to the handler through
`http.read_body_part`. Without that total, a client that sends one body byte
every few seconds would hold its connection slot indefinitely, and 512 such
clients would fill `max_connections`. 60 s carries the default 10 MB body at about 170 KB/s. An
app that takes larger or slower uploads raises it with
`with_read_body_timeout(ms)`, and 0 turns it off. Every read must also make
progress within 5 s, or within the longest of `read_timeout_ms`,
`read_header_timeout_ms` and `idle_timeout_ms` when that is longer. A config
with no read timeouts at all turns that bound off too.

Change any of it with `with_config`:

```pith
config := (web.server_config()
    .with_read_timeout(30000)
    .with_write_timeout(30000)
    .with_max_body_bytes(1048576))
app := web.new().with_config(config).get("/", home)
```

A request that runs out of time after its first byte gets `408 Request Timeout`
and the connection closes. A handler reading a streamed body sees the body
deadline as a `read_body_part` failure equal to `http.BODY_TIMEOUT_MESSAGE`,
and should answer 408 itself. A connection that times out before sending anything,
or that sits idle between requests, is closed with no response, as net/http
does. A response that runs out of write time is cut off and the connection
closes. The config does not apply to HTTP/2 (`listen_h2c`, or a `listen_tls`
client that negotiates h2), which keeps the HTTP/2 server's own limits.

## HTTP/2

The same app serves HTTP/2 with a different call. `listen_h2c` speaks cleartext
HTTP/2 (h2c), and `listen_tls` speaks HTTP/2 over TLS with an HTTP/1.1 fallback.
Either way the request runs through the same routing, middleware, and
observability as `listen`. Only the transport changes, so nothing in your app
has to know which one it is.

```
# cleartext http/2, no certificate needed
app.listen_h2c("0.0.0.0", 8080)!
```

`listen_tls` offers ALPN `["h2", "http/1.1"]` and branches per connection. A
client that negotiates `h2` is served over HTTP/2. Anything else, including a
client that sends no ALPN, is served as HTTP/1.1 over the same TLS session. One
listener handles a modern HTTP/2 client and a plain HTTPS client alike.

```
# http/2 over tls, falling back to http/1.1 for clients that ask for it
app.listen_tls("0.0.0.0", 8443, "cert.pem", "key.pem")!
```

`cert` and `key` are paths to PEM files: the server certificate and its private
key. h2c needs neither, which makes it the easy choice behind a load balancer
that terminates TLS. Use `listen_tls` when the Pith server terminates TLS
itself.

### HTTP/2 limits

An HTTP/2 connection is served by `std.net.http2.server`, with fixed limits of
its own in place of the `ServerConfig` above:

| Limit | Value | Past it |
| --- | --- | --- |
| Concurrent streams per connection | 128, advertised in `SETTINGS` | A new stream is refused with `RST_STREAM` `REFUSED_STREAM` |
| Header fields per request | 100 | The stream is reset with `ENHANCE_YOUR_CALM` |
| Decoded header list per request | 64 KB, names plus values | The stream is reset with `ENHANCE_YOUR_CALM` |
| Compressed header block | 80 KB, `HEADERS` plus `CONTINUATION` | The connection ends with `GOAWAY` |
| `CONTINUATION` frames per header block | 64 | The connection ends with `GOAWAY` |
| Request body | `http.MAX_REQUEST_BODY` (10 MB) | The stream is reset with `ENHANCE_YOUR_CALM` |
| Outbound frame queue per connection | 256 | Handlers and the reader wait for the writer |
| Connections, across every HTTP/2 listener in the process | 512 | The accept loop waits for one to finish |
| Socket inactivity | 120 s | The connection is dropped |

A stream counts against the 128 until its handler returns, including after
the client resets it. A client that opens and immediately resets streams (the
Rapid Reset attack) cannot keep more than 128 handlers running on one
connection, and every stream past that is refused with `REFUSED_STREAM`.

## Graceful shutdown

`listen` and `listen_tls` serve forever until a shutdown is requested. One call
wires that to `SIGTERM`:

```pith
import std.shutdown as shutdown

fn main() -> Int!:
    shutdown.on_signals()!
    web.new().get("/", home).listen("0.0.0.0", 8080)!
    return 0
```

On `SIGTERM` (or `SIGINT`/`SIGHUP`) the listener stops accepting and releases its
port, the in-flight requests are given a grace period to finish, and `listen`
returns the work that was still unfinished when that period expired (`0` for a
clean drain). A rolling deploy no longer severs responses mid-flight. Without the
`on_signals()` call nothing changes and `listen` blocks forever, as before.

[docs/signals.md](signals.md) covers the drain, the coordinator, and what
becomes of a stream that would otherwise never end.

## A runnable example

`examples/web_hello.pith` is a complete, self-checking server: it defines a couple of
routes, spawns the server, makes a few requests against itself, and prints the
replies. `examples/web_cors.pith` puts a CORS policy in front of an API and drives it
from two origins, one allowed and one not. `examples/web_session.pith` runs the whole
life of one session: an anonymous visit, a sign-in that rotates the ID, a forged
cookie, and a sign-out. `examples/web_csrf.pith` posts a form the legitimate way and then
the way another site would. `examples/web_login.pith` puts the whole stack together
(Argon2id, a session, a CSRF token and a guard) and drives a login, a protected page
and a sign-out through it. `examples/web_observability.pith` does the
same and then scrapes `/metrics` to show the request counter the framework kept on its
own. `examples/web_h2.pith` serves
the same kind of app over HTTP/2 (h2c) and drives it with the `std.net.http2` client.
`bench/web_hello.pith` is the same idea as a real blocking server, wired up for
`bench/http_bench.sh`. `examples/graceful_shutdown.pith` serves, signals itself,
and prints the drain outcome.

## Rendering HTML

`http.html(200, body)` will send whatever body you hand it, so what goes into
that body is the whole of your XSS story. Anything the caller controls (a
query parameter, a path parameter, a header, a stored value that arrived from a
request once) goes through `std.html` on the way in:

```pith
import std.html as html

fn greet(req: web.Request) -> http.HttpResponse:
    return http.html(200, "<h1>hello, " + html.escape(req.param("name")) + "</h1>")
```

For a whole page instead of a fragment, `std.template` renders from a context
and escapes every interpolated value for where it lands in the page, so the
safe thing is what happens when you do not think about it:

```pith
import std.template as template

fn greet(req: web.Request) -> http.HttpResponse:
    ctx := template.context().set("name", req.param("name"))
    return http.html(200, template.render_string("<h1>hello, <%= name %></h1>", ctx) catch "")
```

A page served per request should be `compile`d once at startup instead of on
every call, so that a syntax error is a failure to boot instead of a 500.

A `<%= %>` in element text or a quoted attribute is HTML-escaped. One in an
unquoted attribute has every character that could end the attribute turned
into an entity. One in a URL attribute such as `href` or `src` is checked with
`html.safe_url` first, so `javascript:`, `data:` and `//other.host` become
`about:blank`. Compile refuses a `<%= %>` where escaping cannot help: inside
`<script>` or `<style>`, in an `on...` event handler, `style` or `srcdoc`
attribute, or in a tag or attribute name.

[docs/html.md](html.md) is the full account of both: the five characters, why
there is no separate attribute escaper, why a URL in an `href` needs
`html.escape_url` when you build the page by hand, the contexts where escaping
is not the fix, and the template syntax with its one greppable opt-out.

To send a browser to a destination the request named, such as a `next`
parameter after login, use `web.local_redirect(target, fallback)`. It answers
303 to `target` only when `web.is_local_path` accepts it as a path on this
site (one leading `/`, not `//` or `/\`, no control characters), and to
`fallback` otherwise.
