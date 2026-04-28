# security & reliability

pith is designed to never panic or crash, regardless of input. this
document summarizes the safeguards in the bootstrap compiler.

## the no-panic guarantee

the compiler contains zero `@panic` calls. all error paths return
structured diagnostics instead of crashing. force-unwraps (`.?`) are
restricted to test code where a panic IS the test failure.

## input validation

- **file size**: source files are capped at 10 MiB (`max_source_size`
  in main.zig). prevents accidental reads of large binary files.
- **UTF-8**: source is validated as UTF-8 before lexing. invalid bytes
  produce a clear error message.

## resource limits

the compiler caps recursion and nesting to prevent stack overflow from
pathological inputs:

| limit | value | location |
|---|---|---|
| expression nesting depth | 256 | parser.zig `max_depth` |
| type resolution depth | 128 | checker.zig `max_resolve_depth` |
| string interpolation segments | 64 | lexer.zig `max_interpolation_depth` |
| indentation levels | 256 | lexer.zig `max_indent_level` |

all limits produce clear error messages when hit.

## container security

the dockerfile runs the pith binary as a non-root user (`pith`,
uid 1000) in the final stage.

## error handling convention

diagnostic emit calls use `catch {}` — if adding an error message
fails (OOM), the message is lost but compilation still fails. this is
documented in checker.zig (lines 12-14) and is an intentional
trade-off: losing a diagnostic under extreme memory pressure is
acceptable since the compilation will still report failure.

## reporting vulnerabilities

if you find a way to crash the compiler, please open an issue. crashes
are bugs, not features.
