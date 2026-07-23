# Changelog

## 2.16.0 - 2026-07-23

### Minor changes

- Expose random-number functions to the pseudo-op scripting engine via the
  [`rhai-rand`](https://docs.rs/rhai-rand) package: scripts can now call `rand()`,
  `rand(min, max)`, `rand_float()`, `rand_bool()`, and the array `shuffle`/`sample`
  helpers for procedural noise and randomized data tables. Available on native
  builds; absent from the WebAssembly build (no system entropy source), the same as
  filesystem access. Also add a `--mlist` flag that includes macro-created labels
  in the `-l`/`--list` output — such labels (e.g. `\@`-uniquified loop targets) are
  hidden from the list by default so it stays readable. Documents both changes and
  adds guidance on choosing macros vs. scripts.
- Add `.phase` / `.dephase` directives for bank-swapped code. Labels defined inside
  a `.phase ADDRESS` block take the run-time (post-swap) address while ROM layout
  keeps flowing from `.org`, so there's no need to subtract the swap offset from
  every label by hand. The block ends at `.dephase` or a bank/segment switch.

## 2.15.0 - 2026-07-20

### Minor changes

- The language server now surfaces `nessemble lint` findings inline as you type. Lint diagnostics use a gentle severity (Information/Hint) and a `nessemble-lint` source so they read as suggestions distinct from assembler errors, honor the project's `.nessemblerc` `lint` config, and clear as soon as the flagged block is documented. Also documents the `lint` command and its `.nessemblerc` config in the manual.
- Add a `nessemble lint` subcommand that reports style problems without rewriting source — the ESLint to `nessemble format`'s Prettier. Its first rule, `require-block-comment`, flags a block-opening label that has no comment nearby. Configure it in `.nessemblerc` under a `lint` block: per-rule `off`/`warn`/`error` severities, a comment `window`, and an `ignore` list of regexes that exempt matching label names (e.g. machine-generated `loc_`/`data_` labels). Errors fail the run; warnings do not unless `--max-warnings` is exceeded.

## 2.14.0 - 2026-07-19

### Minor changes

- Add a `nessemble coverage <infile.asm> --cdl <file.cdl>` subcommand that reports
  runtime execution coverage of an assembled ROM against an emulator CDL capture.
  It classifies each PRG source line as code / data / mixed / unaccessed and writes
  JSON and/or LCOV reports (`--format`, default both) plus a one-line summary.
  FCEUX and Mesen flat-mask CDLs are supported via `--emulator` (default `fceux`);
  multiple `--cdl` files are merged by bitwise OR. Phase 2 of
  `plans/007-cdl-based-coverage.md`.
- Add `nessemble coverage --scripts`, which also reports line coverage for the
  project's `-p` Rhai pseudo-op scripts — revealing script code that never runs
  during assembly. Executed lines come from a debugger-instrumented engine; the
  coverable set is the compiled AST, so never-run branches show as uncovered. Each
  script joins the JSON/LCOV report as its own file. Behind the `coverage` Cargo
  feature (on by default). Phase 4 of `plans/007-cdl-based-coverage.md`.
- Remove the `-C`/`--coverage` assemble-time write-coverage flag. It reported
  per-bank byte counts (a disassembly-progress metric) that was not useful in
  practice, and is superseded by the new `coverage` subcommand, which reports true
  runtime execution coverage from an emulator CDL. Scripts invoking `nessemble -C …`
  should switch to `nessemble coverage …`. Phase 3 of
  `plans/007-cdl-based-coverage.md`.

### Patch changes

- Add a `coverage` module to `nessemble-core` that classifies a byte-exact source
  map against an emulator CDL capture: a `CdlSource` trait, a `FlatMaskCdl` reader
  (FCEUX masks), `classify_span`, and a per-file/per-line `CoverageReport` model.
  This is Phase 1 of the CDL-based coverage plan
  (`plans/007-cdl-based-coverage.md`); no CLI surface yet.
- Fix `nessemble coverage` report paths so `genhtml` (and other LCOV tools) can
  find the sources. The source map now identifies each file by its resolved
  absolute path instead of the assembler's per-file display name (which lost the
  top-level directory and left includes relative to a different base), and the
  `coverage` command emits each `SF:`/JSON path relative to the current directory
  (clean, no `../..`) when the file is under it, else absolute. Running
  `genhtml coverage.lcov` from the project root now resolves every source.
