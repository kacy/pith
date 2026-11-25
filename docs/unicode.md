# Unicode

A Pith `String` is a sequence of UTF-8 bytes. `s.len()` counts bytes, `s[i]`
reads a byte, and `s.substring(a, b)` cuts between bytes. Go and Rust use the
same model, and it is the right one for a server language: string indexing
stays O(1) and no program pays for character bookkeeping it never asked for.

The cost of that model is that byte offsets and character offsets agree only
while the text is ASCII. `std.text` is the layer that closes the gap.

```pith
import std.text as text

text.char_count("Ärger")     # 5, where "Ärger".len() is 6
text.truncate("Ärger", 1)    # "Ä"
text.is_valid(raw)           # is this really utf-8?
```

## Substring aborts rather than corrupt a character

A cut inside a character would leave half of it, a lead byte with its
continuation missing, which no longer round-trips through UTF-8. So
`substring` aborts instead:

```
pith runtime error: substring(0, 1) would split the character 'Ä' at byte 1 (it occupies bytes 0..2)
  a String is indexed in bytes, so a byte offset can land inside a multi-byte character. use std.text for character-aware work:
    text.slice(s, a, b)         slice by character index
    text.truncate(s, n)         keep n characters
    text.truncate_bytes(s, n)   keep n bytes, ending on a boundary
```

This follows the strict accessors already in the runtime
(`pith_list_get_value_strict`, `pith_bytes_get_strict`, and the strict map
accessors), which all abort with a diagnostic instead of returning a silently
wrong value. `substring` keeps its `String` return type, and code that cuts on
boundaries never reaches the abort.

The check only fires when the cut lands strictly inside a *well-formed*
multi-byte character. A `String` is a byte string, and code holding binary
data in one may slice it anywhere; only real text has boundaries to respect.
If you are slicing arbitrary bytes, use `Bytes`, which is what the rest of the
standard library does.

## A length check does not make an offset safe

The abort is correct and the caller is what needs fixing, so this section
names the mistake that produces one. `len()` counts bytes and `substring` takes
byte offsets, so **a length check never proves an offset falls on a character
boundary**. `s.len() >= 2` says nothing about where character two starts.
Neither does "this field is thirteen characters by the spec", when the field
came off a socket and the spec is the peer's to ignore.

Three things do prove it:

- **An ASCII byte you matched.** An ASCII byte can never appear inside a
  multi-byte character, so the position of one is always a boundary. This
  covers most parsing: `index_of("=")`, a scan that stops on `:`, a cursor that
  only moved across digits it checked. It is why slicing at a delimiter is safe
  without thinking about it.
- **An ASCII prefix or suffix you matched.** `starts_with("Bearer ")` proves
  offset 7 is a boundary; `ends_with("Z")` proves `len - 1` is.
- **A boundary helper.** `text.floor_boundary`, `text.ceil_boundary`,
  `text.byte_offset_of` and the `std.text` slicing functions answer the question
  directly.

These do not prove it: a fixed offset on unvalidated input, an arithmetic index
like `len - 1` behind nothing but a length check, a count of bytes read or
written, or a position from a scan that steps one byte at a time over arbitrary
text. When you have one of those and need to cut, either move to the boundary
beside it or read the byte in place with `s[i]`, which returns one byte and
never cuts anything.

Comparing is the common case, and comparing does not need a cut at all.
Testing whether a word sits at a position by slicing it out produced the worst
of these bugs. Compare the bytes one at a time instead, which is both safe and
cheaper.

## Picking a unit

Three units, in increasing order of how much they cost and how closely they
match what a reader sees:

| Unit | What it is | Use it for |
| --- | --- | --- |
| Byte | `s.len()`, `s[i]`, `substring` | Protocol framing, storage limits, ASCII parsing |
| Character | `text.char_count`, `text.slice` | Anything that must not corrupt text |
| Grapheme | `graphemes.count`, `graphemes.truncate` | Text a person reads |

A "character" here is a Unicode scalar value, the unit Go's `for range` and
Rust's `chars()` yield. It is not always one mark on screen: `e` followed by a
combining acute accent is two scalar values but one thing a reader sees, and
a family emoji is several. When you are truncating a display name or a chat
message, graphemes are the right unit.

