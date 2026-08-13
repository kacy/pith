# editor setup

`pith lsp` speaks the language server protocol over stdio (see
[lsp.md](lsp.md) for what it supports). any editor with a generic lsp
client can use it; here is the exact wiring for neovim and vs code.

## neovim

put this in your `init.lua`. it teaches neovim the `pith` filetype and
starts the server; it works on both current neovim (0.11+, via
`vim.lsp.config`) and 0.10 (via a `vim.lsp.start` autocommand) —
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

open any `.pith` file inside a project (a directory with a `pith.toml`
or `.git`) and diagnostics, hover (`K`), document symbols, and
formatting (`vim.lsp.buf.format()`) work out of the box.

if `pith` is not on your PATH, put an absolute path in `pith_cmd`.
pointing it straight at the frontend binary — for example
`{ "/path/to/repo/self-host/pith_main", "lsp" }` — also sidesteps the
`pith` wrapper's working-directory-relative frontend lookup, which
matters when the editor is launched outside the repo.

## vs code

a minimal extension lives in `tooling/editors/vscode/`. it has no build
step — install its one dependency and run it as a development
extension:

```
cd tooling/editors/vscode
npm install
code --extensionDevelopmentPath="$PWD" /path/to/your/pith/project
```

the extension starts `pith lsp` using the `pith` binary on your PATH;
the `pith.serverPath` setting overrides that with an absolute path.
see `tooling/editors/vscode/README.md` for details.

## troubleshooting

- the server logs to stderr. neovim collects that in the lsp log
  (`:LspLog`); vs code shows it in the output panel under
  "Pith Language Server".
- in neovim, `:LspInfo` (`:checkhealth vim.lsp` on 0.11+) shows whether
  the client attached and which command it ran.
- if nothing attaches, check that the `pith` binary is on the PATH of
  the editor process — gui editors often launch with a shorter PATH
  than your shell.
