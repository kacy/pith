# YAML

`std.yaml` reads YAML into a tree of nodes you address by opaque `Int`
handles, the same shape `std.json` and `std.toml` use. The three are
meant to feel like siblings: the accessors are spelled the same way,
they share the node-pool scope discipline, and typed decoding works the
same in all three.

```pith
import std.yaml as yaml

root := yaml.parse(text)
name := yaml.get_string(root, "name")
server := yaml.get_map(root, "server")
host := yaml.get_string(server, "host")
```

`parse` returns `-1` when the document is invalid and `last_error()`
says why. `parse_required` turns that into a `YamlDecodeError` you can
propagate with `!`.

## What is supported

YAML 1.2 is a much bigger language than a configuration file needs, so
this parser implements the part configuration actually uses:

- Block mappings nested by indentation, and block sequences written
  with `-`, in any combination: a sequence of mappings, a mapping of
  sequences, a sequence of sequences.
- The compact form where a mapping starts on the dash, `- name: a`,
  with the rest of the mapping lined up under `name`.
- A sequence written at its key's own indentation:

  ```yaml
  ports:
  - 80
  - 443
  ```

- Flow collections, `{a: 1, b: 2}` and `[1, 2, 3]`, nested inside each
  other and inside block structure. A flow collection has to open and
  close on one line.
- Plain scalars, resolved against the YAML 1.2 core schema. `null`, `~`
  and an empty value are null. `true` and `false`, along with their
  `True` and `TRUE` spellings, are booleans. Integers can be decimal,
  `0x` or `0o`. Floats are decimal with an optional exponent. Anything
  else is a string.
- Single-quoted scalars, where `''` is a literal quote and a backslash
  is just a backslash.
- Double-quoted scalars, with the `\n`, `\t`, `\r`, `\0`, `\a`, `\b`,
  `\f`, `\v`, `\e`, `\"`, `\\`, `\/` and `\uXXXX` escapes. `\uXXXX`
  is written out as UTF-8.
- Comments, whole-line and trailing. A `#` only starts a comment at the
  beginning of a line or after a space, so `tag: a#b` keeps its hash,
  and a `#` inside a quoted scalar is content.
- Block scalars, literal `|` and folded `>`, with the strip (`-`) and
  keep (`+`) chomping indicators. Clipping is the default.
- Multiple documents separated by `---`, with `...` as an optional end
  marker. `parse` returns the first document; `parse_all` returns a
  sequence node holding one root per document.
- A document that is a single scalar, or that is empty, which is null.

### About `yes` and `no`

`yes`, `no`, `on` and `off` are strings here, not booleans. That mapping
was a YAML 1.1 rule and the 1.2 core schema dropped it. It is also the
reason the country code `NO` famously reads as false in older parsers.
If you want a boolean, write `true` or `false`.

## What is not supported

Each of these is refused with a message that names it. None of them is
silently mis-parsed. A file that means one thing here and another thing
in every other parser is worse than a file that fails to load.

| Feature | Why it is out |
| --- | --- |
| Anchors and aliases (`&a`, `*a`) | Alias expansion is the billion-laughs amplification vector: a few kilobytes of aliases expand into gigabytes of nodes. A config parser that expands them hands an attacker a denial of service, and a depth cap does not help, because the blowup is in width. Doing it safely needs a hard expansion budget, which is a different design. |
| Merge keys (`<<`) | Merging is defined in terms of aliases, so it goes with them. |
| Tags (`!!str`, `!Foo`) | A tag selects a type resolution this parser does not have. A config file that needs one is really asking for a schema, and `decode[T]` is the answer to that here. |
| Complex keys (`? `) | A key that is itself a collection has no representation in a `Map[String, _]` shaped tree. |
| Directives (`%YAML`, `%TAG`) | `%TAG` only matters with tags, and `%YAML` would be a version claim this parser cannot honour. |
| Multi-line plain scalars | A plain scalar continued on the next line reads as an indentation error instead. Dropping the continuation would silently change what the document says. Use `>` when you want a folded string. |
| Explicit block-scalar indentation indicators (`\|2`) | Rare, and the automatic indentation detection covers what people write. |
| Flow collections spanning lines | A `{` or `[` has to close on the line it opens on. |
| Content on a `---` line | Write the document on the lines after the marker. |

