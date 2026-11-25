# Text and bytes

Pith keeps text and raw bytes separate on purpose.

That means most code should stay in one of two lanes:
- `std.strings` when you are shaping text
- `std.bytes` when you are moving raw data around

Cross the boundary explicitly when you need to.

`std.strings` works in bytes, which is the same thing as characters only while
the text is ASCII. When the text is not ASCII (a name, a chat message, anything
a user typed), counting, slicing and truncating belong in `std.text`. See
[unicode.md](unicode.md).

## Text helpers

`std.strings` already had the lower-level pieces like `split_lines`,
`partition`, and trimming helpers. The newer surface is meant for the more
common "read a blob, break it up, shape it, move on" flow.

Use these for the usual cases:
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

That shape is clearer than a magic sentinel and easier to work with in
examples.

## Bytes helpers

`std.bytes` is the explicit text/bytes boundary.

The low-level wrappers are there when you want a module-qualified surface:
- `bytes.len(data)`
- `bytes.is_empty(data)`
- `bytes.get(data, idx)`
- `bytes.slice(data, start, end)`
- `bytes.concat(left, right)`
- `bytes.eq(left, right)`

UTF-8 decoding has two paths:

```pith
raw := bytes.from_string_utf8("pith")

typed := bytes.decode_utf8(raw)
if typed.is_ok:
    print(typed.ok)

legacy := bytes.to_string_utf8(raw)!
print(legacy)
```

Use `decode_utf8(...)` when callers should be able to inspect the error.
Keep `to_string_utf8(...)` when a plain string error is enough.

## Byte buffers

`ByteBuffer` is still the simplest way to assemble raw data incrementally.

The newer helpers make the common UTF-8 path shorter:

```pith
buf := bytes.buffer()
buf.write_string_utf8("hello")!
buf.write_line_utf8("world")!
print(buf.bytes().to_string_utf8()!)
buf.reset()
```

`reset()` is a clearer alias for `clear()`.

A buffer frees its storage when the last value naming it dies. `free()`
releases the contents early and `take_bytes()` moves them out as `Bytes`; both
leave an empty buffer that still works. The buffer's handle is private to
`std.bytes`, and the `byte_buffer_*` builtins behind it are callable only
there, so a program never holds a handle to freed memory.

A buffer counts as a collection for the task rules: hand it to a `spawn` or a
channel whole, as a list would be (E248). It cannot become an interface value
such as `io.BytesWriter`, since the checks could not follow it from there; a
generic function bounded by the interface still takes it. To give a writer to
code that takes a `BytesWriter`, use `io.memory_writer()`, whose bytes sit
behind a lock:

```pith
out := io.memory_writer()
writer := out.buffered_chunked(64)
http.send_buffered(writer, http.text(200, "hi"))!
print(out.string())
```

`write` and `write_string_utf8` return the number of bytes appended, so
appending an empty value succeeds and returns 0. Text built piece by piece, such
as a blank header value or an empty field, needs no guard around the write.

## Practical rule

If data came from a file, socket, process, or protocol layer, keep it as
`Bytes` until you have a reason to decode it.

If you are already working with text, stay in `std.strings` and use the
higher-level helpers instead of rebuilding split/join loops by hand.
