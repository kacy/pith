# markdown

`std.markdown` parses commonmark and renders it to html. it implements all of
commonmark 0.31.2 and passes the spec's 652 examples. the defaults are set for
rendering text someone else wrote, such as a comment, an issue body or a
profile field, so the one-line call is the safe one:

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

### text and attributes are escaped

text, titles, alt text and the language class of a code block are escaped with `html.escape`'s five characters. a
destination is percent-encoded the way cmark does it, with `&` and `'` written
as entities.

the check runs where the events are made, not in the html renderer, so a
renderer of your own (see below) never receives a refused url, and receives
raw html only when the options allow it.

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
| `gfm(Bool)` | `true` | the github extensions. stored now; tables, strikethrough, task lists and extended autolinks are not parsed yet |
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
html one, and `render` and `to_html` are `render_with` over it. a renderer that
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
- many reference definitions and many references.

two bounds make that hold:

- `MAX_NESTING` is 32: block quotes and list items nest at most this deep, and
  a `>` or list marker that would open a container past it is read as text.
  each line walks the open containers, so without the cap a few thousand
  nested list markers followed by blank lines would take quadratic time.
- parentheses nest at most 32 deep in a link destination, as in cmark, which
  bounds how far a failed destination scan can reach.

nothing in the parser, the event walk or the html renderer recurses on the
nesting of the input, so deep emphasis or links cannot exhaust the stack.

the output is linear in the input too. each construct writes a bounded amount
of markup per input byte, except a reference link, which repeats its
definition's destination and title. as in cmark, those copies are budgeted:
once they add up to the size of the input (or 100 KB, whichever is larger),
further references render as the text they were written as. a 200 KB
destination referenced 10,000 times expands once.

there is no input size cap and no error path. every input renders to html;
bound the size where the input arrives, as `std.net.http` does for request
bodies.

## conformance

`tests/markdown/commonmark-0.31.2.json` holds the spec's examples, and
`tests/cases/test_markdown_commonmark.pith` renders each one with raw html and
every url allowed and gfm off, and prints `commonmark: 652/652 passed`. the
comparison is exact once `&#39;` and `&#x27;` are read as `'`. the in-file
tests in `std/markdown.pith` cover the api and the safety rules, the
`javascript:` entity bypasses included.

`bench/markdown_render.pith` renders the repository's docs, repeated to 10 MB,
and reports throughput: about 65 MB/s on the development machine.

## what comes next

later work builds on the event stream and the options above:

- the github extensions behind `gfm(true)`: tables, strikethrough, task list
  items and extended autolinks.
- `heading_ids(true)`, and use from `std.template` and `std.web`, with
  `tools/sitegen` moving to this module.
- a terminal renderer that writes ansi styles, and code highlighting hooks for
  the html and terminal renderers.