Tabs used for indentation are refused too. YAML itself forbids them, so this
is not a Pith restriction; it is stated here because the failure is otherwise
baffling.

## Limits

Every quantity the input controls is bounded, and each bound is a module
constant you can read for yourself:

| Constant | Value | What it stops |
| --- | --- | --- |
| `MAX_DOCUMENT_BYTES` | 4 MiB | An oversized document, refused before any parsing happens. This is the same ceiling `std.net.grpc` puts on a message. |
| `MAX_NESTING_DEPTH` | 64 | A file of nothing but indentation or brackets. Indentation drives recursion here, and a stack overflow is a `SIGSEGV` that no error handler can catch, so the parser has to stop short of one rather than recover from it. |
| `MAX_NODES` | 262144 | A document under the size cap that still asks for an enormous number of tiny nodes. |
| `MAX_COLLECTION_ENTRIES` | 65536 | One mapping or sequence growing without bound. |
| `MAX_SCALAR_BYTES` | 1 MiB | One scalar, including a folded block scalar's body. |
| `MAX_KEY_BYTES` | 4096 | A document that is one enormous key. |

## Accessors

Lookups take a mapping handle and a key:

```pith
yaml.get_value(root, "port")        # the child handle, or -1
yaml.get_string(root, "name")       # "" when missing or another kind
yaml.get_int(root, "port")
yaml.get_float(root, "ratio")
yaml.get_bool(root, "tls")
yaml.get_string_or(root, "name", "unnamed")
yaml.get_int_or(root, "port", 8080)
yaml.get_map(root, "server")        # get_table is the same thing
yaml.get_array(root, "ports")
yaml.keys(root)                     # in document order
yaml.has(root, "port")
```

`require_string`, `require_int` and `require_bool` are the same lookups
with a `YamlDecodeError` instead of a fallback.

Sequence elements have no key to look them up by, so read them straight
off the handle:

```pith
ports := yaml.get_array(root, "ports")
mut i := 0
while i < yaml.array_len(ports):
    print("{yaml.scalar_int(yaml.array_get(ports, i))}")
    i = i + 1
```

`scalar_string`, `scalar_int`, `scalar_float` and `scalar_bool` all
return a zero value for the wrong kind. `type_of` gives the kind
(`"map"`, `"seq"`, `"string"`, `"int"`, `"float"`, `"bool"`, `"null"`, or
`"invalid"` for a handle that names no live node), and `is_null` asks
about the null kind directly.

## Typed decoding

`decode[T]` fills a struct with public `String`, `Int` and `Bool` fields
straight out of a mapping. Optional fields, field defaults and nested
structs all work:

```pith
struct Server:
    pub host: String
    pub port: Int = 8080
    pub tls: Bool = false

struct App:
    pub name: String
    pub server: Server

app := yaml.decode_text[App](text)!
```

The variants match `std.toml`. `decode` takes a handle, `decode_text` a
string, `decode_file` a path, and `decode_path`, `decode_text_path` and
`decode_file_path` each take a dotted path naming a nested mapping. The
text and file forms own the parse they run, so they reclaim their pool
nodes before returning.

## The node pool

Nodes live in a per-task pool in threadlocal storage. A program that
reads its configuration once at startup can ignore all of this. A
program that parses a document per request cannot. Nothing gives the
nodes back on its own, so the pool grows for the life of the task.

Bracket the parse with a scope:

```pith
scope := yaml.open_scope()
defer yaml.close_scope(scope)
root := yaml.parse(body)
```

Copy what you need out of the tree before the scope closes. Scopes nest
and close innermost-first; closing an outer scope also closes any inner
one still open, because an arena can only give back a suffix of what it
handed out.

Node ids are never reused. A handle held past its scope reads as
`"invalid"` instead of aliasing whatever document landed on that id
next, so a lifetime mistake shows up instead of producing a silent wrong
answer. `pool_live_nodes()` reports current occupancy, and that is what a
test should assert on. The id counter is not a measure of occupancy.
