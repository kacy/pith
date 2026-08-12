# the pith language server

`pith lsp` runs a language server speaking the language server protocol
over stdio. it is written in pith, lives in `self-host/`, and reuses the
compiler's own front end — the diagnostics an editor shows are exactly
what `pith check` reports.

## what v1 supports

- **lifecycle** — initialize/initialized/shutdown/exit, with
  position-encoding negotiation: utf-16 by default, utf-8 when the
  client offers it in `capabilities.general.positionEncodings`.
- **document sync** — full-text sync (`textDocumentSync: 1`) via
  didOpen/didChange/didSave/didClose.
- **publishDiagnostics** — parse, import, and type errors from the real
  checker, published after a 300ms debounce. per file: capped at 20,
  deduplicated by line and code, sorted by position. a document whose
  problems disappear gets one clearing publish.
- **hover** — the checker's inferred type for the expression at the
  cursor, as plaintext.
- **documentSymbol** — a flat list of top-level fn, struct, enum, and
  interface declarations.
- **formatting** — one whole-document edit produced by the same
  formatter as `pith fmt`, or no edits when the text is already clean.

## how it works

one loop on the main task reads stdin with a timeout, feeds a frame
reader (`std.lsp.transport`), and handles each message synchronously,
writing every response frame immediately. there are no spawned tasks in
v1: a single task cannot starve itself, and green tasks waiting on fd 0
hit an epoll limitation when stdin is a regular file. edits do not
touch disk — document text is routed to the compiler through the
driver's source overlays, so unsaved buffers are checked as-is.

when a document changes, the server marks it dirty and shortens its
read timeout to 300ms; when that timeout passes without further input,
it runs `run_check_pipeline` on the dirty document and publishes what
falls out. setting `PITH_LSP_NO_DEBOUNCE=1` runs analysis immediately
inside each didOpen/didChange/didSave instead, which is what the
transcript tests rely on for deterministic output.

the modules:

- `self-host/lsp_server.pith` — the loop, routing, and lifecycle
- `self-host/lsp_state.pith` — open documents, uri mapping, and all
  position conversion (lsp 0-based utf-16/utf-8 characters vs the
  compiler's 1-based byte columns)
- `self-host/lsp_features.pith` — diagnostics, hover, symbols,
  formatting

## running it

```
pith lsp
```

point an editor's generic lsp client at that command with stdio
transport. for example, in neovim:

```lua
vim.lsp.start({
  name = "pith",
  cmd = { "pith", "lsp" },
  root_dir = vim.fn.getcwd(),
})
```

## transcript tests

`tests/lsp/cases/*.jsonl` hold one raw json-rpc message per line, no
framing. `tooling/lsp_check.sh` frames each line with a byte-accurate
content-length header, pipes the stream through
`PITH_LSP_NO_DEBOUNCE=1 pith_main lsp`, deframes stdout, replaces the
absolute repo root with `__ROOT__` in both directions, and diffs
against `tests/lsp/expected/<name>.jsonl`. run them with:

```
make lsp-check        # builds first
make lsp-check-only   # binaries already built
```

`LSP_CHECK_UPDATE=1 ./tooling/lsp_check.sh` regenerates the expected
files; eyeball every line before freezing a new expectation.

## current limitations

- no go-to-definition, references, rename, or completion yet;
  go-to-definition is the next planned feature.
- analysis blocks the loop for its duration (typically 10ms-1.2s);
  incoming messages wait in the pipe buffer meanwhile. a
  snapshot/multi-task split is a later, measured change.
- document sync is full-text only; incremental sync is not offered.
- one document is analyzed per debounce — the most recently changed
  one — though diagnostics for other open documents that fall out of
  that run are published too.
- hover positions are approximate at expression granularity: an ast
  node records the position of the last token that formed it.
