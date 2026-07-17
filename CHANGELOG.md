# Changelog

## 2.12.0 - 2026-07-17

### Minor changes

- Give the in-browser `<nessemble-assembler>` toolbar icon buttons with tooltips: Reset, Clear, the byte-output toggle (eye / eye-off, "Show output" / "Hide output"), and Download become icon-only controls. Add a new "Format code" button that reformats the editor contents with `nessemble format`, backed by a new `format` export in the WebAssembly build.
- Add a `nessemble format <path>...` subcommand that formats assembly source. A single file is printed to stdout; `--write` rewrites files in place (reporting each changed file); `--check` lists unformatted files and exits non-zero for CI. Directories are walked recursively for `.asm` files and require `--write` or `--check`. This is Phase 1 of `plans/005-formatter.md`; it uses the default formatting options (indentation, comma spacing, trailing-whitespace tidy) — the opinionated structural rules and `.nessemblerc` config follow in later phases.
- Add opt-in case normalization to `nessemble format` (Phase 4 of `plans/005-formatter.md`): `.nessemblerc` gains `mnemonicCase` and `hexDigitCase` keys (`"preserve"` | `"lower"` | `"upper"`, default `"preserve"`). `mnemonicCase` re-cases only the instruction mnemonic (labels, registers, and identifiers are left alone); `hexDigitCase` re-cases the hex-digit letters of numeric literals (`$ab` ↔ `$AB`). Directive names are never re-cased, since nessemble is case-sensitive about them. Both are byte-safe — nessemble matches mnemonics and hex literals case-insensitively — and covered by a byte-preservation test.
- Add a configurable formatting API to `nessemble-core::tooling`: `format_with(source, &FormatOptions)` with `FormatOptions` (`indent_style`, `indent_width`, `comma_spacing`) and `IndentStyle`. The existing `format` now delegates to it with default options, so output is unchanged (parity 122/122, language server formatting identical). This is Phase 0 of the built-in `nessemble format` command specified in `plans/005-formatter.md`.
- Make `nessemble format` opinionated and configurable (Phases 2–3 of `plans/005-formatter.md`). The formatter now, by default, consolidates adjacent `.db`/`.dw`/`.color` data into eight values per line (honoring `; @fmt stride=N` hint comments), inserts a blank line after `RTS`/`RTI`, collapses runs of more than two blank lines, and normalizes a single trailing newline. Formatting stays cosmetic — the assembled ROM is unchanged (guarded by a byte-preservation test). Rules are tunable via an optional `.nessemblerc` JSON file (strict keys), discovered up the directory tree, with `--config`/`--no-config`, an `extensions` filter, `.nessembleignore` exclusions, and prettier-style per-glob `overrides`. Because the language server shares this engine, editor on-format output gains the same house style.
- Serve the documentation at extensionless directory URLs (`/docs/syntax/` instead of `/docs/syntax.html`). Each chapter is rendered to its own `index.html` and the generated links (and the `llms.txt` index) are trimmed to match.
- Publish an `llms.txt` index at the documentation root so LLMs and agents can discover the manual. It is generated from the book's own `SUMMARY.md` on every site build, keeping it in step with the documentation.

## 2.11.0 - 2026-07-17

### Minor changes

- Support line continuation in comma-separated directives. A trailing comma at the
  end of a line now continues the operand list onto the next (indented) line, so a
  long run can be wrapped across several lines:
  
  ```nessemble
  .db $00, $01, $02, $03,
      $04, $05, $06, $07
  ```
  
  This already worked for `.defchr`; it now applies uniformly to `.db`/`.byte`,
  `.dw`/`.word`, `.fill`, `.color`, `.hibytes`, and `.lobytes`, as well as to
  custom (`--pseudo`) directives, whose argument lists — numbers or quoted
  strings — can now be wrapped the same way.

## 2.10.0 - 2026-07-16

### Minor changes

- Add `.inesbat`, `.ines4scr`, `.inesprgram`, `.inestv`, `.inesvs`, and
  `.inespc10` pseudo-instructions so the battery, four-screen, PRG-RAM size, TV
  system, VS Unisystem, and PlayChoice-10 fields of the iNES header can be
  configured from source. `.inestv 1` (PAL) is also mirrored into the unofficial
  Flags 10 TV-system field.
- Add NES 2.0 header support. The new `.ines2` pseudo-instruction emits a NES 2.0
  header, widening `.inesmap` to 12-bit mappers and `.inesprg`/`.ineschr` to
  12-bit sizes, and enabling the companion directives `.inessubmap`,
  `.inesprgnvram`, `.ineschrram`, `.ineschrnvram`, `.inestiming`, `.inesconsole`,
  `.inesvsppu`, `.inesvshw`, `.inesmiscrom`, and `.inesexpansion`. In NES 2.0 mode
  `.inesprgram` takes a byte size, `.inestv` provides the timing fallback, and
  `.inesvs`/`.inespc10` become console-type sugar.

## 2.9.0 - 2026-07-16

### Minor changes

- Rebuild the in-browser assembler component's editor on CodeMirror 6. Text
  selection now works and shows what's selected, and Cmd-F opens a working
  in-editor search instead of breaking the highlighting. Syntax colors and the
  overall look are unchanged.

## 2.8.2 - 2026-07-16

### Patch changes

- Add the `xtask changeset` command group (add/check/status/version) that parses
  `.changeset/` files, computes the next semantic version from the accumulated
  changesets, and — via `cargo set-version` — bumps the whole workspace. Internal
  release tooling (plan 004, Phase 1); no shipped-behavior change.
