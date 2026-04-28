# text and bytes

pith keeps text and raw bytes separate on purpose.

that means most code should stay in one of two lanes:
- `std.strings` when you are shaping text
- `std.bytes` when you are moving raw data around

cross the boundary explicitly when you need to.

## text helpers

`std.strings` already had the lower-level pieces like `split_lines`,
`partition`, and trimming helpers. the newer surface is meant for the more
common "read a blob, break it up, shape it, move on" flow.

use these for the usual cases:
- `lines(text)` for read-style line splitting
- `lines_keep_empty(text)` when exact empty segments matter
- `words(text)` for whitespace tokenization
- `split_once(text, sep)` and `rsplit_once(text, sep)` for one split from the
  left or right
- `before(...)`, `after(...)`, `before_last(...)`, `after_last(...)` when you
  only want one side
- `join_lines(...)` to stitch lines back together with `\n`

`split_once` and `rsplit_once` return a small struct:

```pith
pair := strings.split_once("name=pith", "=")
if pair.found:
    print(pair.left)
    print(pair.right)
```

that shape ended up being clearer than a magic sentinel and easier to work
with in examples.

## bytes helpers

`std.bytes` is the explicit text/bytes boundary.

the low-level wrappers are there when you want a module-qualified surface:
- `bytes.len(data)`
- `bytes.is_empty(data)`
- `bytes.get(data, idx)`
- `bytes.slice(data, start, end)`
- `bytes.concat(left, right)`
- `bytes.eq(left, right)`

utf-8 decoding now has two paths:

```pith
raw := bytes.from_string_utf8("pith")

typed := bytes.decode_utf8(raw)
if typed.is_ok:
    print(typed.ok)

legacy := bytes.to_string_utf8(raw)!
print(legacy)
```

use `decode_utf8(...)` when callers should be able to inspect the error.
keep `to_string_utf8(...)` when a plain string error is enough.

## byte buffers

`ByteBuffer` is still the simplest way to assemble raw data incrementally.

the newer helpers are just there to make the common utf-8 path shorter:

```pith
buf := bytes.buffer()
buf.write_string_utf8("hello")!
buf.write_line_utf8("world")!
print(buf.bytes().to_string_utf8()!)
buf.reset()
```

`reset()` is just a clearer alias for `clear()`.

## practical rule

if data came from a file, socket, process, or protocol layer, keep it as
`Bytes` until you have a reason to decode it.

if you are already in text land, stay in `std.strings` and use the higher-level
helpers instead of rebuilding split/join loops by hand.
