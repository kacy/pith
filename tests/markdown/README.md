# Markdown spec examples

## CommonMark

`commonmark-0.31.2.json` holds the 652 examples of the CommonMark spec,
version 0.31.2, as published at https://spec.commonmark.org/0.31.2/spec.json.
The file is unchanged. Each entry has the Markdown, the HTML the spec expects,
the example number and the spec section it comes from.

The spec and its examples are by John MacFarlane and are licensed under the
Creative Commons Attribution-ShareAlike 4.0 International license
(CC BY-SA 4.0), https://creativecommons.org/licenses/by-sa/4.0/.

`tests/cases/test_markdown_commonmark.pith` runs every example through
`std.markdown` and compares the output with the expected HTML.

## GitHub Flavored Markdown

`gfm-0.29-spec.txt` is the GitHub Flavored Markdown spec, version 0.29-gfm,
from the cmark-gfm repository's test directory:
https://raw.githubusercontent.com/github/cmark-gfm/master/test/spec.txt.
Its extension examples are the ones marked `example table`,
`example strikethrough`, `example autolink`, `example tagfilter` and
`example disabled` (the two task list examples); the rest are the CommonMark
0.29 examples.

`cmark-gfm-extensions.txt` is cmark-gfm's extension test file, which covers
tables, strikethrough, autolinks, the tag filter, footnotes and task lists:
https://raw.githubusercontent.com/github/cmark-gfm/master/test/extensions.txt.

Both files are unchanged and use the spec's example format, in which `→`
stands for a tab. Both are licensed under CC BY-SA 4.0, as their front matter
states. The GFM spec builds on John MacFarlane's CommonMark spec, and the
extension test file names Yuki Izumi as its author.

`tests/cases/test_markdown_gfm.pith` runs the extension examples of both
files through `std.markdown` with the GFM extensions on and compares the
output with the expected HTML.
