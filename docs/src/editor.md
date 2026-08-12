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
- **Lint hints** — the same style findings as the
  [`nessemble lint`](usage.md#lint-opt--path-) CLI appear inline, at a gentler
  severity (Information/Hint) and tagged with the `nessemble-lint` source so they
  read as suggestions distinct from assembler errors. They honor the project's
  [`.nessemblerc` `lint`](usage.md#lint) config (rule severities, comment window,
  ignored label names), and clear as soon as you document the flagged block. The
  same pass checks [routine signatures](usage.md#documenting-routines) against
  the code — a routine that writes a register its `@nessemble-clobbers` omits is
  reported where the comment and the code disagree — and flags a mistyped or
  misplaced [comment directive](usage.md#comment-directives) — a directive that
  would otherwise fail silently.
- **Project-aware analysis** — when a workspace folder is open, a file that is
  `.include`d into a larger program is analyzed *in the context of that program*,
  so symbols defined in sibling or parent files are not reported as undefined.
  The server discovers entry points from the workspace's `.include` graph (no
  configuration needed) and reflects unsaved edits across files. The open
  workspace folder also doubles as the
  [project root](syntax.md#project-root-relative-paths) for `@/` paths,
  overriding the usual `.nessemblerc` walk-up the same way the CLI's
  [`--root`](usage.md#root-dir) does — so a file opened directly, with no
  workspace folder, falls back to that walk-up instead.
- **Completion** — instruction mnemonics, assembler directives, and the
  labels, constants, and macros defined in the current buffer. Typing `.`
  triggers directive completion. Inside a comment, the
  [comment directives](usage.md#comment-directives) are offered instead of code —
  including `@nessemble-coverage-ignore` pre-filled with `start` and with `end` —
  each with its documentation. A comment directly above an undocumented label
  also offers a **routine signature block**, which scaffolds
  `@nessemble-param` / `-returns` / `-clobbers` in one insertion.
  Inside a **filename argument** — the path of `.include`, `.incbin`, `.incpng`,
  `.incpal`, `.incrle`, `.incwav`, `.inestrn`, or any argument written with a
  [`file://` prefix](syntax.md#declaring-a-filename-argument) — filenames from
  that directory are offered instead, filtered to what the directive can use
  (`.incpng` offers PNGs, `.include` offers assembly sources, a custom
  pseudo-instruction offers everything, since its script may read any format).
  Directories are always offered, and typing `/` walks into one.
  [`@/`](syntax.md#project-root-relative-paths) is offered at the start of an
  empty argument, and typing it in switches completion to list the project
  root instead of the current directory.
- **Formatting** — “format document” applies the opinionated house style
  (indentation, comma spacing, data-block consolidation, routine spacing) while
  preserving comments. It runs the **same engine** as the
  [`nessemble format`](usage.md#format-opt--path-) CLI command, so editors and
  the command line produce identical output. Formatting is idempotent and never
  changes the assembled ROM.
- **Semantic highlighting** — tokens are classified (mnemonic, directive,
  number, string, comment, identifier, operator) for richer coloring than a
  regex grammar can offer. A comment carrying a
  [comment directive](usage.md#comment-directives) additionally gets the
  `documentation` modifier, so themes can set it apart from prose.
- **Outline & navigation** — a document outline of labels, constants, and
  macros, with a documented routine's clobber list shown in its detail, so a
  file's register discipline reads at a glance; go-to-definition (cmd/ctrl-click) and find-all-references for symbols.
  With a workspace folder open, go-to-definition follows `.include`s across the
  project, so it reaches a symbol defined in a sibling or parent file.
- **Clickable file paths** — every filename argument is a link, so
  cmd/ctrl-clicking the path in `.include "defs.asm"` or `.incpng "hero.png"`
  opens that file. A custom pseudo-instruction's argument becomes clickable when
  it is declared with a
  [`file://` prefix](syntax.md#declaring-a-filename-argument), which is how the
  editor knows the string is a path at all. Paths resolve the way the assembler
  resolves them — relative to the file that contains the directive, or from the
  project root for a [`@/`](syntax.md#project-root-relative-paths) path — and a
  path that doesn't resolve is deliberately *not* linked: it is reported as an
  error instead.
- **Hover** — opcode and addressing-mode details for an instruction, the
  description of a directive or [comment directive](usage.md#comment-directives),
  and the resolved value of a constant or label.
  A constant or label is also documented with the run of comment lines
  immediately preceding its definition, so an explanatory comment written above
  a symbol appears when you hover over any use of it.
  A routine carrying [signature annotations](usage.md#documenting-routines)
  additionally shows its calling convention as a table — what it takes, what it
  returns, and what it clobbers — at **every** use, including the operand of a
  `JSR`, and including calls into another open file. That is the whole point:
  "does this call eat my `Y`?" is answered without leaving the call site.
  Hovering [`.color`](syntax.md#color) previews the palette it produces: the
  whole argument list is shown as the row of NES colors it maps to, with each
  argument's RGB, the palette index the assembler emits for it, and the color
  the PPU actually shows; hovering a single argument previews just that one
  color. Arguments are expressions, so constants and arithmetic are resolved
  first, and an argument the buffer can't resolve is listed as unresolved rather
  than guessed at. The swatches are drawn as an image, which graphical editors
  render inline; a terminal editor shows the same values as text.
  Hovering a **filename argument** shows the absolute path it resolved to and what
  is there — the file's size, a PNG's pixel dimensions, or **not found** — which
  answers "is it picking up the file I think it is?" without assembling. For a
  [`@/`](syntax.md#project-root-relative-paths) path this doubles as "what root
  did it pick?": a `.nessemblerc` added anywhere above the file changes the
  answer, and this is where that becomes visible without a build.
- **Folding** — macro (`.macrodef`…`.endm`) and conditional (`.if*`…`.endif`)
  blocks, and runs of consecutive comments, can be collapsed.
- **Rename** — renaming a symbol updates its definition and every use across the
  open buffers.
- **Code actions** — convert a numeric literal between hexadecimal, decimal, and
  binary, rename a deprecated comment directive (`@fmt`) to its canonical
  spelling (`@nessemble-format`), scaffold a signature block over an
  undocumented routine, and — when the linter catches a routine writing a
  register its `@nessemble-clobbers` omits — add that register to the list,
  keeping the list in canonical order and any trailing prose intact.
- **Inlay hints** — a `JSR` whose target declares a clobber list shows that list
  at the end of the call line (`JSR draw_sprite  ‹A, X, Y›`), so the cost of a
  call is visible without hovering. Editors toggle inlay hints on and off with
  their own setting.
- **Custom pseudo-instructions** — directives declared in a `--pseudo`-style
  mapping file in the workspace are recognized, so they aren't flagged as unknown;
  cmd/ctrl-click on one opens the script that implements it, and hovering it shows
  the script's path and the doc comment above its `custom` function, if any.

## Pseudo-op scripts (`.rhai`)

The server also understands the [Rhai](https://rhai.rs) scripts a `--pseudo`
mapping's directives run — the host API those scripts call is documented once,
in the [Extending](extending.md#host-api-reference) page's reference table, and
served here as completion and hover so a script author doesn't have to keep
that page open in another tab. Opening a `.rhai` file gets:

- **Completion, hover, and signature help** for every function, method, and
  property a script can call — signature, one-line summary, an availability
  note when a build doesn't have it (a script running in the browser assembler
  has no filesystem or random-number functions), and a link to the docs
  section that explains it. This half needs no scripting host at all, so it
  works even in a build made with `--no-default-features --features lsp`.
- **Syntax diagnostics** from the same compiler the assembler runs the script
  through, plus four lints that catch mistakes Rhai's own dynamic dispatch
  would otherwise defer to a build that reaches the directive: a script a
  `pseudo.txt` maps that defines no `custom(ints, texts)` function; `custom`
  declared with other than two parameters; a statement written outside every
  `fn` (it never runs — `custom` is called without evaluating the script body
  first, so a top-level `const` is `Variable not found` the moment a `.if`
  branch that used to skip the call stops skipping it); and a call that
  resolves to no script-local function and no host function, when it is a
  near-miss of one that is (`decode_png_fil` is flagged; an unrelated Rhai
  built-in this catalog doesn't list is not).
- **An outline** of the script's functions (`custom` first), and **folding**
  of each function's body and of comment runs.
- **Go-to-definition and find-all-references** for a script-local function.

Diagnostics, the lints, the outline, folding, and script-local navigation need
the `scripting` feature (on by default; see [Notes](#notes)).

Editors other than VS Code need `.rhai` routed to the server the same way
`.asm`/`.s` is — add the `rhai` language id alongside `nessemble` in the
client's document selector. A Rhai syntax-highlighting extension, if you have
one, keeps working: coloring and this server's diagnostics/completion are
independent providers for the same language id.

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

VS Code can't spawn a stdio language server on its own — it needs a client
extension. `nessemble` ships one, built and attached to every release as
`nessemble_<v>.vsix`. (Cursor is a VS Code fork and uses the same extension
model, so the same `.vsix` installs there.)

The extension is a thin client: it registers a `nessemble` language for `.asm`
and `.s` files, a `rhai` language for `.rhai` files, and runs `nessemble lsp`.
Every feature listed above comes from the server, so the editor can't drift
from the assembler or the CLI. It carries no copy of `nessemble` — one
universal `.vsix` serves every platform, and the executable it drives is the
one you installed.

1. Make sure `nessemble` is on your `PATH` (`nessemble --version` should print
   `2.5.0` or newer). If it lives somewhere off `PATH`, point the
   [`nessemble.serverPath`](#extension-settings) setting at it.

2. Download `nessemble_<v>.vsix` from the
   [releases page](https://github.com/kevinselwyn/nessemble-rs/releases).

3. Install it — either from the Extensions view's *Install from VSIX…* command
   (the `…` menu in its title bar), or from a terminal:

   ```text
   code --install-extension nessemble_<v>.vsix
   ```

   In Cursor, the command is `cursor --install-extension`.

4. Open a `.asm` file. Diagnostics, lint hints, completion, hover, formatting,
   semantic highlighting, outline, go-to-definition, and rename all work
   immediately; the server starts on the first `nessemble` file you open.
   Opening a `.rhai` script a `--pseudo` mapping refers to gets the
   [pseudo-op script features](#pseudo-op-scripts-rhai) the same way.

If the executable can't be found, the extension says so and offers to open the
installation docs or the setting — it does not fail silently.

#### Extension settings

| Setting | Default | What it does |
|---|---|---|
| `nessemble.serverPath` | `nessemble` | Path to the `nessemble` executable. Looked up on `PATH` when left as the bare name. |
| `nessemble.serverArgs` | `["lsp"]` | Arguments used to start the server. |
| `nessemble.trace.server` | `off` | Log LSP traffic to the *nessemble* output channel (`messages` or `verbose`). Useful when reporting a bug. |

Changing the path or arguments restarts the server in place — no window reload.

#### Coloring

The extension deliberately ships **no TextMate grammar**. Coloring comes from
the server's semantic tokens, produced by the assembler's own lexer, so it can
never disagree with how a file actually assembles — the same reasoning that
governs the [in-browser assembler](https://kevinselwyn.github.io/nessemble-rs/)
and the code blocks in these docs. One consequence: a `.asm` file is uncolored
for the moment before the server connects, and stays uncolored if `nessemble`
isn't installed.

#### Building the extension from source

The extension lives in [`editors/vscode/`](https://github.com/kevinselwyn/nessemble-rs/tree/main/editors/vscode).
With `npm` on your `PATH`:

```text
cargo run -p xtask -- vsix
```

That packages `nessemble_<workspace version>.vsix` in the repository root —
the exact artifact the release pipeline publishes. To iterate on the extension
instead, open `editors/vscode/` in VS Code, run `npm install`, and press
<kbd>F5</kbd> to launch an Extension Development Host with it loaded.

#### Format on save

The server advertises document formatting, so once the extension is connected you
can have VS Code / Cursor reformat on every save. Add this to your `settings.json`
(User or Workspace):

```json
{
  "[nessemble]": {
    "editor.formatOnSave": true,
    "editor.defaultFormatter": "kevinselwyn.nessemble"
  }
}
```

- The `[nessemble]` scope targets the language id the extension registers. If you
  instead associated `.asm` with VS Code's built-in `asm` language, use `"[asm]"`.
- `editor.formatOnSave` performs the on-save formatting.
- `editor.defaultFormatter` names the provider to use — the extension identifier,
  `<publisher>.<name>`. Setting it avoids the "multiple formatters" prompt when
  another extension also claims `.asm` files.

Because the server shares its engine with the CLI, saving a file produces exactly
the same result as running `nessemble format --write` on it.

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
- A `.rhai` script's completion, hover, and signature help are compiled in
  unconditionally — even a build with no scripting host at all serves them.
  Its diagnostics, lints, outline, folding, and script-local
  definition/references need the `scripting` feature, which is also on by
  default; `--no-default-features --features lsp` drops them the same way it
  drops `lsp`'s own features from a build with no `scripting`.

[lsp]: https://microsoft.github.io/language-server-protocol/
