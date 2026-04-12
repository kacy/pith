# http apps

forge now has a more practical http layer in `std.net.http`.

the important split is:
- `HttpRequestBytes` is the main request type
- `HttpResponse` is the main response type
- routing stays explicit in normal forge code

that means you can write small apps without hand-rolling query parsing,
response serialization, or request building every time.

## request helpers

use `HttpRequestBytes` for normal server-side code:

```fg
fn app(req: HttpRequestBytes) -> HttpResponse:
    if http.match_route(req, "GET", "/users/:id") catch false:
        user_id := req.path_param("/users/:id", "id") catch ""
        tab := req.query_param("tab") catch ""
        return http.text(200, "user=" + user_id + " tab=" + tab)
    return http.not_found_response()
```

use these helpers instead of reparsing strings by hand:
- `req.query_param(...)`
- `req.has_query_param(...)`
- `req.path_parts()`
- `req.matches_path(...)`
- `req.path_param(...)`
- `req.body_json()`

## response helpers

`HttpResponse` is a plain value with a small builder surface:

```fg
resp := http.json_value(200, payload).header("X-Trace", "abc123")
http.send_buffered(writer, resp)!
```

the common constructors are:
- `http.response(status)`
- `http.text(status, body)`
- `http.html(status, body)`
- `http.json(status, body)`
- `http.json_value(status, handle)`
- `http.not_found_response()`
- `http.redirect_response(url)`

the older raw string helpers still exist for compatibility:
- `html_response(...)`
- `json_response(...)`
- `not_found()`
- `redirect(...)`

but the object form is the better default for new code.

## serving one request

for tests, in-memory examples, or small buffered handlers:

```fg
http.serve_one(reader, writer, app)!
```

that keeps the handler shape simple:
- input: `HttpRequestBytes`
- output: `HttpResponse`

if a handler wants to recover from parse or query issues, just map those
results into a normal response:

```fg
payload := req.body_json() catch -1
if payload < 0:
    return http.server_error_response()
```

## client helpers

the client side now uses the same explicit shape:

```fg
mut req := http.request("POST", "example.com", 80, "/items")
req = req.header("X-Test", "yes")
req = req.json_body(payload)
raw := req.to_bytes()!
```

for real requests:

```fg
resp := http.request("GET", "127.0.0.1", 8080, "/health").send()!
print(resp.status_code().to_string())
print(resp.body_text()!)
```

that keeps request construction, response parsing, and body decoding in one
module instead of splitting the work across app code.
