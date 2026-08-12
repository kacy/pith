# terminal uis

pith's terminal-ui stack lives under `std.term`. the foundation —
raw mode, escape sequences, input events, the session — is documented in
[terminal.md](terminal.md); this page covers the layers an application
builds with. it grows as the stack lands: styling and layout are here now,
the widget set and the application runtime arrive next.

## styling and layout

`std.term.style` styles blocks of text and arranges them, all as pure
string functions. a style is built once and applied with `render`; every
builder returns a new style, so a base can be shared and specialised:

```pith
import std.term.style as style

accent := style.new().fg(6).bold()
card := accent.pad(1).border(style.BORDER_ROUNDED)

print(card.render("orders\n1,204"))
```

the box model applies inner to outer: colors and attributes on the text,
then padding, then the border, then the margin. alignment (`LEFT`,
`CENTER`, `RIGHT`) positions lines inside the content width, and
`min_width`/`min_height` give a block a floor size.

blocks compose with three arrangers:

- `join_h(blocks, valign)` — side by side, aligned `TOP`, `MIDDLE`, or
  `BOTTOM` against the tallest.
- `join_v(blocks, halign)` — stacked, aligned against the widest.
- `place(block, w, h, halign, valign)` — positioned inside a box, the
  workhorse for putting a widget in a pane.

`measure(block)` reports the visible size as `(columns, lines)`.

two properties make layouts dependable. geometry is display columns via
`std.text.width`, never bytes — 日本 measures 4 columns, so borders stay
straight around cjk text and emoji. and measurement strips escape
sequences first, so a styled block and its plain twin lay out identically —
which also means every layout is testable with `assert_eq` on a string,
no terminal required. `examples/tui_style.pith` renders a small dashboard
this way.
