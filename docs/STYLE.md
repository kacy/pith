# Style guide

This guide covers how to write Pith that reads well. The goal is code someone
else can follow quickly and change confidently.

Code is read far more often than it is written, so when the two conflict,
prefer clarity over brevity, explicit over clever, and consistency over novelty.

## Naming

Use descriptive names. Single letters are fine in three places: loop indices
(`i`, `j`), a receiver in a short method (`p` for a `Point`), and mathematical
conventions (`x`, `y` for coordinates). Everywhere else, spell it out.

```pith
# good
mut character_index := 0
mut current_line := ""
mut buffer_capacity := 1024

# bad
mut pos := 0
mut cur := ""
mut cap := 1024
```

Name a loop variable after what it holds:

```pith
# good
for item in items:
    process(item)

for character in input_line:
    if is_whitespace(character):
        continue

# bad
for n in items:
    process(n)
```

For index-based loops, iterate a range instead of counting by hand. `i` is
fine when the body is short; use a real name when the loop runs long or nests.

```pith
for line_index in 0..lines.len():
    process_line(lines[line_index])
```

When you need the element and its position, `for` gives you both:

```pith
for line, line_index in lines:
    print("{line_index + 1}: {line}")
```

Keep `while` for loops that do not step one element at a time, such as a
parser that consumes a different number of characters on each pass.

Functions are `snake_case` and say what they do. Predicates returning `Bool`
start with `is_`, `has_`, or `can_`.

```pith
# good
fn calculate_checksum(data: String) -> Int:
fn parse_http_request(raw_request: String) -> Request:
fn is_valid_email(address: String) -> Bool:

# bad
fn calc(s: String) -> Int:
fn check(a: String) -> Bool:
```

Types are `PascalCase`. The linter enforces both conventions (E300 and E301).

```pith
struct HttpRequest:
    pub method: String
    pub path: String
    pub headers: List[Header]

type UserId = Int
```

A field without `pub` belongs to its module: code in another module can't
read it, assign it, or set it in a constructor (E251). Leave a field private
when its value has to stay consistent with the rest of the struct, such as a
handle the struct owns.

## Organization

Group related declarations and separate the groups with a blank line.

```pith
mut input_buffer := ""
mut output_buffer := ""

mut current_position := 0
mut total_lines := 0
```

Keep functions small. If you cannot describe one in a sentence, it is doing
too much. Prefer early returns to deep nesting:

```pith
fn find_active_user(users: List[User], target_id: Int) -> User?:
    for user in users:
        if user.id != target_id:
            continue
        if not user.is_active:
            continue
        return user
    return none
```

The nested version of the same function is harder to extend, because every new
condition adds a level instead of a line.

## Comments

Every `pub` item needs a doc comment: a `#` comment on the line directly above
it. The linter checks it for public functions and methods (E304), and it is
what the documentation site renders.

```pith
# Read a whole file as text. Fails when the file is missing or unreadable.
pub fn read_config(path: String) -> String!:
    return fs.read(path)!
```

Inside a function, explain *why*, not *what*. The code already says what.

```pith
# good: a linear scan beats a map here; the list is never above ~100 entries
mut current_index := 0

# bad: increment the index by one
current_index = current_index + 1
```

## Errors and optionals

Pith has two failure shapes, and they mean different things. An optional (`T?`,
returning `none`) is for an absence that is ordinary and expected. A result
(`T!`, built with `fail`) is for an operation that could not be completed.

Propagate with `!` when the caller cannot do better:

```pith
pub fn load_user_data(user_id: Int) -> UserData!:
    raw := fetch_from_database(user_id)!
    return parse_user_data(raw)
```

Handle explicitly when you can add context. `.is_err` and `.err` are fields,
not method calls:

```pith
pub fn save_configuration(config: Configuration, path: String) -> Int!:
    written := fs.write(path, encode_config(config))
    if written.is_err:
        fail "could not save config to {path}: {written.err}"
    return 0
```

When the recovery is control flow instead of a value, use the block form of
`catch`. The block runs only on the error path and must leave with `return`,
`fail`, `continue` or `break`:

