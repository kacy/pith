# Markdown

`std.markdown` parses CommonMark and renders it to HTML. It implements all of
CommonMark 0.31.2 and passes the spec's 652 examples. It also implements the
GitHub Flavored Markdown extensions, which pass the 54 extension examples of
the GFM spec and of cmark-gfm's tests. The defaults suit text someone else
wrote, such as a comment, an issue body or a profile field, so the one-line
call is the safe one:

```pith
import std.markdown as markdown

page := markdown.to_html("# hello\n\n*world* <script>alert(1)</script>")
# "<h1>hello</h1>\n<p><em>world</em> &lt;script&gt;alert(1)&lt;/script&gt;</p>\n"
```

## What it supports

It supports every block and inline construct in the spec:

- Blocks: ATX and setext headings, thematic breaks, indented and fenced code,
  the seven kinds of HTML block, link reference definitions, paragraphs, block
  quotes, and bullet and ordered lists with the tight and loose rules.
- Inlines: backslash escapes, entity and numeric character references (the
  full HTML5 table of named entities), code spans, emphasis and strong
  emphasis by the delimiter-run rules, inline links and images, full,
  collapsed and shortcut reference links, URI and email autolinks, raw HTML,
  and hard and soft line breaks.
- Tabs expand to the next multiple of four columns where the spec says so,
  including a tab only partly used by a list item's indentation.
- Link labels match after Unicode case folding, so `[ẞ]` finds `[SS]: /url`.

The HTML is cmark's, byte for byte, with one difference: a `'` is written as
`&#39;`, the escape `std.html` uses, where cmark writes it bare in text and as
`&#x27;` in a URL.

### GitHub Flavored Markdown

`gfm(true)`, the default, adds the GitHub extensions as cmark-gfm implements
them, with cmark-gfm's HTML:

- Tables. A delimiter row such as `| :-- | --: |` turns the line above it into
  a header row when the two have the same number of cells, and the lines after
  it into body rows until a blank line or another block. Cells hold inline
  content, a `\|` is a pipe inside a cell (in a code span too), and the colons
  set each column's alignment. A row shorter than the header is padded with
  empty cells, and a longer one is cut.
- Strikethrough with one or two tildes, `~a~` or `~~a~~`.
- Task list items. `[ ]`, `[x]` or `[X]` and a space at the start of an item's
  first paragraph become a disabled checkbox.
- Extended autolinks: `www.` addresses, `http://`, `https://` and `ftp://`
  URLs, and email addresses become links without angle brackets. Trailing
  punctuation, a `)` with no `(` in the link and an entity-like `&name;` at
  the end are left out of the link. None are looked for inside a link's text.
- Footnotes. `[^label]` refers to a `[^label]: text` definition. The
  definitions are written in a section after the document, numbered in the
  order of their first reference, each with a link back to every reference.
  A definition holds blocks like a list item does, with its later lines
  indented four columns. An unreferenced definition is dropped, a reference
  with no definition stays text, and when two definitions share a label the
  first wins.
- With `allow_html(true)`, the tag filter: the `<` of `<title>`,
  `<textarea>`, `<style>`, `<xmp>`, `<iframe>`, `<noembed>`, `<noframes>`,
  `<script>` and `<plaintext>`, opening or closing, is written as `&lt;`.

`gfm(false)` gives plain CommonMark. The extensions change 11 of the
CommonMark spec's examples: six HTML blocks with `<script>`, `<style>` or
`<textarea>` pass through the tag filter, and five bare URLs and email
addresses become links.

It differs from cmark-gfm in these ways:

- A task list marker counts in any list item's first paragraph. cmark-gfm
  looks for it only when the list marker starts the line, so it misses one in
  `> - [ ] a`.
- A footnote label matches the way a link label does: on the text as written,
  after case folding, and at most 999 bytes long. A reference with no
  definition is parsed as ordinary text, so `[^*a*]` shows emphasis where
  cmark-gfm shows the characters.
- Footnotes are numbered as the event stream reaches their references, and
  definitions come after the document. A footnote referenced only from inside
  another definition is numbered after those the document references, where
  cmark-gfm numbers in source order.
- Non-ASCII punctuation counts as part of an autolink's domain, where
  cmark-gfm stops the domain at it. This matters only for the rule on
  underscores and for a URL whose host starts with such a character.
- The empty cells that pad short rows share a budget the size of the input
  (see the limits below), where cmark-gfm allows a fixed number per table.

