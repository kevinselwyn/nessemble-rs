# nessemble-rs: A Plan for a Built-in Opinionated Formatter

> Status: **Complete — Phases 0–5 done.** This document specifies a
> `nessemble format` subcommand (a prettier-style, opinionated formatter for
> nessemble assembly) and an optional `.nessemblerc` JSON config, building on the
> formatting engine that already backs the language server. All planning
> decisions are settled (see [§11](#11-decisions)). **Phase 0** landed the
> `FormatOptions` seam; **Phase 1** the `nessemble format <path>...` command;
> **Phase 2** the opinionated structural rules (data consolidation + stride
> hints, blank line after `RTS`/`RTI`, blank-run collapsing, final newline),
> on by default; **Phase 3** the `.nessemblerc` JSON config (discovery,
> `--config`/`--no-config`, `extensions`, `.nessembleignore`, per-glob
> `overrides`, strict keys); **Phase 4** opt-in case normalization
> (`mnemonicCase`/`hexDigitCase`); **Phase 5** the user docs (`usage.md`,
> `editor.md`). Parity stays 122/122; byte preservation (including under case
> normalization) is covered by tests (§9).

---

## 1. Goal

Ship a first-class **formatter** in the CLI:

```sh
nessemble format path/to/file.asm      # print formatted source to stdout
nessemble format path/to/directory     # (with --write/--check) format a tree
```

It should feel like [`prettier`](https://prettier.io): **opinionated by default,
lightly configurable** via a discoverable `.nessemblerc` file, safe to run on a
whole project, and CI-friendly (`--check`). The formatted output must **never
change the assembled bytes** — formatting is cosmetic only.

Two hard requirements from the request:

1. Invocable as **`nessemble format <path>`**, where `<path>` is a single
   `.asm` file *or* a directory formatted recursively (§4).
2. **Prettier-like ergonomics**, including an optional **`.nessemblerc`** config
   for tuning specific rules (§5).

## 2. Why this is a good fit

nessemble-rs already contains the hard part. `nessemble-core::tooling` has a
**lossless lexer** (`tooling::lex`) that segments the *entire* source —
whitespace, comments, strings, numbers, directives, identifiers — into
byte-ranged [`Lexeme`]s with no gaps, plus a `tooling::format(source) -> String`
that the language server calls for `textDocument/formatting`
(`nessemble-lsp::format_document`). That function already:

- indents instructions by four spaces and keeps labels / directives / constant
  definitions at column 0,
- normalizes comma spacing (no space before, exactly one after),
- trims trailing whitespace,
- **preserves** comments, blank lines, identifier case, and other internal
  spacing, and
- is **idempotent**.

What is missing is (a) a **CLI surface** to run it on files and trees, (b) the
**opinionated structural rules** that make a formatter feel finished (data-block
consolidation, routine spacing, blank-line hygiene), and (c) a **config layer**.
This plan adds those three things while reusing the existing lossless-lexer
foundation, so highlighting and LSP formatting keep sharing one engine.

## 3. Current state — what we have and what's missing

Grounded in the current code, not aspirational:

**Available today**

- `nessemble-core::tooling`
  - `lex(source) -> Vec<Lexeme>` — gap-free, reversible, UTF-8-safe segmentation
    with `LexKind` (`Whitespace`, `Newline`, `Comment`, `String`, `Char`,
    `Number`, `Directive`, `Ident`, `Punct`).
  - `format(source) -> String` — the whitespace/indent/comma tidier described
    above; idempotent; consumed by the LSP.
  - `classify` / `highlight` — shared token classification (unaffected by this
    plan, but sharing the same `lex`).
- CLI subcommand dispatch in `crates/nessemble-cli/src/main.rs`: a hand-rolled,
  getopt-style parser with a clean `dispatch()` that already routes
  `init` / `lsp` / `scripts` / `reference` / `config`. A new `format` arm drops
  in exactly like the others.
- `serde` / `serde_json` are already present in `Cargo.lock` (transitively), so
  a JSON config parser adds no new third-party crate to the tree — only a direct
  dependency edge.

**Gaps that shape the plan**

- **No CLI entry point.** `tooling::format` is only reachable through the LSP;
  there is no `nessemble format`.
- **Not yet "opinionated."** `tooling::format` deliberately preserves structure.
  The prettier-like rules (consolidate `.db`/`.dw`/`.color`, blank line after
  `RTS`/`RTI`, collapse excess blank lines, optional case normalization) do not
  exist here. This plan adds them (§6), drawing on the well-worn conventions of
  6502/NES disassembly source formatting.
- **No configuration.** There is a `~/.nessemble/config` key/value store
  (`config` subcommand) for *global tool* settings, but nothing per-project and
  nothing that shapes formatting. `.nessemblerc` is new (§5).
- **No options seam.** `format` takes only `source`. To be configurable it needs
  a `FormatOptions` parameter (§7, Phase 0).

## 4. CLI surface

A new subcommand, parsed inside the existing `dispatch()` (a new
`"format" => return format::run(&args.positionals[1..])` arm), with its own small
option parser in `crates/nessemble-cli/src/format.rs`.

```
nessemble format [options] <path>...

  <path>            one or more .asm files and/or directories
  -w, --write       rewrite files in place (required for directory input)
  -c, --check       exit non-zero if any file is not already formatted;
                    writes nothing; prints the list of files that differ
      --config F    use F as the .nessemblerc instead of discovering one
      --no-config   ignore any .nessemblerc; format with built-in defaults
  -h, --help        print this message
```

**Default behavior (prettier-style):**

- **A single file with no `-w`/`-c`** → print the formatted source to **stdout**
  (leaving the file untouched). Ideal for piping and editor "format selection".
- **`--write`** → format each file in place; print the path of each file that
  changed (a `formatted <file>` line, like `gofmt -l`/`prettier --write`).
  Unchanged files are silent.
- **`--check`** → format in memory and compare; print each path that *would*
  change and exit `1` (the CI gate). No file is written.
- **A directory** is walked recursively for files with a formattable extension
  (default `.asm`; see `extensions` in §5). A directory argument **requires**
  `--write` or `--check` — dumping many files to stdout is a foot-gun, so it is
  an error otherwise, with a message pointing at `--write`.

**Discovery & precedence for `.nessemblerc`:** for each input file, walk up from
its directory to the filesystem root and use the nearest `.nessemblerc` /
`.nessemblerc.json`; stop at the first one found. `--config F` overrides
discovery for all inputs; `--no-config` disables it. A **`.nessembleignore`**
file (gitignore-style globs, discovered the same way) excludes matching paths
from directory walks; per-glob **`overrides`** in the config refine options for
matching files (both are v1 — §5).

**Exit codes** reuse the CLI's existing constants: `0` success, `1`
(`RETURN_EPERM`) for I/O / parse errors *and* for a failed `--check`. Usage
errors print help and exit `129` (`RETURN_USAGE`) like the rest of the CLI.

> Note on `-c`: at top level `-c` means assemble-mode `--check`, but the `format`
> subcommand owns its own argument vector (`args.positionals[1..]`), so `-c` /
> `--check` here is unambiguous and local to `format`.

## 5. `.nessemblerc` — the config file

**Format: JSON** (`.nessemblerc` or `.nessemblerc.json`), matching prettier's
convention and parseable with `serde_json` (already in the tree). Every key is
optional; an omitted key takes its default, and the defaults reproduce the house
style so that a project with **no** `.nessemblerc` still gets fully-formatted
output.

```jsonc
{
  // ── Discovery ─────────────────────────────────────────────
  "extensions": [".asm"],         // file extensions formatted in directory walks

  // ── Layout ────────────────────────────────────────────────
  "indentStyle": "space",        // "space" | "tab"
  "indentWidth": 4,               // columns per instruction indent (space mode)
  "commaSpacing": true,           // ", " between operands/data values
  "finalNewline": true,           // ensure the file ends in exactly one "\n"

  // ── Data blocks (.db / .dw / .color) ──────────────────────
  "dataPerLine": 8,               // values per consolidated line; 0 = leave as-is
  "respectStrideHints": true,     // honor "; @fmt stride=N[,N,...]" overrides

  // ── Vertical spacing ──────────────────────────────────────
  "blankLineAfterReturn": true,   // one blank line after RTS / RTI
  "maxConsecutiveBlankLines": 2,  // collapse longer runs down to this

  // ── Case & literals (default: preserve) ───────────────────
  "mnemonicCase": "preserve",     // "preserve" | "lower" | "upper" (mnemonic only)
  "hexDigitCase": "preserve",     // "preserve" | "lower" | "upper" ($ab vs $AB)
  // Directive names (".db", ".DB") are never normalized — nessemble directives
  // are case-sensitive, so touching them could change legality.

  // ── Per-glob overrides (prettier-style) ───────────────────
  "overrides": [
    { "files": "src/data/**/*.asm", "options": { "dataPerLine": 16 } }
  ]
}
```

> **Trailing whitespace** is always trimmed (it is inherent to line
> normalization, not a toggle), so there is no `trimTrailingWhitespace` key.

**`extensions`** governs which files a directory walk formats (default `.asm`);
explicit file arguments are always formatted regardless. **`.nessembleignore`**
(gitignore-style globs, discovered by the same parent-dir walk as
`.nessemblerc`) excludes matching paths from directory walks. **`overrides`** is
a prettier-style ordered list of `{ files: <glob>, options: { … } }` entries; for
each formatted file, later matching entries layer their options on top of the
base config. Both ship in v1.

**Strict keys.** An **unknown** key in `.nessemblerc` (or inside an `overrides`
`options` block) is a **hard error** with the offending key and file path —
never a silent no-op. This catches typos (`dataPerline`) early; the schema can
be relaxed later if it proves too rigid.

**Mapping to the engine.** The CLI owns a `serde`-derived `RcConfig` struct that
deserializes the JSON, then maps it onto the plain `FormatOptions` that
`nessemble-core::tooling` understands (§7). This keeps `serde` **out of core** —
core stays dependency-light and the config schema stays a CLI concern. Unknown
keys are rejected with a clear error (`#[serde(deny_unknown_fields)]`), per the
strict-keys decision above.

**Defaults = house style.** `FormatOptions::default()` encodes exactly the table
above with the shown defaults. Because the LSP calls the engine with defaults,
adopting these defaults means **the language server's on-format output gains the
new structural rules too** — one formatter, one house style everywhere. That is
intended (§10 covers the version/behavior consequence).

## 6. Formatting rules (the opinions)

The engine runs as an ordered pipeline of passes over the lossless lexeme
stream, each gated by `FormatOptions`. The comma/whitespace parts of Pass 0
already exist; the rest are new.

**Pass 0 — Line normalization (exists today).** Re-indent (instructions →
`indentWidth`; labels, directives, constant `NAME = …` lines, and anonymous `:`
labels → column 0), normalize comma spacing, trim trailing whitespace, preserve
comments/case/blank-lines. Comment-only lines keep their original indentation.

**Pass 1 — Data-block consolidation** (`dataPerLine > 0`). Adjacent `.db` /
`.dw` / `.color` directives with **no trailing comment** are merged and
re-emitted `dataPerLine` values per line. Guards:

- A **directive-type change** (`.db` → `.dw`) flushes the current group.
- A line carrying a **trailing comment** is emitted verbatim and never merged
  (comments pin structure).
- A **label**, **constant**, **instruction**, or **blank line** between data
  lines flushes the group (never merge across them).
- `.dw`/`.color` are grouped independently from `.db`.

**Stride hints** (`respectStrideHints`). A `; @fmt stride=N[,N,...]` comment
immediately before a block overrides `dataPerLine` for that block: strides are
consumed in order and the final one repeats; a type change still forces a break.
The hint stays active until a non-data/non-label line or two consecutive blank
lines.

**Pass 2 — Blank line after `RTS`/`RTI`** (`blankLineAfterReturn`). Insert one
blank line after any line whose only instruction is `RTS` or `RTI` (optionally
trailed by a comment) when the next line is non-blank — a visual routine
boundary.

**Pass 3 — Collapse blank-line runs** (`maxConsecutiveBlankLines`). Reduce runs
of more than *N* consecutive blank lines to *N* (default 2).

**Pass 4 — Case & literal normalization** (default **preserve**, opt-in).
When configured, lower/upper-case mnemonics (`Ident` lexemes that name an opcode
per the shared `MNEMONICS` set) and/or the hex digits of `Number` lexemes. Never
touches identifiers/labels/strings/char literals — and **never directive names**,
which nessemble treats case-sensitively (`.db` ≠ `.DB`), so normalizing them
could change what assembles.

**Pass 5 — Final newline** (`finalNewline`). Ensure the output ends in exactly
one `\n` (the current formatter already preserves *presence*; this makes it a
normalizing rule).

**Invariants across all passes**

- **Idempotent:** `format(format(x)) == format(x)` for every option set.
- **Byte-preserving:** the assembled ROM of the formatted source is identical to
  that of the original. This is the load-bearing safety property (§9 test).
- **Trivia-safe:** comments and string/char literals are moved but never
  rewritten (except case normalization, which only touches mnemonics/directives/
  hex digits and is off by default).

## 7. Architecture

**Core (`nessemble-core/src/tooling.rs`) — add an options seam.**

```rust
pub struct FormatOptions {
    pub indent_style: IndentStyle,     // Space | Tab
    pub indent_width: usize,           // default 4
    pub comma_spacing: bool,
    pub trim_trailing_whitespace: bool,
    pub final_newline: bool,
    pub data_per_line: usize,          // 0 = disabled
    pub respect_stride_hints: bool,
    pub blank_line_after_return: bool,
    pub max_consecutive_blank_lines: usize,
    pub mnemonic_case: Case,           // Preserve | Lower | Upper
    pub hex_digit_case: Case,          // directive names are never re-cased
}
impl Default for FormatOptions { /* = the §5 defaults */ }

pub fn format_with(source: &str, opts: &FormatOptions) -> String { … }

// Back-compat shim so the LSP and any caller keep compiling unchanged:
pub fn format(source: &str) -> String { format_with(source, &FormatOptions::default()) }
```

Core gains **no new dependencies** — `FormatOptions` is plain data with enums.
The passes reuse the existing `lex` + per-line splitting already in `format`.

**CLI (`nessemble-cli/src/format.rs`) — new module.**

- Argument parsing (path list, `--write`, `--check`, `--config`, `--no-config`)
  in the same hand-rolled style as `main.rs`.
- **File discovery**: a small recursive directory walk using `std::fs` (no
  `walkdir`/`ignore` dependency), filtering by the configured `extensions`
  (default `.asm`) and skipping paths matched by a discovered `.nessembleignore`.
- **Config**: a `serde`-derived `RcConfig` (deserialized with `serde_json`,
  `deny_unknown_fields`), discovered by walking parent directories, then mapped
  to `FormatOptions`; `overrides` globs layer per-file options on top.
  `--config`/`--no-config` short-circuit discovery. Glob matching for
  `overrides`/`.nessembleignore` uses a small dependency-free matcher (or a
  minimal `glob`-style crate if warranted).
- **Execution**: read → `tooling::format_with` → stdout / write-if-changed /
  check-and-collect. Aggregate a non-zero exit for `--check` differences and for
  any I/O error.
- Add `serde` + `serde_json` as **direct** dependencies of `nessemble-cli`
  (already in the lock; add the edges to `Cargo.toml` and
  `[workspace.dependencies]`).

**Wiring.** `main.rs`: new `"format"` dispatch arm; `usage.rs`: a
`("format [options] <path>...", "format assembly source")` row in `COMMANDS`.
`docs/src/usage.md` gains a `format` section and a `.nessemblerc` reference; the
mdBook `SUMMARY.md` gets an entry if a standalone page is warranted.

## 8. Phased plan

**Phase 0 — Options seam (pure refactor, no behavior change). — ✅ done.**
Introduced `FormatOptions` (`indent_style`, `indent_width`, `comma_spacing`) +
`IndentStyle` + `format_with` in `nessemble-core::tooling`; `format` now delegates
to `format_with(source, &FormatOptions::default())`, whose output is
byte-identical to before. The LSP (the sole `format` caller) is untouched; the
struct grows with the opinionated options in Phases 2/4 rather than shipping
fields that do nothing now. *Verified:* existing `tooling` tests pass unchanged,
five new `format_with` tests (custom indent width/tabs, tight commas,
default-parity, idempotency), full workspace suite green, and parity **122/122**.

**Phase 1 — `nessemble format` subcommand (defaults only). — ✅ done.**
Added `crates/nessemble-cli/src/format.rs`, a `format` dispatch arm, a usage-block
row, recursive file/dir discovery (`.asm`), and `--write` / `--check` / stdout,
all using `FormatOptions::default()` (still just the whitespace tidy). Because
`format`'s own flags (`-w`; `-c` collides with assemble's `--check`) would trip
the assemble-mode option parser, the parser hands everything after a leading
`format` to the subcommand raw. Behavior: a single file prints to stdout and is
left untouched; `--write` rewrites changed files in place and reports each;
`--check` lists unformatted files and exits `1`; a directory requires
`--write`/`--check`; `--write` and `--check` are mutually exclusive. *Verified:*
six new CLI integration tests (stdout, check exit/list, recursive write +
non-`.asm` skip + no-op re-run, directory-needs-flag, missing path, help), full
workspace suite green, parity **122/122**.

**Phase 2 — Opinionated structural rules. — ✅ done.** Added Passes 1–3 and 5
to `nessemble-core::tooling` behind new `FormatOptions` fields
(`data_per_line`, `respect_stride_hints`, `blank_line_after_return`,
`max_consecutive_blank_lines`, `final_newline`), on by default: `.db`/`.dw`/
`.color` consolidation with `; @fmt stride=N` hints (type-change/label/comment/
blank flush), a blank line after
`RTS`/`RTI`, blank-run collapsing, and a normalized final newline. `format`
(the LSP entry point) now emits the full house style; its two format tests use
inputs unaffected by the new passes, so they still pass. *Verified:* per-rule
cases, idempotency across passes, and the **byte-preservation test** (assemble
original vs. formatted → identical ROM), plus parity 122/122.

**Phase 3 — `.nessemblerc` + discovery. — ✅ done.** Added
`crates/nessemble-cli/src/rc.rs`: a `serde`-derived `RcConfig`/`RcOptions`
(camelCase, `deny_unknown_fields`) parsed with `serde_json`, mapped onto
`FormatOptions`; parent-dir discovery of `.nessemblerc`/`.nessemblerc.json`;
`--config` / `--no-config`; the `extensions` filter; `.nessembleignore`
exclusion; and prettier-style `overrides` with a dependency-free `*`/`**`/`?`
glob matcher. `serde`/`serde_json` are added as direct deps of `nessemble-cli`
only (core stays dependency-free). *Verified:* 10 `rc` unit tests (globs,
strict keys, mapping) + CLI integration tests (per-file `dataPerLine`,
`--config`, `--no-config`, unknown-key error, override + ignore + extensions).

**Phase 4 — Case & literal normalization (Pass 4). — ✅ done.** Added a `Case`
enum (`Preserve`/`Lower`/`Upper`) and `mnemonic_case`/`hex_digit_case` to
`FormatOptions` (default preserve), applied in `format_line`: the mnemonic (only
the first significant token of an *instruction* line, matched against the shared
`MNEMONICS` set) is re-cased per `mnemonic_case`; the hex-digit letters of
numeric literals per `hex_digit_case`. Labels, registers, identifiers, strings,
and **directive names** are never touched. The `.nessemblerc` gains
`mnemonicCase`/`hexDigitCase` keys. Both are byte-safe (nessemble matches
mnemonics and hex case-insensitively), covered by a dedicated byte-preservation
test. *Verified:* mnemonic/hex mapping tests (including the label-named-like-a-
mnemonic and register guards), idempotency, byte preservation, parity 122/122.

**Phase 5 — Docs. — ✅ done.** `docs/src/usage.md` gains a `format` command
section and a full `.nessemblerc` reference (option table, stride hints,
`overrides`, `.nessembleignore`); `docs/src/editor.md` now notes that the
editor's "format document" runs the same engine as the CLI. The optional
`nessemble format --check` CI step is deferred — the repo's corpus files are
byte-exact golden fixtures, so gating them on the formatter is left as a separate
decision.

## 9. Testing strategy

- **Core unit tests** (in `tooling.rs`, alongside the existing ones): one per
  pass (consolidation, type-change flush, comment pinning, stride hints,
  RTS/RTI spacing, blank collapsing, case normalization).
- **Idempotency**: `format_with(format_with(x, o), o) == format_with(x, o)`
  across a matrix of option sets.
- **Default-parity golden test**: `FormatOptions::default()` with the structural
  rules *disabled* reproduces today's `format` output on a fixture set (guards
  Phase 0).
- **Byte-preservation (the load-bearing test)**: for a corpus of sample sources,
  assemble the original and the formatted output and assert **identical ROMs**
  (nessemble-core's existing `tests/corpus.rs` harness is the model). This is the
  formatter's ROM-equivalence safety net.
- **CLI integration tests** (`crates/nessemble-cli/tests/`): tempdir fixtures for
  single-file stdout, `--write` changed/unchanged, `--check` exit codes,
  recursive directory formatting, the `extensions` filter, `.nessembleignore`
  exclusion, `overrides` glob layering, `.nessemblerc` discovery/precedence,
  `--config`, `--no-config`, and malformed / unknown-key config errors.

## 10. Risks & mitigations

- **LSP output changes.** Turning the structural rules on by default means
  `textDocument/formatting` now consolidates data and adds routine spacing.
  *Mitigation:* this is the intended single-house-style outcome; update the LSP
  formatting tests, call it out in the changelog, and bump **minor**. If we ever
  want editors to stay conservative, the seam allows the LSP to pass a lighter
  `FormatOptions` — but the default is one style everywhere.
- **A formatting change alters assembled bytes.** *Mitigation:* the
  byte-preservation corpus test (§9) is a hard gate; the consolidation guards
  (never merge across labels/comments/instructions/blanks) preserve semantics,
  and `.db`/`.dw` grouping is purely presentational.
- **Data consolidation eats meaningful line breaks.** Hand-laid tables can carry
  meaning in their line structure. *Mitigation:* comments pin structure (a
  commented line never merges), `; @fmt stride=N` hints give explicit control,
  and `dataPerLine: 0` disables consolidation per project/override.
- **Directive-name casing.** nessemble directives are **case-sensitive**
  (`.db` ≠ `.DB`), so re-casing a directive name could change what assembles.
  *Mitigation:* there is **no** directive-case option — directive names are
  always emitted verbatim. (Detection of *which* directive a line is may still
  match case-insensitively, but the emitted text is never altered.)
- **Config foot-guns.** Malformed JSON or unknown keys. *Mitigation:* fail loudly
  with a path + reason and a non-zero exit; never silently format with a
  half-parsed config.

## 11. Decisions

**Settled (from the planning discussion):**

1. **Config format** — **JSON** (`.nessemblerc` / `.nessemblerc.json`), the
   prettier convention; parsed with `serde_json`, already in the tree.
2. **Default CLI behavior** — **prettier-style**: single file → stdout; `--write`
   edits in place; `--check` is the CI gate; a directory requires `--write` or
   `--check`.
3. **Rule scope** — adopt **all** of: `.db`/`.dw`/`.color` consolidation
   (+ `; @fmt stride=N` hints), blank line after `RTS`/`RTI`, collapse excess
   blank lines, and **case/literal normalization** (the last **off by default**,
   opt-in via config). Case normalization covers **mnemonics and hex digits
   only** — directive names are never re-cased, because nessemble directives are
   case-sensitive.
4. **One engine** — extend `nessemble-core::tooling` via a `FormatOptions` seam;
   the LSP and the CLI share it, and the defaults *are* the house style.
5. **Core stays dependency-light** — `serde` config lives in the **CLI**, mapped
   onto a plain `FormatOptions` in core.
6. **`overrides` + `.nessembleignore` ship in v1** — prettier-style per-glob
   option layering and gitignore-style exclusion are part of the first release,
   not a follow-up.
7. **Strict config keys** — unknown `.nessemblerc` keys are a **hard error**
   (`deny_unknown_fields`), not silently ignored.
8. **No `--stdout` flag** — a single file already prints to stdout by default;
   an explicit `--stdout` is unnecessary.
9. **`extensions` config** — directory walks format `.asm` by default,
   overridable via the `extensions` key; explicit file arguments always format.

*All planning decisions are settled; remaining choices are implementation
details within each phase.*

---

*Phase 0 (the `FormatOptions` seam) is implemented; Phases 1 → 5 follow, each
landing with tests. Phase 0 carries a `minor` changeset for the new
`nessemble-core::tooling` public API (`format_with` / `FormatOptions` /
`IndentStyle`); the later phases add their own changesets as user-facing surface
lands.*
