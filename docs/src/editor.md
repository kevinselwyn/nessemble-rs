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
- **Project-aware analysis** — when a workspace folder is open, a file that is
  `.include`d into a larger program is analyzed *in the context of that program*,
  so symbols defined in sibling or parent files are not reported as undefined.
  The server discovers entry points from the workspace's `.include` graph (no
  configuration needed) and reflects unsaved edits across files.
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
  macros; go-to-definition (cmd/ctrl-click) and find-all-references for symbols.
  With a workspace folder open, go-to-definition follows `.include`s across the
  project, so it reaches a symbol defined in a sibling or parent file.
- **Hover** — opcode and addressing-mode details for an instruction, the
  description of a directive, and the resolved value of a constant or label.
- **Folding** — macro (`.macrodef`…`.endm`) and conditional (`.if*`…`.endif`)
  blocks, and runs of consecutive comments, can be collapsed.
- **Rename** — renaming a symbol updates its definition and every use across the
  open buffers.
- **Code actions** — convert a numeric literal between hexadecimal, decimal, and
  binary.
- **Custom pseudo-instructions** — directives declared in a `--pseudo`-style
  mapping file in the workspace are recognized, so they aren't flagged as unknown,
  and cmd/ctrl-click on one opens the script that implements it.

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

Cursor is a VS Code fork and uses the same extension model. There is no
published Marketplace extension yet, but Cursor can't spawn a stdio language
server on its own — it needs a small client extension. A minimal one is a few
files; you can develop it locally and run it from Cursor without publishing.

1. Make sure `nessemble` is on your `PATH` (`nessemble --version` should print
   `2.5.0` or newer).

2. Create a folder, e.g. `nessemble-vscode/`, with these two files:

   `package.json`:

   ```json
   {
     "name": "nessemble",
     "displayName": "nessemble",
     "version": "0.0.1",
     "engines": { "vscode": "^1.75.0" },
     "categories": ["Programming Languages"],
     "activationEvents": ["onLanguage:nessemble"],
     "main": "./extension.js",
     "contributes": {
       "languages": [
         {
           "id": "nessemble",
           "aliases": ["nessemble", "NES assembly"],
           "extensions": [".asm", ".s"]
         }
       ]
     },
     "dependencies": { "vscode-languageclient": "^9.0.0" }
   }
   ```

   `extension.js`:

   ```js
   const { LanguageClient } = require("vscode-languageclient/node");

   let client;

   function activate() {
     const serverOptions = {
       command: "nessemble",
       args: ["lsp"],
     };
     const clientOptions = {
       documentSelector: [{ scheme: "file", language: "nessemble" }],
     };
     client = new LanguageClient(
       "nessemble",
       "nessemble",
       serverOptions,
       clientOptions
     );
     client.start();
   }

   function deactivate() {
     return client ? client.stop() : undefined;
   }

   module.exports = { activate, deactivate };
   ```

3. From that folder, run `npm install` to fetch `vscode-languageclient`.

4. Open the folder in Cursor and press <kbd>F5</kbd> ("Run Extension") to launch
   an Extension Development Host with the extension loaded. Open a `.asm` file
   in that window — diagnostics, completion, hover, formatting, outline, and
   go-to-definition should all work.

   To install it permanently instead of running the dev host, package it with
   [`vsce`](https://github.com/microsoft/vscode-vsce) (`vsce package`) and
   install the resulting `.vsix` via the Extensions view's *Install from
   VSIX…* command.

Any other client that can spawn a stdio language server for `.asm`/`.s` files
works the same way.

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
- Project-aware analysis needs a **workspace folder** to be open (most editors
  send one automatically). Opening a lone file with no folder still works, but
  each file is then analyzed on its own, so cross-file symbols may be reported as
  undefined.
- Custom pseudo-instructions are discovered from any `*.txt` mapping file in the
  workspace (or next to the open file) whose `.name = script` entries point at
  existing scripts — the same mapping you pass to the CLI's `--pseudo`. Their
  scripts are **not** executed during analysis, so the bytes they emit aren't
  modeled; addresses after a custom pseudo-op may be approximate.

[lsp]: https://microsoft.github.io/language-server-protocol/
