# html escaping and templates

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

## templates

`std.html` is the call you have to remember. `std.template` is the one you do
not: it renders a page from a context and escapes every interpolated value on
the way out, unless a human wrote the opt-out.

```pith
import std.template as template

page := template.compile("<h1>hello, <%= name %></h1>")!
ctx := template.context().set("name", "<script>alert(1)</script>")
template.render(page, ctx)!
# "<h1>hello, &lt;script&gt;alert(1)&lt;/script&gt;</h1>"
```

that default is the entire point. a helper you have to call protects the pages
you remembered on; a renderer that escapes by default protects the ones you
forgot, and the ones you forgot are the ones that get exploited.

`examples/templating.pith` renders a page from hostile input and prints what
survived.

### the syntax

| tag | does |
| --- | ---- |
| `<%= path %>` | escaped output — the default |
| `<%raw path %>` | unescaped output |
| `<% if path %> … <% end %>` | conditional |
| `<% if not path %> … <% end %>` | negated conditional |
| `<% if path %> … <% else %> … <% end %>` | with an alternative |
| `<% for item in path %> … <% end %>` | loop |
| `<%# … %>` | comment, dropped |

that is all of it. a `path` is a name or dot-joined names (`user.name`) and
nothing else: no function calls, no arithmetic, no comparisons, no filters, no
`and`/`or`, no inheritance, no partials or includes, no whitespace control. if
you need a comparison, do it in pith and put a boolean in the context. this is
a renderer, not a second language to debug.

### why `<% %>` and not `{{ }}`

pith already owns `{` and `}` inside a string literal — they are its own
interpolation, and a literal brace has to be doubled. a mustache-style tag
written inline would be `"{{{{name}}}}"` to mean `{{name}}`, in every template
and in every test. `<% %>` cannot collide with pith syntax, so a template reads
the same in a `.html` file and in a pith string.

### the opt-out

`<%raw value %>` emits a value untouched. it is spelled in words rather than in
punctuation on purpose: `grep -rn '<%raw' templates/` is a complete audit of
every place your templates trust a value, which is a property `<%== %>` and
`{{{ }}}` do not give you.

use it for markup your own program produced, and for a value you have already
escaped by hand — a url that went through `html.escape_url`, for instance,
where a second pass of `escape` would corrupt the `&amp;` it just wrote:

```pith
fn note(title: String, link: String) -> template.Ctx:
    return template.context().set("title", title).set("link", html.escape_url(link))
```

```
<a href="<%raw note.link %>"><%= note.title %></a>
```

### the context

a context is a `std.json` object node behind a typed front, so a value that
arrived as json can be rendered directly with `template.from_json(handle)`, and
the node pool's scoping rules apply — see the json node pool section of
[docs/web.md](web.md). inside a request handler the pool is already scoped for
you and there is nothing to do.

```pith
rows := template.list()
rows.push(template.context().set("title", "first"))
rows.push(template.context().set("title", "second"))

ctx := template.context()
ctx.set("heading", heading)
ctx.set_int("count", 2)
ctx.set_bool("admin", false)
ctx.set_child("rows", rows)
```

a path that does not resolve renders as empty and is falsy. that is deliberate
for a page renderer: a missing field should leave a hole, not take the response
down. if a missing field is a bug for you, check the context before rendering.

`if` treats missing, null, `false`, `0`, `""`, and the empty list as false.
`for` over anything that is not a list runs zero times.

### compiling, and files

`compile` checks everything: an unknown tag, an unbalanced `end`, an expression
that is not a plain dotted path, nesting deeper than 32. compile your pages at
startup and a broken template fails the boot rather than a request.

`load(path)` compiles a template from a file, capped at 1 MiB. the path is used
exactly as given, so it has to come from your own code or configuration.
`load_in(dir, name)` is the one for a name that came from a request: `name` must
be a plain file name with no `/`, no `\`, no `..`, and no leading `.`. the rule
is checked on the name as written rather than on a resolved path, so there is no
window between the check and the read and no symlink to race — a name that
cannot express a traversal cannot become one.

### what the escaping does not cover

the renderer escapes with `html.escape`, so it inherits exactly the limits
above. a `<%= %>` inside a `<script>` block is not safe, and a `<%= %>` in an
`href` escapes the quotes but does not stop `javascript:`. put the url through
`html.escape_url` in pith and emit it with `<%raw %>`.

rendered output is capped at 8 MiB; a page that exceeds it fails rather than
growing until the process does.

## see also

- [docs/web.md](web.md) — the routing and middleware layer these responses go
  back through
- [docs/http_apps.md](http_apps.md) — request helpers and response builders
