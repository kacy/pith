# unicodegen

This tool generates the Unicode tables that `std.text` compiles against.

```
python3 tools/unicodegen/generate.py
```

The script downloads the pinned Unicode Character Database, builds the tables,
and rewrites the generated modules under `std/text/`. Commit the result. The
generated files are checked in so that building Pith never needs the network.

## Pinning

The Unicode version is pinned in `generate.py` (`UNICODE_VERSION`). To move to
a new release, change it, rerun the command, and commit the diff. The version
is recorded in the header of every generated file and in `docs/unicode.md`, so
the tables can never silently disagree about which release they came from.

The pin also has to match the `unicodedata` module of the Python that runs the
generator, because the generator checks its own output against it (see below).
On a mismatch, `generate.py` refuses to run, since it could not verify the
tables it would emit.

## Why Python

This is a build-time tool, and its code is not shipped. Python's `unicodedata`
is an independent implementation of the same standard, so the generator can
check every table it emits against it before writing anything: each case fold,
each composition, each combining class. That check is the reason for the
language choice. A generator written against the same UCD text files it parses
can only verify itself.
