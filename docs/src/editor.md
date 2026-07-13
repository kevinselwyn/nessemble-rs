# Editor support

`nessemble` ships a built-in [Language Server][lsp] for its flavor of 6502
assembly. It runs from the CLI and speaks the Language Server Protocol over
stdio, so any LSP-capable editor — VS Code, Cursor, Neovim, Helix, Emacs
(`eglot`/`lsp-mode`), Sublime Text (LSP), and others — can drive it.

## Starting the server

```text
nessemble lsp
```

The server reads LSP messages on `stdin` and writes them to `stdout`, the
transport every LSP client expects. You normally don't run this by hand; you
point your editor's LSP client at it and the editor manages the process.

## Features

Once connected, the server provides:

- **Diagnostics** — errors and warnings as you type, each underlined at the
  offending token. Several problems are reported at once (the analyzer recovers
  past the first error), and includes are followed.
- **Completion** — instruction mnemonics, assembler directives, and the
  labels, constants, and macros defined in the current buffer. Typing `.`
  triggers directive completion.
- **Formatting** — “format document” tidies indentation and comma spacing while
  preserving comments, blank lines, and letter case. Formatting is lossless and
  idempotent.
- **Semantic highlighting** — tokens are classified (mnemonic, directive,
  number, string, comment, identifier, operator) for richer coloring than a
  regex grammar can offer.
- **Outline & navigation** — a document outline of labels, constants, and
  macros; go-to-definition and find-all-references for symbols.
- **Hover** — opcode and addressing-mode details for an instruction, the
  description of a directive, and the resolved value of a constant or label.

## Editor setup

The server needs no configuration beyond the command `nessemble lsp` and a file
type. Associate the `.asm` extension (or a dedicated language id such as
`nessemble`) with the server in your editor's LSP settings.

### Neovim (`nvim-lspconfig`)

```lua
vim.api.nvim_create_autocmd('FileType', {
  pattern = 'asm',
  callback = function(args)
    vim.lsp.start({
      name = 'nessemble',
      cmd = { 'nessemble', 'lsp' },
      root_dir = vim.fs.dirname(args.file),
    })
  end,
})
```

### Helix (`languages.toml`)

```toml
[language-server.nessemble]
command = "nessemble"
args = ["lsp"]

[[language]]
name = "assembly"
language-servers = ["nessemble"]
```

### VS Code / Cursor

Use a generic LSP client extension (or a thin extension of your own) that
launches `nessemble lsp` for `.asm` files. A dedicated extension is not yet
published; any client that can spawn a stdio language server works.

### Emacs (`eglot`)

```elisp
(add-to-list 'eglot-server-programs
             '(asm-mode . ("nessemble" "lsp")))
```

## Notes

- The server was compiled in by default. A build made with `--no-default-features`
  (without the `lsp` feature) still accepts `nessemble lsp`, but the command
  reports that language-server support was not included.
- The server analyzes the in-editor buffer, so diagnostics reflect unsaved
  changes.

[lsp]: https://microsoft.github.io/language-server-protocol/
