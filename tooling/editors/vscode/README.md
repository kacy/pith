# pith for vs code

syntax highlighting for `.pith` files plus a client for the pith
language server: diagnostics, hover, document symbols, and formatting.

## running from source

there is no build step. install the one dependency, then launch vs code
with this directory as a development extension:

```
cd tooling/editors/vscode
npm install
code --extensionDevelopmentPath="$PWD" /path/to/your/pith/project
```

open a `.pith` file and the extension activates, starting `pith lsp`
over stdio.

## the server executable

the extension runs the `pith` binary from your PATH by default. if it
lives somewhere else, point the `pith.serverPath` setting at it:

```json
{
  "pith.serverPath": "/home/you/code/forge/self-host/pith_main"
}
```

## troubleshooting

the server logs to stderr, which vs code collects in the output panel:
view -> output, then pick "Pith Language Server" from the dropdown. a
spawn failure (wrong `pith.serverPath`, binary not executable) shows up
there too.
