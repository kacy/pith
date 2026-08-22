# terminal uis

pith's terminal-ui stack lives under `std.term`. the foundation —
raw mode, escape sequences, input events, the session — is documented in
[terminal.md](terminal.md); this page covers the layers an application
builds with: the elm-style runtime, styling and layout, and the widget
set.

## the application runtime

`std.term.app` is the elm architecture: an application is a struct with a
message type, an event handler, an update, and a view — the runtime owns
the session, the loop, timers, and repainting only changed lines.

```pith
import std.term.app as app
from std.term.app import App, Cmd
import std.term.input as input
from std.term.input import Event

enum Msg:
    Inc
    Halt

struct Counter:
    mut count: Int

impl App for Counter:
    type Msg = Msg
    fn init() -> Cmd[Msg]:
        return app.nothing[Msg]()
    fn on_event(ev: Event) -> Cmd[Msg]:
        match ev:
            Event.KeyPress(k) =>
                if input.key_name(k) == "+":
                    return self.update(Msg.Inc)
                if input.key_name(k) == "q":
                    return self.update(Msg.Halt)
                return app.nothing[Msg]()
            _ =>
                return app.nothing[Msg]()
        return app.nothing[Msg]()
    fn update(msg: Msg) -> Cmd[Msg]:
        match msg:
            Msg.Inc =>
                self.count = self.count + 1
            Msg.Halt =>
                return app.quit[Msg]()
        return app.nothing[Msg]()
    fn view() -> String:
        return "count: {self.count}"

fn main() -> Int!:
    return app.run(Counter(count: 0))!
```

`on_event` owns the event-to-message dispatch — usually a match that calls
`update` — because the message type is the application's own business; the
runtime only ever sees commands. import `App` and `Cmd` unqualified, as
shown: an impl names its interface bare, and a cross-module generic
annotation needs the unqualified name.

### commands

`update` and `view` are total. side effects are command values the runtime
interprets: `nothing[Msg]()`, `quit[Msg]()`, `emit(m)` (a follow-up message
this turn), `perform(f)` (deferred work whose return value becomes a
message — fold errors into the message, the closure must be total),
`after(ms, f)` and `every(ms, f)` (timers building a message from the
clock), and `batch([...])`. zero-argument constructors name the message
type explicitly — the parameter cannot be inferred from nothing.

this runtime is single-tasked: command work runs between frames and timers
fire from the event loop, so a `perform` closure that blocks holds the
next frame back exactly as long as it runs. keep them quick.

### testing an application

`drive(application, events)` runs the same cycle headless over a scripted
event list and returns every frame plus whether the application quit —
deterministic, no terminal, `assert_eq` on strings.
`examples/tui_counter.pith` is exactly that shape, and its output is a ci
golden.

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

## widgets

`std.term.ui` is the widget set. every widget renders to a plain `String`,
takes its geometry in display columns, and mutates only through `key()`,
`advance()`, or an explicit setter — so a whole dashboard prints
deterministically, and every widget doubles as a snapshot test.
`examples/tui_widgets.pith` renders one of everything as a ci golden.

### text and indicators

- `wrap(text, cols)` word-fills to a width; a word wider than the box
  breaks at a character boundary, never mid-cluster.
- `span(text)` / `styled(text, st)` + `paragraph(spans, cols)` — styled
  runs that keep their styling across line breaks.
- `block().with_title("items").render(content)` draws a border around
  content; a title wider than the content widens the box rather than
  truncating.
- `progress(cols)` renders a clamped ratio: `bar.view(0.4)`.
- `spinner()` steps braille frames with `advance()` — drive it from
  `app.every`, one message per tick.
- `tabs(titles, active, cols)` highlights the active tab and ellipsizes
  on overflow.
- `help_short(bindings, cols)` / `help_full(bindings, cols)` render the
  same `app.Binding` list the application matches keys with — the help
  bar can never drift from the actual keymap.

### scrolling and selection

components with cursors embed in the model and mutate in place. the
application forwards a key press and then reads whatever state it needs —
widgets never produce commands:

```pith
items := ui.list_view(["build", "test", "deploy"], 4)
items.key(k)                  # j/k, arrows, home/end, pgup/pgdn
picked := items.selected()    # String? — none when empty
```

- `viewport(cols, rows)` scrolls any text: `set_content`, `scroll_by`,
  `to_bottom`, `at_bottom()` (for follow-the-log), `percent()`, and
  `key()` for the usual motions. `view()` is always exactly `rows` lines.
- `list_view(items, rows)` keeps the window invariant `offset <= cursor <
  offset + rows`, so the selection is always visible.
- `table(headers, rows_data, body_rows)` sizes columns by display width —
  cjk cells pad to equal columns — fills ragged rows with empty cells,
  and scrolls its body with the same keys.
- `focus_ring(count)` tracks which pane is active: `next()`/`prev()`, and
  the application routes keys to the widget at `ring.active`.

### text entry

- `text_input(cols)` is a single-line field: cursor position is counted
  in characters and rendered in columns, horizontal scrolling follows the
  cursor, the cursor cell renders reverse-video (the session hides the
  hardware cursor), and `paste()` accepts bracketed paste. a masked mode
  covers passwords.
- `textarea(cols, rows)` is the multi-line editor: enter splits,
  backspace and delete join lines, vertical motion remembers the column
  it started in, page keys move by a screenful, multi-line paste, and
  both scroll axes follow the cursor. deliberately not in v1: undo,
  selection, clipboard, soft-wrap, word motions, and mouse support.

focus is a convention shared by the interactive widgets: `focus()` /
`blur()` toggle whether `key()` responds and whether the cursor draws.
a form is a `FocusRing` plus widgets, with tab cycling in `update`:

```pith
DemoMsg.Cycle =>
    self.ring.next()
    ...blur all, focus the one at self.ring.active...
DemoMsg.Forward(k) =>
    ...key(k) on the focused widget...
```

`examples/tui_demo.pith` is the full composition — tabs, list, input,
textarea, spinner, and a help bar with tab-cycled focus — meant to be run
on a real terminal: `pith run examples/tui_demo.pith`.

## limitations

- the renderer diffs whole lines, not cells: a one-character change
  repaints that line. fine in practice; a cell-buffer layer would be an
  addition, not a rewrite.
- a runtime trap mid-frame restores the terminal (the raw-mode atexit
  hook), but SIGKILL or a segfault cannot — run `reset` if a killed
  program leaves the terminal raw.
- truecolor is available through `ansi.rgb()`, but the default palette
  sticks to indexed colors; there is no terminal capability detection.
- the session owns the process-global signal queue, so an open session
  cannot coexist with `std.shutdown.on_signals` (see
  [signals.md](signals.md)).
- textarea scope is listed above; east asian ambiguous characters measure
  narrow.
