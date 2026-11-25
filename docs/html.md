# HTML escaping and templates

`std.html` sits between a request and a page. A server that renders
`req.query_param("name")` into `<h1>hello, {name}</h1>` serves whatever the
caller wrote, including a `<script>` tag, to the next person who follows the
link. `html.escape` is the call that stops that, and escaping is all this
module does.

```pith
import std.html as html

greeting := "<h1>hello, " + html.escape(name) + "</h1>"
```

`examples/http_api.pith` and `examples/http_websocket_app.pith` both render
a query parameter and both go through here.

## Escape

`escape` replaces the five characters that can change the meaning of markup:

| Character | Becomes  |
| --------- | -------- |
| `&`       | `&amp;`  |
| `<`       | `&lt;`   |
| `>`       | `&gt;`   |
| `"`       | `&quot;` |
| `'`       | `&#39;`  |

`&` is handled in the same table as the others, not in a separate first pass.
That makes double-escaping impossible: each input byte is read once and
produces one output, so the `&` inside an entity this call just emitted is
never looked at again. A two-pass escaper that escapes `&` second turns `<`
into `&amp;lt;`, which is the usual way pages end up displaying their own
markup.

`'` becomes the numeric `&#39;` instead of `&apos;`. `&apos;` is an XML entity
that HTML 4 never defined, and a browser in quirks mode can render it
literally, which puts the quote character back.

An input with nothing to escape is returned unchanged instead of rebuilt. The
scan is over bytes, so non-ASCII text is copied through whole.

There is no separate attribute escaper. Because it escapes both quote
characters, `escape` is already correct inside `attr="…"` and inside
`attr='…'`. A second function that did the same thing would only raise the
question of which one a given call site should have used. An attribute escaper
is usually written for URLs and unquoted attributes, and escaping more
characters fixes neither of those (see below).

## Where escaping is enough

Two contexts, which together cover nearly all of a page:

- Element text: `<p>HERE</p>`, `<title>HERE</title>`, `<td>HERE</td>`
- A quoted attribute value: `<input value="HERE">` or `value='HERE'`

## Where escaping is not enough

`escape` is not a sanitizer. Calling it in the wrong context gives no
protection.

**Inside `<script>`.** HTML entities are not decoded in a script block. The
value stays live JavaScript, and `&lt;` reaches the parser as the four
characters `&lt;`, not as `<`. Escaping there has no effect. Serialize the
value as JSON and read it from a `<script type="application/json">` block or
from a data attribute instead of pasting it into code.

**Inside `<style>`.** The same reason applies, and CSS also has an injection
surface of its own in `url(…)` and the legacy `expression(…)`.

**A URL attribute** (`href`, `src`, `action`, `formaction`). There is nothing
to escape in `javascript:alert(1)`: it survives `escape` word for word and is
still clickable. Use `escape_url`.

**An unquoted attribute** (`<a class=HERE>`). A space, a tab or a newline ends
the attribute and starts a new one, and none of those are escaped. Quote your
attributes; this module assumes you did.

