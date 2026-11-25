# markdown

`std.markdown` parses commonmark and renders it to html. it implements all of
commonmark 0.31.2 and passes the spec's 652 examples, and the github flavored
markdown extensions, which pass the 54 extension examples of the gfm spec and
of cmark-gfm's tests. the defaults are set for rendering text someone else
wrote, such as a comment, an issue body or a profile field, so the one-line
call is the safe one:

```pith
import std.markdown as markdown

page := markdown.to_html("# hello\n\n*world* <script>alert(1)</script>")
# "<h1>hello</h1>\n<p><em>world</em> &lt;script&gt;alert(1)&lt;/script&gt;</p>\n"
```

## what it supports

every block and inline construct in the spec:

- blocks: atx and setext headings, thematic breaks, indented and fenced code,
  the seven kinds of html block, link reference definitions, paragraphs, block
  quotes, and bullet and ordered lists with the tight and loose rules.
- inlines: backslash escapes, entity and numeric character references (the
  full html5 table of named entities), code spans, emphasis and strong
  emphasis by the delimiter-run rules, inline links and images, full,
  collapsed and shortcut reference links, uri and email autolinks, raw html,
  and hard and soft line breaks.
- tabs expand to the next multiple of four columns where the spec says so,
  including a tab only partly used by a list item's indentation.
- link labels match after unicode case folding, so `[ẞ]` finds `[SS]: /url`.

the html is cmark's, byte for byte, with one difference: a `'` is written as
`&#39;`, the escape `std.html` uses, where cmark writes it bare in text and as
`&#x27;` in a url.

### github flavored markdown

`gfm(true)`, the default, adds the github extensions as cmark-gfm implements
them, with cmark-gfm's html:

- tables. a delimiter row such as `| :-- | --: |` turns the line above it into
  a header row when the two have the same number of cells, and the lines after
  it into body rows until a blank line or another block. cells hold inline
  content, a `\|` is a pipe inside a cell (in a code span too), and the colons
  set each column's alignment. a row shorter than the header is padded with
  empty cells, and a longer one is cut.
- strikethrough with one or two tildes, `~a~` or `~~a~~`.
- task list items. `[ ]`, `[x]` or `[X]` and a space at the start of an item's
  first paragraph become a disabled checkbox.
- extended autolinks: `www.` addresses, `http://`, `https://` and `ftp://`
  urls, and email addresses become links without angle brackets. trailing
  punctuation, a `)` with no `(` in the link and an entity-like `&name;` at
  the end are left out of the link. none are looked for inside a link's text.
- footnotes. `[^label]` refers to a `[^label]: text` definition, and the
  definitions are written in a section after the document, numbered in the
  order of their first reference, each with a link back to every reference.
  a definition holds blocks like a list item does, with its later lines
  indented four columns. an unreferenced definition is dropped, a reference
  with no definition stays text, and when two definitions share a label the
  first wins.
- with `allow_html(true)`, the tag filter: the `<` of `<title>`,
  `<textarea>`, `<style>`, `<xmp>`, `<iframe>`, `<noembed>`, `<noframes>`,
  `<script>` and `<plaintext>`, opening or closing, is written as `&lt;`.

`gfm(false)` gives plain commonmark. the extensions change 11 of the
commonmark spec's examples: six html blocks with `<script>`, `<style>` or
`<textarea>` pass through the tag filter, and five bare urls and email
addresses become links.

where this differs from cmark-gfm:

- a task list marker counts in any list item's first paragraph. cmark-gfm
  looks for it only when the list marker starts the line, so it misses one in
  `> - [ ] a`.
- a footnote label matches the way a link label does: on the text as written,
  after case folding, and at most 999 bytes long. a reference with no
  definition is parsed as ordinary text, so `[^*a*]` shows emphasis where
  cmark-gfm shows the characters.
- footnotes are numbered as the event stream reaches their references, and
  definitions come after the document. a footnote referenced only from inside
  another definition is numbered after those the document references, where
  cmark-gfm numbers in source order.
- non-ascii punctuation counts as part of an autolink's domain, where
  cmark-gfm stops the domain at it. this matters only for the rule on
  underscores and for a url whose host starts with such a character.
