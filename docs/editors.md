# Editor setup

`pith lsp` speaks the Language Server Protocol over stdio (see
[lsp.md](lsp.md) for what it supports). Any editor with a generic LSP
client can use it. This page gives the exact setup for Neovim and VS Code.

## Neovim

Put this in your `init.lua`. It teaches Neovim the `pith` filetype and
starts the server. It works on current Neovim (0.11+, via
`vim.lsp.config`) and on 0.10 (via a `vim.lsp.start` autocommand).
`vim.lsp.config` does not exist before 0.11, so an unguarded call to it
errors on older installs:

```lua
vim.filetype.add({ extension = { pith = "pith" } })

local pith_cmd = { "pith", "lsp" }

if vim.lsp.config then
  vim.lsp.config("pith", {
    cmd = pith_cmd,
    filetypes = { "pith" },
    root_markers = { "pith.toml", ".git" },
  })
  vim.lsp.enable("pith")
else
  vim.api.nvim_create_autocmd("FileType", {
    pattern = "pith",
    callback = function(args)
      vim.lsp.start({
        name = "pith",
        cmd = pith_cmd,
        root_dir = vim.fs.root(args.buf, { "pith.toml", ".git" }),
      })
    end,
  })
end
```

Open any `.pith` file inside a project (a directory with a `pith.toml`
or `.git`) and the server works with no further setup: diagnostics,
hover (`K`), go-to-definition, references, completion, document and
workspace symbols, rename, code actions, signature help, inlay hints,
semantic tokens, and formatting (`vim.lsp.buf.format()`).

If `pith` is not on your PATH, put an absolute path in `pith_cmd`.
Pointing it straight at the frontend binary, for example
`{ "/path/to/repo/self-host/pith_main", "lsp" }`, also bypasses the
`pith` wrapper's frontend lookup, which is relative to the working
directory. That matters when the editor is launched outside the repo.

## VS Code

A minimal extension lives in `tooling/editors/vscode/`. It has no build
step. Install its one dependency and run it as a development
extension:

```
cd tooling/editors/vscode
npm install
code --extensionDevelopmentPath="$PWD" /path/to/your/pith/project
```

The extension starts `pith lsp` using the `pith` binary on your PATH,
and the `pith.serverPath` setting overrides that with an absolute path.
See `tooling/editors/vscode/README.md` for details.

## Troubleshooting

- The server logs to stderr. Neovim collects that in the LSP log
  (`:LspLog`), and VS Code shows it in the Output panel under
  "Pith Language Server".
- In Neovim, `:LspInfo` (`:checkhealth vim.lsp` on 0.11+) shows whether
  the client attached and which command it ran.
- If nothing attaches, check that the `pith` binary is on the PATH of
  the editor process. GUI editors often launch with a shorter PATH
  than your shell.

## Syntax highlighting in Neovim

The TextMate grammar serves VS Code and GitHub. Neovim uses a classic
Vim syntax file instead, shipped in `tooling/editors/nvim/` with an
ftdetect rule, so `.pith` files are recognized without any filetype
configuration. Point your runtimepath at it:

```lua
vim.opt.runtimepath:append("/path/to/repo/tooling/editors/nvim")
```

The file highlights keywords, builtin and PascalCase types, strings with
interpolation, numbers in every base, and comments. The grammar in
tooling/highlighting stays the source of truth, so update both when the
language gains a keyword. The language server's semantic tokens will
later layer checker-aware highlighting on top of this base.
