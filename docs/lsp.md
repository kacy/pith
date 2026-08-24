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
  checker, published after a 300ms debounce, with a syntax-only fast
  lane publishing parse errors ahead of it. per file: capped at 20,
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
  compiler fix is mechanically applicable. the gate is the fix text
  rather than the code: a fix beginning `declare with 'mut': mut `
  becomes a WorkspaceEdit inserting `mut ` in front of the binding.
  E216 is what produces that fix today. every other fix the compiler
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
falls out. once the batch of input is drained, and before that wait
begins, the syntax fast lane parses the changed document alone and
publishes any parse errors — see "the syntax fast lane" below for what
it costs and when it takes itself out of the way. setting
`PITH_LSP_NO_DEBOUNCE=1` runs both lanes inline inside each
didOpen/didChange/didSave instead, which is what the transcript tests
rely on for deterministic output.

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
checking the closure, and turning the outcome into publishDiagnostics
notifications. `PITH_LSP_TIMING=2` adds one line per module: what each
import cost to locate, read, lex and parse, and what each module cost
the checker. that is how a slow closure names the module responsible.
it also splits the import phase by activity, so a closure that spends
its time probing the filesystem is told apart from one that spends it
parsing.

measured here on a 2-core host shared with other work. absolute
figures move by a factor of two or more with the load, so what follows
is the share each phase takes, which held steady across runs, with one
quiet run's absolute figures for scale:

| entry file                     | modules | total  | lex+parse | imports | check |
|--------------------------------|---------|--------|-----------|---------|-------|
| tests/lsp/fixtures/clean.pith  |       1 |   1 ms |      0 ms |    0 ms |  1 ms |
| self-host/lsp_state.pith       |      23 | 360 ms |      3 ms |  224 ms | 129 ms|
| self-host/ir_emitter_core.pith |      50 |1300 ms |    137 ms |  299 ms | 864 ms|

what the split says, including two corrections to what this section
claimed before it existed:

- the checker's share tracks the size of the *edited file*, not the
  size of the closure. it is 36% of the mid-size run and 66% of the
  worst-case one, not the 77-88% recorded here before the per-module
  split existed. what changes between those two rows is the entry
  file: 322 lines against 10,128.
- for the worst case the largest single line item is checking the
  edited file itself: 560-585 ms of the 864 ms check phase, 43-45% of
  the whole run. no cache can skip that, because it is the one module
  that changed. per-module caching is bounded by what is left, so it
  takes the worst case from about 1300 ms to about 720 ms, not to tens
  of milliseconds.
- for the mid-size closure the imports phase dominates at 62%, and it
  is lex and parse almost exactly in half (113 ms and 106 ms); locating
  the files and reading them together cost 1.6 ms. a cache over the
  import closure is worth far more here than in the worst case,
  because the edited file is small.

on the worst-case file a further cost sits outside these phases
entirely: full-document sync ships the whole 500 KB buffer in every
didChange, and decoding that frame and copying the text into the
overlay takes 500-1400 ms before any phase above starts. at that size
the protocol costs more than the front end does.

### the syntax fast lane

once a batch of input is drained, the server parses the changed
document on its own and publishes any parse errors, without resolving
imports or running the checker. the full closure check follows on the
debounce and replaces them.

a document that parses cleanly publishes nothing from this lane.
publishing an empty list for it would wipe the last analysis's
findings off the screen and put them back when the closure check
lands, which reads as a flicker; a parse error is different, because
it makes those findings stale by definition.

the lane runs only while parsing the document alone costs less than
the 300 ms debounce it is racing. above that the parse is duplicated
work sitting in front of a full run that parses the same text again,
which pushes the real diagnostics out by however long the file takes
to parse. the cost is measured rather than guessed from file size: the
first change to a document runs the lane and remembers what it cost.
`lsp_state.pith` parses in 3 ms and stays in the lane; the 10,000-line
`ir_emitter_core.pith` parses in about 460 ms and takes itself out
after one measurement.