- the empty cells that pad short rows share a budget the size of the input
  (see the limits below), where cmark-gfm allows a fixed number per table.

## the safe defaults

three rules hold unless the options turn them off.

### raw html is shown as text

an html block or an inline tag is escaped, so `<script>` reaches the page as
`&lt;script&gt;`. an html block renders as a paragraph of its own text. cmark
drops raw html in its safe mode and writes `<!-- raw HTML omitted -->`.
showing the text serves a person who typed a `<` without meaning a tag, and
the escaped text cannot run either way.

### link and image destinations are checked

the destination goes through `html.safe_url` after its character references
and backslash escapes are decoded, which is the form a browser acts on. a
check that ran before decoding would pass `javascript&colon;alert(1)` and
`java&#x09;script:`, both of which a browser decodes into a `javascript:` url.
`safe_url` accepts `http`, `https`, `mailto` and `tel` urls and relative ones,
and refuses every other scheme and any `//host` form. a refused destination
becomes the empty string, so the link renders as `<a href="">` and the image
as `<img src="">`: the text stays, and nothing navigates or loads. `data:`
urls are refused for images as well as links, `data:image/png` included; serve
images from a url instead.

extended autolinks pass the same check. they only make `http:`, `https:`,
`ftp:`, `mailto:` and `xmpp:` urls, so a bare `javascript:` is never linked.
the check refuses `ftp:` and `xmpp:`, and those links render with `href=""`.

### text and attributes are escaped

text, titles, alt text and the language class of a code block are escaped with `html.escape`'s five characters. a
destination is percent-encoded the way cmark does it, with `&` and `'` written
as entities. a footnote label goes into `id` and `href` attributes encoded the
same way, as cmark-gfm does, so `[^"><script>]` becomes `fn-%22%3E%3Cscript%3E`.
a table cell cannot close the table: a pipe only splits cells, and a cell's
content is inline markup, escaped like any other.

the check runs where the events are made, not in the html renderer, so a
renderer of your own (see below) never receives a refused url, and receives
raw html only when the options allow it, already through the gfm tag filter.

## options

`options()` returns the defaults. each builder method returns a changed copy
and leaves the receiver alone, so a shared base can be specialized:

```pith
trusted := markdown.options().allow_html(true)
html := markdown.render(readme_text, trusted)
```

