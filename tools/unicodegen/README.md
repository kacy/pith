# unicodegen

generates the unicode tables `std.text` compiles against.

```
python3 tools/unicodegen/generate.py
```

that downloads the pinned unicode character database, builds the tables, and
rewrites the generated modules under `std/text/`. commit the result: the
generated files are checked in so that building pith never needs the network.

## pinning

the unicode version is pinned in `generate.py` (`UNICODE_VERSION`). to move to
a new release, change it, rerun the command, and commit the diff. the version
is recorded in the header of every generated file and in `docs/unicode.md`, so
the tables can never silently disagree about which release they came from.

the pin also has to match the `unicodedata` module of the python running the
generator, because the generator checks its own output against it -- see
below. `generate.py` refuses to run on a mismatch rather than emitting tables
it cannot verify.

## why python

this is a build-time tool, not shipped code. python's `unicodedata` is an
independent implementation of the same standard, so the generator can check
every table it emits against it before writing anything: each case fold, each
composition, each combining class. that self-check is the reason for the
language choice -- a generator written against the same UCD text files it
parses can only verify itself.
