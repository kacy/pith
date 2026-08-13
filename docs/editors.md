# editor setup

`pith lsp` speaks the language server protocol over stdio (see
[lsp.md](lsp.md) for what it supports). any editor with a generic lsp
client can use it; here is the exact wiring for neovim and vs code.

## neovim (0.11+)

put this in your `init.lua`. it teaches neovim the `pith` filetype,
registers the server, and enables it:

```lua
vim.filetype.add({
  extension = { pith = "pith" },
})

vim.lsp.config("pith", {
  cmd = { "pith", "lsp" },
  filetypes = { "pith" },
  root_markers = { "pith.toml", ".git" },
})
vim.lsp.enable("pith")
```

open any `.pith` file inside a project (a directory with a `pith.toml`
or `.git`) and diagnostics, hover (`K`), document symbols, and
formatting (`vim.lsp.buf.format()`) work out of the box.

if `pith` is not on your PATH, put the absolute path to the binary in
`cmd` instead.

on neovim 0.10 and earlier, `vim.lsp.config`/`vim.lsp.enable` do not
exist; use `vim.lsp.start` from an autocommand instead:

```lua
vim.filetype.add({ extension = { pith = "pith" } })
vim.api.nvim_create_autocmd("FileType", {
  pattern = "pith",
  callback = function(args)
    vim.lsp.start({
      name = "pith",
      cmd = { "pith", "lsp" },
      root_dir = vim.fs.root(args.buf, { "pith.toml", ".git" }),
    })
  end,
})
```

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

## syntax highlighting in neovim

the textmate grammar serves vs code and github; neovim uses a classic
vim syntax file instead, shipped in `tooling/editors/nvim/` (with an
ftdetect rule, so `.pith` files are recognized without any filetype
configuration). point your runtimepath at it:

```lua
vim.opt.runtimepath:append("/path/to/repo/tooling/editors/nvim")
```

keywords, builtin and pascal-case types, strings with interpolation,
numbers in every base, and comments highlight; the grammar in
tooling/highlighting stays the source of truth, so grow both when the
language grows a keyword. the language server's semantic tokens will
later layer checker-aware highlighting on top of this base.
