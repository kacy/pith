# docsite

This tool generates the stdlib reference site from the comments in `std/`.

```
make docsite            # writes docs/site/index.html
open docs/site/index.html
```

It needs only Pith and the source tree: no build step, no dependencies, and
no server.

## How it works

Three Pith modules and three asset files:

| File | Role |
| --- | --- |
| `docsite.pith` | Command-line shell |
| `docsite_lib.pith` | Reads `std/**.pith` and extracts the documentation |
| `docsite_render.pith` | Turns extracted modules into markup |
| `assets/shell.html` | Page skeleton with `<!--docsite:*-->` slots |
| `assets/app.css` | One accent, light and dark from the same tokens |
| `assets/app.js` | Routing, filtering, command palette |

The extractor follows the same rules as `pith doc` (`self-host/docgen.pith`):
a module's leading comment block is its header, and the comment lines directly
above a declaration document it. It reads more than the CLI prints, because a
page has room where a terminal listing does not:

- Module constants (`pub NAME := ...`)
- Struct fields and enum variants
- Methods from `impl` blocks, with the trait recorded for trait impls so an
  inherent `read` and the `read` satisfying `Reader` stay separate items

Modules are listed by group, `std` first and then each sub-package such as
`std.net`, and by name within a group.

The renderer emits every module as a real `<section>`. The page is a complete
document before any script runs: with JavaScript off, every module is
visible, browser find searches the whole stdlib, and anchor links work. The
script only hides the inactive sections, filters the sidebar, and drives the
command palette, which builds its index by reading `data-search` off the DOM
instead of from a duplicate JSON blob.

## Adding to it

The site has no content of its own. It is a projection of the source. To
change what a page says, change the comment in `std/`. To change how it looks,
edit the assets and re-run `make docsite`.

The extractor depends on two rules:

- A module's first line reads `# std.name — one-line summary`, with an em dash
  or a plain hyphen.
- A declaration's docs are the comment lines immediately above it, with no
  blank line between. A divider comment (a run of `=` or `-`) ends them the
  way a blank line does, so a section heading above a doc comment stays out
  of it.