- Remove the oracle/parity developer tooling from xtask (the fetch-oracle, verify-goldens, and parity commands) and its supporting corpus-runner code. The repo has diverged from the original C implementation, so cross-checking output against the v1.1.1 reference binary is no longer useful. The hermetic golden-ROM tests in crates/nessemble-core/tests/corpus.rs remain the source of assembler-output regression coverage. Also drops the now-unused nessemble-core::REFERENCE_VERSION constant and the reference-version workspace metadata.
- Add an opt-in byte-exact source map to the assembler (`Options::source_map`,
  exposed as `Assembly::source_map`), recording which source line emitted each ROM
  byte. Off by default and side-effect free — assembled bytes are unchanged. This
  is the internal seam Phase 0 of the CDL-based coverage plan
  (`plans/007-cdl-based-coverage.md`) needs; no CLI surface yet.

## 2.13.1 - 2026-07-18

### Patch changes

- Internal: two idiomatic-Rust follow-ups from the round-2 review. Hoist the
  `--pseudo` mapping parser into `nessemble_core::parse_pseudo_mapping`, so the CLI
  reader and the language server's project scan share one implementation instead of
  two that had begun to drift; and rewrite the `xtask` doc-pipeline markdown
  scanners (`rewrite_chapter_links`, `strip_md_links`) from manual byte-index loops
  to `find`/slice/`strip_prefix`. No change to assembled output, custom pseudo-op
  resolution, or the generated docs.
- Internal: make the language server's per-keystroke project analysis cheaper. The
  include graph now extracts each disk file's `.include` lines through an
  `(mtime, len)`-keyed cache, so rebuilding it on an edit re-reads only files that
  actually changed on disk (unchanged files are stat'd, not re-read and
  re-scanned), and the open-buffer overlay borrows document text instead of cloning
  every buffer on every change. Behavior is unchanged — an external edit to an
  include line is still reflected, because the cache is keyed on the file's
  signature.
- Internal: fold the repeated opcode-resolution logic in the instruction encoder
  into a single `resolve_opcode` helper (plus an `indexed_mode` helper for the
  `X`/`Y`-indexed forms), and drop the `opcode_byte` sentinel wrapper now that the
  `Option` flows through to emission. No change to assembled output, diagnostics,
  or the addressing-mode selection — Phase 1 of `plans/006-idiomatic-rust-refactor.md`.
- Internal: collapse the ~19 numeric `.inesXxx` directive AST variants into a
  single `Pseudo::Ines(InesField, Expr)` node, driven by a name→field table in the
  parser and a field→member assignment in the assembler. The three non-numeric
  directives (`.ines2`, `.inestiming`, `.inestrn`) keep their own variants. No
  change to the emitted iNES / NES 2.0 header bytes or any diagnostic — Phase 2 of
  `plans/006-idiomatic-rust-refactor.md`.
- Internal: model conditional-assembly nesting (`.if`/`.ifdef`/`.ifndef`/`.else`/
  `.endif`) as a `Vec<bool>` stack instead of a fixed `[bool; N]` array plus a
  manual depth counter — push/pop/flip-top replace the hand-tracked index, and the
  `MAX_NESTED_IFS` cap becomes a suppression guard rather than an array length. The
  suppression predicate (current level, plus the immediate parent when nested) and
  the past-the-limit behavior are preserved exactly. No change to assembled output
  or diagnostics — Phase 3 of `plans/006-idiomatic-rust-refactor.md`.
- Internal: define the highlight token-class wire ids and names once, as
  `TokenClass::wire_id` / `wire_name` / `ALL` in `nessemble-core::tooling`, instead
  of re-deriving the same 0–6 numbering in the wasm highlighter (`tokenize` /
  `token_classes`) and the language server's semantic-token mapping. The wire ids,
  class names, and LSP legend are unchanged — Phase 4 of
  `plans/006-idiomatic-rust-refactor.md`.
