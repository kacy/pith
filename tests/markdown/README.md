# commonmark spec examples

`commonmark-0.31.2.json` holds the 652 examples of the CommonMark spec,
version 0.31.2, as published at https://spec.commonmark.org/0.31.2/spec.json.
The file is unchanged. Each entry has the markdown, the html the spec expects,
the example number and the spec section it comes from.

The spec and its examples are by John MacFarlane and are licensed under the
Creative Commons Attribution-ShareAlike 4.0 International license
(CC BY-SA 4.0), https://creativecommons.org/licenses/by-sa/4.0/.

`tests/cases/test_markdown_commonmark.pith` runs every example through
`std.markdown` and compares the output with the expected html.
