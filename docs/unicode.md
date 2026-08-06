# unicode

a pith `String` is a sequence of utf-8 bytes. `s.len()` counts bytes, `s[i]`
reads a byte, and `s.substring(a, b)` cuts between bytes. that is the same
model Go and Rust use, and it is the right one for a server language: string
indexing stays O(1) and no program pays for character bookkeeping it never
asked for.

the cost of that model is that byte offsets and character offsets agree only
while the text is ascii. `std.text` is the layer that closes the gap.

```pith
import std.text as text

text.char_count("Ärger")     # 5, where "Ärger".len() is 6
text.truncate("Ärger", 1)    # "Ä"
text.is_valid(input)         # is this really utf-8?
```

## substring aborts rather than corrupt a character

`substring` used to clamp its bounds and cut wherever it was told. on
non-ascii text that produced a string containing half a character — a lead
byte with its continuation missing — which no longer round-trips through
utf-8. nothing reported it. the damage showed up later, in a database write
or a rendered page.

it now aborts:

```
pith runtime error: substring(0, 1) would split the character 'Ä' at byte 1 (it occupies bytes 0..2)
  a String is indexed in bytes, so a byte offset can land inside a multi-byte character. use std.text for character-aware work:
    text.slice(s, a, b)         slice by character index
    text.truncate(s, n)         keep n characters
    text.truncate_bytes(s, n)   keep n bytes, ending on a boundary
```

this follows the strict accessors already in the runtime —
`pith_list_get_value_strict`, `bytes_get_strict`, `map_get_strict` — which all
abort with a diagnostic rather than return a quietly wrong value. `substring`
keeps its `String` return type; there is no new error to thread through the
391-odd call sites, and code that was already cutting on boundaries is
unaffected.

the check only fires when the cut lands strictly inside a *well-formed*
multi-byte character. a `String` is a byte string, and code holding binary
data in one may slice it anywhere; only real text has boundaries to respect.
if you are slicing arbitrary bytes, use `Bytes`, which is what the rest of the
standard library does.

## picking a unit

three units, in increasing order of how much they cost and how closely they
match what a reader sees:

| unit | what it is | use it for |
| --- | --- | --- |
| byte | `s.len()`, `s[i]`, `substring` | protocol framing, storage limits, ascii parsing |
| character | `text.char_count`, `text.slice` | anything that must not corrupt text |
| grapheme | `graphemes.count`, `graphemes.truncate` | text a person reads |

a "character" here is a unicode scalar value — the unit Go's `for range` and
Rust's `chars()` yield. it is not always one mark on screen: `e` followed by a
combining acute accent is two scalar values but one thing a reader sees, and
a family emoji is several. when you are truncating a display name or a chat
message, graphemes are the honest unit.

## validation at the edge

bytes arriving from a socket, a file, or a form field are not text until you
have checked them.

```pith
if not text.is_valid(field):
    fail "field is not utf-8"
```

or repair them, when rejecting is too strict:

```pith
safe := text.sanitize(field)   # every undecodable byte becomes U+FFFD
```

`decode_at` rejects overlong encodings, surrogates, and values above U+10FFFF,
not just structurally broken sequences. those shapes are well-formed enough to
fool a lenient decoder, which is how a string gets past a validation check as
one thing and is read downstream as another.

`find_invalid` returns the byte offset of the first bad byte, for when the
error message should say where.

## truncation

three different questions, three functions:

```pith
text.truncate(s, 20)              # at most 20 characters
text.truncate_bytes(s, 255)       # at most 255 bytes, ending on a boundary
text.truncate_with(s, 20, "...")  # at most 20 characters, and say it was cut
```

`truncate_bytes` is the one to reach for when the limit comes from storage — a
`varchar(255)`, a header size cap, a log field budget. it never returns a
partial character, so the result is always valid utf-8.

## fold to compare, to_lower to display

`to_lower` and `to_upper` on `String` are ascii-only. `"Ärger".to_lower()` is
`"Ärger"`, unchanged. `"ПРИВЕТ".to_lower()` is `"ПРИВЕТ"`, unchanged.

that matters more than it looks, because most `to_lower()` calls in auth,
routing and header code are not lowercasing at all — they are caseless
comparisons, written with the only tool that was there:

```pith
if header.to_lower() == "content-type":   # fine, headers are ascii
if username.to_lower() == stored:         # broken the moment a name is not
```

the second one is the bug. use `fold`:

```pith
text.fold("Ärger")              # "ärger"
text.eq_fold("Ärger", "ärger")  # true
```

folding is not lowercasing, and the difference is not pedantic. greek final
sigma lowercases to itself but folds to a plain sigma, so `"ΣΣ"` and `"σς"`
are the same word and only folding says so. folding produces a comparison key;
lowercasing produces text for a reader. keep `to_lower` for the second job.

this is **simple** folding: every character folds to exactly one character.
`"ß"` folds to itself rather than to `"ss"`, so `fold("straße")` and
`fold("STRASSE")` are not equal. full folding would close that gap with a
mapping that changes length; it is not implemented, and the tests pin the
current behaviour so it cannot drift by accident.