- Internal: a batch of low-risk readability cleanups — borrow (rather than clone)
  the stride list in the formatter's data-consolidation pass; collapse the
  `.nessemblerc` scalar-field overlay into a small local macro; replace the obscure
  `&args[args.len().min(1)..]` argv slicing in `xtask` with `args.get(1..)`; and
  flatten the single-variant `AssembleError` enum into a `AssembleError(Diag)`
  newtype. No change to output, formatting, or diagnostics — Phase 5 of
  `plans/006-idiomatic-rust-refactor.md`.
- Internal: type the single-bit iNES Flags-6 toggles (`mir`, `bat`, `fsc`, `trn`)
  as `bool` instead of `i64`, alongside the already-boolean `nes2`. The value-set
  directives mask bit 0 exactly as the header emission's former `& 0x01` did, so
  the emitted header is byte-identical. The multi-value fields (mapper, bank
  counts, RAM sizes, timing, console, …) and the dual-use `vs`/`pc10` flags stay
  `i64` to preserve exact emission and range-check diagnostics — Phase 6 of
  `plans/006-idiomatic-rust-refactor.md`.
- Internal: make the i18n locale catalog process-global (a `OnceLock<RwLock<…>>`
  over the concurrent Fluent bundle) instead of thread-local, so a locale
  registered or selected on one thread is honored on all of them — the language
  server analyzes on worker threads. Message output is unchanged (parity holds);
  `t!` takes only a read lock, off the assembly hot path — Phase 7 of
  `plans/006-idiomatic-rust-refactor.md`.

## 2.13.0 - 2026-07-18

### Minor changes

- Remove the `config` subcommand. Its only purpose in the reference tool was
  storing the package-registry endpoint, and that registry subsystem is out of
  scope for this rewrite — nothing in the assembler, formatter, or language
  server ever read a value it stored, so the command configured nothing. It was
  carried over by mistake during the initial rewrite and is now gone from the
  CLI, help text, and documentation.

### Patch changes

- Replace the hand-rolled CLI argument parser with [clap](https://docs.rs/clap).
  The same flags, subcommands, and exit codes are accepted, but `--help`/usage
  text is now generated from the argument definitions instead of being
  hand-maintained. Two cosmetic differences follow from clap's conventions: the
  help layout is clap's (still listing every in-scope option and command), and
  argument errors are written to stderr rather than stdout. The `-v`/`--version`
  and `-L`/`--license` banners are unchanged.

## 2.12.2 - 2026-07-18

### Patch changes

- `nessemble format` now aligns the continuation lines of a multi-line statement
  (an operand list wrapped onto the next line by a trailing comma) under the
  opening line's first argument, instead of re-indenting them to the block indent.
  `.metasprite` is the motivating case, but the rule applies to any statement whose
  operands span multiple lines:
  
  ```asm
      .metasprite $FA, $02, $00, $FA,
                  $FA, $03, $00, $02,
                  $02, $0D, $00, $FA
  ```
  
  The behavior is gated behind a new `.nessemblerc` boolean `alignContinuations`
  (default `true`); set it to `false` to keep the previous block-indent behavior.
  Alignment is computed from the opening line's actual emitted indent, so it stays
  correct alongside `indentDirectives`; under `indentStyle: "tab"` the continuation
  reuses the opening tab and pads to the first-argument column with spaces. Only
  leading whitespace changes, so the assembled bytes are unaffected (covered by a
  round-trip byte-preservation test with the option both on and off).

## 2.12.1 - 2026-07-17

### Patch changes

- Fix `nessemble format` corrupting assembled output on anonymous-label branches:
  a branch whose operand references an anonymous label (`BEQ :+`, `BNE :-`) was
  misclassified as an anonymous-label *definition* and de-indented to column 0,
  where the assembler then parsed it differently and silently changed the ROM. A
  line is now treated as a label definition only when the `:` ends the line (a
  trailing comment is allowed), matching the assembler's own rule. Add an
  `assemble(x) == assemble(format(x))` regression covering anonymous-label
  branches.
  
  Also add an opt-in `indentDirectives` `.nessemblerc` option (default `false`):
  when enabled, directive lines (`.db`, `.dw`, `.include`, …) are indented to
  block depth like instructions instead of being pinned to column 0, for codebases
  that indent data under labels.

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
