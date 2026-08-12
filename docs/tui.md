# terminal uis

pith's terminal-ui stack lives under `std.term`. the foundation —
raw mode, escape sequences, input events, the session — is documented in
[terminal.md](terminal.md); this page covers the layers an application
builds with: the elm-style runtime and styling now, the widget set next.

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
