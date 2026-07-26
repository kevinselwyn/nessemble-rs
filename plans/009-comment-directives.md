# nessemble-rs: A Plan for Comment Directives

> Status: **Proposed — awaiting feedback; nothing here is implemented.** This
> document codifies the tool-directive comments nessemble reads out of assembly
> source. It (a) promotes the formatter's existing ad-hoc `; @fmt stride=N`
> hint to a namespaced **`; @nessemble-format stride=N`** (with `@fmt` kept as a
> deprecated alias), (b) adds two coverage directives —
> **`; @nessemble-coverage-ignore-next-line`** and
> **`; @nessemble-coverage-ignore`** — and (c) answers the "should the language
> server know about these?" question: **yes, but almost entirely through the
> existing lint→diagnostics pipe rather than new LSP machinery** ([§8](#8-the-language-server-is-this-necessary)).
> The open items needing a decision before implementation are in
> [§13](#13-open-questions-for-the-maintainer).

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

; @nessemble-coverage-ignore                 ; coverage: drop this whole file
```

Three requirements from the request:

1. **Codify `@fmt` as `@nessemble-format`** — the current spelling is
   unnamespaced, undiscoverable, and collides with every other tool that reads
   `@fmt`-ish comments.
2. **Add coverage ignores** — `@nessemble-coverage-ignore-next-line` for one
   line, `@nessemble-coverage-ignore` for a whole file, in the shape every
   coverage tool ships (`istanbul ignore next`, `# pragma: no cover`,
   `LCOV_EXCL_LINE`).
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
| `@nessemble-coverage-ignore` | the whole file | none | `nessemble coverage` |
| `@fmt` | *(deprecated alias of `@nessemble-format`)* | `stride=N[,N,…]` | formatter |

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

### 5.3 `@nessemble-coverage-ignore`

Excludes the **entire file it appears in**, from anywhere in that file
(conventionally the header block). It is per-file and does **not** propagate
through `.include` — an ignored file's includes are still reported, and each
included file may carry its own directive. That keeps the rule local and
readable: the file that says "don't measure me" is the file that isn't measured.

### 5.4 What "ignored" means in a report

Excluded lines and files are **omitted from both the numerator and the
denominator** — the istanbul/`no cover` convention, and the only one that makes
"ignore" useful (counting them as covered would inflate the percentage; leaving
them in would defeat the purpose).

- **JSON** — an ignored line has no `LineCoverage` entry and an ignored file no
  `FileCoverage` entry. Per-file and total rollups gain an **`ignored` count**
  (lines), and the report gains an `ignoredFiles` count, so a reader can see
  that exclusion happened rather than inferring it from absent lines.
- **LCOV** — ignored lines simply produce no `DA:` record and ignored files no
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

### 6.1 Core: `tooling::directive` (new submodule)

```rust
/// A directive's identity. One variant per registry row (§4).
pub enum DirectiveKind { Format, CoverageIgnore, CoverageIgnoreNextLine }

/// A directive comment found in source.
pub struct Directive<'a> {
    pub kind: DirectiveKind,
    pub args: &'a str,       // remainder, trailing prose stripped
    pub line: u32,           // 1-based, the comment's own line
    pub column: u32,         // 1-based, the `@`
    pub deprecated: bool,    // matched a legacy alias (`@fmt`)
    pub own_line: bool,      // false ⇒ trailing comment ⇒ inert (§4.2)
}

/// Every well-formed directive in `source`, in source order.
pub fn scan_directives(source: &str) -> Vec<Directive<'_>>;

/// A comment addressed to nessemble that is *not* a valid directive — the
/// input to the `unknown-comment-directive` rule (§7).
pub struct MalformedDirective { pub line: u32, pub column: u32,
                                pub token: String, pub reason: MalformedReason }
pub fn scan_directives_with_errors(source: &str)
    -> (Vec<Directive<'_>>, Vec<MalformedDirective>);
```

Built on the existing `lex` + `split_lines` helpers, so it sees exactly what the
formatter, highlighter, and linter see. **No new dependencies** — this is string
handling over the lexeme stream.

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
#[derive(Default)]
pub struct CoverageIgnores {
    pub files: HashSet<String>,          // whole-file exclusions
    pub lines: HashSet<(String, u32)>,   // (file, line) exclusions
}

pub fn build_report_with_ignores(source_map: &SourceMap, cdl: &dyn CdlSource,
                                 ignores: &CoverageIgnores) -> CoverageReport;

// unchanged, now a shim: build_report(map, cdl)
//   = build_report_with_ignores(map, cdl, &CoverageIgnores::default())
```

Keys are the source map's canonical absolute paths, so exclusion happens
**before** the CLI's relative-path rewrite and sort — no path-normalization
ambiguity.

The CLI, after assembling, walks the distinct file paths in the source map (it
already has them), reads each once, runs `scan_directives`, and populates
`CoverageIgnores`. Rhai `--scripts` files go through the `//` scanner (§5.5) and
are filtered as they are folded into the report. A file that cannot be re-read
(deleted or renamed mid-run) yields no exclusions and a warning on stderr — a
coverage report is never blocked by a missing comment.

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
| `unknown-comment-directive` | `warn` | `@nessemble-<name>` that is not in the registry; or a known directive with malformed/unknown args (`@nessemble-format` with no `stride=`, `@nessemble-format stride=x`, `@nessemble-coverage-ignore stride=2`). |
| `deprecated-comment-directive` | `warn` | `@fmt` — "use `@nessemble-format`". |
| `ineffective-comment-directive` | `warn` | A well-formed directive that cannot apply: in a trailing comment (§4.2); `-next-line` with no following significant line; `@nessemble-format` not followed by a data run. |

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
built.**

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

- **Completion.** Inside a comment, offer the three directives as
  `CompletionItem`s with documentation and a `stride=` snippet for the
  formatter one. `complete()` currently returns mnemonics/directives/symbols
  regardless of context; this adds a small "is the cursor in a comment?" check
  over `located_lexemes`, which is also the right fix for offering mnemonics
  inside comments today.
- **Hover.** Hovering a directive token shows its one-paragraph docs — the same
  registry text the docs table renders, so there is one source of truth.
- **Quick fix.** A `CodeAction` on a `deprecated-comment-directive` diagnostic
  rewriting `@fmt` → `@nessemble-format`, which is what makes the deprecation
  actionable rather than nagging. `code_actions` already exists (number-base
  conversions), so this is one more producer.

**Tier 3 — highlighting (declined).** Giving directive comments their own
semantic-token class or modifier would need a change to `TokenClass::wire_id` /
the semantic-token legend, which is documented as a **frozen wire contract**
shared with the wasm highlighter and the web UI. Renumbering a stable contract
for a cosmetic win is a bad trade. Directive comments stay `TokenClass::Comment`.

**Explicitly not in the LSP:** anything coverage-*reporting* related. The editor
has no CDL and no assembled ROM in hand; `@nessemble-coverage-ignore` therefore
has no editor behavior beyond being validated, completed, and documented. That
asymmetry is fine and worth stating so nobody goes looking for coverage
gutters in the LSP.

## 9. Docs

- `docs/src/usage.md` — a new **Comment directives** section (the §4 grammar +
  the registry table) that both the formatter and coverage sections link to;
  § Stride hints re-spelled to `@nessemble-format` with a deprecation note on
  `@fmt` and the `sed` migration line; the `coverage` section gains ignore
  semantics, the report/`ignored` counts, and `--no-ignore`; the lint rule table
  gains the three rules.
- `docs/src/editor.md` — a line on directive completion, hover, and the
  deprecation quick fix.
- `CHANGELOG` entries come from per-phase changesets (§10).

## 10. Phased plan

Each phase leaves the tree green (`cargo test`, `cargo clippy`, `xtask parity`)
and carries its own changeset, per house style.

**Phase 0 — the directive registry in core.** `tooling::directive`
(`DirectiveKind`, `Directive`, `MalformedDirective`, `scan_directives`,
`scan_directives_with_errors`); `parse_hint` re-founded on it so
`@nessemble-format` and `@fmt` share one path. No user-visible change beyond the
new accepted spelling. *Changeset: `minor`.*

**Phase 1 — validation.** The three lint rules (§7), their `RuleId`s, registry
entries, and `.nessemblerc` names in `nessemble-rc`. Lands `nessemble lint`
reporting **and** LSP diagnostics simultaneously (§8 tier 1). *Changeset:
`minor`.*

**Phase 2 — coverage ignores.** `CoverageIgnores` +
`build_report_with_ignores` in core (`build_report` kept as a shim);
CLI source-file scanning, Rhai `//` scanning, `ignored`/`ignoredFiles` in JSON,
the extended stdout summary, and `--no-ignore`. *Changeset: `minor`.*

**Phase 3 — LSP niceties.** Comment-context completion, directive hover, and the
`@fmt` → `@nessemble-format` quick fix (§8 tier 2). *Changeset: `minor`.*

**Phase 4 — docs.** §9. *Changeset: `none`.*

Phases 0–2 are the requested feature; 3 is the recommended editor polish; 4
closes it out. Phase 1 could swap with Phase 2 if coverage is the more urgent
half — they are independent after Phase 0.

## 11. Testing strategy

- **Core scanner unit tests** — each registry name; `;;`/`;;;` leaders and
  leading whitespace; trailing prose after args; a trailing (non-own-line)
  directive marked `own_line: false`; `@todo`/`@param` untouched; an unknown
  `@nessemble-*` name and each `MalformedReason`; `@fmt` flagged `deprecated`;
  case sensitivity; the exact-match property that
  `@nessemble-coverage-ignore-next-line` never resolves as
  `@nessemble-coverage-ignore`.
- **Formatter regression** — every existing `@fmt` stride test keeps passing
  byte-for-byte, duplicated for `@nessemble-format`; `respectStrideHints: false`
  disables both.
- **Lint** — one fixture per rule, plus a clean fixture with prose `@`-comments
  proving no false positives; each rule `off`; per-glob override.
- **Coverage** — a fixture with an ignored line, an ignored file, a stacked
  comment between directive and target, a directive at end of file, and
  `--no-ignore`; assertions that ignored lines leave both numerator and
  denominator (so the percentage moves the way §5.4 says), that LCOV omits them,
  and that JSON carries the `ignored` counts.
- **LSP** — a malformed directive yields one `nessemble-lint` diagnostic;
  completion inside a comment offers the directives and not mnemonics; hover on
  a directive returns its docs; the quick fix's edit produces
  `@nessemble-format`.
- **Parity** — untouched. Nothing here changes assembled bytes; `xtask parity`
  should stay 122/122 through every phase.

## 12. Risks & mitigations

- **Breaking existing `@fmt` users.** *Mitigation:* permanent alias, `warn`-level
  deprecation, quick fix, documented migration. Removal would be a `major` and
  is not proposed.
- **Coverage ignores hiding real gaps.** An ignore comment is a way to lie to
  your own CI. *Mitigation:* `--no-ignore`; ignored counts reported in JSON and
  the stdout summary rather than silently vanishing; the directives are plain
  text a reviewer sees in the diff.
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

## 13. Open questions for the maintainer

These are the decisions I'd want settled before Phase 0 lands; each has a
recommendation so silence means "take the recommendation."

1. **Whole-file directive spelling.** The request specifies bare
   `@nessemble-coverage-ignore` for the file scope. Every prior-art tool marks
   the file case explicitly (`istanbul ignore file`), and a bare "ignore" reads
   ambiguously next to its own `-next-line` sibling. Ship the requested name, or
   `@nessemble-coverage-ignore-file` (optionally with the bare form as an
   alias)? *Recommendation: ship as requested (`@nessemble-coverage-ignore` =
   whole file), documented prominently, since that's the stated intent.*
2. **Should `nessemble format` rewrite `@fmt` → `@nessemble-format`?**
   *Recommendation: no* (§6.2) — the LSP quick fix plus a documented `sed`
   handles migration without the formatter editing comment text. Reversible: it
   could become an opt-in `.nessemblerc` key later.
3. **How far into the LSP?** Tier 1 alone (diagnostics, free), or tiers 1+2
   (completion, hover, quick fix — Phase 3)? *Recommendation: 1+2; tier 2 is
   ~a day and is what makes the directives discoverable at all.*
4. **`--no-ignore` naming and scope.** A single flag disabling both coverage
   directives, or separate `--no-ignore-lines` / `--no-ignore-files`?
   *Recommendation: one flag; split only if a use case appears.*
5. **Deprecation severity.** `deprecated-comment-directive` at `warn` (visible
   in `nessemble lint`, non-failing) vs. `off` by default (silent until a
   project opts in). *Recommendation: `warn` — it is actionable via the quick
   fix and never fails a build on its own.*

---

*Nothing here is implemented. Phase 0 (the `tooling::directive` registry) lands
first with a `minor` changeset for the new core API and the `@nessemble-format`
spelling; later phases carry their own.*
