# terminals

pith talks to the terminal through `std.term` and its submodules. the base
module (`std.term`) colors and styles text; this page covers the raw layer
underneath, `std.term.tty`, and the escape parsing and session handling built
on it. the widget layer above these lives in [docs/tui.md](tui.md).

## what the tty layer is

six runtime calls and nothing else: detect a terminal, enter and leave raw
mode, ask the window size, read bytes with a timeout, write bytes without a
newline. everything above them is ordinary pith.

```pith
from std.term.tty import is_tty, raw_enter, restore, size, read, write_str

fn main() -> Int!:
    if not is_tty(0):
        fail "run me on a terminal"
    raw_enter()
    dims := size()!
    write_str("{dims.0}x{dims.1}\r\n")!
    restore()
    return 0
```

## raw mode

`raw_enter()` saves the terminal state and switches stdin to raw mode: no
line buffering, no echo, and no signal keys — ctrl-c arrives as byte `3` for
the program to handle, not as a signal that kills it. `restore()` puts the
saved state back and is safe to call any number of times.

two safety properties are worth knowing:

- **your shell's stdin is never altered.** raw mode changes terminal
  attributes, not file-descriptor flags; the runtime never sets `O_NONBLOCK`
  on stdin, so nothing leaks into the parent shell's view of the descriptor.
  waiting happens in `poll` (parking a green task on the reactor, exactly
  like a socket read), and the read that follows cannot block because raw
  mode delivers one byte at a time and poll just said one is there.
- **restore survives a crash.** entering raw mode arms a process-exit hook
  that restores the saved state and resets the screen (leave the alternate
  screen, show the cursor, reporting off, styles cleared). every runtime
  trap exits through that hook, so a bug mid-frame still returns a usable
  shell. only `SIGKILL` and a hard fault skip it — `reset` at the shell
  recovers from those.

## reading

`read(max, timeout_ms)` returns whatever bytes are available, waiting up to
`timeout_ms` for the first (−1 waits forever). the three outcomes are kept
deliberately distinct:

- **bytes** — input arrived.
- **an empty `Bytes`** — the timeout passed with nothing typed. this is
  normal and frequent: resolving whether a lone `ESC` byte is the escape key
  or the start of an arrow-key sequence is done with a short timed read.
- **failure** — end of file or a read error. the terminal is gone and the
  session is over.

## writing

`write(data)` and `write_str(s)` put bytes on the terminal exactly as given,
looping internally until everything is out. no newline is appended — cursor
positioning depends on sending exactly the bytes meant — which is the
capability `print()` deliberately does not offer. `examples/term_write.pith`
shows three writes forming one line.

## escape sequences

`std.term.ansi` builds the sequences a tui writes — all as plain strings, so
frames compose and snapshot in tests without a terminal anywhere near them:

```pith
import std.term.ansi as ansi

frame := ansi.cursor_to(0, 0) + ansi.clear_line() + ansi.sgr(ansi.rgb(255, 0, 128), ansi.COLOR_DEFAULT, ansi.ATTR_BOLD) + "deep pink" + ansi.reset()
```

coordinates are 0-based and translated to the terminal's 1-based form at the
boundary. colors are one `Int`: `COLOR_DEFAULT`, `0..255` for the indexed
palette, or `rgb(r, g, b)` for 24-bit. mouse reporting is enabled in the
sgr-1006 encoding only, which is the form the input parser will decode.

the same module strips sequences back out. `ansi.strip` (which
`term.strip` now delegates to) walks the real escape grammar — csi to any
final byte, window titles to either terminator, three-byte ss3 sequences —
so asserting on the visible text of a frame works no matter what the frame
contains. `examples/term_strip.pith` shows both directions.

## input events

`std.term.input` decodes the bytes a raw-mode terminal delivers into typed
events with a push parser: feed it whatever a read returned, get back every
event those bytes completed, and let it hold the tail of an unfinished
sequence until more bytes arrive.

```pith
import std.term.input as input

p := input.parser()
for ev in input.feed(p, chunk):
    match ev:
        input.Event.KeyPress(k) =>
            handle(input.key_name(k))   # "a", "ctrl+c", "shift+tab", "f5"
        input.Event.Paste(s) =>
            insert(s)
        _ =>
            pass_on(ev)
```

it decodes printable and control keys, arrows and function keys in both
their csi and ss3 forms with xterm modifier parameters, sgr-encoded mouse
reports on 0-based cells, bracketed paste as one event (the terminator is
matched incrementally, so a paste split across reads still closes), focus
changes, and utf-8 characters split across feeds. `key_name` renders the
canonical names keybinding tables match against.

the one ambiguity bytes cannot resolve is a lone escape: ESC alone is the
escape key, but ESC also starts every sequence. the parser holds it, and
when a read times out while `pending()` is true, `flush()` resolves the
held prefix — a lone esc becomes the escape key, a truncated sequence is
dropped rather than leaked as text. garbage never wedges the state machine.
`examples/term_input_parse.pith` decodes every shape from byte literals.

## size and resize

`size()` returns `(cols, rows)` for the window. it is a point-in-time query;
to track resizes, arm `SIGWINCH` through `std.signal` and re-query when it
fires — or use the session, which bundles that wiring.

## the session

`std.term.session` ties the pieces into one object: raw mode entered, the
requested modes applied (alternate screen and bracketed paste by default,
mouse and focus reporting opt-in), the hardware cursor hidden, the resize
signal armed, and two tasks feeding one ordered event stream.

```pith
import std.term.session as session

fn main() -> Int!:
    t := session.open()!
    defer t.close()
    while true:
        got := t.events.recv()
        if let ev = got:
            match ev:
                Event.KeyPress(k) =>
                    if input.key_name(k) == "q":
                        return 0
                Event.Resize(cols, rows) =>
                    redraw(cols, rows)
                Event.Closed =>
                    return 0
                _ =>
                    continue
    return 0
```

the stream carries keys, mouse, pastes, focus changes, `Resize` with the new
size already queried, `Signal` for extras armed through `Options.signals`,
and one final `Closed` when the terminal goes away. output goes through the
session's buffered `write`/`flush`, one syscall per frame. `close()` undoes
everything in reverse order and is idempotent — call it from a `defer` so it
runs on every exit path.

two things the session deliberately does not do: it does not own time (an
animating application selects over the event channel and its own
`concurrent.ticker`), and it does not share the signal queue — the session
owns all signal interest for the process, so `std.shutdown.on_signals`
cannot run alongside it. see [signals.md](signals.md).
