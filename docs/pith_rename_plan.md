# Pith rename plan

This file keeps the language rename to Pith easy to resume. The rename is
intentionally broad: public docs, source extensions, command names, generated C
symbols, runtime APIs, package metadata, and build output paths should all use
Pith naming.

## Current target

- Language name: Pith
- Source extension: `.pith`
- CLI command: `pith`
- Package manifest: `pith.toml`
- Build directory: `.pith-build`
- Public C/runtime prefix: `pith_`
- Generated user symbol prefix: `pith_`
- Environment prefix: `PITH_`

## Migration phases

1. Rename tracked source files to `.pith`.
2. Rename public files and entrypoints that still use the legacy name.
3. Update docs, examples, tests, CI, Make targets, Cargo metadata, and editor tooling.
4. Update compiler import resolution, package discovery, generated C names, runtime
   symbols, and self-hosted tool paths.
5. Build the Rust bootstrap CLI, then use it to check or rebuild the self-hosted
   compiler from `.pith` sources.
6. Run the standard example/test suite after the new `pith` command can compile
   itself.

## Compatibility notes

For this first pass, the repo should prefer the new names everywhere. If we later
want a compatibility window, add it deliberately with tests:

- Accept the legacy source extension only as an explicit fallback.
- Keep the legacy command as a deprecated wrapper around `pith`.
- Keep the legacy manifest name only when `pith.toml` is absent.

Do not add those fallbacks casually. The clean rename is easier to reason about,
and compatibility should have a clear removal date.