`std.template` handles both of these for a `<%= %>` value; see
[where a value lands](#where-a-value-lands).

**An HTML comment, a tag name, or an attribute name.** None of those are text.
If untrusted data decides what your tags are called, escaping cannot fix the
problem.

## URLs

`safe_url` filters a URL by its scheme. Its contract is deliberately narrow:
it returns exactly what you gave it, or it returns `"about:blank"`. It never
rewrites, encodes, or repairs a URL, so nothing downstream has to work out
whether the string changed shape.

```pith
html.safe_url("https://example.com/x")   # "https://example.com/x"
html.safe_url("/relative/path")          # "/relative/path"
html.safe_url("javascript:alert(1)")     # "about:blank"
```

Accepted: `http`, `https`, `mailto`, `tel`, and a relative URL with no scheme.
Refused: every other scheme, and a protocol-relative URL (`//evil.example`,
along with the backslash spellings browsers also accept).

`safe_url` stops a link from running script. It does not keep a link on your
site: `https://evil.example` is an `https` URL and passes unchanged. Refusing
`//evil.example` only stops a relative-looking value from leaving the site
under the page's own scheme. For a redirect to a destination the request
chose, which is where an open redirect comes from, use
`web.local_redirect(target, fallback)` from `std.web`. It accepts only a path
on this site: one leading `/`, not followed by `/` or `\`, with no control
characters.

The check runs on a copy of the URL with control characters, spaces, and
delete removed, because a browser does the same before it decides what the
scheme is. `java<TAB>script:alert(1)` looks harmless to a naive
`starts_with("javascript:")` test and runs in a browser; the allowlist here
sees the same string the browser will.

`escape_url` is `safe_url` followed by `escape`, and is what belongs in an
attribute:

```pith
link := "<a href=\"" + html.escape_url(target) + "\">go</a>"
```

## Encoding

The entities are ASCII and the scan is byte-wise, so UTF-8 passes through
untouched. The output is only safe in a document actually served as UTF-8.
Declare it in the `Content-Type` header or with `<meta charset="utf-8">`. A
page whose charset the browser is left to guess can be pushed into an encoding
where some other byte sequence means `<`, and then no amount of escaping helps.

## JSON is not HTML

A JSON body built by pasting strings has the same kind of bug, and the HTML
fix does not apply to it. This:

```pith
body := "{{\"agent\":\"{ua}\"}}"
```

breaks as soon as a `User-Agent` contains a double quote, and lets the caller
add fields of their own. Use the builder, which quotes and escapes as it goes:

```pith
import std.json as json

body := json.make_object()
json.object_set(body, "status", json.make_string("ok"))
json.object_set(body, "agent", json.make_string(ua))
return http.json_response(200, json.encode(body))
```

## Templates

`std.html` is the call you have to remember. `std.template` is the one you do
not: it renders a page from a context and escapes every interpolated value for
where it lands, unless the template's author wrote the opt-out.

```pith
import std.template as template

page := template.compile("<h1>hello, <%= name %></h1>")!
ctx := template.context().set("name", "<script>alert(1)</script>")
template.render(page, ctx)!
# "<h1>hello, &lt;script&gt;alert(1)&lt;/script&gt;</h1>"
```

A helper you have to call protects only the pages where you remembered it. A
renderer that escapes by default also protects the ones you forgot, and the
ones you forgot are the ones that get exploited.

`examples/templating.pith` renders a page from hostile input and prints what
survived.

### The syntax

| Tag | Does |
| --- | ---- |
| `<%= path %>` | Escaped output (the default) |
| `<%md path %>` | The value rendered as Markdown with the safe defaults |
| `<%raw path %>` | Unescaped output |
| `<% if path %> … <% end %>` | Conditional |
| `<% if not path %> … <% end %>` | Negated conditional |
| `<% if path %> … <% else %> … <% end %>` | With an alternative |
| `<% for item in path %> … <% end %>` | Loop |
| `<%# … %>` | Comment, dropped |

That is the whole syntax. A `path` is a name or dot-joined names (`user.name`)
and nothing else: no function calls, no arithmetic, no comparisons, no
filters, no `and`/`or`, no inheritance, no partials or includes, no whitespace
control. If you need a comparison, do it in Pith and put a boolean in the
context. Keeping the syntax this small means there is no second language to
debug.

### Why `<% %>` and not `{{ }}`

Pith already uses `{` and `}` inside a string literal for its own
interpolation, and a literal brace has to be doubled. A Mustache-style tag
written inline would be `"{{{{name}}}}"` to mean `{{name}}`, in every template
and in every test. `<% %>` cannot collide with Pith syntax, so a template reads
the same in a `.html` file and in a Pith string.

### The opt-out

`<%raw value %>` emits a value untouched. It is spelled with a word instead of
punctuation on purpose: `grep -rn '<%raw' templates/` is a complete audit of
every place your templates trust a value, and `<%== %>` and `{{{ }}}` do not
give you that.

Use it for markup your own program produced, and for a value you have already
escaped by hand, where a second pass of `escape` would corrupt the `&amp;` it
wrote. A URL from outside does not need it: `<%= %>` in an `href` already
filters the scheme and escapes.

```
<a href="<%= note.link %>"><%= note.title %></a>
```

`<%raw %>` is allowed in every position, including the ones where `<%= %>` is
refused below. Compilation assumes that what it writes does not move the page
into a different context, such as opening a tag or an attribute it does not
close.

### Markdown

`<%md path %>` renders the value as Markdown with `std.markdown`'s safe
defaults and writes the HTML in place. Raw HTML in the value is shown as text,
and link and image URLs go through `html.safe_url` after their entities are
decoded. No tag and no option lets raw HTML through, so every `<%md` is safe
and the `<%raw` audit can skip it. Text that should keep its raw HTML is
rendered in Pith with `markdown.options().allow_html(true)` and written with
`<%raw %>`, where the audit finds it.

```
<article><h1><%= post.title %></h1><%md post.body %></article>
```

The HTML it writes is block content, so compile accepts `<%md %>` only in
element text, where a `<div>` could go. In an attribute value, a tag, a
comment, `<script>`, `<style>`, `<title>` or `<textarea>`, the template fails
to compile. `heading_ids(true)` and `heading_anchors(true)` on a compiled
template give the headings it renders IDs and links to themselves.
`code_highlighter(highlight.code_block)` highlights its Pith code blocks. The
template trusts what a highlighter returns the way it trusts `<%raw %>`, so the
audit looks for `code_highlighter(` too. The Markdown side is in
[docs/markdown.md](markdown.md), and `std.web.pages` serves whole directories
of Markdown through a layout.

### The context

A context is a `std.json` object node behind a typed front, so a value that
arrived as JSON can be rendered directly with `template.from_json(handle)`, and
the node pool's scoping rules apply (see the JSON node pool section of
[docs/web.md](web.md)). Inside a request handler the pool is already scoped for
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

