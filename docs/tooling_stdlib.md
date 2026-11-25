# Tooling standard library

Pith now has a small set of stdlib modules aimed at tools: walking files,
scanning source-like text, parsing command lines, rendering diagnostics,
lexing and highlighting Pith source, and writing tighter tests.

## File discovery

`std.glob` matches file paths with `*`, `?`, and `**`.

```pith
import std.glob as glob

files := glob.find(".", ["std/**/*.pith"])!
for file in files:
    print(file)
```

By default it skips hidden entries and common build folders like `.git`,
`.pith-build`, `target`, `zig-cache`, `zig-out`, and `.zig-cache`.

Use `find_matches` when you need to know which pattern matched a file, and
`find_excluding` when a tool has include and exclude patterns.

## Text scanning

`std.text.scanner` is a tiny value-oriented cursor for source-like text. It
tracks byte position plus 1-based line and column while callers peek, advance,
and take slices.

The cursor moves in **characters**. `pos` is a byte offset (that is how a
`String` is indexed), but it only ever lands where a character starts, `peek`
returns one whole character, and `col` counts characters. This matters because
taking a slice at a position inside a multi-byte character is a runtime abort.

```pith
import std.text.scanner as scanner

source := "name: pith" + chr(10) + "version: 1"
key := scanner.take_until(scanner.new(source), ":")
line := scanner.take_line(scanner.new(source))

print(key.slice.text)
print(line.slice.text)
```

Use it for tooling code that needs predictable EOF behavior, line handling,
or source positions without writing another local `while i < text.len()` loop.

## CLI parsing

`std.cli` is intentionally stateless. Build a spec, pass `argv`, and inspect
the parsed result.

```pith
import std.cli as cli

fmt := cli.command("fmt", "format files", [cli.flag("check", "c", "check only")], ["pattern"])
spec := cli.app("pith-tool", "small tooling cli", [], [], [fmt])
parsed := cli.parse(spec, ["fmt", "--check", "std/**/*.pith"])!

print(parsed.command)
print(cli.flag_value(parsed, "check").to_string())
print(cli.positional(parsed, 0))
```

The first argument that is not an option or an option's value names the
command. Options declared on the app are accepted anywhere on the line, before
or after the command, so an option every command takes is declared once. After
the command, an option the command defines with the same name or short name
takes precedence over the app's.

This layer is for small Pith tools that want options, flags, subcommands, and
help text without each tool reimplementing the same argument loop. It also
supports POSIX short-flag clustering (`-fsSL`), attached short option values
(`-XPOST`), repeatable `multi_option` flags (read with `cli.strings(parsed, name)`),
`--no-<flag>` negations, and the `--` end-of-options separator.

## Diagnostics

`std.diagnostic` gives tools one shape for human-readable and JSON output.

```pith
import std.diagnostic as diagnostic

diag := diagnostic.with_fix(
    diagnostic.with_span(diagnostic.error("expected expression"), "main.pith", 3, 7, "value ="),
    "add an expression",
)

print(diagnostic.render(diag))
print(diagnostic.render_json(diag))
```

The structs stay simple on purpose: severity, message, a `Span` (empty when
absent), and a `Fix` (empty when absent). That makes it useful for
compiler-adjacent tools without coupling every caller to compiler internals.

## Editor wire protocol

`std.lsp.transport` and `std.lsp.protocol` are the wire layer for editor
tooling: Content-Length framing over bytes, and the JSON-RPC 2.0 envelope
over `std.json` handles. Both are pure data-plane layers with no sockets and
no tasks, so a server loop can be tested entirely with in-memory bytes.

```pith
import std.bytes as bytes
import std.lsp.transport as transport
import std.lsp.protocol as protocol

r := transport.reader()
r.feed(chunk)
body := r.next()

m := protocol.parse_message(bytes.to_string_utf8(body.unwrap_or(bytes.empty()))!)
if m.kind == "request":
    reply := protocol.response_body(m.id, m.id_text, m.id_is_text, result)
```

