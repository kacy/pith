# html escaping

`std.html` is the module that stands between a request and a page. a server
that renders `req.query_param("name")` into `<h1>hello, {name}</h1>` is not
serving a greeting, it is serving whatever the caller wrote — including a
`<script>` tag, to the next person who follows the link. `html.escape` is the
call that stops that, and escaping is the whole of what this module does.

```pith
import std.html as html

greeting := "<h1>hello, " + html.escape(name) + "</h1>"
```

`examples/http_server.pith` and `examples/http_websocket_app.pith` both render
a query parameter and both go through here.

## escape

`escape` replaces the five characters that can change the meaning of markup:

| character | becomes  |
| --------- | -------- |
| `&`       | `&amp;`  |
| `<`       | `&lt;`   |
| `>`       | `&gt;`   |
| `"`       | `&quot;` |
| `'`       | `&#39;`  |

`&` is in that table alongside the others rather than being run as a separate
first pass. this is what makes double-escaping impossible: each input byte is
read once and produces one output, so the `&` inside an entity this call just
emitted is never looked at again. a two-pass escaper that escapes `&` second
turns `<` into `&amp;lt;`, which is the classic way pages end up displaying
their own markup.

`'` becomes the numeric `&#39;` rather than `&apos;`. `&apos;` is an xml entity
html 4 never defined, and a browser in quirks mode can render it literally —
which puts the quote character back.

an input with nothing to escape is returned unchanged rather than rebuilt, and
the scan is over bytes, so non-ascii text is copied through whole.

there is no separate attribute escaper. escaping both quote characters means
`escape` is already correct inside `attr="…"` and inside `attr='…'`, and a
second function that did the same thing would only raise the question of which
one a given call site should have used. the contexts an attribute escaper is
usually invented for are urls and unquoted attributes, and neither of those is
fixed by escaping more characters — see below.

## where escaping is enough

two contexts, which between them are nearly all of a page:

- element text — `<p>HERE</p>`, `<title>HERE</title>`, `<td>HERE</td>`
- a quoted attribute value — `<input value="HERE">` or `value='HERE'`

## where escaping is not enough

this is the part worth reading twice. `escape` is not a sanitizer and calling
it in the wrong context buys nothing except the feeling of having done
something.

**inside `<script>`.** html entities are not decoded in a script block. the
value stays live javascript, and `&lt;` arrives at the parser as the four
characters `&lt;` rather than as `<`. escaping there is not weak, it is inert.
serialize the value as json and read it from a `<script type="application/json">`
block or from a data attribute, rather than pasting it into code.

**inside `<style>`.** same reason, plus css has an injection surface of its own
in `url(…)` and the legacy `expression(…)`.

**a url attribute** — `href`, `src`, `action`, `formaction`. there is nothing
to escape in `javascript:alert(1)`: it survives `escape` word for word and is
still clickable. use `escape_url`.

**an unquoted attribute** — `<a class=HERE>`. a space, a tab, or a newline ends
the attribute and starts a new one, and none of those are escaped. quote your
attributes; this module assumes you did.

**an html comment, a tag name, or an attribute name.** none of those are text.
if untrusted data is deciding what your tags are called, escaping is not the
problem you have.

## urls

`safe_url` filters a url by its scheme. it has a deliberately narrow contract:
it returns exactly what you gave it, or it returns `"about:blank"`. it never
rewrites, encodes, or repairs a url, so nothing downstream has to work out
whether the string changed shape.

```pith
html.safe_url("https://example.com/x")   # "https://example.com/x"
html.safe_url("/relative/path")          # "/relative/path"
html.safe_url("javascript:alert(1)")     # "about:blank"
```

accepted: `http`, `https`, `mailto`, `tel`, and a relative url with no scheme.
refused: every other scheme, and a protocol-relative url (`//evil.example`,
along with the backslash spellings browsers also accept). the last one is not
script execution, but an attacker-supplied `href` that silently navigates
off-site is an open redirect, and an open redirect is exactly what an
attacker-supplied `href` is for.

the check runs on a copy of the url with control characters, spaces, and
delete removed, because that is what a browser does before it decides what the
scheme is. `java<TAB>script:alert(1)` looks harmless to a naive
`starts_with("javascript:")` test and runs perfectly well in a browser; the
allowlist here sees the same string the browser will.

`escape_url` is `safe_url` followed by `escape`, and is what belongs in an
attribute:

```pith
link := "<a href=\"" + html.escape_url(target) + "\">go</a>"
```

## encoding

the entities are ascii and the scan is byte-wise, so utf-8 passes through
untouched. the output is only safe in a document actually served as utf-8 —
declare it, in the `Content-Type` header or with `<meta charset="utf-8">`. a
page whose charset the browser is left to guess can be pushed into an encoding
where some other byte sequence means `<`, and then no amount of escaping helps.

## json is not html

a json body built by pasting strings has the same shape of bug and none of the
same fix. this:

```pith
body := "{{\"agent\":\"{ua}\"}}"
```

breaks the moment a `User-Agent` contains a double quote, and lets the caller
add fields of their own. use the builder, which quotes and escapes as it goes:

```pith
import std.json as json

body := json.make_object()
json.object_set(body, "status", json.make_string("ok"))
json.object_set(body, "agent", json.make_string(ua))
return http.json_response(200, json.encode(body))
```

## see also

- [docs/web.md](web.md) — the routing and middleware layer these responses go
  back through
- [docs/http_apps.md](http_apps.md) — request helpers and response builders