A path that does not resolve renders as empty and is falsy. That is deliberate
for a page renderer: a missing field should leave a hole in the page instead
of taking the response down. If a missing field is a bug for you, check the
context before rendering.

`if` treats missing, null, `false`, `0`, `""`, and the empty list as false.
`for` over anything that is not a list runs zero times.

### Compiling, and files

`compile` checks everything: an unknown tag, an unbalanced `end`, an expression
that is not a plain dotted path, nesting deeper than 32. Compile your pages at
startup, and a broken template fails the boot instead of a request.

`load(path)` compiles a template from a file, capped at 1 MiB. The path is used
exactly as given, so it has to come from your own code or configuration.
`load_in(dir, name)` is the one for a name that came from a request: `name` must
be a plain file name with no `/`, no `\`, no `..`, and no leading `.`. The rule
is checked on the name as written, not on a resolved path, so there is no
window between the check and the read and no symlink to race. A name that
cannot express a traversal cannot become one.

### Where a value lands

`compile` reads the literal HTML of the template with a small tokenizer that
follows the HTML spec's states for text, tags, attribute names and values,
comments, and the raw text of `<script>` and `<style>`. Each `<%= %>` is
escaped for the place where it sits:

| Where | How |
| --- | --- |
| Element text, a comment, `<title>`, `<textarea>` | `html.escape` |
| A quoted attribute value | `html.escape` |
| An unquoted attribute value | Every ASCII byte other than a letter, a digit, `-`, `_` or `.` becomes a numeric entity |
| A URL attribute, quoted or not | `html.safe_url`, then the escaping for its quoting |

The URL attributes are `href`, `src`, `action`, `formaction`, `xlink:href`,
`poster`, `cite`, `background`, `data`, `codebase`, `longdesc`, `manifest`
and `icon`. A value that `safe_url` refuses is written as `about:blank`. The
check runs on the literal text in front of the value plus the value, so
`href="/<%= path %>"` with `path` set to `/evil.example` is refused as the
`//evil.example` a browser would follow, and `href="/users/<%= id %>"` passes
any id. A second value in the same URL is checked by itself, without the
first.

`compile` refuses a `<%= %>` in these places, where no escaping makes a value
safe:

- Inside `<script>` or `<style>`. Pass data to a script as JSON in a data
  attribute or a `<script type="application/json">` block.
- In an event handler attribute (any name starting with `on`), in `style`,
  and in an iframe's `srcdoc`. Put the value in a data attribute and read it
  from script, or choose a class with `<% if %>`.
- In a tag name or an attribute name. Choose between fixed names with
  `<% if %>`.

The paths through an `if`, with or without an `else`, must leave the HTML in
the same place, and so must a `for` body, so that a value after the block has
one context. A template that opens a tag or an attribute value in one branch
and not the other fails to compile.

Rendered output is capped at 8 MiB; a page that exceeds it fails instead of
growing until the process does.

## See also

- [docs/web.md](web.md): the routing and middleware layer these responses go
  back through
- [docs/http_apps.md](http_apps.md): request helpers and response builders
