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
- **definition** — go-to-definition through the checker's use-site ->
  declaration map: local bindings, function calls, user method calls,
  generic calls, and qualified module calls, across module boundaries
  (the location's uri points into the imported file when the
  declaration lives there).
- **references** — all uses resolving to the declaration under the
  cursor, sorted by (file, line, column); the declaration itself joins
  when the client sends `includeDeclaration`. the cursor may sit on a
  use or on the declaration line itself.
- **completion** — triggered on `.` or on demand. after a `.` the
  receiver's type is looked up heuristically and its struct fields and
  methods are offered; anywhere else, every binding name from the last
  run plus the language keywords, filtered by the word prefix under
  the cursor. plain `{label, kind}` items, capped at 200.
- **semantic tokens** — full-document only, lexer-based: the document
  is re-lexed alone, so highlighting keeps working mid-edit when the
  text does not parse. the legend is `keyword`, `string`, `number`,
  `comment`, `function`, `type`, `variable`, `operator`, with no
  modifiers. identifiers classify by shape: an uppercase first letter
  is a type, a directly following `(` makes a function, anything else
  is a variable. an interpolated string highlights as one string
  token spanning the whole literal.
- **signature help** — triggered on `(` and `,`. the innermost
  unclosed `(` on the cursor's line names the callee, top-level
  commas before the cursor pick the active parameter, and the label
  is the declaration's own source line (`name(params) -> Ret`), with
  one parameter entry per top-level comma. free functions and local
  function bindings resolve; methods do not yet.
- **inlay hints** — a `: Type` hint after the name of each `x := expr`
  binding in the requested range, from the last run's binding tables.
  a binding whose name cannot be matched to exactly one `name :=` on
  its declaration line is skipped rather than guessed at, which also
  skips multi-line initializers.
- **rename** — `textDocument/rename` resolves the declaration the way
  references does and returns one WorkspaceEdit covering every
  reference plus the declaration's own identifier, grouped per file
  across the walked module closure. `textDocument/prepareRename`
  answers with the identifier's range, or null when the cursor is not
  on a renameable symbol. a rename to an empty string or anything that
  is not a plain identifier (first byte alpha or `_`, the rest
  alphanumeric or `_`) is refused with null.