keystroke to first diagnostic, interleaved A/B against the same server
built without the lane, nine trials each, alternating one edit at a
time:

| closure                  | edit         | before  | after   |
|--------------------------|--------------|---------|---------|
| self-host/lsp_state.pith | syntax error |  409 ms |   18 ms |
| self-host/lsp_state.pith | valid edit   | 1746 ms | 1711 ms |

`ir_emitter_core.pith` is not in the table because it takes itself out
of the lane after its first parse: neither kind of edit there moves,
which is the point of the budget.

the arena makes this safe: `run_parse_only` records the node count on
entry and truncates back to it afterwards, so the type cache, the
definition map and the module ranges still describe the same nodes.
without it, parsing one file to find its syntax errors would leave
completion and signature help, which read the last completed analysis
on purpose, pointing at nodes that are no longer the ones they
described.

### memory over a session

the server does not give an analysis's memory back. every run rebuilds
the node arena and the checker's tables, and rebinding a container to a
fresh empty one strands what the old one held rather than releasing it,
so a long editing session grows without bound. twenty edits in the
23-module closure take a server from 56 mb resident to 679 mb; before
`reset_arena` was changed to empty the node list in place rather than
rebind it, the same twenty took it to 1017 mb. that is 48 mb an edit
down to 31, and the 31 that remain are the thirty tables
`initialize_checker` rebinds and the twenty-one the driver rebinds, all
in the same shape.

the underlying defect is in the emitter, not in any of those call
sites: a program that pushes 64 struct nodes onto a global list and
then rebinds it to `[]` grows to 390 mb over 20,000 rounds and 778 mb
over 40,000, while the same program calling `clear()` sits flat at
2.8 mb over both. until that is fixed, an editor session on a large
closure will eventually be killed for memory, and the fix is worth more
to this server than any of the latency work above.

### what per-module caching would take

caching a module's parse or check result across runs is not a matter
of adding a map. node identity is a position in one global arena that
`run_check_pipeline` resets on every run, and the entry file is parsed
into it first, so an import's node indices are not stable between
runs. `initialize_checker` clears thirty maps and lists keyed on those
indices or on scope ids, which are arena positions too, and the driver
clears twenty-one more. a cache would have to reorder the arena so
imports come first and the edited file last, key every module's node
range on its content, and snapshot and restore all of that checker
state. a single structure missed there leaves stale, wrong diagnostics
rather than a crash.

one piece of it sidesteps the arena entirely. `lex_all` returns a
plain `List[Token]`, no node indices involved, so memoizing it per
import on content is a safe change on its own. it is worth 113 ms of
the mid-size closure's 360 ms and 152 ms of the worst case's 1300 ms.
measure memory before taking it: the closure holds about 1 MB of
imported source, and the server would keep every token of it
resident.

queries never wait on any of this: hover, definition, references and
the rest answer from the last completed analysis, so the figures above
are diagnostic latency only.

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
- analysis blocks the loop for its duration (a millisecond on a
  single-file closure, seconds on the largest ones); incoming messages
  wait in the pipe buffer meanwhile. a snapshot/multi-task split is a
  later, measured change.
- the only incremental step is the syntax fast lane. every full
  analysis re-reads, re-lexes, re-parses and re-checks the whole import
  closure, however little of it changed; see "what per-module caching
  would take" above for what stands in the way.
- editing a module does not re-report its dependents. the server
  analyzes the closure of the changed document, and a dependent picks
  up the change only when it is the changed document itself.
- document sync is full-text only; incremental sync is not offered. on
  a large file the cost of shipping and decoding the whole buffer per
  keystroke exceeds the cost of parsing it.
- the server grows by tens of megabytes per analysis and never gives it
  back, so a long session on a large closure ends in the oom killer.
  see "memory over a session" above.
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
