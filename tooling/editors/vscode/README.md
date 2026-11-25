# Pith for VS Code

This extension adds syntax highlighting for `.pith` files and a client for the
Pith language server. The client supports diagnostics, hover, document and
workspace symbols, formatting, go-to-definition, references, completion,
semantic tokens, signature help, inlay hints, rename, and code actions.

## Running from source

There is no build step. Install the one dependency, then launch VS Code
with this directory as a development extension:

```
cd tooling/editors/vscode
npm install
code --extensionDevelopmentPath="$PWD" /path/to/your/pith/project
```

Open a `.pith` file and the extension activates, starting `pith lsp`
over stdio.

## The server executable

The extension runs the `pith` binary from your PATH by default. If it
lives somewhere else, point the `pith.serverPath` setting at it:

```json
{
  "pith.serverPath": "/home/you/code/pith/target/release/pith"
}
```

## Troubleshooting

The server logs to stderr, which VS Code collects in the Output panel:
choose View -> Output, then pick "Pith Language Server" from the dropdown. A
spawn failure (a wrong `pith.serverPath`, or a binary that is not executable)
shows up there too.
