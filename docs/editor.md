# EMI in the editor (`emi lsp`)

`emi lsp` is a language server on stdio for Python and JavaScript/TypeScript
files.

- **Diagnostics.** EMI's rule catalog, plus the workspace's `.emi/rules/*.yml`,
  appears as errors, warnings and information. A finding under a valid
  `emi-ignore[...]` suppression is not shown, and an invalid or expired
  suppression is shown itself. Functions past the erosion thresholds
  (complexity 10, 50 lines) get an informational hint with their
  complexity, size and mass.
- **Code lenses.** Each notable function carries a lens:
  `E 4.7 (Δ +2.1) · cc 13 · 36 lines`. The delta compares the function
  with the same function (by name and order) at the base revision:
  `HEAD` by default, or set `initializationOptions.base` to
  `"origin/main"` for "burden this branch added". The lens says `(new)`
  for functions the base lacks.

## Neovim (0.10+)

```lua
vim.api.nvim_create_autocmd("FileType", {
  pattern = { "python", "javascript", "typescript", "javascriptreact", "typescriptreact" },
  callback = function()
    vim.lsp.start({
      name = "emi",
      cmd = { "emi", "lsp" },
      root_dir = vim.fs.root(0, { ".git" }),
      init_options = { base = "origin/main" },
    })
    vim.lsp.codelens.refresh({ bufnr = 0 })
  end,
})
```

## Helix (`languages.toml`)

```toml
[language-server.emi]
command = "emi"
args = ["lsp"]
config = { base = "HEAD" }

[[language]]
name = "python"
language-servers = ["pyright", "emi"]
```

## VS Code

Use any generic LSP client extension (for example "Generic LSP Client")
with the command `emi lsp`, or run it next to your language's own server.

## Zed

```json
{"lsp": {"emi": {"binary": {"path": "emi", "arguments": ["lsp"]}}},
 "languages": {"Python": {"language_servers": ["pyright", "emi"]}}}
```
