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
| `heading_ids(Bool)` | `false` | reserved for `id` attributes on headings. no effect yet |

both escape hatches are spelled out as method names, so
`grep -rn 'allow_html(true)\|allow_unsafe_urls(true)'` lists every place a
program trusts its markdown.

## events

`markdown.events(text, opts)` returns an iterator of `Event` values, in the
style of rust's pulldown-cmark. the html renderer is built on it, and any other
output can be. this prints a document's headings:

```pith
import std.markdown as markdown
from std.markdown import Event, Tag

mut heading := 0
for event in markdown.events(source, markdown.options()):
    if let Event.Start(opened) = event:
        if let Tag.Heading(level) = opened:
            heading = level
    if let Event.End(closed) = event:
        if let Tag.Heading(level) = closed:
            heading = 0
    if let Event.Text(content) = event:
        if heading > 0:
            print("h{heading}: {content}")
```

import the enum names with `from std.markdown import ...`: a pattern cannot
name a module (`markdown.Event.Text(x)` does not parse), and a variant
pattern nested in another (`Event.Start(Tag.Heading(level))`) passes the
checker but does not compile yet, so match one level at a time.

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
    Heading(Int)              # the level, 1 to 6
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

- `heading_ids(true)`, and use from `std.template` and `std.web`, with
  `tools/sitegen` moving to this module.
- a terminal renderer that writes ansi styles, and code highlighting hooks for
  the html and terminal renderers.