The reader tolerates what editors actually send (case-insensitive header
names, extra headers like Content-Type, bare `\n` line endings) and
poisons itself with a message from `error()` when a header block is
malformed or declares a body over 64 MiB, so a corrupt length cannot
balloon memory. `parse_message` classifies bodies as request,
notification, response, or invalid with a reason, and the `*_body`
builders produce the matching outgoing envelopes.

## Pith source: lexing and highlighting

`std.pith.lexer` is the lexer the compiler runs, and `std.pith.highlight`
colors Pith code with it. The compiler's front end reads the same token
stream, so a highlighted page and the compiler agree on where a string ends
and which words are keywords.

```pith
import std.pith.highlight as highlight

code := "fn add(a: Int, b: Int) -> Int:\n    return a + b\n"
style := "<style>" + highlight.STYLESHEET + "</style>"
block := "<pre><code class=\"language-pith\">" + highlight.highlight_html(code) + "</code></pre>"
```

`highlight_html(code)` returns the inside of a code block. It escapes all text
with `std.html` and wraps each span that is not plain in
`<span class="...">`. The class names are stable:

| Kind          | Class        | Covers                                                        |
|---------------|--------------|---------------------------------------------------------------|
| `keyword`     | `pith-kw`    | Keywords, plus `locked`, `guard`, `holding`, `where` and `frozen` where a name follows them |
| `type`        | `pith-ty`    | A name that starts with an uppercase letter                   |
| `function`    | `pith-fn`    | A name called directly, `f(`, or declared, `fn f`             |
| `variable`    | `pith-var`   | Any other name                                                |
| `string`      | `pith-str`   | String literals, the literal text of an interpolated string, a format spec |
| `number`      | `pith-num`   | Integer and float literals                                    |
| `comment`     | `pith-com`   | `#` and `##` comments                                         |
| `operator`    | `pith-op`    | `+`, `->`, `:=`, `==`, `..`, `?`, `|` and the rest            |
| `punctuation` | `pith-punct` | `.`, `,`, `:`, brackets, and the braces of an interpolation   |
| `plain`       | None         | Whitespace, `_`, and bytes the lexer rejects                  |

The expression inside an interpolation highlights as code, so
`"total {format(n):>8}"` shows a function call between two pieces of string.
`STYLESHEET` is a default theme for these classes, with a light scheme and a
dark one under `prefers-color-scheme: dark`. Variables and plain text keep the
page's own color. A site with its own theme can skip it and style the classes
directly.

`spans(code)` is the form for other renderers, such as a terminal. It returns
a `List[Span]`, where each span has a byte `start`, a byte `len` and a `kind`
from the table above. The spans cover every byte of the input once, in order,
and never split a UTF-8 character. Adjacent spans never share a kind, and only
a plain span holds a line break, so a renderer can color line by line.

Neither function fails on any input: half-typed code, an unterminated string,
control bytes and invalid UTF-8 all highlight, and the work is linear in the
input. The language server builds its semantic tokens from `spans` as well, so
an editor and a rendered page color code the same way.

`std.pith.lexer` also works on its own. `lex(src)` returns the token stream
the compiler parses, ending in an `eof` token. `lex_with_comments(src)` adds a
token for each comment that sits alone on its line, which `lex` skips like a
blank line. Each `Token` has a `kind`, the source text as its `value`, a
1-based `line` and `column`, and a byte `offset` and `len`. A lexing error
becomes an `err` token and scanning continues. All scan state belongs to one
call, so tasks can lex at the same time. The compiler's `self-host/lexer.pith`
adds what only the compiler needs: the 16 MiB source limit, the token table
the parser reads by index, and integer literal values.

## Testing helpers

`std.testing` now includes a few helpers for stdlib and self-hosting tests:

- `assert_eq_text(got, want)` for string comparisons, showing a window around
  the first byte that differs
- `assert_file_exists(path)` for file checks
- `assert_dir_exists(path)` for directory checks
- `with_temp_dir(prefix, run)` for scoped filesystem tests

Prefer these in examples and small helper programs when they make the test
intent clearer than hand-written checks. A failing `std.testing` check both
tallies and fails the colocated `test` block it runs inside, so it does not
vanish into a count nobody reads. See [testing.md](testing.md) for the full
picture.
