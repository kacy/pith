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
| grapheme | see below | text a person reads |

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

## what is not here

`std.text` carries no locale. tags, message catalogs, plural rules, and
locale-aware number and date formatting belong in `std.intl`, which builds on
this module. `std.text` never depends on `std.intl`, so a program that only
needs correct text does not pull in locale data.

`to_lower` and `to_upper` on `String` remain ascii-only. they are a display
convenience, not a correctness tool; see the case-folding section once it
lands for the comparison story.