## The safe defaults

Three rules hold unless the options turn them off.

### Raw HTML is shown as text

An HTML block or an inline tag is escaped, so `<script>` reaches the page as
`&lt;script&gt;`. An HTML block renders as a paragraph of its own text. cmark
drops raw HTML in its safe mode and writes `<!-- raw HTML omitted -->`.
Showing the text serves a person who typed a `<` without meaning a tag, and
the escaped text cannot run in either case.

### Link and image destinations are checked

The destination goes through `html.safe_url` after its character references
and backslash escapes are decoded, which is the form a browser acts on. A
check that ran before decoding would pass `javascript&colon;alert(1)` and
`java&#x09;script:`, both of which a browser decodes into a `javascript:` URL.
`safe_url` accepts `http`, `https`, `mailto` and `tel` URLs and relative ones,
and refuses every other scheme and any `//host` form. A refused destination
becomes the empty string, so the link renders as `<a href="">` and the image
as `<img src="">`: the text stays, and nothing navigates or loads. `data:`
URLs are refused for images as well as links, `data:image/png` included; serve
images from a URL instead.

Extended autolinks pass the same check. They only make `http:`, `https:`,
`ftp:`, `mailto:` and `xmpp:` URLs, so a bare `javascript:` is never linked.
The check refuses `ftp:` and `xmpp:`, and those links render with `href=""`.

### Text and attributes are escaped

Text, titles, alt text and the language class of a code block are escaped
with `html.escape`'s five characters. A destination is percent-encoded the way
cmark does it, with `&` and `'` written as entities. A footnote label goes
into `id` and `href` attributes encoded the same way, as cmark-gfm does, so
`[^"><script>]` becomes `fn-%22%3E%3Cscript%3E`. A table cell cannot close
the table: a pipe only splits cells, and a cell's content is inline markup,
escaped like any other.

The check runs where the events are made, before any renderer sees them, so a
renderer of your own (see below) never receives a refused URL, and receives
raw HTML only when the options allow it, already through the GFM tag filter.

## Options

`options()` returns the defaults. Each builder method returns a changed copy
and leaves the receiver alone, so a shared base can be specialized:

```pith
trusted := markdown.options().allow_html(true)
html := markdown.render(readme_text, trusted)
```