```pith
for line in lines:
    row := parse_row(line) catch:
        continue
    rows.push(row)
```

Name the error when the block uses it, as in `catch err:`. The name is bound
to the result's error inside the block only.

Put cleanup for the error path in an `errdefer` right after the acquisition,
instead of repeating it before every `fail`. Use `defer` when the cleanup runs
on every exit. See [defer.md](defer.md) for the rules.

```pith
# good
fn open_session(host: String, port: Int) -> Session!:
    fd := tcp.connect(host, port)!
    errdefer tcp.close(fd)
    send_greeting(fd)!
    banner := read_reply(fd)!
    return Session(fd: fd, banner: banner)

# bad: each new step needs its own copy of the cleanup
fn open_session(host: String, port: Int) -> Session!:
    fd := tcp.connect(host, port)!
    sent := send_greeting(fd)
    if sent.is_err:
        tcp.close(fd)
        fail sent.err
    reply := read_reply(fd)
    if reply.is_err:
        tcp.close(fd)
        fail reply.err
    return Session(fd: fd, banner: reply.ok)
```

Consume an optional with `if let` when you need its value, with `unwrap_or`
when a default will do, or with `match` when both cases do real work. `if let`
works in any function:

```pith
fn describe(users: List[User], target_id: Int) -> String:
    if let user = find_active_user(users, target_id):
        return user.name
    return "no active user with that id"
```

The checker does not narrow an optional after `if found == none: return`, so
`found.name` on a later line is still an error. `found?.name` unwraps it, but
`?` propagates the empty case, so it only compiles in a function that returns
`T!`.

## Common patterns

Build a string from a few pieces with interpolation. To append in a loop, use
a string buffer, which does not copy the text built so far on every append:

```pith
import std.io as io

fn join_trimmed(input_lines: List[String]) -> String!:
    out := io.string_buffer()
    defer out.close()
    for line in input_lines:
        trimmed_line := line.trim()
        if trimmed_line.len() == 0:
            continue
        out.write_line(trimmed_line)!
    return out.string()
```

Use `+=`, `-=`, `*=` and `/=` on numeric variables, fields, and list or map
elements:

```pith
mut total_bytes := 0
for chunk in chunks:
    total_bytes += chunk.len()

counts[key] += 1
```

Compound assignment does not work on strings (E217), so write `s = s + ...`
or use `io.string_buffer()` in a loop.

Name the fields when you build a struct whose fields share a type, or one that
takes booleans. A reader should not have to open the declaration to know which
`""` is which:

```pith
# good
ParseResult(is_error: true, key: "", value: "", error_message: "missing '='")

# bad
ParseResult(true, "", "", "missing '='")
```

Positional construction is fine for small structs whose order is obvious, such
as `Point(3, 4)`.

Match on an enum instead of chaining `if` tests, so the checker reports a
variant you forgot to handle (E204). A pattern names the enum
(`Shape.Circle(radius)`); a bare name in a pattern is a binding that matches
every value:

```pith
fn area(shape: Shape) -> Float:
    return match shape:
        Shape.Circle(radius) => 3.14159 * radius * radius
        Shape.Rect(width, height) => width * height
```

A collection literal with no elements needs a type, either from an annotation
or from the first thing stored into it:

```pith
mut active_users: List[User] := []
for user in all_users:
    if not user.is_active:
        continue
    active_users.push(user)
```

Mathematical code may use conventional short names:

```pith
fn calculate_distance(x1: Float, y1: Float, x2: Float, y2: Float) -> Float:
    dx := x2 - x1
    dy := y2 - y1
    return math.sqrt(dx * dx + dy * dy)
```

## What to avoid

**Cryptic abbreviations.** `g_ooff` is not a name. `buf`, `tmp`, and `cur` are
barely better. Write `buffer`, `temporary_file`, `current_user`.

**Mixing abbreviation styles.** Pick one and hold it. `current_position` next
to `pos` next to `idx` next to `current_pos` means four names for one idea.