| method | default | effect |
| --- | --- | --- |
| `allow_html(Bool)` | `false` | pass raw html through. only for text you would put in a template yourself |
| `allow_unsafe_urls(Bool)` | `false` | skip the url check. the spec tests need it for schemes such as `irc:`; a page that shows untrusted text should never set it |
| `gfm(Bool)` | `true` | the github extensions: tables, strikethrough, task lists, extended autolinks, footnotes, and the tag filter on raw html |
| `heading_ids(Bool)` | `false` | give each heading an `id` made from its text, as github does (see [heading ids](#heading-ids-and-tables-of-contents)) |
| `heading_anchors(Bool)` | `false` | also start each heading with a link to itself. turns heading ids on |
| `code_highlighter(hook)` | none | write each code block through `hook` (see [highlighting code](#highlighting-code)). what the hook returns is trusted like raw html |

the escape hatches are spelled out as method names, so
`grep -rn 'allow_html(true)\|allow_unsafe_urls(true)\|code_highlighter('`
lists every place a program trusts its markdown or the code that renders it.

## events

`markdown.events(text, opts)` returns an iterator of `Event` values, in the
style of rust's pulldown-cmark. the html renderer is built on it, and any other
output can be. this prints a document's headings:

```pith
import std.markdown as markdown
from std.markdown import Event, Tag

mut heading := 0
for event in markdown.events(source, markdown.options()):
    if let Event.Start(Tag.Heading(level, _)) = event:
        heading = level
    if let Event.End(Tag.Heading(_, _)) = event:
        heading = 0
    if let Event.Text(content) = event:
        if heading > 0:
            print("h{heading}: {content}")
```

a pattern can nest, as `Event.Start(Tag.Heading(level, _))` does, and it can
name the module instead of importing the enums: `markdown.Event.Text(x)`
matches the same events as `Event.Text(x)`.

```pith
pub enum Event:
    Start(Tag)
    End(Tag)
    Text(String)        # text, never empty; adjacent text arrives merged
    Code(String)        # a code span
    Html(String)        # an html block, only with allow_html(true)
    InlineHtml(String)  # inline html, only with allow_html(true)
    SoftBreak
    HardBreak
    Rule                # a thematic break
    TaskMarker(Bool)    # a task list item's checkbox, true when checked
    FootnoteReference(String)  # the label of the footnote's definition

pub enum Tag:
    Paragraph
    Heading(Int, String)      # the level, 1 to 6, and the id ("" without heading ids)
    BlockQuote
    CodeBlock(String)         # the info string, "" for none
    List(Int?, Bool)          # the start number (none for a bullet list), tight
    Item
    Emphasis
    Strong
    Link(String, String)      # destination, title
    Image(String, String)     # source, title
    Table(List[Alignment])    # each column's alignment
    TableHead                 # the header row
    TableRow                  # a body row
    TableCell(Alignment)      # a cell and its column's alignment
    Strikethrough
    FootnoteDefinition(String)  # the label as the definition writes it

pub enum Alignment:
    None
    Left
    Center
    Right
```

how the stream is shaped:

- `End` carries the same tag as its `Start`.
- a code block's code is one `Text` event between `Start(CodeBlock(info))` and
  `End`, with no `Text` at all when the block is empty.
- inside a tight list, the paragraphs directly in an item produce no
  `Start(Paragraph)` and `End(Paragraph)`, the same choice the html output
  makes.
- without `allow_html(true)`, raw html arrives as `Text`: an html block as a
  paragraph, inline html merged into the text around it.
- an image's alt text is the events between its `Start` and `End`.
- a table is `Start(Table)`, then `Start(TableHead)` holding one `TableCell`
  per column, then a `TableRow` per body row, each also with one `TableCell`
  per column, then `End(Table)`. a cell holds inline events.
- a task list item's `TaskMarker` comes right after its `Start(Item)`.
- footnote definitions come after the rest of the document, each between
  `Start(FootnoteDefinition(label))` and `End`, in the order of their first
  `FootnoteReference`. a `FootnoteReference` always has its definition in the
  stream; a reference with no definition arrives as text.
- extended autolinks are `Link` tags like any other.

the block structure is parsed when `events` is called, since a link may use a
reference defined at the end of the document. inline content is parsed one
paragraph or heading at a time as the iterator reaches it.

`line()` on the iterator is the line, counted from 1, that the block of the
last event starts on: a paragraph's first line, a list item's marker, a fenced
code block's opening fence. an inline event reports its paragraph's line.
`pith doc --check` finds each example's line this way:

```pith
stream := markdown.events(source, markdown.options())
for event in stream:
    if let Event.Start(opened) = event:
        if let Tag.CodeBlock(info) = opened:
            print("a block of {info} code at line {stream.line()}")
```

## renderers

`Renderer` is the interface a backend implements:

```pith
pub interface Renderer:
    fn event(event: Event)
    fn finish() -> String
```

`markdown.render_with(text, opts, renderer)` feeds a renderer every event in
order and returns what its `finish` returns. `markdown.html_renderer()` is the
html one, and `render` and `to_html` are `render_with` over it. it numbers
footnotes from the `FootnoteReference` events as they arrive, puts the links
back to the references inside the last paragraph of each definition, and
closes the footnote section in `finish`. a renderer that
measures a post's text:

```pith
from std.markdown import Event, Renderer

struct TextLength:
    total: Int

impl Renderer for TextLength:
    fn event(event: Event):
        if let Event.Text(content) = event:
            self.total += content.len()

    fn finish() -> String:
        return "{self.total}"

length := markdown.render_with(post, markdown.options(), TextLength(total: 0))
```

## highlighting code

`options().code_highlighter(hook)` makes the html renderer write each code
block through a function. the hook gets the block's language, which is the
first word of its info string, and its code. it returns the html for the whole
block, or `none` to have the block written as usual. `std.pith.highlight` has
one for pith:

```pith
import std.markdown as markdown
import std.pith.highlight as highlight

opts := markdown.options().code_highlighter(highlight.code_block)
page := markdown.render("```pith\nx := 1\n```\n", opts)
# "<pre><code class=\"language-pith\"><span class=\"pith-var\">x</span> ..."
```

`code_block` takes the blocks whose language is `pith`, colors them with the
compiler's own lexer, and escapes all of the code, so nothing in a block can
close the `<pre>` or open a tag. for any other language it returns `none`, and
those blocks are escaped as before. it writes the classes `pith-kw`,
`pith-str` and the rest, and `highlight.STYLESHEET` is a light and a dark theme
for them.

the renderer writes what a hook returns into the page unescaped. that makes a
hook a trust boundary of the same kind as `allow_html(true)`. a hook of your
own has to escape the code itself, and the grep under [options](#options)
finds every program that sets one. `std.markdown` does not import the
highlighter, so a program that renders markdown without one does not compile
the lexer.

## heading ids and tables of contents

`options().heading_ids(true)` gives each heading an `id` made from its text,
by the rule github uses for a readme:

```pith
markdown.render("# Hello, World!\n\n## Usage", markdown.options().heading_ids(true))
# "<h1 id=\"hello-world\">Hello, World!</h1>\n<h2 id=\"usage\">Usage</h2>\n"
```

- the text is the heading's text and code spans with the markup dropped, so
  ``## Set *up* `now` `` reads `Set up now`.
- letters are lowercased (unicode simple case folding), each space becomes
  `-`, and everything that is not a letter, a mark, a number, `-` or `_` is
  dropped. `## C++ & Rust` becomes `c--rust` and `# Café au lait` becomes
  `café-au-lait`.
- a heading with nothing left becomes `section`.
- an id is unique within its document. the second `## Usage` is `usage-1` and
  the third `usage-2`, and a heading whose own text is `Usage 1` after them
  becomes `usage-1-1`, never a second `usage-1`.

an id holds only letters, marks, numbers, `-` and `_`, so no quote, `<`, `&`
or space can end the attribute, and it is escaped when written all the same.
the id is part of the event stream: `Tag.Heading(level, id)` carries it, and
it is `""` when ids are off, so a renderer of your own gets the same ids.

`heading_anchors(true)` also starts each heading with a link to itself, which
turns ids on:

```html
<h2 id="usage"><a class="anchor" href="#usage" aria-hidden="true" tabindex="-1">#</a>Usage</h2>
```

the link is kept out of the tab order and away from screen readers, since the
heading already says what it is. style `.anchor` to show it on hover.

ids come from what the author wrote. on a page that shows text from several
people, an id can repeat one the page already uses, or name a global that a
script reads, since an element with an id is reachable as `window.<id>`. leave
heading ids off for that kind of text, which is the default.

`markdown.toc(source, opts)` lists the headings for a table of contents:

```pith
for heading in markdown.toc(source, markdown.options()):
    print("{heading.level} {heading.text} #{heading.id}")
```

each `Heading` has `level`, `text` (the plain text, trimmed) and `id`. the ids
are the ones `render` gives the same headings with the same options, so a link
to `#id` reaches its heading. `toc` makes ids whether or not the options ask
for them.

## front matter

`markdown.split_front_matter(source)` splits a block of metadata off the start
of a document, yaml between `---` lines or toml between `+++` lines:

```pith
split := markdown.split_front_matter("---\ntitle: Hello\n---\n# Hello\n")!
# split.format is "yaml", split.data is "title: Hello\n", split.body is "# Hello\n"
```

`format` is `"yaml"`, `"toml"` or `""` when there is no front matter, `data`
is the text between the fences, and `body` is the rest. parsing `data` is left
to `std.yaml` or `std.toml`.

- the opening fence is the first line, after an optional byte order mark. the
  closing fence is the next line that holds the same fence. either may have
  trailing spaces, and lines may end in `\n` or `\r\n`.
- a first line that opens a block no later line closes is not front matter.
  `format` is `""` and the whole source is the body, where commonmark reads
  the `---` as a thematic break.
- the data may be at most `MAX_FRONT_MATTER_BYTES`, 64 KiB, and a split with
  more fails. a document whose stray `---` lines happen to pair up therefore
  never reaches a yaml parser whole.

## in templates

`std.template` writes a value as markdown with `<%md path %>`:

```pith
page := template.compile("<article><%md post.body %></article>")!
```

the html is what `markdown.to_html` writes, so raw html in the value is shown
as text and every url is checked. a template has no way to let raw html
through, so a `grep -rn '<%raw'` audit can skip `<%md`. text that should keep
its raw html is rendered in pith with `markdown.options().allow_html(true)` and
written with `<%raw %>`, where the audit finds it.

the html is block content, so compile refuses `<%md %>` anywhere but element
text. in an attribute value, a tag, a comment, `<script>`, `<style>`,
`<title>` or `<textarea>` the template fails to compile, the way a misplaced
`<%= %>` does. `page.heading_ids(true)` and `page.heading_anchors(true)` on a
compiled template turn on heading ids and anchors for its `<%md %>` tags.
[docs/html.md](html.md) covers the rest of the template language.

`page.code_highlighter(hook)` writes the code blocks of a template's `<%md %>`
tags through a hook, as [highlighting code](#highlighting-code) describes:

```pith
page := template.compile("<article><%md post.body %></article>")!
highlighted := page.code_highlighter(highlight.code_block)
```

a template trusts its hook's html the way it trusts `<%raw %>`, so the `<%raw`
audit looks for `code_highlighter(` as well. `<%md %>` highlights nothing until
a template is given a hook. `std.template` does not import the highlighter,
because every program that uses templates would then compile the lexer.

## pages in std.web

`std.web.pages` serves a directory of markdown files as html through a layout
template. `pages.handler(dir, layout)` answers a route whose last segment is
`*name`:

```pith
app := web.new().get("/docs/*path", pages.handler("docs", layout))
```

`GET /docs/guide/setup` renders `docs/guide/setup.md`, and a path that names a
directory renders its `index.md`. a directory's page lives at its path with a
trailing slash and any other page without one, as with net/http's
`FileServer`: `/docs` gets a 301 to `/docs/`, and `/docs/guide/setup/` one to
`/docs/guide/setup`. a relative link in a page then resolves against the
directory its file is in, the same as when the markdown is read on disk.

the layout is rendered with this context:

| key | value |
| --- | --- |
| each front matter key | its value from the yaml or toml, nested maps and lists included |
| `title` | the front matter's title, or else the text of the first heading |
| `content` | the markdown body, for `<%md content %>` |
| `toc` | the headings, each with `level`, `text` and `id` |

`content` and `toc` are set after the front matter, so the front matter cannot
replace them. the body reaches the page through `<%md %>`, which renders it
with the safe defaults and with heading ids, so a layout needs no `<%raw %>`
and nothing in a markdown file can write raw html or a `javascript:` url. pass
`layout.heading_anchors(true)` for heading anchors.

pith code blocks are highlighted with `highlight.code_block`, unless the layout
already has a code highlighter of its own (`layout.highlights_code()`). the
spans carry classes, so the page needs a stylesheet for them. serve
`highlight.STYLESHEET` and link it from the layout, as the site below does.

what it serves:

- only markdown. a page is found by adding `.md` or `/index.md` to the path,
  so a request cannot name any other file.
- nothing outside the directory. a `..` segment is redirected to the clean
  path before the route runs, as it is for static files. a segment that still
  starts with `.`, such as a `..` behind an encoded slash or a hidden file,
  gets 404, and a NUL byte gets 400. symbolic links are followed, as they are
  for static files.
- at most `pages.MAX_PAGE_BYTES`, 1 MiB, of markdown. a larger file, front
  matter that does not parse, or a layout that fails to render gets 500, and
  the reason goes to the log.
- `Last-Modified`, a strong `ETag`, and 304 and 412 for conditional requests,
  from `http.serve_content`. the time is the later of the file's modification
  time and the time the handler was made, because a new layout changes every
  page without touching its file.
- `Content-Type: text/html; charset=utf-8`.

each request reads and renders its page, so there is no cache to go stale.
served this way on the development machine, this file (10 KB) takes about
0.27 ms a request and `docs/web.md` (55 KB) about 1.4 ms.

`std.toml` does not parse dates yet, so toml front matter with a bare date
fails to parse. quote the date or use yaml.

## a docs site in twenty-five lines

`layout.html`:

```html
<!doctype html>
<meta charset="utf-8">
<title><%= title %></title>
<link rel="stylesheet" href="/pith.css">
<nav><% for h in toc %><a class="h<%= h.level %>" href="#<%= h.id %>"><%= h.text %></a>
<% end %></nav>
<main><%md content %></main>
```

`site.pith`:

```pith
import std.net.http as http
import std.pith.highlight as highlight
import std.template as template
import std.web as web
import std.web.pages as pages

# the theme for the classes of highlighted pith code
fn stylesheet(req: web.Request) -> http.HttpResponse:
    return http.response(200).with_body_text(highlight.STYLESHEET).header("Content-Type", "text/css; charset=utf-8")

fn main() -> Int!:
    layout := template.load("layout.html") catch:
        print("layout.html does not compile")
        return 1
    app := web.new().get("/pith.css", stylesheet).get("/*path", pages.handler("docs", layout.heading_anchors(true)))
    app.listen("0.0.0.0", 8080)!
    return 0
```

with `docs/index.md` and `docs/guide/setup.md` in place, `/` serves the first
and `/guide/setup` the second, each with its own table of contents and its
pith code highlighted. the routes `web.new()` adds, `/healthz` and `/metrics`,
come first and keep working.

## in the terminal

`std.markdown.terminal` lays a document out for a terminal. headings, lists,
block quotes and tables become text, styles become ansi codes, pith code is
highlighted, and every line is wrapped to the width:

```pith
import std.markdown as markdown
import std.markdown.terminal as terminal

print(terminal.render(source, 80))
plain := markdown.render_with(source, markdown.options(), terminal.renderer(80).colors(false))
```

without colors, part of this file at 40 columns reads:

```text
## options

`options()` returns the defaults. each
builder method returns a changed copy
and leaves the receiver alone, so a
shared base can be specialized:

    trusted := markdown.options().allow_
    html(true)
```

- widths are counted in terminal columns with `std.text.width`, so 日 takes
  two and a family emoji two. text wraps between words, a word wider than a
  line breaks between graphemes, and a line of code breaks at the width.
- a heading keeps its `#` marker. emphasis is italic, strong text bold,
  strikethrough struck, and a code span has a color of its own. a link is
  underlined, with its url after it in parentheses unless the text is the url.
- code blocks are indented four columns, and a pith block is highlighted
  from `highlight.spans`.
- lists keep their bullets or numbers, task items show `[ ]` and `[x]`, block
  quotes have a `│` bar, and footnotes follow a rule at the end.
- each table column is as wide as its widest cell. when the table is wider
  than the terminal, the widest columns are cut to the largest width that fits
  and their cells wrap. a table with so many columns that one column each does
  not fit runs past the width.
- `renderer(columns)` writes styles unless NO_COLOR is set (see
  `term.no_color()`), and `colors(false)` or `colors(true)` overrides that.
  without colors, code spans keep their backticks and strikethrough keeps its
  tildes.

a terminal acts on the escape sequences it reads, so the renderer shows the
control characters of the document in caret notation (`^[` for escape) and c1
controls as U+FFFD. the only escape sequences in its output are the styles it
writes, and each line closes its own.

`pith doc <topic>` shows a guide this way, so `pith doc markdown` shows this
file, and `pith doc <file.md>` shows any markdown file. on a terminal the text
takes the terminal's width, up to 100 columns, and its colors, and a guide
longer than the screen goes through `$PAGER`, or through `less` with `LESS=FRX`
when `PAGER` is unset. sent anywhere else, it is plain text 80 columns wide.
the guides come from the `docs/` directory of the checkout or the install the
command belongs to, and the release packages include it.

## limits

parsing takes time linear in the input on every input. the emphasis and link
rules use the bounded algorithms of the spec's appendix and of cmark, and each
construct that makes a naive parser quadratic is covered by
`tests/cases/test_markdown_linear.pith`, which renders 50k to 160k repetitions
of it under a time bound:

- nested and mismatched emphasis (`*a **a *a ...`, `*a_ ...`, runs whose
  lengths are multiples of three);
- brackets that never close, closers with no opener, `[ (](`, `![[]()` and
  links left open (`[a](b`, `[a](<b`, `[a](b()`, `[a](b "`);
- backtick runs of every length, unclosed comments, processing instructions,
  declarations, cdata and attribute values, and runs of `<`;
- deeply nested block quotes and lists, and blank lines under a deep list;
- many reference definitions and many references;
- tables with many rows, with many columns, and with a wide header over rows
  of one cell, cmark-gfm's table case, and paragraphs of rows before a
  delimiter row;
- runs of tildes, strikethrough openers or closers alone, nested and
  unbalanced strikethrough, and strikethrough mixed with emphasis;
- many footnotes, many references to one footnote, labels that nest, and
  definitions nested on one line;
- autolink candidates with long trailing punctuation or unbalanced
  parentheses, `www.` repeated, domains with underscores, long schemes, email
  candidates, `mailto:` repeated, and disallowed raw html tags.

these bounds make that hold:

- `MAX_NESTING` is 32: block quotes and list items nest at most this deep, and
  a `>` or list marker that would open a container past it is read as text.
  each line walks the open containers, so without the cap a few thousand
  nested list markers followed by blank lines would take quadratic time.
- parentheses nest at most 32 deep in a link destination, as in cmark, which
  bounds how far a failed destination scan can reach.
- a footnote definition counts toward `MAX_NESTING` the way a block quote
  does, and a footnote label is at most 999 bytes, as a link label is, so a
  run of nested `[^` brackets cannot make each `]` scan the whole run.
- an extended autolink's domain is rejected for an underscore in its last
  two segments only when it has ten segments or fewer, the cap cmark-gfm
  added so that a run of failing candidates stays linear.

nothing in the parser, the event walk or the html renderer recurses on the
nesting of the input, so deep emphasis or links cannot exhaust the stack.

the output is linear in the input too. each construct writes a bounded amount
of markup per input byte, with two exceptions, and both are budgeted. a
reference link repeats its definition's destination and title. as in cmark,
once those copies add up to the size of the input (or 100 KB, whichever is
larger), further references render as the text they were written as, so a
200 KB destination referenced 10,000 times expands once. a table row shorter
than its header is padded with empty cells, and those cells have a budget of
the same size, counted in cells. without it, a header of 5,000 columns over
50,000 rows of one cell would write 250 million cells. a row the budget
cannot pad ends the table and starts a paragraph, as a row past cmark-gfm's
fixed limit does.

there is no input size cap and no error path. every input renders to html;
bound the size where the input arrives, as `std.net.http` does for request
bodies.

## conformance

`tests/markdown/commonmark-0.31.2.json` holds the spec's examples, and
`tests/cases/test_markdown_commonmark.pith` renders each one with raw html and
every url allowed and gfm off, and prints `commonmark: 652/652 passed`. the
comparison is exact once `&#39;` and `&#x27;` are read as `'`. the in-file
tests in `std/markdown.pith` cover the api and the safety rules, the
`javascript:` entity bypasses included. the runner also renders every example
with gfm on and lists the 11 whose html the extensions change.

`tests/cases/test_markdown_gfm.pith` runs the extension examples of the gfm
spec and cmark-gfm's extension tests (see `tests/markdown/README.md`) with gfm
on and raw html and every url allowed, and prints `gfm: 54/54 passed`.

`bench/markdown_render.pith` renders the repository's docs, repeated to 10 MB,
and reports throughput: about 64 MB/s with gfm on and 73 MB/s with it off on
the development machine. the docs hold many tables, and each table cell is
parsed on its own.

## what comes next

later work builds on the event stream and the options above:

- highlighters for languages other than pith, through the same hook.
- links in the terminal as osc 8 hyperlinks, once `std.term` can write them.
  until then a link's url follows its text.
- source positions for inline events. `line()` gives the line of an event's
  block, which is as precise as `pith doc --check` needs.