There is a fourth unit that Pith never uses for its own strings but has to
speak at protocol boundaries: the UTF-16 code unit, which the Language Server
Protocol and the JavaScript world count positions in. `std.text.utf16`
converts between UTF-16 offsets and byte offsets (`length`, `to_byte_offset`,
`from_byte_offset`); a code point outside the Basic Multilingual Plane counts
as two units, everything else as one.

## Validation at the edge

Bytes arriving from a socket, a file, or a form field are not text until you
have checked them.

```pith
if not text.is_valid(field):
    fail "field is not utf-8"
```

Or repair them, when rejecting is too strict:

```pith
safe := text.sanitize(field)   # every undecodable byte becomes U+FFFD
```

`decode_at` rejects overlong encodings, surrogates, and values above U+10FFFF,
as well as structurally broken sequences. Those shapes are well-formed enough
to fool a lenient decoder, which is how a string gets past a validation check
as one thing and is read downstream as another.

`find_invalid` returns the byte offset of the first bad byte, for when the
error message should say where.

## Truncation

Each of these three functions answers a different question:

```pith
text.truncate(s, 20)              # at most 20 characters
text.truncate_bytes(s, 255)       # at most 255 bytes, ending on a boundary
text.truncate_with(s, 20, "...")  # at most 20 characters, and say it was cut
```

`truncate_bytes` is the one to reach for when the limit comes from storage: a
`varchar(255)`, a header size cap, a log field budget. It never returns a
partial character, so the result is always valid UTF-8.

## Fold to compare, to_lower to display

`to_lower` and `to_upper` on `String` are ASCII-only. `"Ärger".to_lower()` is
`"Ärger"`, unchanged. `"ПРИВЕТ".to_lower()` is `"ПРИВЕТ"`, unchanged.

That matters because most `to_lower()` calls in auth, routing and header code
are really caseless comparisons, written with the only tool that was there:

```pith
if header.to_lower() == "content-type":   # fine, headers are ascii
    ...
if username.to_lower() == stored:         # broken the moment a name is not
    ...
```

The second one is the bug. Use `fold`:

```pith
text.fold("Ärger")              # "ärger"
text.eq_fold("Ärger", "ärger")  # true
```

Folding and lowercasing are different operations, and the difference has
consequences. Greek final sigma lowercases to itself but folds to a plain
sigma, so `"ΣΣ"` and `"σς"` are the same word and only folding says so.
Folding produces a comparison key; lowercasing produces text for a reader.
Keep `to_lower` for the second job.

This is **simple** folding: every character folds to exactly one character.
`"ß"` folds to itself, not to `"ss"`, so `fold("straße")` and
`fold("STRASSE")` are not equal. Full folding would close that gap with a
mapping that changes length; it is not implemented, and the tests pin the
current behaviour so it cannot drift by accident.

### But do not fold a protocol token

`fold` is for text a person typed. It is the wrong tool for an identifier a
specification defines, and using it there is a security regression.

Two non-ASCII code points fold *into* ASCII:

| Code point | Folds to |
| --- | --- |
| U+017F LATIN SMALL LETTER LONG S `ſ` | `s` |
| U+212A KELVIN SIGN `K` | `k` |

So `text.eq_fold("cloſe", "close")` is **true**, and so is
`text.eq_fold("websocKet", "websocket")` with a Kelvin sign. HTTP, WebSocket
and TLS all define their keywords as ASCII case-insensitive and nothing wider;
folding them would accept values the RFC says are different, which is the
shape a header-smuggling or filter-bypass bug takes.

For those, `strings.equals_ignore_case` and `to_lower` are correct, and this
is why the standard library's own `Connection: close`, `Upgrade: websocket`,
`Bearer ` and header-name comparisons were deliberately left alone:

```pith
strings.equals_ignore_case(header_name, "content-type")   # protocol token
text.eq_fold(display_name, stored_name)                   # text a user typed
```

Decide by where the value came from, even when a token and typed text look
alike. A specification wrote the token; a person wrote the text.

## Normalize so equal-looking text is equal

`"é"` can be one code point or an `"e"` followed by a combining acute accent.
They render identically, a user cannot tell them apart, and they are different
bytes, so they hash differently, compare unequal, and land in a database as
two rows.

```pith
text.normalize(name)    # NFC, the composed form
```

