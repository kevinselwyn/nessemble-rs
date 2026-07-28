# nessemble for VS Code

NES 6502 assembly support for VS Code and Cursor, powered by the language
server built into [`nessemble`](https://github.com/kevinselwyn/nessemble-rs) —
a 6502 assembler for the Nintendo Entertainment System.

The extension is a thin client: it registers the `nessemble` language for
`.asm`/`.s` files and runs `nessemble lsp`. Everything you see comes from the
assembler itself, so the editor can never disagree with what the command line
assembles.

## Features

- **Diagnostics** — errors and warnings as you type, with recovery past the
  first error, following `.include`s.
- **Lint hints** — the same findings as `nessemble lint`, inline at a gentler
  severity and honoring the project's `.nessemblerc`.
- **Project-aware analysis** — a file that is `.include`d into a larger program
  is analyzed in the context of that program, discovered from the workspace's
  `.include` graph.
- **Completion** — mnemonics, directives, and the labels, constants, and macros
  in scope; comment directives inside comments.
- **Formatting** — the same engine as `nessemble format`, so the editor and the
  CLI produce identical output.
- **Semantic highlighting** — coloring from the assembler's own lexer.
- **Hover** — opcode and addressing-mode details, directive documentation,
  resolved constant values, and a palette preview for `.color`.
- **Outline, go-to-definition, find references, rename, folding, code actions.**

## Requirements

`nessemble` must be installed and on your `PATH`:

```sh
nessemble --version
```

Get it from the [releases page][releases] or see the
[installation docs][install]. If it lives somewhere else, point the extension
at it with the `nessemble.serverPath` setting.

## Settings

| Setting | Default | What it does |
|---|---|---|
| `nessemble.serverPath` | `nessemble` | Path to the `nessemble` executable. |
| `nessemble.serverArgs` | `["lsp"]` | Arguments used to start the server. |
| `nessemble.trace.server` | `off` | Log LSP traffic to the *nessemble* output channel. |

### Format on save

```json
{
  "[nessemble]": {
    "editor.formatOnSave": true,
    "editor.defaultFormatter": "kevinselwyn.nessemble"
  }
}
```

## Installing

The `.vsix` is attached to every [release][releases] as
`nessemble_<version>.vsix`. Install it with the Extensions view's *Install from
VSIX…* command, or:

```sh
code --install-extension nessemble_<version>.vsix
```

## Documentation

Full editor documentation, including setup for other editors, is at
<https://kevinselwyn.github.io/nessemble-rs/docs/editor/>.

## License

GPL-3.0-or-later, the same as `nessemble` itself.

[releases]: https://github.com/kevinselwyn/nessemble-rs/releases
[install]: https://kevinselwyn.github.io/nessemble-rs/docs/installation/
