# The Pith language server

`pith lsp` runs a language server that speaks the Language Server Protocol
over stdio. It is written in Pith, lives in `self-host/`, and reuses the
compiler's own front end, so the diagnostics an editor shows are exactly
what `pith check` reports.

## What v1 supports

- **Lifecycle**: initialize/initialized/shutdown/exit, with
  position-encoding negotiation. UTF-16 is the default, and UTF-8 is used
  when the client offers it in `capabilities.general.positionEncodings`.
- **Document sync**: full-text sync (`textDocumentSync: 1`) via
  didOpen/didChange/didSave/didClose. A didChange entry that carries a
  range anyway is spliced into the text at that range, in order.
- **publishDiagnostics**: parse, import, and type errors from the real
  checker, published after a 300ms debounce. A syntax-only fast lane
  publishes parse errors ahead of it. Per file, diagnostics are capped at
  20, deduplicated by line and code, and sorted by position. A document
  whose problems disappear gets one clearing publish.
- **Hover**: the checker's inferred type for the expression at the
  cursor, as plaintext. On an identifier the text is `name: Type`,
  followed by the doc comment of the fn, struct, enum, or interface it
  resolves to when that declaration has one.
- **documentSymbol**: a flat list of top-level fn, struct, enum, and
  interface declarations.
- **Formatting**: one whole-document edit produced by the same
  formatter as `pith fmt`, or no edits when the text is already clean.
- **Definition**: go-to-definition through the checker's use-site ->
  declaration map. It covers local bindings, function calls, user method
  calls, generic calls, and qualified module calls, across module
  boundaries (the location's URI points into the imported file when the
  declaration lives there).
- **References**: all uses that resolve to the declaration under the
  cursor, sorted by (file, line, column). The declaration itself is
  included when the client sends `includeDeclaration`. The cursor may sit
  on a use or on the declaration line itself.
- **Completion**: triggered on `.` or on demand. After a `.` the
  receiver's type is looked up heuristically and its struct fields and
  methods are offered. Anywhere else, the server offers every binding
  name from the last run plus the language keywords, filtered by the word
  prefix under the cursor. Items are plain `{label, kind}`, capped at 200.
- **Semantic tokens**: full-document only and lexer-based. The document
  is re-lexed alone, so highlighting keeps working mid-edit when the
  text does not parse. The legend is `keyword`, `string`, `number`,
  `comment`, `function`, `type`, `variable`, `operator`, with no
  modifiers. The classification is `std.pith.highlight`'s, the same one
  HTML highlighting uses: an uppercase first letter is a type, a name
  that is declared after `fn` or directly followed by `(` is a
  function, anything else is a variable, and `locked`, `guard`,
  `holding`, `where` and `frozen` are keywords where a name follows
  them. Inside an interpolated string the literal pieces are strings
  and the expressions highlight as code.
- **Signature help**: triggered on `(` and `,`. The innermost
  unclosed `(` on the cursor's line names the callee, top-level
  commas before the cursor pick the active parameter, and the label
  is the declaration's own source line (`name(params) -> Ret`), with
  one parameter entry per top-level comma. Free functions and local
  function bindings resolve; methods do not yet.
- **Inlay hints**: a `: Type` hint after the name of each `x := expr`
  binding in the requested range, from the last run's binding tables.
  A binding whose name cannot be matched to exactly one `name :=` on
  its declaration line gets no hint, which also skips multi-line
  initializers.
- **Rename**: `textDocument/rename` resolves the declaration the way
  references does and returns one WorkspaceEdit covering every
  reference plus the declaration's own identifier, grouped per file
  across the walked module closure. `textDocument/prepareRename`
  answers with the identifier's range, or null when the cursor is not
  on a renameable symbol. A rename to an empty string, or to anything
  that is not a plain identifier (first byte alpha or `_`, the rest
  alphanumeric or `_`), is refused with null.