Normalize at the edge, where text arrives, and store the result. For an
identifier a user typed, you usually want both operations:

```pith
text.eq_fold_normalized(typed, stored)
```

Only NFC is implemented. NFD, NFKC and NFKD are not. NFC is the form to store
and compare. The compatibility forms discard information (they flatten a
fullwidth letter and its ASCII form into the same string), which is the wrong
default for text you are keeping.

## Graphemes, for anything a person reads

A scalar value is still not what a reader counts. `"é"` written as an `e` plus
a combining accent is two scalar values and one mark on screen. A family emoji
is five. A flag is two. Truncating by scalar value can strip an accent off its
letter, take the skin tone off a person, or leave half a flag.

`std.text.graphemes` splits on what a reader would call a character:

```pith
import std.text.graphemes as graphemes

graphemes.count("👨‍👩‍👧")            # 1, where text.char_count is 5
graphemes.truncate(name, 20)     # never cuts a cluster in half
graphemes.split(message)
```

Use graphemes when the number or the cut is user-visible: a display name, a
chat message, a character counter next to an input box. Use the scalar
functions in `std.text` when you are working with the text itself: parsing,
validating, encoding. Graphemes cost a table lookup per code point and a small
amount of state, which is worth paying for a truncation and not for a parser.

These are the extended grapheme cluster rules from Unicode Annex 29, the same
definition Swift's `Character` and a text editor's arrow keys use. The
implementation is checked against the standard's own test file: all 1187 cases
of `GraphemeBreakTest.txt` run as a colocated test, from data the generator
packs into the tables, so regenerating for a new Unicode release re-verifies
the segmenter as well as replacing its data.

## Display width, for terminal layout

A terminal cell is a fourth unit, alongside bytes, characters and graphemes:
日 fills two columns, a combining accent fills none, and a family emoji built
from several code points fills two. `std.text.width` counts columns and lays
text out against that count:

```pith
import std.text.width as width

width.of("日本")                        # 4 — two columns per character
width.pad_right("日", 5)                # "日   "
width.center("abc", 6)                  # " abc  "
width.truncate_to_with("日本語です", 7, "…")  # "日本語…"
```

Counting is grapheme-aware: a flag pair measures 2, an accented letter 1, and
truncation never splits a cluster. A two-column character that half-fits is
dropped, so the result can come up one column short but never overflows.
Widths follow East Asian Width with Ambiguous counted narrow (wcwidth's
default); variation selector 16 forces emoji presentation and two columns.
The byte-based padders in `std.strings` are unchanged. They are for byte work,
and terminal layout should use this module instead.

## The tables

The case folding and normalization data is generated from the Unicode
Character Database, pinned at **15.1.0**, by `tools/unicodegen/generate.py`:

```
python3 tools/unicodegen/generate.py
```

The generated modules are checked in, so building Pith never needs the
network. To move to a new Unicode release, change `UNICODE_VERSION` in that
script, rerun it, and commit the diff. The version is recorded in
`text.unicode_version()` and in the header of every generated file.

The generator checks every table it emits against Python's `unicodedata`
before writing, and refuses to run if that module's version does not match the
pin. A generator that only parses the UCD files can confirm that it read them
consistently, but it cannot confirm that it read them correctly.

The tables are packed into string constants instead of list literals, and
binary-searched in place, so they cost nothing at startup.

| Table | Records | Size |
| --- | --- | --- |
| Case folding | 205 runs (from 1457 pairs) | 4.0 KB |
| Combining classes, decompositions, compositions | 388 + 2061 + 941 | 42.0 KB |
| Grapheme break properties, plus the conformance cases | 733 ranges + 3009 | 25.0 KB |
| Display width (wide and zero-width runs) | 120 + 356 runs | 4.7 KB |

That is 76 KB in total, all of it compiled in instead of loaded, and a program
that does not call into these functions touches none of it.

## What is not here

`std.text` carries no locale. Tags, message catalogs and plural rules belong
in `std.intl` (see [i18n.md](i18n.md)), which builds on this module.
`std.text` never depends on `std.intl`, so a program that only needs correct
text does not pull in locale data.

Also not implemented, deliberately: full and special case mapping (`ß` → `SS`,
Turkish dotted and dotless i), NFD/NFKC/NFKD, word and sentence segmentation,
and bidirectional text.