| Method | Default | Effect |
| --- | --- | --- |
| `allow_html(Bool)` | `false` | Pass raw HTML through. Only for text you would put in a template yourself |
| `allow_unsafe_urls(Bool)` | `false` | Skip the URL check. The spec tests need it for schemes such as `irc:`; a page that shows untrusted text should never set it |
| `gfm(Bool)` | `true` | The GitHub extensions: tables, strikethrough, task lists, extended autolinks, footnotes, and the tag filter on raw HTML |
| `heading_ids(Bool)` | `false` | Give each heading an `id` made from its text, as GitHub does (see [heading IDs](#heading-ids-and-tables-of-contents)) |
| `heading_anchors(Bool)` | `false` | Also start each heading with a link to itself. Turns heading IDs on |
| `code_highlighter(hook)` | none | Write each code block through `hook` (see [highlighting code](#highlighting-code)). What the hook returns is trusted like raw HTML |

The escape hatches are spelled out as method names, so
`grep -rn 'allow_html(true)\|allow_unsafe_urls(true)\|code_highlighter('`
lists every place a program trusts its Markdown or the code that renders it.

## Events

`markdown.events(text, opts)` returns an iterator of `Event` values, in the
style of Rust's pulldown-cmark. The HTML renderer is built on it, and any other
output can be. This prints a document's headings:

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

A pattern can nest, as `Event.Start(Tag.Heading(level, _))` does, and it can
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

The stream has this shape:

- `End` carries the same tag as its `Start`.
- A code block's code is one `Text` event between `Start(CodeBlock(info))` and
  `End`, with no `Text` at all when the block is empty.
- Inside a tight list, the paragraphs directly in an item produce no
  `Start(Paragraph)` and `End(Paragraph)`, the same choice the HTML output
  makes.
- Without `allow_html(true)`, raw HTML arrives as `Text`: an HTML block as a
  paragraph, inline HTML merged into the text around it.
- An image's alt text is the events between its `Start` and `End`.
- A table is `Start(Table)`, then `Start(TableHead)` holding one `TableCell`
  per column, then a `TableRow` per body row, each also with one `TableCell`
  per column, then `End(Table)`. A cell holds inline events.
- A task list item's `TaskMarker` comes right after its `Start(Item)`.
- Footnote definitions come after the rest of the document, each between
  `Start(FootnoteDefinition(label))` and `End`, in the order of their first
  `FootnoteReference`. A `FootnoteReference` always has its definition in the
  stream; a reference with no definition arrives as text.
- Extended autolinks are `Link` tags like any other.

The block structure is parsed when `events` is called, since a link may use a
reference defined at the end of the document. Inline content is parsed one
paragraph or heading at a time as the iterator reaches it.

`line()` on the iterator is the line, counted from 1, that the block of the
last event starts on: a paragraph's first line, a list item's marker, a fenced
code block's opening fence. An inline event reports its paragraph's line.
`pith doc --check` finds each example's line this way:

```pith
stream := markdown.events(source, markdown.options())
for event in stream:
    if let Event.Start(opened) = event:
        if let Tag.CodeBlock(info) = opened:
            print("a block of {info} code at line {stream.line()}")
```

## Renderers

`Renderer` is the interface a backend implements:

```pith
pub interface Renderer:
    fn event(event: Event)
    fn finish() -> String
```

`markdown.render_with(text, opts, renderer)` feeds a renderer every event in
order and returns what its `finish` returns. `markdown.html_renderer()` is the
HTML one, and `render` and `to_html` are `render_with` over it. It numbers
footnotes from the `FootnoteReference` events as they arrive, puts the links
back to the references inside the last paragraph of each definition, and
closes the footnote section in `finish`. This renderer measures a post's text:

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

## Highlighting code

`options().code_highlighter(hook)` makes the HTML renderer write each code
block through a function. The hook gets the block's language, which is the
first word of its info string, and its code. It returns the HTML for the whole
block, or `none` to have the block written as usual. `std.pith.highlight` has
one for Pith:

```pith
import std.markdown as markdown
import std.pith.highlight as highlight

opts := markdown.options().code_highlighter(highlight.code_block)
page := markdown.render("```pith\nx := 1\n```\n", opts)
# "<pre><code class=\"language-pith\"><span class=\"pith-var\">x</span> ..."
```

`code_block` takes the blocks whose language is `pith`, colors them with the
compiler's own lexer, and escapes all of the code, so nothing in a block can
close the `<pre>` or open a tag. For any other language it returns `none`, and
those blocks are escaped as before. It writes the classes `pith-kw`,
`pith-str` and the rest, and `highlight.STYLESHEET` is a light and a dark theme
for them.

The renderer writes what a hook returns into the page unescaped. That makes a
hook a trust boundary of the same kind as `allow_html(true)`. A hook of your
own has to escape the code itself, and the grep under [options](#options)
finds every program that sets one. `std.markdown` does not import the
highlighter, so a program that renders Markdown without one does not compile
the lexer.

## Heading IDs and tables of contents

`options().heading_ids(true)` gives each heading an `id` made from its text,
by the rule GitHub uses for a README:

```pith
markdown.render("# Hello, World!\n\n## Usage", markdown.options().heading_ids(true))
# "<h1 id=\"hello-world\">Hello, World!</h1>\n<h2 id=\"usage\">Usage</h2>\n"
```

- The text is the heading's text and code spans with the markup dropped, so
  ``## Set *up* `now` `` reads `Set up now`.
- Letters are lowercased (Unicode simple case folding), each space becomes
  `-`, and everything that is not a letter, a mark, a number, `-` or `_` is
  dropped. `## C++ & Rust` becomes `c--rust` and `# Café au lait` becomes
  `café-au-lait`.
- A heading with nothing left becomes `section`.
- An ID is unique within its document. The second `## Usage` is `usage-1` and
  the third `usage-2`, and a heading whose own text is `Usage 1` after them
  becomes `usage-1-1`, never a second `usage-1`.

An ID holds only letters, marks, numbers, `-` and `_`, so no quote, `<`, `&`
or space can end the attribute, and it is escaped when written all the same.
The ID is part of the event stream: `Tag.Heading(level, id)` carries it, and
it is `""` when IDs are off, so a renderer of your own gets the same IDs.

`heading_anchors(true)` also starts each heading with a link to itself, which
turns IDs on:

```html
<h2 id="usage"><a class="anchor" href="#usage" aria-hidden="true" tabindex="-1">#</a>Usage</h2>
```

The link is kept out of the tab order and away from screen readers, since the
heading already says what it is. Style `.anchor` to show it on hover.

IDs come from what the author wrote. On a page that shows text from several
people, an ID can repeat one the page already uses, or name a global that a
script reads, since an element with an ID is reachable as `window.<id>`. Leave
heading IDs off for that kind of text, which is the default.

`markdown.toc(source, opts)` lists the headings for a table of contents:

```pith
for heading in markdown.toc(source, markdown.options()):
    print("{heading.level} {heading.text} #{heading.id}")
```

Each `Heading` has `level`, `text` (the plain text, trimmed) and `id`. The IDs
are the ones `render` gives the same headings with the same options, so a link
to `#id` reaches its heading. `toc` makes IDs whether or not the options ask
for them.

## Front matter

`markdown.split_front_matter(source)` splits a block of metadata off the start
of a document, YAML between `---` lines or TOML between `+++` lines:

```pith
split := markdown.split_front_matter("---\ntitle: Hello\n---\n# Hello\n")!
# split.format is "yaml", split.data is "title: Hello\n", split.body is "# Hello\n"
```

`format` is `"yaml"`, `"toml"` or `""` when there is no front matter, `data`
is the text between the fences, and `body` is the rest. Parsing `data` is left
to `std.yaml` or `std.toml`.

- The opening fence is the first line, after an optional byte order mark. The
  closing fence is the next line that holds the same fence. Either may have
  trailing spaces, and lines may end in `\n` or `\r\n`.
- A first line that opens a block no later line closes is not front matter.
  `format` is `""` and the whole source is the body, where CommonMark reads
  the `---` as a thematic break.
- The data may be at most `MAX_FRONT_MATTER_BYTES`, 64 KiB, and a split with
  more fails. A document whose stray `---` lines happen to pair up therefore
  never reaches a YAML parser whole.

## In templates

`std.template` writes a value as Markdown with `<%md path %>`:

```pith
page := template.compile("<article><%md post.body %></article>")!
```

The HTML is what `markdown.to_html` writes, so raw HTML in the value is shown
as text and every URL is checked. A template has no way to let raw HTML
through, so a `grep -rn '<%raw'` audit can skip `<%md`. Text that should keep
its raw HTML is rendered in Pith with `markdown.options().allow_html(true)` and
written with `<%raw %>`, where the audit finds it.

The HTML is block content, so compile refuses `<%md %>` anywhere but element
text. In an attribute value, a tag, a comment, `<script>`, `<style>`,
`<title>` or `<textarea>` the template fails to compile, the way a misplaced
`<%= %>` does. `page.heading_ids(true)` and `page.heading_anchors(true)` on a
compiled template turn on heading IDs and anchors for its `<%md %>` tags.
[docs/html.md](html.md) covers the rest of the template language.

`page.code_highlighter(hook)` writes the code blocks of a template's `<%md %>`
tags through a hook, as [highlighting code](#highlighting-code) describes:

```pith
page := template.compile("<article><%md post.body %></article>")!
highlighted := page.code_highlighter(highlight.code_block)
```

A template trusts its hook's HTML the way it trusts `<%raw %>`, so the `<%raw`
audit looks for `code_highlighter(` as well. `<%md %>` highlights nothing until
a template is given a hook. `std.template` does not import the highlighter,
because every program that uses templates would then compile the lexer.

## Pages in std.web

`std.web.pages` serves a directory of Markdown files as HTML through a layout
template. `pages.handler(dir, layout)` answers a route whose last segment is
`*name`:

```pith
app := web.new().get("/docs/*path", pages.handler("docs", layout))
```

`GET /docs/guide/setup` renders `docs/guide/setup.md`, and a path that names a
directory renders its `index.md`. A directory's page lives at its path with a
trailing slash and any other page without one, as with net/http's
`FileServer`: `/docs` gets a 301 to `/docs/`, and `/docs/guide/setup/` one to
`/docs/guide/setup`. A relative link in a page then resolves against the
directory its file is in, the same as when the Markdown is read on disk.

The layout is rendered with this context:

| Key | Value |
| --- | --- |
| Each front matter key | Its value from the YAML or TOML, nested maps and lists included |
| `title` | The front matter's title, or else the text of the first heading |
| `content` | The Markdown body, for `<%md content %>` |
| `toc` | The headings, each with `level`, `text` and `id` |

`content` and `toc` are set after the front matter, so the front matter cannot
replace them. The body reaches the page through `<%md %>`, which renders it
with the safe defaults and with heading IDs, so a layout needs no `<%raw %>`
and nothing in a Markdown file can write raw HTML or a `javascript:` URL. Pass
`layout.heading_anchors(true)` for heading anchors.

Pith code blocks are highlighted with `highlight.code_block`, unless the layout
already has a code highlighter of its own (`layout.highlights_code()`). The
spans carry classes, so the page needs a stylesheet for them. Serve
`highlight.STYLESHEET` and link it from the layout, as the site below does.

What it serves:

- Only Markdown. A page is found by adding `.md` or `/index.md` to the path,
  so a request cannot name any other file.
- Nothing outside the directory. A `..` segment is redirected to the clean
  path before the route runs, as it is for static files. A segment that still
  starts with `.`, such as a `..` behind an encoded slash or a hidden file,
  gets 404, and a NUL byte gets 400. Symbolic links are followed, as they are
  for static files.
- At most `pages.MAX_PAGE_BYTES`, 1 MiB, of Markdown. A larger file, front
  matter that does not parse, or a layout that fails to render gets 500, and
  the reason goes to the log.
- `Last-Modified`, a strong `ETag`, and 304 and 412 for conditional requests,
  from `http.serve_content`. The time is the later of the file's modification
  time and the time the handler was made, because a new layout changes every
  page without touching its file.
- `Content-Type: text/html; charset=utf-8`.

Each request reads and renders its page, so there is no cache to go stale.
Served this way on the development machine, this file (10 KB) takes about
0.27 ms a request and `docs/web.md` (55 KB) about 1.4 ms.

`std.toml` does not parse dates yet, so TOML front matter with a bare date
fails to parse. Quote the date or use YAML.

## A docs site in twenty-five lines

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

With `docs/index.md` and `docs/guide/setup.md` in place, `/` serves the first
and `/guide/setup` the second, each with its own table of contents and its
Pith code highlighted. The routes `web.new()` adds, `/healthz` and `/metrics`,
come first and keep working.

## In the terminal

`std.markdown.terminal` lays a document out for a terminal. Headings, lists,
block quotes and tables become text, styles become ANSI codes, Pith code is
highlighted, and every line is wrapped to the width:

```pith
import std.markdown as markdown
import std.markdown.terminal as terminal

print(terminal.render(source, 80))
plain := markdown.render_with(source, markdown.options(), terminal.renderer(80).colors(false))
```

Without colors, part of this file at 40 columns reads:

```text
## Options

`options()` returns the defaults. Each
builder method returns a changed copy
and leaves the receiver alone, so a
shared base can be specialized:

    trusted := markdown.options().allow_
    html(true)
```

- Widths are counted in terminal columns with `std.text.width`, so 日 takes
  two and a family emoji two. Text wraps between words, a word wider than a
  line breaks between graphemes, and a line of code breaks at the width.
- A heading keeps its `#` marker. Emphasis is italic, strong text bold,
  strikethrough struck, and a code span has a color of its own. A link is
  underlined, with its URL after it in parentheses unless the text is the URL.
- Code blocks are indented four columns, and a Pith block is highlighted
  from `highlight.spans`.
- Lists keep their bullets or numbers, task items show `[ ]` and `[x]`, block
  quotes have a `│` bar, and footnotes follow a rule at the end.
- Each table column is as wide as its widest cell. When the table is wider
  than the terminal, the widest columns are cut to the largest width that fits
  and their cells wrap. A table with so many columns that one column each does
  not fit runs past the width.
- `renderer(columns)` writes styles unless NO_COLOR is set (see
  `term.no_color()`), and `colors(false)` or `colors(true)` overrides that.
  Without colors, code spans keep their backticks and strikethrough keeps its
  tildes.

A terminal acts on the escape sequences it reads, so the renderer shows the
control characters of the document in caret notation (`^[` for escape) and C1
controls as U+FFFD. The only escape sequences in its output are the styles it
writes, and each line closes its own.

`pith doc <topic>` shows a guide this way, so `pith doc markdown` shows this
file, and `pith doc <file.md>` shows any Markdown file. On a terminal the text
takes the terminal's width, up to 100 columns, and its colors, and a guide
longer than the screen goes through `$PAGER`, or through `less` with `LESS=FRX`
when `PAGER` is unset. Sent anywhere else, it is plain text 80 columns wide.
The guides come from the `docs/` directory of the checkout or the install the
command belongs to, and the release packages include it.

## Limits

Parsing takes time linear in the input on every input. The emphasis and link
rules use the bounded algorithms of the spec's appendix and of cmark, and each
construct that makes a naive parser quadratic is covered by
`tests/cases/test_markdown_linear.pith`, which renders 50k to 160k repetitions
of it under a time bound:

- Nested and mismatched emphasis (`*a **a *a ...`, `*a_ ...`, runs whose
  lengths are multiples of three);
- Brackets that never close, closers with no opener, `[ (](`, `![[]()` and
  links left open (`[a](b`, `[a](<b`, `[a](b()`, `[a](b "`);
- Backtick runs of every length, unclosed comments, processing instructions,
  declarations, CDATA and attribute values, and runs of `<`;
- Deeply nested block quotes and lists, and blank lines under a deep list;
- Many reference definitions and many references;
- Tables with many rows, with many columns, and with a wide header over rows
  of one cell, cmark-gfm's table case, and paragraphs of rows before a
  delimiter row;
- Runs of tildes, strikethrough openers or closers alone, nested and
  unbalanced strikethrough, and strikethrough mixed with emphasis;
- Many footnotes, many references to one footnote, labels that nest, and
  definitions nested on one line;
- Autolink candidates with long trailing punctuation or unbalanced
  parentheses, `www.` repeated, domains with underscores, long schemes, email
  candidates, `mailto:` repeated, and disallowed raw HTML tags.

These bounds make that hold:

- `MAX_NESTING` is 32: block quotes and list items nest at most this deep, and
  a `>` or list marker that would open a container past it is read as text.
  Each line walks the open containers, so without the cap a few thousand
  nested list markers followed by blank lines would take quadratic time.
- Parentheses nest at most 32 deep in a link destination, as in cmark, which
  bounds how far a failed destination scan can reach.
- A footnote definition counts toward `MAX_NESTING` the way a block quote
  does, and a footnote label is at most 999 bytes, as a link label is, so a
  run of nested `[^` brackets cannot make each `]` scan the whole run.
- An extended autolink's domain is rejected for an underscore in its last
  two segments only when it has ten segments or fewer, the cap cmark-gfm
  added so that a run of failing candidates stays linear.

Nothing in the parser, the event walk or the HTML renderer recurses on the
nesting of the input, so deep emphasis or links cannot exhaust the stack.

The output is linear in the input too. Each construct writes a bounded amount
of markup per input byte, with two exceptions, and both are budgeted. A
reference link repeats its definition's destination and title. As in cmark,
once those copies add up to the size of the input (or 100 KB, whichever is
larger), further references render as the text they were written as, so a
200 KB destination referenced 10,000 times expands once. A table row shorter
than its header is padded with empty cells, and those cells have a budget of
the same size, counted in cells. Without it, a header of 5,000 columns over
50,000 rows of one cell would write 250 million cells. A row the budget
cannot pad ends the table and starts a paragraph, as a row past cmark-gfm's
fixed limit does.

There is no input size cap and no error path. Every input renders to HTML;
bound the size where the input arrives, as `std.net.http` does for request
bodies.

## Conformance

`tests/markdown/commonmark-0.31.2.json` holds the spec's examples, and
`tests/cases/test_markdown_commonmark.pith` renders each one with raw HTML and
every URL allowed and GFM off, and prints `commonmark: 652/652 passed`. The
comparison is exact once `&#39;` and `&#x27;` are read as `'`. The in-file
tests in `std/markdown.pith` cover the API and the safety rules, the
`javascript:` entity bypasses included. The runner also renders every example
with GFM on and lists the 11 whose HTML the extensions change.

`tests/cases/test_markdown_gfm.pith` runs the extension examples of the GFM
spec and cmark-gfm's extension tests (see `tests/markdown/README.md`) with GFM
on and raw HTML and every URL allowed, and prints `gfm: 54/54 passed`.

`bench/markdown_render.pith` renders the repository's docs, repeated to 10 MB,
and reports throughput: about 64 MB/s with GFM on and 73 MB/s with it off on
the development machine. The docs hold many tables, and each table cell is
parsed on its own.

## What comes next

Later work builds on the event stream and the options above:

- Highlighters for languages other than Pith, through the same hook.
- Links in the terminal as OSC 8 hyperlinks, once `std.term` can write them.
  Until then a link's URL follows its text.
- Source positions for inline events. `line()` gives the line of an event's
  block, which is as precise as `pith doc --check` needs.
