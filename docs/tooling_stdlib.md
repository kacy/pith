# tooling standard library

pith now has a small set of stdlib modules aimed at tools: walking files,
scanning source-like text, parsing command lines, rendering diagnostics, and
writing tighter tests.

## file discovery

`std.glob` matches file paths with `*`, `?`, and `**`.

```pith
import std.glob as glob

files := glob.find(".", ["std/**/*.pith"])!
for file in files:
    print(file)
```

by default it skips hidden entries and common build folders like `.git`,
`.pith-build`, `target`, `zig-cache`, `zig-out`, and `.zig-cache`.

use `find_matches` when you need to know which pattern matched a file, and
`find_excluding` when a tool has include and exclude patterns.

## text scanning

`std.text.scanner` is a tiny value-oriented cursor for source-like text. it
tracks byte position plus 1-based line and column while callers peek, advance,
and take slices.

```pith
import std.text.scanner as scanner

source := "name: pith" + chr(10) + "version: 1"
key := scanner.take_until(scanner.new(source), ":")
line := scanner.take_line(scanner.new(source))

print(key.slice.text)
print(line.slice.text)
```

use it for tooling code that needs predictable eof behavior, line handling,
or source positions without writing another local `while i < text.len()` loop.

## cli parsing

`std.cli` is intentionally stateless. build a spec, pass `argv`, and inspect
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

this layer is for small pith tools that want options, flags, subcommands, and
help text without each tool reimplementing the same argument loop.

## diagnostics

`std.diagnostic` gives tools one shape for human-readable and json output.

```pith
import std.diagnostic as diagnostic

diag := diagnostic.with_fix(
    diagnostic.with_span(diagnostic.error("expected expression"), "main.pith", 3, 7, "value ="),
    "add an expression",
)

print(diagnostic.render(diag))
print(diagnostic.render_json(diag))
```

the structs stay simple on purpose: severity, message, optional span, and an
optional fix string. that makes it useful for compiler-adjacent tools without
coupling every caller to compiler internals.

## testing helpers

`std.testing` now includes a few helpers for stdlib and self-hosting tests:

- `assert_eq_text(got, want)` for string comparisons with a compact length hint
- `assert_file_exists(path)` for file checks
- `assert_dir_exists(path)` for directory checks
- `with_temp_dir(prefix, run)` for scoped filesystem tests

prefer these in examples and small helper programs when they make the test
intent clearer than hand-written checks.
