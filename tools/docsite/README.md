# docsite

The stdlib reference site, generated from the comments in `std/`.

```
make docsite            # writes docs/site/index.html
open docs/site/index.html
```

Nothing but pith and the source tree goes into it — no build step, no
dependencies, no server.

## How it works

Three pith modules and three asset files:

| file | role |
| --- | --- |
| `docsite.pith` | command-line shell |
| `docsite_lib.pith` | reads `std/**.pith` and extracts the documentation |
| `docsite_render.pith` | turns extracted modules into markup |
| `assets/shell.html` | page skeleton with `<!--docsite:*-->` slots |
| `assets/app.css` | one accent, light and dark from the same tokens |
| `assets/app.js` | routing, filtering, command palette |

The extractor follows the same rules as `pith doc` (`self-host/docgen.pith`):
a module's leading comment block is its header, and the comment lines directly
above a declaration document it. It reads more than the CLI prints, because a
page has room where a terminal listing does not:

- module constants (`pub NAME := ...`)
- struct fields and enum variants
- methods from `impl` blocks, with the trait recorded for trait impls so an
  inherent `read` and the `read` satisfying `Reader` stay separate items

The renderer emits every module as a real `<section>`. The page is a complete
document before any script runs: with JavaScript off, every module is
visible, browser find searches the whole stdlib, and anchor links work. The
script only hides the inactive sections, filters the sidebar, and drives the
command palette, which builds its index by reading `data-search` off the DOM
rather than from a duplicate JSON blob.

## Adding to it

The site has no content of its own — it is a projection of the source. To
change what a page says, change the comment in `std/`. To change how it looks,
edit the assets and re-run `make docsite`.

Two rules the extractor depends on:

- a module's first line reads `# std.name — one-line summary`, with an em dash
  or a plain hyphen
- a declaration's docs are the comment lines immediately above it, with no
  blank line between