**Deep nesting.** Invert the conditions and return early instead:

```pith
if not condition_a:
    return
if not condition_b:
    return
do_something()
```

**Long chains of single letters.** Cryptographic and codec code attracts these,
and it is the code that most needs to be checkable by eye. Name the
state (`hash_state_a`) or, when the spec's own letters are the clearest
reference, say so in a comment above the block.

## Abbreviations that are fine

These are common enough to read as words:

| Short | Means | Use it for |
|-------|-------|------------|
| `ctx` | Context | Request or execution context |
| `cfg` | Configuration | When `config` is already taken |
| `err` | Error | A local error value |
| `id` | Identifier | IDs of any kind |
| `req` | Request | HTTP and RPC handlers |
| `resp` | Response | HTTP and RPC handlers |

When in doubt, spell it out.

## File structure

Start a file with a short description of what the module is for:

```pith
# std.net.http - http/1.1 client with connection pooling
#
# keep-alive connections, retries, and custom headers.

import std.net.tcp as tcp
```

Imports go at the top, grouped standard library first and then modules from
your own package. There is no relative-import syntax: every import names a
module path.

```pith
import std.fs as fs
import std.json as json

import myapp.types as types
```

## Before you submit

- Every `pub` item has a doc comment
- Names are descriptive, and abbreviations are from the table above
- Functions do one thing
- Early returns instead of nesting
- Cleanup sits next to the acquisition, in a `defer` or `errdefer`
- Structs with same-typed or boolean fields are built with named fields
- Comments explain why

`pith fmt --check` and `pith lint` check the mechanical half of this list. The
rest is what review is for.

## A worked example

```pith
# config_parser.pith - parse key=value configuration files
#
# supports # comments, blank lines, and reports per-line errors.

import std.fs as fs

pub struct Configuration:
    pub settings: Map[String, String]
    pub errors: List[String]

pub struct ParseResult:
    pub is_error: Bool
    pub key: String
    pub value: String
    pub error_message: String

# Parse a configuration file. A missing or empty file yields an empty
# configuration rather than an error; malformed lines are collected in
# `errors` so the caller can report all of them at once.
pub fn parse_configuration_file(file_path: String) -> Configuration:
    file_contents := fs.read(file_path) catch ""
    if file_contents.len() == 0:
        return Configuration(settings: {}, errors: [])
    return parse_configuration_contents(file_contents)

fn parse_configuration_contents(contents: String) -> Configuration:
    mut settings: Map[String, String] := {}
    mut errors: List[String] := []

    for raw_line, line_index in contents.split("\n"):
        current_line := raw_line.trim()
        if should_skip_line(current_line):
            continue

        parsed := parse_setting_line(current_line, line_index + 1)
        if parsed.is_error:
            errors.push(parsed.error_message)
        else:
            settings[parsed.key] = parsed.value

    return Configuration(settings: settings, errors: errors)

fn should_skip_line(line: String) -> Bool:
    if line.len() == 0:
        return true
    return line[0] == "#"

fn parse_setting_line(line: String, line_number: Int) -> ParseResult:
    equals_position := line.index_of("=")
    if equals_position < 0:
        message := "line {line_number}: missing '=' in setting"
        return ParseResult(is_error: true, key: "", value: "", error_message: message)

    setting_key := line.substring(0, equals_position).trim()
    setting_value := line.substring(equals_position + 1, line.len()).trim()
    return ParseResult(is_error: false, key: setting_key, value: setting_value, error_message: "")
```

The example demonstrates descriptive names, small functions with one job each,
early returns, a `for` that supplies the line index instead of a hand-kept
counter, named fields where several values share a type, a doc comment that
says what the caller needs to know instead of restating the signature, and
errors collected instead of thrown away.

## Contributing

Follow this guide, and update it when you introduce a pattern it does not
cover. [idiomatic_pith.md](idiomatic_pith.md) covers which library calls and
language features to reach for; this guide covers how to name, lay out and
comment the code that uses them. Before you submit, ask whether someone reading
this in six months will understand it.