- **Code actions**: one quickfix per diagnostic in the request whose
  compiler fix is mechanically applicable. The server selects fixes by
  their text, not by diagnostic code: a fix beginning
  `declare with 'mut': mut ` becomes a WorkspaceEdit inserting `mut ` in
  front of the binding. E216 is what produces that fix today. Every
  other fix the compiler attaches is prose advice ("import it from the
  module that defines it", "use ? to unwrap an optional value"). Those
  stay in the diagnostic message and produce no action, because an
  action without an edit is a button that does nothing.
- **Workspace symbols**: `workspace/symbol` lists top-level fn,
  struct, enum, and interface declarations across every file the last
  analysis walked (the analyzed document and its transitive imports),
  filtered by a case-insensitive (ASCII) substring match on the query.
  An empty query matches everything. Results are capped at 200 entries.

## How it works

One loop on the main task reads stdin with a timeout, feeds a frame
reader (`std.lsp.transport`), and handles each message synchronously,
writing every response frame immediately. There are no spawned tasks in
v1: a single task cannot starve itself, and green tasks waiting on fd 0
hit an epoll limitation when stdin is a regular file. Edits do not
touch disk. Document text is routed to the compiler through the
driver's source overlays, so unsaved buffers are checked as they are.

When a document changes, the server marks it dirty and shortens its
read timeout to 300ms. When that timeout passes without further input,
it runs `run_check_pipeline` on the dirty document and publishes the
results. Once the batch of input is drained, and before that wait
begins, the syntax fast lane parses the changed document alone and
publishes any parse errors. See "The syntax fast lane" below for what it
costs and when it turns itself off. Setting
`PITH_LSP_NO_DEBOUNCE=1` runs both lanes inline inside each
didOpen/didChange/didSave instead, which the transcript tests rely on
for deterministic output.

The pipeline keeps the import closure between runs. When the edit was
in the document itself and every module it imports reads as it did, the
run lexes, parses and checks that one document against the imports'
cached state. When the edit changed the document's interface (a
signature, a type, a global; anything but the body of a non-generic
function), every open document whose closure holds it is analyzed next
and its diagnostics republished. See "The closure cache" below.

The modules:

