# nessemble-rs: A Plan for Built-in Linting Rules

> Status: **Planning — nothing implemented yet.** This document specifies a
> `nessemble lint` subcommand and an in-editor lint pass that report — but never
> rewrite — style problems in nessemble assembly, starting with a single rule:
> **a code block that opens without an explanatory comment is flagged.** It is
> the ESLint to the formatter's Prettier — the two are deliberately separate
> tools that share one lexer and one `.nessemblerc`. All open decisions have been
> settled with the maintainer (see [§11](#11-decisions)), including the two
> former implementation choices: the ignore regex uses **`regex-lite`**, and the
> config layer is **promoted to a shared `nessemble-rc` crate**.

---

## 1. Goal

Ship a first-class **linter** in the CLI and the language server:

```sh
nessemble lint path/to/file.asm        # report problems for one file
nessemble lint path/to/directory       # walk a tree and report problems
```

Where [`nessemble format`](005-formatter.md) is **Prettier** — opinionated,
byte-preserving, and it *rewrites* — `nessemble lint` is **ESLint**: it
*reports* problems and changes nothing. The first (and, for v1, only) rule
enforces the well-established disassembly convention that **every code block
carries a comment**: a named label that opens a new block should have an
explanatory comment nearby, or the linter warns.

Two hard requirements from the request:

1. Report a **warning/error when a code block has no comment**, the way a linter
   layered on top of a formatter would (Prettier formats; ESLint-style rules
   flag). Findings are **advisory** — the byte-exact ROM is never touched.
2. Let a project **exempt certain label/constant name formats via regex**, so
   machine-generated labels (`loc_NN_XXXX`, `data_NN_XXXX`, …) don't drown the
   report in noise.

## 2. Why this is a good fit

nessemble-rs already contains the hard parts:

- **`nessemble-core::tooling`** has the same **lossless lexer** (`tooling::lex`)
  the formatter uses: it segments the *entire* source — whitespace, comments,
  labels, directives — into byte-ranged [`Lexeme`]s with no gaps. Identifying "a
  line that is just a `label:` definition," "a comment line," and "a blank line"
  falls straight out of that stream, so the block-entry / nearby-comment analysis
  needs no new parser.
- **`.nessemblerc` discovery, `extensions`, `.nessembleignore`, and per-glob
  `overrides`** already exist for the formatter (`nessemble-cli::rc`). The linter
  reuses the identical discovery, file-walk, and ignore machinery — a project has
  **one** config file governing both tools.
- **The language server already publishes diagnostics**
  (`textDocument/publishDiagnostics`, via `nessemble-lsp`'s `single_file` /
  project analysis paths, mapping `nessemble-core::Diag` → LSP `Diagnostic`).
  Lint findings slot in as one more diagnostic source at a gentle severity.

What's missing is (a) the **rule engine** (a `tooling::lint` seam mirroring
`tooling::format`), (b) a **`nessemble lint` CLI surface**, (c) a **`lint`
section** in `.nessemblerc`, and (d) **wiring the findings into the LSP**. This
plan adds those four things while reusing the lossless-lexer + config foundations
so highlighting, formatting, and linting keep sharing one engine.

## 3. Current state — what we have and what's missing

Grounded in the current code:

**Available today**

- `nessemble-core::tooling::lex` — gap-free, reversible segmentation with
  `LexKind` (`Whitespace`, `Newline`, `Comment`, `Number`, `Directive`, `Ident`,
  `Punct`, …). The formatter already splits this stream into physical lines; the
  linter reuses the same split.
- `nessemble-core::tooling::{format_with, FormatOptions}` — the model this plan
  copies: an options struct in core, a pure transform, a back-compat shim, and
  the LSP + CLI as the two callers.
- `nessemble-cli::rc` — `.nessemblerc` parsing (`serde`, `deny_unknown_fields`),
  parent-dir discovery, `--config` / `--no-config`, `extensions`,
  `.nessembleignore`, and a dependency-free glob matcher for `overrides`.
- `nessemble-cli::format::{collect_files, run}` — the recursive walk (extension
  filter + ignore) and the path/dir/stdout argument handling to mirror.
- `nessemble-lsp` — a working diagnostics pipeline (`compute_diagnostics`,
  `single_file`, `project_diag_to_lsp`) with `DiagnosticSeverity` mapping.

**Gaps that shape the plan**

- **No rule engine.** There is nothing in core that reports style findings; the
  block-entry / nearby-comment analysis is new.
- **No `nessemble lint` command.** The CLI dispatches
  `init`/`scripts`/`reference`/`lsp`/`format`/`coverage`; a `lint` arm is new.
- **No lint config.** `.nessemblerc` shapes formatting only. A `lint` section
  (rule severities, comment window, ignore regexes) is new.
- **No regex crate anywhere in the tree.** The ignore-by-name feature needs one.
  This is the one genuinely new third-party dependency (§7); it stays out of
  `nessemble-core`.
- **The LSP doesn't read `.nessemblerc`.** It formats with `FormatOptions::
  default()` today. To honor a project's lint config in the editor, config
  resolution must become reachable from the LSP (§7, Phase 3).

## 4. CLI surface

A new subcommand, added to the clap `Command` enum in
`crates/nessemble-cli/src/main.rs` (a `Lint(lint::LintArgs)` variant dispatched
to `lint::run`), with its own module `crates/nessemble-cli/src/lint.rs`.

```
nessemble lint [options] <path>...

  <path>              one or more .asm files and/or directories
      --config F      use F as the .nessemblerc instead of discovering one
      --no-config     ignore any .nessemblerc; lint with built-in defaults
      --max-warnings N  exit non-zero if more than N warnings are reported
                        (errors always fail regardless); default: unlimited
      --quiet         report errors only; suppress warning-level findings
  -h, --help          print this message
```

**No `--write` / `--fix`.** A linter reports; it never edits. (Autofix is a
possible far-future follow-up but is explicitly out of scope — the formatter owns
rewriting.) A single file and a directory behave the same: both print a report.
A directory is walked recursively for the configured `extensions` (default
`.asm`), skipping `.nessembleignore` matches — reusing `format`'s
`collect_files`.

**Output (ESLint-style, grouped by file):**

```
src/prg/07.asm
     42:1   warning   sound_engine   require-block-comment
    103:1   warning   note_table     require-block-comment

✖ 2 problems (0 errors, 2 warnings)
```

- Findings are grouped under each file's relative path, then
  `  LINE:COL  <severity>  <label>  <rule-id>` (1-based line/column). The
  `<label>` column names the offending block label; `<rule-id>` is the rule that
  fired.
- A summary footer tallies errors and warnings. When there are none:
  `✓ No problems.`

**Exit codes** reuse the CLI's constants: `0` (`RETURN_OK`) when no **error**-
severity findings exist and `--max-warnings` is not exceeded; `1`
(`RETURN_EPERM`) when any **error** finding exists, when warnings exceed
`--max-warnings`, or on an I/O / config error. Usage errors exit `129`
(`RETURN_USAGE`), like the rest of the CLI. **Warnings alone do not fail the
build** (ESLint semantics) — a project makes a rule blocking by configuring it as
`"error"`, or gates warnings with `--max-warnings 0`.

**Config discovery** is identical to `format`: per input, walk up to find the
nearest `.nessemblerc` / `.nessemblerc.json`; `--config` forces one,
`--no-config` uses built-in defaults; `.nessembleignore` excludes paths from
directory walks.

## 5. `.nessemblerc` — the `lint` section

Linting is configured in the **same** `.nessemblerc` as the formatter, under a
new top-level `lint` key. Everything is optional; with no config, the built-in
default (the one rule at `"warn"`, no exemptions, window ±3) applies — so a
project with no `.nessemblerc` still gets a useful report.

```jsonc
{
  // ── formatter keys (unchanged, see plan 005) ──────────────
  "dataPerLine": 8,

  // ── linter ────────────────────────────────────────────────
  "lint": {
    // Per-rule severity: "off" | "warn" | "error", or a
    // [severity, { …ruleOptions }] pair (ESLint's shape).
    "rules": {
      "require-block-comment": ["warn", { "window": 3 }]
    },

    // Label/constant NAMES matching any of these regexes are exempt from
    // every rule — e.g. machine-generated disassembly labels. Anchors are
    // the author's to add ("^loc_" vs "loc_").
    "ignore": ["^loc_[0-9A-Fa-f]", "^data_[0-9A-Fa-f]"]
  }
}
```

**Rule severity.** Each rule is `"off"`, `"warn"`, or `"error"`, optionally as a
`[severity, options]` pair. The **`window`** option (default `3`) is the
`require-block-comment` search radius: a block label is clean if any line within
`±window` lines contains a comment (`;`). Increase it for loosely-commented
files; decrease it to enforce tight co-location.

**`ignore`.** A list of regex strings. A block label whose **name** matches any
pattern is skipped by all rules — the requested "ignore certain label/constant
formats via regex." No patterns ship by default (the maintainer chose an
explicit list over built-in exemptions), so a fresh project flags *every*
undocumented block until it opts specific name shapes out. The patterns are the
author's to anchor.

**Strictness.** The `lint` object and each rule's `options` use
`deny_unknown_fields`, matching the formatter's strict-keys decision — a typo
(`windwo`) is a hard error with the key and file path. The **rule-name keys**
inside `rules` are validated against the known rule registry after parse; an
unknown rule id (`require-block-commnt`) is likewise a hard error, not a silent
no-op. Malformed regexes fail loudly at config-load with the offending pattern.

**Interaction with `overrides`.** Per-glob `overrides` (already in the formatter
config) may carry a `lint` block too, so `src/data/**` can loosen or disable a
rule for hand-laid tables. Override layering reuses the existing mechanism.

## 6. The rule (the one opinion, and the seam for more)

**`require-block-comment`** — *a code block that opens without a nearby comment
is flagged.*

A **block-opening label** is a line whose only significant tokens are an
identifier followed by `:` (a label definition), where — scanning **backwards**
over any comment lines — the first non-comment line is **blank** or the **top of
the file**. That is the "this label starts a new, documented section" signal.

- Labels that **follow code directly** (internal branch targets — the preceding
  non-comment line is an instruction/data line, not a blank) are **not** blocks
  and are never flagged. They're jump destinations, not documented entry points.
- **Anonymous labels** (`:`, `:+`, `:-`, `:++`, …) are never flagged.
- A block label is **clean** when any line within `±window` lines contains a
  comment (`;`), whether above, on, or below the label. Otherwise it warns.
- A label whose **name matches any `ignore` regex** is skipped entirely.

This is a faithful port of a proven "document every code block" lint, but built
on the lossless lexer rather than line regexes: the block-entry and
nearby-comment scans run over the same physical-line split the formatter uses, so
label detection is exact (a `Ident` + `Punct(":")` line) and robust to spacing.

**Scope decision (settled): labels only.** Constant definitions
(`NAME = value`, `.equ`) are **not** subjects in v1 — only block-opening labels
are checked. The `ignore` regex still matches against a label's *name*; the
"label/constant formats" phrasing is honored by the regex matching names, not by
adding a constant rule. (A `require-constant-comment` rule is a natural future
addition through the seam below.)

**The extensibility seam.** Even with one rule, the engine is a **registry**, not
a hard-coded check, so new rules drop in without restructuring:

```rust
// nessemble-core::tooling (new `lint` module)

/// A lint finding: which rule, where, and the subject name.
pub struct Finding {
    pub rule: RuleId,          // e.g. RuleId::RequireBlockComment
    pub line: u32,             // 1-based
    pub column: u32,           // 1-based (label column)
    pub subject: String,       // the label name
}

/// Per-run configuration, mapped from `.nessemblerc` by the caller.
pub struct LintOptions<'a> {
    /// Severity per rule; `Off` rules are not run.
    pub severities: RuleSeverities,   // plain data (enum per rule)
    /// Window for `require-block-comment`.
    pub window: usize,
    /// Names matching this predicate are exempt from every rule. The closure
    /// keeps regex OUT of core — the caller compiles the patterns.
    pub ignore: &'a dyn Fn(&str) -> bool,
}

/// Run every enabled rule over `source`, returning findings in source order.
pub fn lint(source: &str, opts: &LintOptions) -> Vec<Finding> { … }
```

Each rule is a function `fn(&[PhysicalLine], &LintOptions, &mut Vec<Finding>)`
registered in a small table keyed by `RuleId`; `lint` runs the enabled ones and
sorts findings by `(line, column)`. Adding a rule = one function + one registry
entry + one config-name mapping. **Severity is not applied in core** — `lint`
emits raw findings tagged with their `RuleId`; the caller maps `RuleId →
severity` for display, exit codes, and LSP severity. This keeps core free of both
`serde` and `regex`.

**Invariants**

- **Read-only:** `lint` never mutates source and has no `--write`. The ROM is
  untouchable by design — this is the clean ESLint/Prettier split.
- **Deterministic & order-stable:** findings come back in source order for
  reproducible reports and stable test fixtures.
- **Trivia-exact:** built on the lossless lexer, so label/comment/blank
  detection matches what the highlighter and formatter see.

## 7. Architecture

**Core (`nessemble-core/src/tooling.rs`, new `lint` submodule) — the engine.**
`Finding`, `RuleId`, `RuleSeverity`, `LintOptions`, and `lint(source, &opts)` as
in §6. Reuses the existing `lex` + physical-line split (factor the formatter's
inline split into a shared helper both call). **Core gains no new dependencies:**
`LintOptions` is plain data plus an `ignore` **closure**, so `regex` and `serde`
stay out of core — exactly the boundary the formatter drew for `serde`.

**Config (where `regex` lives).** The `.nessemblerc` `lint` section is parsed
with `serde` and compiled to a `LintOptions` — including turning the `ignore`
regex list into the `&dyn Fn(&str) -> bool` predicate core wants. `regex-lite`
compilation is the **only** new third-party dependency, and it belongs in the
shared `nessemble-rc` config crate (below), **never in core** — the same pattern
as `serde`.

> **Regex crate (settled): `regex-lite`.** Pure-Rust, zero transitive deps, in
> keeping with the "core stays dependency-light" ethos and small binary size;
> anchored name patterns don't need the full `regex` engine. It's a leaf
> dependency of the `nessemble-rc` config crate only — never of core.

**CLI (`nessemble-cli/src/lint.rs`) — new module.** Argument parsing
(`LintArgs`: paths, `--config`, `--no-config`, `--max-warnings`, `--quiet`) in
the clap-derive style of `format.rs`; file/dir discovery via the shared
`collect_files`; config resolution via the shared config layer; then read →
`tooling::lint` → group, apply severities, print the ESLint-style report, and
compute the exit code. Wires a `("lint [options] <path>...", …)` help row and a
`main.rs` dispatch arm.

**LSP (`nessemble-lsp`) — findings as diagnostics.** After (or alongside) the
existing assemble-based diagnostics, run `tooling::lint` on each open buffer and
publish findings as additional `Diagnostic`s. **On by default, at a gentle
severity:** lint diagnostics use `DiagnosticSeverity::INFORMATION` (or `HINT`) —
deliberately quieter than the assembler's `ERROR`/`WARNING` squiggles — with
`source = "nessemble-lint"` and the rule id in the message/`code`, so editors let
users filter or dim them. This is a per-buffer, single-file analysis (no include
graph needed — the rule is intra-file), so it also runs when no workspace root is
open.

> **Sharing config between CLI and LSP (settled).** The LSP must read the same
> `.nessemblerc` `lint` section the CLI does, but `rc.rs` currently lives inside
> `nessemble-cli` (which the LSP can't depend on). The plan **promotes the config
> layer into a new shared crate, `nessemble-rc`**, that owns `.nessemblerc`
> discovery + parsing + `regex-lite` compilation, depended on by both the CLI and
> the LSP. `nessemble-cli` re-exports or thinly wraps it so the existing
> `format` config path keeps working unchanged. Bonus: this also lets the LSP
> honor the *formatter*'s `.nessemblerc` options later, which it doesn't today.

**Wiring.** `main.rs`: `Lint(lint::LintArgs)` variant + dispatch arm.
`docs/src/usage.md`: a `lint` command section and a `.nessemblerc` `lint`
reference (rule table, `ignore`, `window`, severities). `docs/src/editor.md`: a
note that the editor shows the same lint findings as the CLI.

## 8. Phased plan

**Phase 0 — Lint engine seam in core.** Add the `lint` submodule to
`nessemble-core::tooling`: `Finding`, `RuleId`, `RuleSeverity`, `LintOptions`
(with the `ignore` closure), the registry, and `require-block-comment` built on
the shared physical-line split. No new core dependencies. *Verify:* ported unit
tests for the block-entry scan, the nearby-comment scan, and end-to-end
`lint` (the reference tool's cases, translated to Rust), plus an ignore-predicate
test; full workspace suite green; parity unchanged.

**Phase 1 — `nessemble lint` subcommand (defaults only).** Add
`nessemble-cli/src/lint.rs`, the `Lint` dispatch arm, a usage row, single-file +
recursive-directory discovery (reuse `collect_files`), the ESLint-style grouped
report, and exit codes — using built-in defaults (rule at `warn`, window 3, no
ignores). *Verify:* CLI integration tests for grouped output, the summary footer,
`--quiet`, `--max-warnings`, a clean file, a directory walk, and a missing path.

**Phase 2 — `nessemble-rc` shared crate + `.nessemblerc` `lint` section + regex
ignore.** Extract the existing `nessemble-cli/src/rc.rs` config layer into a new
**`nessemble-rc`** crate (`.nessemblerc` discovery, parsing, glob matching,
`overrides`), and have `nessemble-cli` depend on it so the `format` config path
is unchanged. Extend the schema with the `lint` block (`rules` severity map with
`[severity, options]`, `window`, `ignore`), `deny_unknown_fields` + post-parse
rule-name validation; add the **`regex-lite`** dependency to `nessemble-rc` and
compile `ignore` into the predicate; map severities to display/exit. Support
`lint` inside `overrides`. *Verify:* `nessemble-rc` unit tests (severity parse,
unknown-key and unknown-rule errors, malformed-regex error, override layering,
glob matching moved with the crate) + CLI integration tests (ignore regex exempts
matching labels, per-rule `off`, `error` → non-zero exit, `--max-warnings`).

**Phase 3 — LSP diagnostics.** Add a `nessemble-rc` dependency to
`nessemble-lsp`; resolve each open buffer's `.nessemblerc` `lint` config through
it; run `tooling::lint` per buffer; publish findings as `INFORMATION`/`HINT`
diagnostics with `source = "nessemble-lint"`, honoring the discovered config;
clear them when a comment is added. *Verify:* LSP tests that an undocumented
block produces a low-severity lint diagnostic, that adding a nearby comment
clears it, that an `ignore`-matched label produces none, and that an `off` rule
is silent.

**Phase 4 — Docs + changeset.** `usage.md` `lint` section and `.nessemblerc`
`lint` reference; `editor.md` note; `SUMMARY.md` entry if a standalone page is
warranted; a `minor` changeset for the new subcommand, the new
`nessemble-core::tooling` public API, and the new LSP diagnostic source.

## 9. Testing strategy

- **Core unit tests** (in the `lint` submodule): the block-entry backward scan
  (top-of-file, blank-separated, comment-run-to-top, code-precedes → not a
  block), the nearby-comment scan across the window (above/on/below, window
  boundaries, clamping near file edges), anonymous-label skipping, and the
  ignore predicate. These port the reference tool's proven cases 1:1.
- **Determinism/order:** findings come back sorted by `(line, column)` for a
  multi-label fixture.
- **CLI integration tests** (`crates/nessemble-cli/tests/`): grouped report shape
  and footer, exit codes (clean = 0, `error` = 1, `--max-warnings` gate),
  `--quiet`, directory walk with `.nessembleignore`, `--config` / `--no-config`,
  the `ignore` regex, per-rule `off`, and unknown-key / unknown-rule /
  bad-regex config errors.
- **LSP tests** (`crates/nessemble-lsp`): an undocumented block yields exactly one
  low-severity lint diagnostic with `source = "nessemble-lint"`; adding a comment
  within the window clears it; an `ignore`-matched or `off` rule yields none.
- **No byte-preservation test needed** — the linter never rewrites source (its
  defining difference from the formatter).

## 10. Risks & mitigations

- **New `regex` dependency vs. the dependency-light ethos.** *Mitigation:* it's a
  leaf dependency of the **config layer only** (`regex-lite` preferred, zero
  transitive deps); `nessemble-core` stays regex-free behind the `ignore`
  closure, exactly as it stayed serde-free behind `FormatOptions`.
- **Report noise on machine-generated labels.** A raw disassembly has thousands
  of `loc_`/`data_` labels; flagging all of them is useless. *Mitigation:* the
  `ignore` regex list, per-rule `off`, `overrides` per directory, and the
  block-entry-only scope (branch targets are never flagged) all bound the noise;
  a project tunes the report to what it actually wants documented.
- **LSP squiggle fatigue.** Editor diagnostics for a style rule could annoy.
  *Mitigation:* lint diagnostics ship at `INFORMATION`/`HINT` — quieter than
  assembler errors — with a distinct `source` for filtering, and any rule can be
  `off`.
- **Config sharing across CLI and LSP.** Extracting `rc.rs` into a shared crate
  touches the formatter's config path. *Mitigation:* the extraction is a
  mechanical move behind the existing API; a lighter LSP-local discovery is the
  fallback if the crate split proves disruptive.
- **Rule/formatter coupling.** Blurring lint and format would resurrect the
  Prettier/ESLint confusion. *Mitigation:* strict separation — `format` rewrites
  and never reports; `lint` reports and never rewrites; they share only the lexer
  and the config file.

## 11. Decisions

All settled with the maintainer:

1. **Command surface** — a **separate `nessemble lint` subcommand** *plus*
   **LSP diagnostics**. Lint and format stay distinct (ESLint vs. Prettier);
   they share the lexer and `.nessemblerc`.
2. **Rule scope** — **one rule now (`require-block-comment`) behind an
   extensible rule-registry seam**, so more rules add cleanly later.
3. **Severity model** — **ESLint-style per-rule** `off`/`warn`/`error`: any
   `error` fails the run (non-zero exit); `warn` alone does not (gate with
   `--max-warnings`).
4. **Config location** — a **`lint` section in the existing `.nessemblerc`**,
   reusing discovery, `overrides`, and strict-keys.
5. **Ignore regex** — a **list of regex strings matched against the label
   name**; no built-in exemptions (the project supplies its own).
6. **Block subjects** — **block-opening labels only** (named labels whose
   preceding non-comment line is blank/top-of-file); internal branch targets and
   anonymous labels are exempt; constants are not checked in v1.
7. **LSP behavior** — findings publish **on by default at
   `INFORMATION`/`HINT` severity**, with `source = "nessemble-lint"`, so they
   read as gentle suggestions distinct from assembler errors.
8. **Console output** — **ESLint-style, grouped by file**, with a
   `LINE:COL  severity  label  rule-id` body and a problem-count footer.

9. **Regex crate** — **`regex-lite`** (pure-Rust, zero transitive deps); a leaf
   dependency of the `nessemble-rc` config crate, never of core (§7).
10. **Shared config crate** — **promote `rc.rs` to a new `nessemble-rc` crate**
    shared by the CLI and the LSP, so both honor the same `.nessemblerc` (§7,
    Phase 2); the formatter's config path moves with it, unchanged.

---

*Nothing here is implemented yet. Phase 0 (the `tooling::lint` seam) lands first,
carrying a `minor` changeset for the new `nessemble-core` public API; the later
phases add their own changesets as the CLI subcommand, config, and LSP surface
land.*
