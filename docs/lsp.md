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

the pipeline keeps the import closure between runs: when the edit was
in the document itself and every module it imports reads as it did, the
run lexes, parses and checks that one document against the imports'
cached state. when the edit changed the document's interface (a
signature, a type, a global; anything but the body of a non-generic
function), every open document whose closure holds it is analyzed next
and its diagnostics republished. see "the closure cache" below.

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
parsing. the line ends with `cache=hit|miss` and the cost of copying
the closure cache's snapshot in or out (`snapshot=`); see "the closure
cache" below.

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
the node arena and the checker's tables, and for a long time it also
stranded them: an assignment over a global released nothing, so `xs =
{}` kept every entry the old table held. twenty edits in the 23-module
closure took a server to 679 mb that way, and to 1017 mb before
`reset_arena` was changed to empty the node list in place rather than
rebind it.

the assignment releases the outgoing value now. the same twenty edits
peak at 284 mb, measured interleaved against a server built from the
same tree without that release, which came back at 672 mb. one edit
costs 57 mb rather than 87, so the slope across the remaining nineteen
is 12 mb an edit rather than 31. the thirty tables `initialize_checker`
rebinds and the twenty-one the driver rebinds all reclaim.

what was left per edit was one emitter defect, found by diffing a
full valgrind leak check after five analyses against one after two: `for ch in s`
binds a fresh one-byte string every iteration (`char_at` allocates it,
where a list loop's get borrows the element) and nothing released it.
the checker walks module paths that way in its name helpers on every
lookup, and the driver in every path helper, so the closure leaked
about 60,000 one-byte strings an analysis before the arena or a table
was counted. the loop owns that string now (see the `for` rule in
[ownership.md](ownership.md)), and `tests/leaks/leak_string_chars`
pins every exit a string loop has.

resident set after 1, 10 and 50 analyses, one server driven over stdio
with the same one-line edit repeated, sampled from `/proc` after each
publish:

| closure                       | modules | before (1 / 10 / 50)  | after (1 / 10 / 50)  |
|-------------------------------|---------|-----------------------|----------------------|
| self-host/pith_main.pith      |      53 |  65 / 116 / 317 mb    |  61 / 73 / 74 mb     |
| examples/web_login.pith       |      54 |  88 / 191 / 649 mb    |  77 / 86 / 87 mb     |

the slope was 5 mb an analysis on the first closure and 11.5 mb on the
second; it is under 30 kb an analysis on both now, which is the
allocator settling rather than a leak. what remains between the first
analysis and the tenth is the working set reaching its size.

### the closure cache

nearly every analysis finds the import closure exactly as the last one
left it: the keystroke was in the open document. `run_check_pipeline`
in cache mode (the server turns it on with `enable_closure_cache`; `pith
check` never does) keeps what the imports produced and re-does only the
entry file.

it keeps three things. the imports' nodes stay in the arena,
because the arena is laid out imports first: the entry file is parsed,
its import list is read off its nodes, the nodes are lifted out
(`take_nodes_from`) so the walk parses the imports from index 0, and
the entry's nodes go back at the end with their child indices shifted
(`append_relocated_nodes`). a later run truncates the arena to the
imports' end and parses the new entry text there, so every node index
the checker's tables hold for an import is the index it was. the
driver's post-walk state is copied out (`DriverSnapshot`). and the
checker's whole state after the imports were checked is copied out
(`CheckerSnapshot`): the type table, the scope arena, the diagnostics,
the method and interface registries, and every one of the checker
module's own tables, transients included. `initialize_checker` also
now clears six associated-type tables it used to leave alone between
runs; those held node indices from the previous arena.

what it keys on: the entry path, the entry's import list in order, and
the walk itself. before a hit the driver resolves every recorded import
edge again and reads every module file again through the overlays, and
one different answer misses (`walk_is_unchanged`). an edited module, an
edited overlay, a file that appeared on a resolution path where none
was, an import added to the entry file: all miss and rebuild. a miss
costs what a cold run always cost plus one copy of the state; a hit
costs one copy of the state back in plus the entry file. the copy is
the whole of the hit's fixed cost and the log line reports it as
`snapshot=`.

keystroke to diagnostics on the two large closures (53 and 54
modules), the same one-line edit repeated 49 times, `PITH_LSP_NO_DEBOUNCE=1`, median of
the edits, measured on a quiet box against a server from the same tree
without the cache:

| closure                  | before   | after  | of which snapshot | check |
|--------------------------|----------|--------|-------------------|-------|
| self-host/pith_main.pith | 1525 ms  |  65 ms |             25 ms | 13 ms |
| examples/web_login.pith  | 1113 ms  |  51 ms |             34 ms |  3 ms |

the residual is the copy. the 54-module closure's checker state
describes 184,000 nodes, 6,700 types and 17,000 bindings, and copying
it back costs more than checking the 200-line entry file does. the
copy is what keeps the snapshot pristine for the next hit; a journal
that undid the entry file's writes instead would remove it, at the
price of wrapping every table write in the checker.

memory: the snapshot is a second copy of the tables, so a session sits
about 15 mb higher than without the cache (89 mb against 74 mb after
50 analyses of pith_main.pith, 104 mb against 87 mb for web_login.pith)
and is flat from there.

one entry at a time. the arena is one global list, so a snapshot for a
second entry file would need an arena of its own, and switching
between two open documents misses on each switch. a workflow that edits
a module while its dependents are open pays the miss on every return to
the module once a dependent has been analyzed.

#### dependents

the server keeps, per analyzed document, the module files its closure
covered and a rendering of its interface: every top-level declaration
except tests, with the bodies of non-generic functions and methods left
out (`module_interface_digest`). after an analysis whose rendering
differs from the document's previous one, every other open document
whose closure holds the file is analyzed and republished. a body edit
changes nothing a dependent's check can see, so it re-runs nothing; a
signature change re-runs every open dependent, each as a full run
because the module they import changed. generic bodies stay in the
rendering because a dependent's check walks them at each instantiation.
three transcripts pin this: `dependency_signature_edit` (a parameter
added to an imported function reports E207 in the importer and clears
when it is removed), `dependency_body_edit` (a changed return value
publishes for the module alone), and `cached_types` (the type a hover
answers after a cold run, after a cache hit, after the imported
module's return type changed, and after the importer was edited against
the new type).

#### what a per-module cache would still take

the closure cache re-checks the whole closure when any module in it
changes. skipping the modules that neither changed nor import a changed
module needs what this cache does not have: a checker state that can be
cut at a module boundary. type ids, scope ids and node indices are all
positions in shared arenas, so a module's registration cannot be lifted
out and dropped back in at another position; the checker would have to
register modules into per-module tables and resolve across them, which
is the registration split the checker does not have today. the cheaper
piece, memoizing `lex_all` per import on content, is now moot for the
hit path (no import is lexed on a hit) and worth 113 ms of a miss on
the mid-size closure.

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
- the closure cache holds one entry file. switching between two open
  documents misses on each switch, and an edit to a module re-checks
  its whole closure, not only the modules that import it; see "what a
  per-module cache would still take" above.
- dependents are re-reported only while they are open, and an
  interface change re-runs each of them in full. a dependent that is
  not open picks up the change when it is opened or edited.
- the interface rendering that gates dependents is syntactic and
  conservative: a private function's signature is part of it, so
  changing one re-runs the dependents although none can see it.
- document sync is full-text only; incremental sync is not offered. on
  a large file the cost of shipping and decoding the whole buffer per
  keystroke exceeds the cost of parsing it.
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