- `self-host/lsp_server.pith`: the loop, routing, and lifecycle
- `self-host/lsp_state.pith`: open documents, URI mapping, and all
  position conversion (LSP 0-based UTF-16/UTF-8 characters vs the
  compiler's 1-based byte columns)
- `self-host/lsp_features.pith`: diagnostics, hover, symbols,
  workspace symbols, code actions, formatting
- `self-host/lsp_navigation.pith`: definition, references,
  completion, rename
- `self-host/lsp_tokens.pith`: semantic tokens, signature help, and
  inlay hints

### Positions and multi-byte text

The server answers `initialize` with `positionEncoding: "utf-16"`, the
protocol's default unit. When the client lists `utf-8` in
`capabilities.general.positionEncodings`, the server picks `utf-8`
instead, and a position's character is then a byte offset within its
line.

The compiler reports 1-based lines and 1-based byte columns. Every
handler converts between the two through `lsp_state.pith`, and no
handler computes an LSP character itself. The conversion:

- Counts UTF-16 code units, so `é`, `€` and `中` are one unit each and
  `𝄞` is two (a surrogate pair)
- Moves a position that lands inside a character back to that
  character's start: between the two halves of a surrogate pair in
  UTF-16, or inside a multi-byte sequence in UTF-8
- Clamps a character past the end of its line to the line's end, and a
  line past the end of the document to the last line
- Ends a CRLF line before its `\r`

Handlers that scan a line byte by byte compare bytes with
`text_has_at` instead of cutting the line with `substring`. `substring`
aborts when an offset falls inside a character, so a cut is only made
at an offset that came from the conversion. Without it, an
inlay-hint request on a line holding a multi-byte character ended the
server (#1070).

The `multibyte_*` transcript cases cover each handler on lines with 4-,
3- and 2-byte characters in front of and between the positions they
use, in both encodings.

## Running it

```
pith lsp
```

Point an editor's generic LSP client at that command with stdio
transport. For example, in Neovim:

```lua
vim.lsp.start({
  name = "pith",
  cmd = { "pith", "lsp" },
  root_dir = vim.fn.getcwd(),
})
```

## Editors

Ready-made setup for Neovim and VS Code, including the minimal
extension in `tooling/editors/vscode/`, is in
[editors.md](editors.md).

## Transcript tests

`tests/lsp/cases/*.jsonl` hold one raw JSON-RPC message per line, with no
framing. `tooling/lsp_check.sh` frames each line with a byte-accurate
Content-Length header, pipes the stream through
`PITH_LSP_NO_DEBOUNCE=1 pith_main lsp`, deframes stdout, replaces the
absolute repo root with `__ROOT__` in both directions, and diffs
against `tests/lsp/expected/<name>.jsonl`. Run them with:

```
make lsp-check        # builds first
make lsp-check-only   # binaries already built
```

`LSP_CHECK_UPDATE=1 ./tooling/lsp_check.sh` regenerates the expected
files. Check every line by eye before freezing a new expectation.

## Analysis performance

`PITH_LSP_TIMING=1` logs one line per analysis on stderr with the wall
time split by phase: lexing the entry file, parsing it, resolving
imports (which reads, lexes, and parses every module in the closure),
checking the closure, and turning the outcome into publishDiagnostics
notifications. `PITH_LSP_TIMING=2` adds one line per module: what each
import cost to locate, read, lex and parse, and what each module cost
the checker. That is how a slow closure names the module responsible.
It also splits the import phase by activity, so a closure that spends
its time probing the filesystem can be told apart from one that spends
it parsing. The line ends with `cache=hit|miss` and the cost of copying
the closure cache's snapshot in or out (`snapshot=`); see "The closure
cache" below.

These figures were measured on a 2-core host shared with other work.
Absolute figures move by a factor of two or more with the load, so what
follows is the share each phase takes, which held steady across runs,
with one quiet run's absolute figures for scale:

| Entry file                     | Modules | Total  | Lex+parse | Imports | Check |
|--------------------------------|---------|--------|-----------|---------|-------|
| tests/lsp/fixtures/clean.pith  |       1 |   1 ms |      0 ms |    0 ms |  1 ms |
| self-host/lsp_state.pith       |      23 | 360 ms |      3 ms |  224 ms | 129 ms|
| self-host/ir_emitter_core.pith |      50 |1300 ms |    137 ms |  299 ms | 864 ms|

What the split says:

- The checker's share tracks the size of the *edited file*, not the
  size of the closure. It is 36% of the mid-size run and 66% of the
  worst-case one. What changes between those two rows is the entry
  file: 322 lines against 10,128.
- For the worst case the largest single line item is checking the
  edited file itself: 560-585 ms of the 864 ms check phase, 43-45% of
  the whole run. No cache can skip that, because it is the one module
  that changed. Per-module caching is bounded by what is left, so it
  can take the worst case from about 1300 ms to about 720 ms, and no
  lower.
- For the mid-size closure the imports phase dominates at 62%, and it
  splits almost exactly in half between lex and parse (113 ms and
  106 ms). Locating the files and reading them together cost 1.6 ms. A
  cache over the import closure is worth far more here than in the
  worst case, because the edited file is small.

On the worst-case file a further cost sits outside these phases
entirely: full-document sync ships the whole 500 KB buffer in every
didChange, and decoding that frame and copying the text into the
overlay takes 500-1400 ms before any phase above starts. At that size
the protocol costs more than the front end does.

### The syntax fast lane

Once a batch of input is drained, the server parses the changed
document on its own and publishes any parse errors, without resolving
imports or running the checker. The full closure check follows on the
debounce and replaces them.

A document that parses cleanly publishes nothing from this lane.
Publishing an empty list for it would wipe the last analysis's
findings off the screen and put them back when the closure check
lands, which reads as a flicker. A parse error is different, because
it makes those findings stale by definition.

The lane runs only while parsing the document alone costs less than
the 300 ms debounce it is racing. Above that, the parse is duplicated
work in front of a full run that parses the same text again, which
delays the real diagnostics by however long the file takes to parse.
The server measures the cost instead of estimating it from file size:
the first change to a document runs the lane and records what it cost.
`lsp_state.pith` parses in 3 ms and stays in the lane; the 10,000-line
`ir_emitter_core.pith` parses in about 460 ms and leaves the lane
after one measurement.

Keystroke to first diagnostic, interleaved A/B against the same server
built without the lane, nine trials each, alternating one edit at a
time:

| Closure                  | Edit         | Before  | After   |
|--------------------------|--------------|---------|---------|
| self-host/lsp_state.pith | Syntax error |  409 ms |   18 ms |
| self-host/lsp_state.pith | Valid edit   | 1746 ms | 1711 ms |

`ir_emitter_core.pith` is not in the table because it leaves the lane
after its first parse, so neither kind of edit there changes. That is
what the budget is for.

The arena makes this safe: `run_parse_only` records the node count on
entry and truncates back to it afterwards, so the type cache, the
definition map and the module ranges still describe the same nodes.
Without it, parsing one file to find its syntax errors would leave
completion and signature help, which read the last completed analysis
on purpose, pointing at nodes that are no longer the ones they
described.

### Memory over a session

Every run rebuilds the node arena and the checker's tables. Assigning
over a global releases the value it replaces (see
[ownership.md](ownership.md)), so the tables `initialize_checker`
and the driver rebind are reclaimed each run, and
`reset_arena` empties the node list in place. A `for` loop over a string
releases the one-byte string it binds each iteration, which the checker's
name helpers and the driver's path helpers do on every lookup;
`tests/leaks/leak_string_chars` pins every exit such a loop has.

Resident set after 1, 10 and 50 analyses, one server driven over stdio
with the same one-line edit repeated, sampled from `/proc` after each
publish, without the closure cache below:

| Closure                       | Modules | 1 / 10 / 50 analyses |
|-------------------------------|---------|----------------------|
| self-host/pith_main.pith      |      53 |  61 / 73 / 74 MB     |
| examples/web_login.pith       |      54 |  77 / 86 / 87 MB     |

Growth past the tenth analysis is under 30 KB an analysis on both. That
is the allocator settling, not a leak. What remains between the first
analysis and the tenth is the working set reaching its size.

### The closure cache

Nearly every analysis finds the import closure exactly as the last one
left it, because the keystroke was in the open document.
`run_check_pipeline` in cache mode (the server turns it on with
`enable_closure_cache`; `pith
check` never does) keeps what the imports produced and redoes only the
entry file.

It keeps three things. First, the imports' nodes stay in the arena,
because the arena is laid out imports first: the entry file is parsed,
its import list is read off its nodes, the nodes are lifted out
(`take_nodes_from`) so the walk parses the imports from index 0, and
the entry's nodes go back at the end with their child indices shifted
(`append_relocated_nodes`). A later run truncates the arena to the
imports' end and parses the new entry text there, so every node index
the checker's tables hold for an import is the index it was. Second,
the driver's post-walk state is copied out (`DriverSnapshot`). Third,
the checker's whole state after the imports were checked is copied out
(`CheckerSnapshot`): the type table, the scope arena, the diagnostics,
the method and interface registries, and every one of the checker
module's own tables, transients included. `initialize_checker` also
clears six associated-type tables between runs, since those hold node
indices from the previous arena.

The cache keys on the entry path, the entry's import list in order, and
the walk itself. Before a hit the driver resolves every recorded import
edge again and reads every module file again through the overlays, and
one different answer is a miss (`walk_is_unchanged`). An edited module,
an edited overlay, a file that appeared on a resolution path where none
was, and an import added to the entry file all miss and rebuild. A miss
costs what a cold run always cost plus one copy of the state; a hit
costs one copy of the state back in plus the entry file. The copy is
the whole of the hit's fixed cost, and the log line reports it as
`snapshot=`.

Keystroke to diagnostics on the two large closures (53 and 54
modules), the same one-line edit repeated 49 times, `PITH_LSP_NO_DEBOUNCE=1`, median of
the edits, measured on a quiet box against a server from the same tree
without the cache:

| Closure                  | Before   | After  | Of which snapshot | Check |
|--------------------------|----------|--------|-------------------|-------|
| self-host/pith_main.pith | 1525 ms  |  65 ms |             25 ms | 13 ms |
| examples/web_login.pith  | 1113 ms  |  51 ms |             34 ms |  3 ms |

The residual is the copy. The 54-module closure's checker state
describes 184,000 nodes, 6,700 types and 17,000 bindings, and copying
it back costs more than checking the 200-line entry file does. The
copy is what keeps the snapshot pristine for the next hit. A journal
that undid the entry file's writes would remove it, at the price of
wrapping every table write in the checker.

The snapshot costs memory: it is a second copy of the tables, so a
session sits about 15 MB higher than without the cache (89 MB against
74 MB after 50 analyses of pith_main.pith, 104 MB against 87 MB for
web_login.pith) and is flat from there.

The cache holds one entry at a time. The arena is one global list, so a
snapshot for a second entry file would need an arena of its own, and
switching between two open documents misses on each switch. A workflow
that edits a module while its dependents are open pays the miss on
every return to the module once a dependent has been analyzed.

#### Dependents

The server keeps, per analyzed document, the module files its closure
covered and a rendering of its interface: every top-level declaration
except tests, with the bodies of non-generic functions and methods left
out (`module_interface_digest`). After an analysis whose rendering
differs from the document's previous one, every other open document
whose closure holds the file is analyzed and republished. A body edit
changes nothing a dependent's check can see, so it re-runs nothing. A
signature change re-runs every open dependent, each as a full run
because the module they import changed. Generic bodies stay in the
rendering because a dependent's check walks them at each instantiation.
Three transcripts pin this: `dependency_signature_edit` (a parameter
added to an imported function reports E207 in the importer and clears
when it is removed), `dependency_body_edit` (a changed return value
publishes for the module alone), and `cached_types` (the type a hover
answers after a cold run, after a cache hit, after the imported
module's return type changed, and after the importer was edited against
the new type).

#### What a per-module cache would still take

The closure cache re-checks the whole closure when any module in it
changes. Skipping the modules that neither changed nor import a changed
module needs what this cache does not have: a checker state that can be
cut at a module boundary. Type ids, scope ids and node indices are all
positions in shared arenas, so a module's registration cannot be lifted
out and dropped back in at another position. The checker would have to
register modules into per-module tables and resolve across them, which
is the registration split the checker does not have today. The cheaper
piece, memoizing `lex_all` per import on content, is now moot for the
hit path (no import is lexed on a hit) and worth 113 ms of a miss on
the mid-size closure.

Queries never wait on any of this: hover, definition, references and
the rest answer from the last completed analysis, so the figures above
are diagnostic latency only.

## Current limitations

- Rename covers what the definition map covers: local bindings,
  parameters, and directly-called functions, structs, enums, and
  interfaces resolved by the checker. It does not rename struct
  fields, module names, or anything inside a string or comment. Some
  symbols have a reference the server cannot pin to the identifier's
  token: methods and module-qualified calls, whose use-def entries
  are keyed on the call node at the call's closing token. Such a symbol
  is refused outright (null from both prepareRename and rename) and is
  never renamed partially, because editing the declaration but not such
  a call site would break the program silently. A binding or parameter
  declaration identifier is found by scanning its declaring line for
  `name:` / `name :=` (falling back to the first whole-word
  occurrence), which can still mis-locate in a pathological line that
  reuses the same `name:` text earlier in a string literal.
- Code actions apply only fixes that name an exact mechanical edit;
  the one shape today is E216's "declare with 'mut'". Prose-advice
  fixes produce no action, and a diagnostic the client echoes back
  after further edits (so it no longer matches the last analysis) is
  ignored.
- The workspace/symbol request reads the last completed analysis as it
  is. It names no document, so it cannot re-analyze, and it only sees
  the analyzed document's import closure, not unrelated files in the
  workspace. Results cap at 200 entries; the substring match is ASCII
  case-insensitive.
- Completion is heuristic and reads the last completed analysis on
  purpose: mid-edit text usually does not parse, so the request never
  re-runs the front end. Right after a change (or when the last run
  failed to parse), the type cache and binding names describe the
  previous text, and member completion can miss or misattribute the
  receiver. The receiver itself is found by walking the line's bytes
  back over identifier, `)`, and `]` characters. Chains through
  string indexing or arbitrary expressions may not resolve, and then
  every known method name is offered instead.
- Definition and references only know what the checker recorded:
  local bindings, function calls, user method calls, generic calls,
  and module-qualified calls. A method or module call is keyed on its
  call node, which sits at the call's first token. The cursor
  resolves such a call from the receiver onward, while the cursor on
  the method name itself resolves the method by name.
- Method declarations inside `impl` blocks are located by their node
  position (the end of the method body), because the declaration-line
  scan only recognizes top-level declarations.
- Analysis blocks the loop for its duration (a millisecond on a
  single-file closure, seconds on the largest ones), and incoming
  messages wait in the pipe buffer meanwhile. A snapshot/multi-task
  split is a later, measured change.
- The closure cache holds one entry file. Switching between two open
  documents misses on each switch, and an edit to a module re-checks
  its whole closure, not only the modules that import it; see "What a
  per-module cache would still take" above.
- Dependents are re-reported only while they are open, and an
  interface change re-runs each of them in full. A dependent that is
  not open picks up the change when it is opened or edited.
- The interface rendering that decides when dependents re-run is
  syntactic and conservative: a private function's signature is part
  of it, so changing one re-runs the dependents although none can see
  it.
- Document sync is full-text only; incremental sync is not offered. On
  a large file the cost of shipping and decoding the whole buffer per
  keystroke exceeds the cost of parsing it.
- One document is analyzed per debounce (the most recently changed
  one), though diagnostics for other open documents that fall out of
  that run are published too.
- Hover positions are approximate at expression granularity: an
  expression node records the position of the token it starts at, and
  the answer comes from the node whose start is nearest at or before
  the cursor. A call starts at its callee or receiver, so hovering
  there answers the call's type, not the receiver's own.
- Semantic tokens never consult the checker, so an identifier's class
  comes from its shape alone. There is no delta or range variant, and
  no token modifiers.
- Signature help resolves free functions and local function bindings
  only (method calls return null), and the scan is confined to the
  cursor's line, so a call whose `(` sits on an earlier line does not
  resolve. A declaration spanning several lines falls back to a label
  rendered from the function's type, which names parameter types but
  not parameters.