- **code actions** — one quickfix per diagnostic in the request whose
  compiler fix is mechanically applicable. today that is exactly
  E216's "declare with 'mut'", which returns a WorkspaceEdit inserting
  `mut ` in front of the binding. every other fix the compiler
  attaches is prose advice ("import it from the module that defines
  it", "use ? to unwrap an optional value"); those stay in the
  diagnostic message and produce no action, because an action without
  an edit is a button that does nothing.
- **workspace symbols** — `workspace/symbol` lists top-level fn,
  struct, enum, and interface declarations across every file the last
  analysis walked (the analyzed document and its transitive imports),
  filtered by a case-insensitive (ascii) substring match on the query;
  an empty query matches everything. capped at 200 entries.

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
  workspace symbols, code actions, formatting
- `self-host/lsp_navigation.pith` — definition, references,
  completion, rename
- `self-host/lsp_tokens.pith` — semantic tokens, signature help, and
  inlay hints

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

## editors

ready-made wiring for neovim and vs code — including the minimal
extension in `tooling/editors/vscode/` — is in
[editors.md](editors.md).

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

## analysis performance

`PITH_LSP_TIMING=1` logs one line per analysis on stderr with the wall
time split by phase: lexing the entry file, parsing it, resolving
imports (which reads, lexes, and parses every module in the closure),
and checking the closure.

measured on this repository (three runs per file, variance under five
percent, quiet 2-core host):

| entry file                      | total   | lex+parse | imports | check   | modules |
|---------------------------------|---------|-----------|---------|---------|---------|
| small fixture                   |   47 ms |      0 ms |   23 ms |   24 ms |       4 |
| std/term/ui.pith                |  234 ms |     18 ms |   96 ms |  120 ms |      19 |
| self-host/checker.pith          |  772 ms |     88 ms |   87 ms |  597 ms |      17 |
| std/net/http.pith               | 2268 ms |     60 ms |  312 ms | 1896 ms |      36 |
| self-host/ir_emitter_core.pith  | 3438 ms |    118 ms |  279 ms | 3042 ms |      50 |

what the split says about incrementality:

- the checker is 77-88% of every closure large enough to hurt, so
  checking is the only phase worth making incremental. a lex cache
  keyed by content hash — the candidate this instrumentation was built
  to judge — could save at most the imports phase, under a tenth of
  the worst total, and is rejected on those numbers.
- queries never wait on any of this: hover, definition, references,
  and the rest answer from the last completed analysis, so the split
  above is diagnostic latency only.
- the cheap next step the numbers do justify is a syntax-only fast
  lane: lexing and parsing one edited file costs tens of milliseconds
  even for the largest files, so parse errors could publish almost
  immediately with full diagnostics following. checker-level
  incrementality (per-module result caching) is the real lever and a
  design of its own.

## current limitations

- rename covers what the definition map covers: local bindings,
  parameters, and directly-called functions, structs, enums, and
  interfaces resolved by the checker. it does not rename struct
  fields, module names, or anything inside a string or comment. a
  symbol with a reference the server cannot pin to the identifier's
  token — methods and module-qualified calls, whose use-def entries
  are keyed on the call node at the call's closing token — is refused
  outright (null from both prepareRename and rename) rather than
  renamed partially, because editing the declaration but not such a
  call site would break the program silently. a binding's declaration
  identifier is found by scanning its declaring line for the first
  whole-word occurrence of the name, which picks the wrong occurrence
  in the pathological case of a declaration line reusing the name
  earlier as a different symbol.
- code actions apply only fixes that name an exact mechanical edit;
  the one shape today is E216's "declare with 'mut'". prose-advice
  fixes produce no action, and a diagnostic the client echoes back
  after further edits (so it no longer matches the last analysis) is
  ignored.
- workspace/symbol reads the last completed analysis as-is — it names
  no document, so it cannot re-analyze — and only sees the analyzed
  document's import closure, not unrelated files in the workspace.
  results cap at 200 entries; the substring match is ascii
  case-insensitive.
- completion is heuristic and reads the last completed analysis, on
  purpose: mid-edit text usually does not parse, so the request never
  re-runs the front end. right after a change (or when the last run
  failed to parse), the type cache and binding names describe the
  previous text and member completion can miss or misattribute the
  receiver. the receiver itself is found by walking the line's bytes
  back over identifier, `)`, and `]` characters — chains through
  string indexing or arbitrary expressions may not resolve, in which
  case every known method name is offered instead.
- definition and references only know what the checker recorded:
  local bindings, function calls, user method calls, generic calls,
  and module-qualified calls. a method or module call is keyed on its
  call node, which sits at the call's last token — the cursor
  resolves such a call from its closing token, while the cursor on
  the method name itself resolves the receiver.
- a local binding's declaration node is its initializer's last token,
  so definition and the includeDeclaration entry point at the
  initializer on the binding line, not at the name.
- method declarations inside `impl` blocks are located by their node
  position (the end of the method body), because the declaration-line
  scan only recognizes top-level declarations.
- analysis blocks the loop for its duration (typically 10ms-1.2s);
  incoming messages wait in the pipe buffer meanwhile. a
  snapshot/multi-task split is a later, measured change.
- document sync is full-text only; incremental sync is not offered.
- one document is analyzed per debounce — the most recently changed
  one — though diagnostics for other open documents that fall out of
  that run are published too.
- hover positions are approximate at expression granularity: an ast
  node records the position of the last token that formed it.
- semantic tokens never consult the checker, so an identifier's class
  comes from its shape alone; there is no delta or range variant, and
  no token modifiers.
- signature help resolves free functions and local function bindings
  only — method calls return null — and the scan is confined to the
  cursor's line, so a call whose `(` sits on an earlier line does not
  resolve. a declaration spanning several lines falls back to a label
  rendered from the function's type, which names parameter types but
  not parameters.
