# nessemble-rs: A Plan for Comment Directives

> Status: **In progress — Phase 0 done ([§10](#10-phased-plan)); decisions
> settled ([§13](#13-decisions)).** This document codifies the tool-directive comments nessemble
> reads out of assembly source. It (a) promotes the formatter's existing ad-hoc
> `; @fmt stride=N` hint to a namespaced **`; @nessemble-format stride=N`** (with
> `@fmt` kept as a deprecated alias), (b) adds coverage directives —
> **`; @nessemble-coverage-ignore-next-line`** for one line and
> **`; @nessemble-coverage-ignore start` / `end`** for a region — and (c) answers
> the "should the language server know about these?" question: **yes, and the
> bulk of it rides the existing lint→diagnostics pipe rather than new LSP
> machinery** ([§8](#8-the-language-server-is-this-necessary)).

---

## 1. Goal

Give nessemble **one documented, namespaced, validated way** for a comment to
address a nessemble tool:

```asm
; @nessemble-format stride=2                 ; formatter: re-flow this data run
    .db $01, $02
    .db $03, $04

; @nessemble-coverage-ignore-next-line       ; coverage: drop the next line
    .db $FF                                  ; hardware-quirk pad, never read

; @nessemble-coverage-ignore start           ; coverage: drop everything from here…
    .incbin "unreachable_demo.bin"
; @nessemble-coverage-ignore end             ; …to here (or to end of file)
```

Three requirements from the request:

1. **Codify `@fmt` as `@nessemble-format`** — the current spelling is
   unnamespaced, undiscoverable, and collides with every other tool that reads
   `@fmt`-ish comments.
2. **Add coverage ignores** — `@nessemble-coverage-ignore-next-line` for one
   line and a `start`/`end` **region** pair for anything larger (up to and
   including a whole file, by opening a region and never closing it), in the
   shape every coverage tool ships (`istanbul ignore next`,
   `# pragma: no cover`, `LCOV_EXCL_START`/`LCOV_EXCL_STOP`).
3. **Decide the language-server story** and, if warranted, implement it (§8).

The through-line is that these stop being three one-off string checks and become
**one registry** with one grammar, one scanner, one validation path, and one
docs table — so directive number four costs a table row.

## 2. Why this is worth doing now

`@fmt` is the only comment directive in the tree today, and it is parsed by a
hand-rolled `strip_prefix("@fmt")` inside the formatter
(`nessemble-core::tooling::parse_hint`). That was right for one hint. It is the
wrong foundation for three, for four reasons:

- **No namespace.** `@fmt` is a generic token. `@nessemble-format` says who it
  is addressed to and cannot be confused with an editor's or another
  assembler's pragma.
- **Silent failure.** A typo (`; @fmt strid=2`, `; @nessmble-format stride=2`)
  is indistinguishable from prose today: the hint quietly does nothing and the
  data re-flows to `dataPerLine` with no complaint. Every directive added widens
  that hole.
- **No shared parse.** The coverage directives need the same "is this comment
  addressed to a tool?" question answered, over the same lossless lexer, in a
  second crate (`coverage`) and a third consumer (the LSP). Three copies of a
  fragile string check is exactly what happened to the byte-count estimator that
  [plan 007](007-cdl-based-coverage.md) replaced.
- **No discoverability.** Nothing offers, documents, or validates these in the
  editor. A feature nobody can find is a feature nobody uses.

## 3. Current state

**What exists**

- `nessemble-core::tooling::lex` — the lossless lexer; `LexKind::Comment` already
  delimits every comment exactly, so directive detection needs no new parsing.
- `tooling::parse_hint` — the `@fmt stride=N[,N,…]` parser, called only from
  `consolidate_data`, gated by `FormatOptions::respect_stride_hints`
  (`.nessemblerc` `respectStrideHints`, default `true`).
- `tooling::lint` — the [plan 008](008-linting-rules.md) rule registry
  (`RuleId`, `RULES`, `LintOptions`, `Finding`), already surfaced **twice**: as
  the `nessemble lint` CLI report and as LSP diagnostics sourced
  `nessemble-lint`. **Adding a rule here reaches both surfaces for free.**
- `nessemble-core::coverage` — `build_report(&SourceMap, &dyn CdlSource)`
  building `CoverageReport`/`FileCoverage`/`LineCoverage` from the byte-exact
  source map, plus `to_json` / `to_lcov`; `nessemble-cli::coverage` drives it and
  folds in Rhai script coverage.
- `nessemble-rc` — the shared `.nessemblerc` layer (formatter options, `lint`
  section with per-rule severities, `overrides`), read by both the CLI and LSP.

**What's missing**

- No directive **registry or grammar** — `@fmt` is a bare string literal in one
  function.
- No **validation** of any directive, anywhere.
- `coverage::build_report` has **no notion of an excluded line or file**, and
  nothing reads source text on the coverage path (core is deliberately fs-free;
  see §6.3).
- The LSP knows nothing about directive comments: no completion, no hover, no
  quick fix. (It does *honor* `@fmt` when formatting, because it calls
  `tooling::format`, which uses `FormatOptions::default()` —
  `respect_stride_hints: true`.)

## 4. The grammar

One shape, for every directive, present and future:

```
; @nessemble-<name> [args]   [; trailing prose]
```

Rules, all decided so the scanner is unambiguous:

1. **Comment-only.** A directive is recognized only inside a `LexKind::Comment`
   lexeme — never in a string literal, never in code.
2. **Own-line only (v1).** The comment must be the only significant content on
   its physical line. A trailing directive (`LDA #$00 ; @nessemble-coverage-ignore-next-line`)
   is **inert** and is flagged by lint (§7). This preserves `@fmt`'s current
   behavior exactly rather than introducing a new restriction.
3. **First token.** After the leading `;` run (`;`, `;;`, `;;;`) and any
   whitespace, the first token must start with `@`. Prose that merely mentions
   `@nessemble-format` mid-sentence is not a directive.
4. **Exact name match, case-sensitive, lower-case.** The name token runs to the
   next whitespace or end of comment and is looked up **exactly** in the
   registry — so `@nessemble-coverage-ignore-next-line` and
   `@nessemble-coverage-ignore` never shadow each other, with no
   longest-prefix subtleties.
5. **Args are per-directive**, parsed from the remainder up to an optional
   trailing `;` (so `; @nessemble-format stride=2 ; two per line` works, as it
   does today).
6. **Unknown `@nessemble-*` names are an error, other `@…` tokens are prose.**
   `@todo`, `@author`, `@param` are never touched; `@nessemble-formt` is
   reported (§7). `@fmt` is in the registry as a deprecated alias, so it is
   reported too — as a deprecation, not an unknown.

### The registry (v1)

| Directive | Scope | Args | Consumer |
|---|---|---|---|
| `@nessemble-format` | the data run that follows | `stride=N[,N,…]` | formatter |
| `@nessemble-coverage-ignore-next-line` | the next significant line | none | `nessemble coverage` |
| `@nessemble-coverage-ignore` | from `start` to `end` (or to EOF) | `start` \| `end` | `nessemble coverage` |
| `@fmt` | *(deprecated alias of `@nessemble-format`)* | `stride=N[,N,…]` | formatter |

**`@nessemble-coverage-ignore` requires its `start`/`end` argument.** A bare
`; @nessemble-coverage-ignore` is *malformed*, not a whole-file shorthand
(§13.1): with regions, a bare form appearing mid-file would have to mean
"ignore the whole file" while `start` on the same line means "ignore from here
down", and that pair of readings is exactly the trap worth not building. The
whole-file case is a `start` at the top with no `end`, which is one rule instead
of two. The malformed bare form is reported with a message naming both
arguments.

Directive five is a row in this table, a match arm, and a docs line.

## 5. Semantics

### 5.1 `@nessemble-format`

Behavior is **unchanged** from today's `@fmt` — this is a rename plus a
registry, not a formatter change. `stride=N[,N,…]` overrides `dataPerLine` for
the following data run; multiple strides cycle and the last repeats; two
consecutive blank lines end the run. `respectStrideHints: false` disables *both*
spellings.

**`@fmt` stays working.** It is a shipped, documented, released feature (see
`docs/src/usage.md` § Stride hints); silently dropping it would re-flow real
projects' hand-laid tables on their next format. It remains honored
indefinitely, is marked deprecated in the docs, is flagged by a `warn`-level
lint rule (§7), and carries an LSP quick fix that rewrites it (§8). Removal, if
ever, is a `major`.

### 5.2 `@nessemble-coverage-ignore-next-line`

Applies to the **next significant line** — the first following line that is
neither blank nor a comment. Skipping trivia (rather than taking line *N+1*
literally) is what makes the common shape work:

```asm
; @nessemble-coverage-ignore-next-line
; the NMI stub is only reachable from a mapper IRQ we can't trigger in CI
    JMP nmi_stub
```

- Stacked directives resolving to the same target line collapse to one exclusion.
- A directive with no following significant line (end of file) is inert and
  flagged (§7).
- The target is a **source line**, not an instruction: if that line emitted no
  PRG bytes it has no entry in the report and the directive is a harmless no-op
  (also flagged as ineffective, §7).

### 5.3 `@nessemble-coverage-ignore start` / `end`

A **region**: every line from the `start` directive's line through the matching
`end` directive's line is excluded. Both directive lines are themselves comments
and so were never in the report; what the boundaries do is delimit the lines
between them.

```asm
; @nessemble-coverage-ignore start
    ; the mapper-3 path is dead on every board we test against
mapper3_init:
    LDA #$00
    STA mapper_reg
; @nessemble-coverage-ignore end
```

The rules that pin down the corner cases:

- **An unclosed region runs to end of file.** A `start` with no matching `end`
  is *not* an error — it is the idiom for "ignore this whole file" (put it at the
  top) and for "ignore this trailing scratch section". It is deliberately silent,
  since flagging it would make the whole-file case noisy.
- **Regions are per-file and do not propagate through `.include`.** A region open
  at the point of an `.include` does not extend into the included file, and does
  not resume differently after it — regions are resolved per file, over that
  file's own text, before anything is joined. An included file carries its own
  directives. This keeps the rule readable from the file you are looking at.
- **Regions do not nest.** A `start` inside an already-open region is redundant
  and is flagged as ineffective (§7); it does not open a second region, and the
  first `end` closes the region.
- **An `end` with no open region** is inert and flagged (§7).
- **Whole-file** is therefore `; @nessemble-coverage-ignore start` in the header
  block and nothing else.

`-next-line` and regions compose: a `-next-line` inside a region is redundant
(the line is already out) and needs no special handling.

### 5.4 What "ignored" means in a report

Excluded lines and files are **omitted from both the numerator and the
denominator** — the istanbul/`no cover` convention, and the only one that makes
"ignore" useful (counting them as covered would inflate the percentage; leaving
them in would defeat the purpose).

- **JSON** — an ignored line has no `LineCoverage` entry. Per-file and total
  rollups gain an **`ignored` count** (lines), plus an `ignoredFiles` count, so a
  reader can see that exclusion happened rather than inferring it from absent
  lines.
- **A file with every line ignored is dropped entirely** — no `FileCoverage`
  entry, no `SF:` block — rather than reported as an empty 0/0 file, and counts
  toward `ignoredFiles`. This is what makes the unclosed-region-at-the-top idiom
  read as "this file is not measured".
- **LCOV** — ignored lines simply produce no `DA:` record and dropped files no
  `SF:` block. LCOV has no exclusion concept, so this is exactly how every other
  tool emits it; `LF`/`LH` fall out consistently.
- **stdout summary** — grows a tail when anything was excluded:
  `coverage: 812/900 lines (90.2%) — 14 lines, 1 file ignored`.
- **`--no-ignore`** on `nessemble coverage` disables all coverage directives for
  the run, so CI (or a suspicious reviewer) can see the unfiltered truth.

### 5.5 Rhai scripts

`--scripts` coverage (plan 007 §8) reports `.rhai` files, which comment with
`//`. Both coverage directives are recognized there too, with the same
semantics, scanned line-wise (`//` + optional whitespace + `@…`) rather than
through the asm lexer. `@nessemble-format` is **not** recognized in `.rhai` —
nessemble does not format Rhai.

## 6. Architecture

### 6.1 Core: the directive registry — **as built (Phase 0)**

A "Comment directives" section of `nessemble-core/src/tooling.rs`, placed just
before the linting section — matching how plan 008's rule engine landed (a
section of the same file, items at the `tooling::` path) rather than a separate
submodule file:

```rust
/// A directive's registry name — its identity, independent of arguments.
pub enum DirectiveName { Format, CoverageIgnore, CoverageIgnoreNextLine }
impl DirectiveName {
    pub const ALL: [DirectiveName; 3];
    pub fn canonical(self) -> &'static str;   // "@nessemble-format", …
    pub fn arg_syntax(self) -> &'static str;  // "stride=N[,N,...]" | "start|end" | ""
}

/// Arguments, parsed once in the scanner so no consumer re-parses text.
pub enum DirectiveArgs { Strides(Vec<usize>), Region(RegionBound), None }
pub enum RegionBound { Start, End }

pub struct Directive {
    pub name: DirectiveName,
    pub args: DirectiveArgs,
    pub line: u32,           // 1-based, the comment's own line
    pub column: u32,         // 1-based char column of the `@`
    pub start: usize,        // byte range of the `@…` token, for editors that
    pub end: usize,          //   narrow a diagnostic or rewrite the token
    pub deprecated: bool,    // matched a legacy alias (`@fmt`)
    pub own_line: bool,      // false ⇒ trailing comment ⇒ inert (§4.2)
}

pub enum MalformedReason { UnknownName, BadArgs(DirectiveName) }
pub struct MalformedDirective { pub reason: MalformedReason, pub line: u32,
                                pub column: u32, pub start: usize, pub end: usize }

pub fn scan_directives(source: &str) -> Vec<Directive>;
pub fn scan_directives_with_errors(source: &str)
    -> (Vec<Directive>, Vec<MalformedDirective>);
```

Two deviations from the sketch this plan opened with, both settled by writing it:

- **Typed, pre-parsed arguments** instead of a raw `args: &str`. Stride lists and
  `start`/`end` are parsed inside the scanner, which is what lets it distinguish
  "malformed" from "prose" at all — the `BadArgs` reason falls out of the same
  parse. It also drops the lifetime, so `Directive` is owned and easy to hold.
- **The token's byte range** (`start`/`end`) rides along, because the LSP quick
  fix (Phase 3) has to rewrite exactly that span, and diagnostics narrow to it.

The registry itself is a `const DIRECTIVES: &[(&str, DirectiveName, bool)]` —
token, name, is-deprecated-alias — looked up by **exact** token match, so
`@nessemble-coverage-ignore-next-line` can never resolve as
`@nessemble-coverage-ignore` with a stray argument.

Built on the existing `lex` + `split_lines` helpers, so it sees exactly what the
formatter, highlighter, and linter see. **No new dependencies** — this is string
handling over the lexeme stream.

**One deliberate widening over `@fmt`:** the scanner skips the whole leading `;`
run, so `;; @nessemble-format stride=2` is a directive, where the old parser
(single `;` only) treated it as prose. This makes `;;`-banner comment styles
work, at the cost of one idiom: "comment out a hint by adding a `;`" no longer
disables it. Reversible in one line if that trade reads wrong.

`parse_hint` is re-founded on it: `consolidate_data` asks the scanner for a
`DirectiveKind::Format` on the line and parses `stride=` from its `args`. Both
spellings arrive through one path, so they cannot drift.

### 6.2 Formatter

No behavioral change beyond accepting the new spelling (§5.1). One decision
recorded: **the formatter does not rewrite `@fmt` to `@nessemble-format`.**
Comments are the author's text; a formatter that edits comment *content* (as
opposed to spacing) is a new and surprising power. Migration is served by the
LSP quick fix (§8) and a one-line `sed` in the docs. See §13 Q2 if you'd rather
it normalize.

### 6.3 Coverage

Core stays **fs-free** (it must: `nessemble-wasm` builds it), so the CLI reads
source text and hands core a pre-computed exclusion set:

```rust
// nessemble-core::coverage
/// Per-file excluded line ranges, inclusive. A single ignored line is a
/// one-line range; an unclosed region is a range ending at `u32::MAX`, so the
/// caller never needs to know how long the file is.
#[derive(Default)]
pub struct CoverageIgnores { ranges: HashMap<String, Vec<(u32, u32)>> }

impl CoverageIgnores {
    pub fn ignore_line(&mut self, file: &str, line: u32);
    pub fn ignore_range(&mut self, file: &str, start: u32, end: u32);
    pub fn contains(&self, file: &str, line: u32) -> bool;
}

pub fn build_report_with_ignores(source_map: &SourceMap, cdl: &dyn CdlSource,
                                 ignores: &CoverageIgnores) -> CoverageReport;

// unchanged, now a shim: build_report(map, cdl)
//   = build_report_with_ignores(map, cdl, &CoverageIgnores::default())
```

One range type carries both directives — `-next-line` resolves to a one-line
range, a region to its `start..=end`, an unclosed region to `start..=u32::MAX`.
`build_report_with_ignores` skips excluded lines while grouping spans, tallies
what it skipped into the `ignored` counts, and drops any file left with no lines
(§5.4).

Keys are the source map's canonical absolute paths, so exclusion happens
**before** the CLI's relative-path rewrite and sort — no path-normalization
ambiguity.

The CLI, after assembling, walks the distinct file paths in the source map (it
already has them), reads each once, runs `scan_directives`, resolves the
directive sequence into ranges (a small state machine: open on `start`, close on
`end`, one-line range per `-next-line`), and populates `CoverageIgnores`. Rhai
`--scripts` files go through the `//` scanner (§5.5) and are filtered as they are
folded into the report. A file that cannot be re-read (deleted or renamed
mid-run) yields no exclusions and a warning on stderr — a coverage report is
never blocked by a missing comment.

### 6.4 Lint rules

Three rules join the existing registry (§7); severity mapping, `.nessemblerc`
plumbing, the CLI report, and the LSP diagnostics all already exist and need no
change beyond the new `RuleId` entries and their `.nessemblerc` names.

### 6.5 LSP

See §8. The bulk is free via lint; the additions are completion, hover, and one
code action.

## 7. Validation — three lint rules

The whole point of a namespace is that a mistyped directive is now *detectable*:
any `@nessemble-…` token that isn't in the registry was meant to be a directive.

| Rule id | Default | Fires on |
|---|---|---|
| `unknown-comment-directive` | `warn` | `@nessemble-<name>` that is not in the registry; or a known directive with malformed/missing/unknown args (`@nessemble-format` with no `stride=`, `@nessemble-format stride=x`, a bare `@nessemble-coverage-ignore` with no `start`/`end`, `@nessemble-coverage-ignore begin`). |
| `deprecated-comment-directive` | `warn` | `@fmt` — "use `@nessemble-format`". |
| `ineffective-comment-directive` | `warn` | A well-formed directive that cannot apply: in a trailing comment (§4.2); `-next-line` with no following significant line; `@nessemble-format` not followed by a data run; `@nessemble-coverage-ignore end` with no open region; `@nessemble-coverage-ignore start` inside an already-open region. |

An **unclosed region is not flagged** — it is the documented whole-file idiom
(§5.3), and warning on it would put a squiggle in every file that opts out of
coverage.

- Detection is **scoped to the `@nessemble-` prefix plus the `@fmt` alias**, so
  `@todo`/`@author`/Doxygen-style prose is never flagged. This is the guard
  against the "linter yells about my comments" failure mode.
- `Finding.subject` carries the directive token (the field is documented as the
  offending label name; the doc comment widens to "the offending subject").
- Each rule is independently `off`-able and glob-overridable through the
  existing `.nessemblerc` `lint` section — no new config shape.

## 8. The language server — is this necessary?

**Short answer: not for correctness, yes for the feature to be usable — and the
valuable 80 % costs almost nothing because it rides the pipe plan 008 already
built.** All three tiers below are **in scope** (§13.3).

Taking it in tiers, most to least valuable:

**Tier 1 — diagnostics (do it; ~zero new LSP code).** The lint rules in §7 are
published as LSP diagnostics automatically: `nessemble-lsp`'s `with_lint` step
already folds `tooling::lint` findings into every document it publishes, sourced
`nessemble-lint` at `HINT`/`INFORMATION`. So `; @nessemble-formt stride=2` gets
a squiggle in the editor the moment the rule exists, with no LSP change at all.
**This is the answer to the question.** A directive that fails silently is the
whole hazard (§2); the editor is where it gets caught, and it comes for the cost
of the rules we want for `nessemble lint` regardless.

Also note the LSP already *honors* `@nessemble-format` on format-on-save the
instant core accepts the spelling, since `format_document` calls
`tooling::format`. Nothing to wire.

**Tier 2 — discoverability (recommended; small and self-contained).**

- **Completion.** Inside a comment, offer the directives as `CompletionItem`s
  with documentation — four entries, since `@nessemble-coverage-ignore` is
  offered pre-filled as `start` and as `end` rather than as a bare stem the user
  can leave malformed, and `@nessemble-format` carries a `stride=` snippet.
  `complete()` currently returns mnemonics/directives/symbols
  regardless of context; this adds a small "is the cursor in a comment?" check
  over `located_lexemes`, which is also the right fix for offering mnemonics
  inside comments today.
- **Hover.** Hovering a directive token shows its one-paragraph docs — the same
  registry text the docs table renders, so there is one source of truth.
- **Quick fix.** A `CodeAction` on a `deprecated-comment-directive` diagnostic
  rewriting `@fmt` → `@nessemble-format`, which is what makes the deprecation
  actionable rather than nagging. `code_actions` already exists (number-base
  conversions), so this is one more producer.

**Tier 3 — highlighting (in scope, via a modifier — *not* a new token class).**
Directive comments should not look like prose comments; an editor that greys
them out identically hides the one comment that changes tool behavior. The
important constraint is *how* to do it: `TokenClass::wire_id` is documented as a
**frozen contract** shared with the wasm highlighter and the web UI, so adding a
`DirectiveComment` class and renumbering is off the table.

The way that costs nothing is the **semantic-token modifier bitset**, which is
orthogonal to token *types*: the LSP legend currently declares
`token_modifiers: Vec::new()`, so populating it is purely additive and touches
no existing id.

- Legend gains one modifier — `documentation` (a standard LSP modifier, so
  editors and themes already style it) — and the directive comment's token is
  emitted as `TokenClass::Comment` with that modifier bit set.
- The token **type** stays `Comment` (wire id 5), so every existing client,
  theme, and the wasm highlighter are unaffected; clients that ignore modifiers
  see exactly what they see today.
- **The wasm/web highlighter is deliberately not changed.** Its `tokenize`
  output has no modifier channel, and adding one is a wire change to a published
  contract for a cosmetic gain in a different surface. If the web UI wants this
  later it is its own additive change, tracked separately.

That gives the tier-3 outcome — directive comments visually distinct in the
editor — without renumbering anything.

**Explicitly not in the LSP:** anything coverage-*reporting* related. The editor
has no CDL and no assembled ROM in hand; `@nessemble-coverage-ignore` therefore
has no editor behavior beyond being validated, completed, and documented. That
asymmetry is fine and worth stating so nobody goes looking for coverage
gutters in the LSP.

## 9. Docs

- `docs/src/usage.md` — a new **Comment directives** section (the §4 grammar +
  the registry table) that both the formatter and coverage sections link to;
  § Stride hints re-spelled to `@nessemble-format` with a deprecation note on
  `@fmt` and the `sed` migration line; the `coverage` section gains region and
  next-line ignore semantics (including the unclosed-region whole-file idiom),
  the report/`ignored` counts, and `--no-ignore`; the lint rule table gains the
  three rules.
- `docs/src/editor.md` — a line on directive completion, hover, and the
  deprecation quick fix.
- `CHANGELOG` entries come from per-phase changesets (§10).

## 10. Phased plan

Each phase leaves the tree green (`cargo test`, `cargo clippy`, `xtask parity`)
and carries its own changeset, per house style.

**Phase 0 — the directive registry in core. — ✅ done.** Added the "Comment
directives" section to `nessemble-core::tooling`: `DirectiveName` (+ the
`DIRECTIVES` registry, `canonical`, `arg_syntax`), `DirectiveArgs`,
`RegionBound`, `Directive`, `MalformedReason`, `MalformedDirective`,
`scan_directives`, and `scan_directives_with_errors`, all over the existing
lossless lexer with no new dependencies (§6.1). `parse_hint` is re-founded on the
scanner, so `@nessemble-format` and `@fmt` share one parse and cannot drift; the
`FormatOptions::respect_stride_hints` docs name the new spelling. *Verified:*
directive unit tests (every registry name; `start`/`end`; exact-match so
`-next-line` never resolves as the region directive; the `@fmt` alias flagged
deprecated but honored; indentation and `;;`/`;;;` leaders; trailing prose after
args; a trailing-comment directive marked `own_line: false`; prose `@todo`/
`@param`/mid-sentence/`@NESSEMBLE-FORMAT`/`@fmtstride=2` untouched; a directive
inside a string literal not scanned; unknown namespaced name → `UnknownName`;
eight bad-argument cases → `BadArgs` with no directive emitted; position and
token byte range; source order; registry round-trip), plus formatter regressions
proving the old `@fmt` tests still pass byte-for-byte, the same fixtures pass
with `@nessemble-format`, `respectStrideHints: false` disables both spellings,
and a trailing hint stays inert. Full workspace suite green (`cargo fmt --check`,
`cargo clippy --workspace --all-targets`, `cargo test --workspace`, including the
golden-ROM corpus). *Changeset: `minor`.*

**Phase 1 — validation.** The three lint rules (§7), their `RuleId`s, registry
entries, and `.nessemblerc` names in `nessemble-rc`. Lands `nessemble lint`
reporting **and** LSP diagnostics simultaneously (§8 tier 1). *Changeset:
`minor`.*

**Phase 2 — coverage ignores.** `CoverageIgnores` (line + region ranges) +
`build_report_with_ignores` in core (`build_report` kept as a shim); the CLI's
directive→range state machine, source-file scanning, Rhai `//` scanning,
`ignored`/`ignoredFiles` in JSON, dropping fully-ignored files, the extended
stdout summary, and `--no-ignore`. *Changeset: `minor`.*

**Phase 3 — LSP surface.** Comment-context completion, directive hover, the
`@fmt` → `@nessemble-format` quick fix (§8 tier 2), and the `documentation`
semantic-token modifier on directive comments (§8 tier 3). *Changeset: `minor`.*

**Phase 4 — docs.** §9. *Changeset: `none`.*

Phases 0–2 are the requested feature; 3 is the recommended editor polish; 4
closes it out. Phase 1 could swap with Phase 2 if coverage is the more urgent
half — they are independent after Phase 0.

## 11. Testing strategy

- **Core scanner unit tests** *(done, Phase 0)* — each registry name; `;;`/`;;;`
  leaders and leading whitespace; trailing prose after args; a trailing
  (non-own-line) directive marked `own_line: false`; `@todo`/`@param` untouched;
  an unknown `@nessemble-*` name and each `MalformedReason`; `@fmt` flagged
  `deprecated`; case sensitivity; the exact-match property that
  `@nessemble-coverage-ignore-next-line` never resolves as
  `@nessemble-coverage-ignore`.
- **Formatter regression** *(done, Phase 0)* — every existing `@fmt` stride test
  keeps passing byte-for-byte, duplicated for `@nessemble-format`;
  `respectStrideHints: false` disables both.
- **Lint** — one fixture per rule, plus a clean fixture with prose `@`-comments
  proving no false positives; each rule `off`; per-glob override.
- **Coverage** — fixtures for an ignored line, a closed region, an **unclosed**
  region running to EOF, a whole-file opt-out (`start` in the header),
  `end`-without-`start` and nested-`start` (inert, report unchanged), a stacked
  comment between `-next-line` and its target, a directive at end of file, a
  region that does **not** leak into an `.include`d file, and `--no-ignore`
  restoring every line. Assertions: ignored lines leave both numerator and
  denominator (so the percentage moves the way §5.4 says), LCOV omits them, a
  fully-ignored file produces no `SF:` block, and JSON carries the `ignored` /
  `ignoredFiles` counts.
- **LSP** — a malformed directive yields one `nessemble-lint` diagnostic;
  completion inside a comment offers the directives and not mnemonics; hover on
  a directive returns its docs; the quick fix's edit produces
  `@nessemble-format`; a directive comment's semantic token keeps type `Comment`
  (wire id 5) and gains the `documentation` modifier bit, while an ordinary
  comment's modifier bitset stays `0`.
- **Parity** — untouched. Nothing here changes assembled bytes; `xtask parity`
  should stay 122/122 through every phase.

## 12. Risks & mitigations

- **Breaking existing `@fmt` users.** *Mitigation:* permanent alias, `warn`-level
  deprecation, quick fix, documented migration. Removal would be a `major` and
  is not proposed.
- **Coverage ignores hiding real gaps.** An ignore comment is a way to lie to
  your own CI, and a region is a bigger hammer than a line — an unclosed `start`
  drifting up a file silently swallows everything below it. *Mitigation:*
  `--no-ignore`; ignored line **and file** counts reported in JSON and the stdout
  summary rather than silently vanishing (an over-broad region shows up as a
  jump in that number); regions are plain text a reviewer sees in the diff, and
  they never cross a file boundary.
- **Lint noise on ordinary `@` prose.** *Mitigation:* detection is scoped to the
  `@nessemble-` prefix plus `@fmt`; everything else is prose. Rules default to
  `warn` (non-failing) and are individually `off`-able.
- **Line attribution through macros and includes** — an emitting line inside a
  macro body may attribute to the macro definition or the invocation site,
  which decides where the ignore comment must go. *Mitigation:* the source map
  is the single authority (plan 007 §6.1); Phase 2 pins the behavior with a
  macro fixture and documents it. Flagged as an implementation-time verification,
  not a design unknown.
- **Scope creep into a pragma language.** *Mitigation:* the registry is closed
  and small; each new directive needs a real consumer. `@nessemble-format-ignore`
  (skip formatting a region) is the obvious next candidate and is deliberately
  **not** in v1.
- **Extra file reads on the coverage path.** One read per source file, only when
  `nessemble coverage` runs, only over files the source map already names. No
  effect on the assemble hot path.

## 13. Decisions

Settled with the maintainer:

1. **Coverage exclusion shape** — a **`start`/`end` region pair**
   (`; @nessemble-coverage-ignore start` … `end`) rather than a whole-file
   directive; an unclosed region runs to end of file, which *is* the whole-file
   idiom (§5.3). Regions are per-file and do not follow `.include`s.
   *Derived:* the `start`/`end` argument is **required** — a bare
   `; @nessemble-coverage-ignore` is malformed and reported, because with
   regions in play a bare form would need a second, conflicting meaning (§4).
   Say the word if you'd rather it stay a whole-file shorthand.
2. **No automatic rewrite** of `@fmt` → `@nessemble-format` by the formatter
   (§6.2). Migration is the LSP quick fix plus a documented `sed`; `@fmt` keeps
   working indefinitely.
3. **Full LSP surface — tiers 1–3** (§8): lint-sourced diagnostics,
   completion + hover + quick fix, and distinct highlighting for directive
   comments. Tier 3 ships as a **semantic-token modifier** (`documentation`) on
   the existing `Comment` token type, not a new token class, so the frozen
   `TokenClass::wire_id` contract and the wasm highlighter are untouched.
4. **One `--no-ignore` flag** on `nessemble coverage`, disabling every coverage
   directive for the run; no split line/region flags.
5. **`deprecated-comment-directive` defaults to `warn`** — visible in
   `nessemble lint` and the editor, actionable via the quick fix, and
   non-failing on its own.

---

*Nothing here is implemented. Phase 0 (the `tooling::directive` registry) lands
first with a `minor` changeset for the new core API and the `@nessemble-format`
spelling; later phases carry their own.*