### but do not fold a protocol token

`fold` is for text a person typed. It is the wrong tool for an identifier a
specification defines, and reaching for it there is a security regression
rather than a fix.

Two non-ASCII code points fold *into* ASCII:

| code point | folds to |
| --- | --- |
| U+017F LATIN SMALL LETTER LONG S `ſ` | `s` |
| U+212A KELVIN SIGN `K` | `k` |

So `text.eq_fold("cloſe", "close")` is **true**, and so is
`text.eq_fold("websocKet", "websocket")` with a Kelvin sign. HTTP, WebSocket
and TLS all define their keywords as ASCII case-insensitive and nothing wider;
folding them would accept values the RFC says are different, which is exactly
the shape a header-smuggling or filter-bypass bug takes.

For those, `strings.equals_ignore_case` and `to_lower` are correct, and this
is why the standard library's own `Connection: close`, `Upgrade: websocket`,
`Bearer ` and header-name comparisons were deliberately left alone:

```pith
strings.equals_ignore_case(header_name, "content-type")   # protocol token
text.eq_fold(display_name, stored_name)                   # text a user typed
```

The rule is about where the value came from, not how it looks. A specification
wrote the token; a person wrote the text.

## normalize so equal-looking text is equal

`"é"` can be one code point or an `"e"` followed by a combining acute accent.
they render identically, a user cannot tell them apart, and they are different
bytes — so they hash differently, compare unequal, and land in a database as
two rows.

```pith
text.normalize(input)   # NFC, the composed form
```

normalize at the edge, where text arrives, and store the result. for an
identifier a user typed, you usually want both operations:

```pith
text.eq_fold_normalized(typed, stored)
```

only NFC is implemented. NFD, NFKC and NFKD are not. NFC is the form to store
and compare; the compatibility forms discard information — they flatten a
fullwidth letter and its ascii form into the same string — which is the wrong
default for text you are keeping.

## graphemes, for anything a person reads

a scalar value is still not what a reader counts. `"é"` written as an `e` plus
a combining accent is two scalar values and one mark on screen. a family emoji
is five. a flag is two. truncating by scalar value can strip an accent off its
letter, take the skin tone off a person, or leave half a flag.

`std.text.graphemes` splits on what a reader would call a character:

```pith
import std.text.graphemes as graphemes

graphemes.count("👨‍👩‍👧")            # 1, where text.char_count is 5
graphemes.truncate(name, 20)     # never cuts a cluster in half
graphemes.split(message)
```

use graphemes when the number or the cut is user-visible — a display name, a
chat message, a character counter next to an input box. use the scalar
functions in `std.text` when you are working with the text itself: parsing,
validating, encoding. graphemes cost a table lookup per code point and a small
amount of state, which is worth paying for a truncation and not for a parser.

these are the extended grapheme cluster rules from unicode annex 29, the same
definition Swift's `Character` and a text editor's arrow keys use. the
implementation is checked against the standard's own test file: all 1187 cases
of `GraphemeBreakTest.txt` run as a colocated test, from data the generator
packs into the tables, so regenerating for a new unicode release re-verifies
the segmenter rather than just replacing its data.

## the tables

the case folding and normalization data is generated from the unicode
character database, pinned at **15.1.0**, by `tools/unicodegen/generate.py`:

```
python3 tools/unicodegen/generate.py
```

the generated modules are checked in, so building pith never needs the
network. to move to a new unicode release, change `UNICODE_VERSION` in that
script, rerun it, and commit the diff. the version is recorded in
`text.unicode_version()` and in the header of every generated file.

the generator checks every table it emits against python's `unicodedata`
before writing, and refuses to run if that module's version does not match the
pin — a generator that only parses the UCD files can confirm it read them
consistently, not that it read them correctly.

the tables are packed into string constants rather than list literals, and
binary-searched in place. that is not premature cleverness: compiling a
module-level pith list of N integers is superlinear in N — 500 entries take
2s, 1000 take 33s, and 3000 do not finish in two minutes — while a 60KB string
constant compiles in about a second and costs nothing at startup.

| table | records | size |
| --- | --- | --- |
| case folding | 205 runs (from 1457 pairs) | 4.0 KB |
| combining classes, decompositions, compositions | 388 + 2061 + 941 | 42.0 KB |
| grapheme break properties, plus the conformance cases | 733 ranges + 3009 | 25.0 KB |

71 KB in total, all of it compiled in rather than loaded, and none of it
touched by a program that does not call into these functions.

## what is not here

`std.text` carries no locale. tags, message catalogs and plural rules belong
in `std.intl` — see [i18n.md](i18n.md) — which builds on this module.
`std.text` never depends on `std.intl`, so a program that only needs correct
text does not pull in locale data.

also not implemented, deliberately: full and special case mapping (`ß` → `SS`,
turkish dotted and dotless i), NFD/NFKC/NFKD, word and sentence segmentation,
and bidirectional text.
