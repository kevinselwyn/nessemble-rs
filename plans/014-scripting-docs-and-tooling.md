# nessemble-rs: A Plan for Scripting Documentation and Tooling

> Status: **Shipped** ([§9](#9-phased-plan); as built,
> [§12.1](#121-phase-0), [§12.2](#122-phase-1), [§12.3](#123-phase-2),
> [§12.4](#124-phase-3), [§12.5](#125-phase-4)). This document treats the host functions a pseudo-op
> script can call as what they actually are — **a public API** — and gives them
> the three things every public API in this repo already has and this one does
> not: a **reference table of contents**, grouped by domain, in the
> [Extending](../docs/src/extending.md) docs ([§4](#4-the-docs-table-of-contents));
> **editor support** for the `.rhai` files themselves, with the same docs served
> as hover and completion ([§5](#5-lsp-support-for-rhai-scripts)); and a
> **coverage report that includes every script**, not only the ones that
> happened to run ([§6](#6-script-coverage-what-the-report-leaves-out)).
>
> All three are downstream of one missing thing: there is **no machine-readable
> list of what a script can call**. The engine builder
> ([`nessemble-script/src/lib.rs:313`](../crates/nessemble-script/src/lib.rs))
> registers roughly forty functions, properties, and methods; the docs describe
> most of them in prose; nothing enumerates them. So the plan's spine is a
> **catalog** ([§3](#3-the-catalog-one-table-four-consumers)) — the same move
> [`nessemble_isa::DIRECTIVES`](../crates/nessemble-isa/src/lib.rs) already made
> for assembler directives, which is why `nessemble reference directives` and
> the language server's directive hover are one table and not two.
>
> The through-line: **a script author should be able to find out what they can
> call without reading `lib.rs`.**

---

## 1. Goal

Three deliverables, one shared foundation.

1. **A reference TOC in `docs/src/extending.md`**, grouped by domain (files,
   images, structured data, strings and numbers, randomness, output), listing
   every Nessemble-provided function with its signature, a one-line summary, and
   a link to the section that explains it.

2. **`nessemble lsp` understands `.rhai`.** Open a pseudo-op script and get
   diagnostics, completion of the host API with its documentation, hover,
   signature help, and an outline — plus, from an `.asm` file, jump from a
   `.foo` directive to the script that implements it.

3. **`nessemble coverage --scripts` reports every script**, including the ones
   that never ran, and works without an emulator capture.

## 2. The problem: three symptoms, one cause

### 2.1 The Extending page is a tutorial, filed as a reference

[`extending.md`](../docs/src/extending.md) is 681 lines and genuinely good at
what it does: it teaches. "Macros or scripts?", a worked `.product` example,
then filesystem access, PNG decoding, structured data, caching. Read top to
bottom, it works.

It is not usable the other way round. A script author three weeks in does not
want to be taught; they want to know whether the thing that turns a shade value
into a NES palette index is called `nes_shade`, `quantize`, or `to_shade`, and
what it takes. Today that means scrolling for a `###` that sounds right, or
opening `crates/nessemble-script/src/lib.rs` and reading `register_fn` calls —
which is what actually happens, because it is faster.

The page has no index, no per-domain grouping, and no signature list. Every
symptom below is the same page-shaped hole seen from a different tool.

### 2.2 The editor goes dark at the `.rhai` boundary

The language server is thorough on the assembly side of the line — diagnostics,
completion, hover, definition, references, folding, rename, inlay hints, code
actions, lint findings, routine signatures
([`nessemble-lsp/src/lib.rs`](../crates/nessemble-lsp/src/lib.rs),
[plan 001](001-language-server.md)). It knows about scripts, too: `custom_scripts`
([`lib.rs:369`](../crates/nessemble-lsp/src/lib.rs)) reads every `pseudo.txt` in
the workspace so a `.foo` directive is not flagged as unknown.

It stops exactly there. The script that `.foo` maps to is, to the editor, a text
file. Worse, the document store is language-blind: `didOpen`
([`lib.rs:146`](../crates/nessemble-lsp/src/lib.rs)) inserts whatever arrives and
`analyze_and_publish` runs the 6502 analyzer over it, so a client that *did* route
`.rhai` files to this server would get a screen of nonsense diagnostics. The
capability is not merely absent; the door is unlocked and opens onto a wall.

What is lost is not syntax highlighting (the Rhai community extension does that
fine) but everything nessemble-specific:

- **Nothing knows the host API.** `decode_png_fil("x.png")` is accepted by the
  editor, accepted by the Rhai compiler, and fails at assemble time — Rhai
  resolves calls at run time, so an unknown function is a *runtime* error on the
  line that calls it. If that line sits in a branch a build does not take, the
  typo survives the build.
- **Nothing knows the entry point.** A mapped script without a `custom` function
  is only diagnosed when a build reaches the directive.
- **Nothing knows the execution model.** Both the ordinary path
  ([`lib.rs:297`](../crates/nessemble-script/src/lib.rs)) and the coverage path
  ([`coverage.rs:129`](../crates/nessemble-script/src/coverage.rs)) call
  `engine.call_fn(…, "custom", …)`, which does **not** evaluate the AST body
  first. So a top-level statement never runs. Verified against the current
  tree — this script:

  ```rhai
  const SCALE = 3;

  fn custom(ints, texts) {
      let out = [];
      for i in ints { out.push(i * SCALE); }
      out
  }
  ```

  fails at build time with `Variable not found: SCALE (line 6, position 22)`.
  That is a footgun with a fixed, detectable shape, invisible until a build
  breaks.

### 2.3 Script coverage only reports the scripts that ran

`nessemble coverage --scripts` ([plan 007](007-cdl-based-coverage.md) §8) is
built on `ScriptCoverage`, a map keyed by script path
([`coverage.rs:31`](../crates/nessemble-script/src/coverage.rs)). The only writer
is `run_with_coverage` ([`coverage.rs:131`](../crates/nessemble-script/src/coverage.rs)),
called from the instrumented resolver
([`custom.rs:295`](../crates/nessemble-cli/src/custom.rs)) on the path a directive
takes when the assembler reaches it.

Which means the report's population is *the set of scripts a build invoked*. A
script mapped in `pseudo.txt` whose directive is never assembled — retired,
behind a false `.if`, used only by a sibling entry point — contributes no entry
at all. It does not read as 0%; it reads as absent, and the percentage is
computed as though the file did not exist. That is the exact failure mode line
coverage is supposed to prevent, reproduced one level up: dead code hiding by
being dead.

The second half is smaller and just as blunt: `--cdl` is `required = true`
([`coverage.rs:60`](../crates/nessemble-cli/src/coverage.rs)). Script coverage is
assemble-time instrumentation that needs no emulator, no ROM, and no CDL — but
you cannot ask for it without producing a capture from a real playthrough first.
In CI, where script coverage is most useful and a playthrough least available,
the flag is unreachable.

### 2.4 The common cause

Each symptom is a consumer that would need a list of the host API and cannot
have one. The docs would render it as a table; the language server would serve it
as completion items and hover text; `nessemble reference` would print it. So the
list comes first, and the three deliverables are its consumers.

Coverage is the exception — its defects are independent of the catalog — so it is
planned as its own track ([§6](#6-script-coverage-what-the-report-leaves-out))
and can land in any order.

## 3. The catalog: one table, four consumers

### 3.1 What an entry is

```rust
pub struct ScriptApi {
    /// Callable name, as written in a script: `decode_png_file`, `find_all`.
    pub name: &'static str,
    /// How it is called, including receiver: `img.tile(col, row, w, h)`.
    pub signature: &'static str,
    /// One line, in the voice of the `DIRECTIVES` table.
    pub summary: &'static str,
    /// Which section of the Extending page groups it (§4.1).
    pub domain: Domain,
    /// Free function, method on a handle, or property.
    pub kind: ApiKind,
    /// `extending.md` heading anchor, without `#`: `decoding-pngs`.
    pub anchor: &'static str,
    /// When it exists: always, or only with a crate feature (§3.4).
    pub availability: Availability,
}
```

Four consumers, and each one is why a field is there:

| Consumer | Uses |
| --- | --- |
| The docs TOC ([§4](#4-the-docs-table-of-contents)) | `signature`, `summary`, `domain`, `anchor`, `availability` |
| LSP completion / hover / signature help ([§5.4](#54-completion-hover-and-signature-help)) | all of them, plus the docs URL built from `anchor` |
| `nessemble reference script` ([§4.4](#44-a-third-reference-category)) | `name`, `signature`, `summary`, `domain` |
| The drift test ([§3.3](#33-keeping-it-honest)) | `name`, `availability` |

### 3.2 Where it lives

A new crate, **`nessemble-script-api`**: data only, no dependencies, no Rhai.

The constraint that decides this is the language server. `nessemble-lsp` must be
able to complete and document the host API in builds that do not have a scripting
host at all — the CLI's `lsp` and `scripting` features are independent
([`nessemble-cli/Cargo.toml`](../crates/nessemble-cli/Cargo.toml)), and
`--no-default-features --features lsp` is a supported build. Putting the catalog
in `nessemble-script` would drag Rhai into every one of them.

Considered and rejected: adding it to `nessemble-isa` next to `DIRECTIVES`. That
table's own doc comment already concedes it is "language-level metadata (not
strictly ISA), colocated here"; a *scripting host* API is a second stretch of the
same seam, in a crate whose name says 6502. Recorded in
[§11.1](#111-a-new-crate-rather-than-nessemble-isa).

### 3.3 Keeping it honest

A catalog that drifts is worse than none — it documents functions that no longer
exist and hides ones that do. The gate is a unit test in `nessemble-script`,
which is the crate that owns `engine()`:

1. Every catalog entry marked `Availability::Always` appears as a registration
   literal in `engine()`'s source (`include_str!("lib.rs")`, scanned for
   `register_fn("<name>"`, `register_get("<name>"`, `register_type_with_name`).
2. Every such literal in `engine()` has a catalog entry.

Text-level, and deliberately so. The alternative — asking the engine itself, via
`Engine::gen_fn_signatures` — needs Rhai's `metadata` feature, and **enabling it
in this workspace does not compile**: rhai 1.25.1 fails its own build with
`no method named with_params_info found for struct FuncRegistration`, from
`#[export_module]` expansions inside rhai's packages. That is a rabbit hole with
no bottom in sight for a docs table, so the plan takes the scan and records the
finding ([§11.2](#112-the-drift-check-scans-source-rather-than-asking-the-engine)).

The scan cannot see functions registered by the `rhai-fs` and `rhai-rand`
packages (`open_file`, the `File` methods, `rand`, `shuffle`, …). Those entries
carry `Availability::Feature("fs" | "rand")`, and are exempt from rule 1 and
listed explicitly — an honest, visible seam rather than a silent gap.

### 3.4 Availability is part of the documentation

`fs` and `rand` are real, shipped configurations, not hypotheticals: the wasm
build turns both off
([`nessemble-wasm/Cargo.toml`](../crates/nessemble-wasm/Cargo.toml)), so in the
browser assembler `read_blob` and `rand` are simply absent and calling one is a
"function not found" error. The docs say this in prose today, scattered across
two sections. As a column it is one glance, and the language server can mark a
completion item accordingly.

## 4. The docs table of contents

### 4.1 The domains

Seven, chosen so that every group maps to a section that already exists on the
page — the TOC is an index of the current document, not a rewrite of it:

| Domain | Covers | Anchor |
| --- | --- | --- |
| **Entry point and output** | `custom(ints, texts)`, return conventions, `emit_source` | `#writing-a-script`, `#emitting-assembly-source` |
| **Files and paths** | `open_file`, the `File` methods, `read_blob(path)`, `@/`, `file://` | `#filesystem-access`, `#declaring-file-arguments` |
| **Images (PNG)** | `decode_png`, `decode_png_file`, `.width`/`.height`/`.pixels`, `.r`, `.pixel`, `.tile`, `.find_cell`, `.cell_equals`, `.nearest_cell` | `#decoding-pngs`, `#pixel-accessors`, `#cell-matching` |
| **Palette** | `quantize`, `nes_shade` | `#palette-quantization` |
| **Structured data** | `parse_xml`, `parse_xml_file`, the `xml_node` members, `parse_json`, `parse_json_file` | `#parsing-structured-data`, `#xml`, `#json` |
| **Numbers, strings, blobs** | `parse_int_list`, `to_char`, `trimmed`, `format_hex` | `#bulk-numeric-decoding`, `#string-and-hex-helpers` |
| **Randomness** | `rand`, `rand(min, max)`, `rand_float`, `rand_bool`, `shuffle`, `sample` | `#random-numbers` |

Every anchor in that table resolves against the page as it stands today, which is
the point: the TOC ships against the existing prose, and the prose gets edited
afterwards where the TOC exposes a thin spot ([§4.3](#43-what-else-the-page-gains)).

### 4.2 Generated, not hand-written

The block is rendered by `cargo run -p xtask -- script-api`, written between
markers in `extending.md`:

```markdown
<!-- BEGIN generated: script-api-toc — edit crates/nessemble-script-api, not this block -->
…
<!-- END generated: script-api-toc -->
```

`--check` re-renders and diffs without writing, and joins the `docs` job in CI.
A hand-maintained TOC of forty entries is a changelog entry away from lying, and
this repo has an explicit rule about generated content
([`CLAUDE.md`](../CLAUDE.md)) — the marked block is the same contract, applied to
one region of an otherwise hand-written page. Everything outside the markers stays
prose that a human writes.

Rendered shape, per domain: a heading, one sentence of orientation, then a table
of `signature` / `summary` / availability, each signature linking to its section.

### 4.3 What else the page gains

Building the TOC surfaces where the prose is thinner than the API:

- `emit_source` is documented under "Emitting assembly source" but never listed
  as a function with a signature.
- `read_blob(path)` (the one-call form) is a bullet inside the `open_file` list.
- `.attr(name)`, `.find(name)`, `.find_all(name)` are shown in examples; their
  return conventions on a miss (`()` vs an empty array) are in
  [plan 013](013-structured-data-parsing.md) §2.2, not the docs.
- The execution model — **`custom()` is called without evaluating the script
  body, so top-level statements never run** ([§2.2](#22-the-editor-goes-dark-at-the-rhai-boundary))
  — is documented nowhere. It gets a short subsection under "Writing a script",
  and the LSP lint in [§5.3](#53-script-specific-lints) points at it.

These are prose edits made while the section is open, not a rewrite of the page.

### 4.4 A third reference category

`nessemble reference` has two categories, `instructions` and `directives`, both
served from static tables ([`reference.rs`](../crates/nessemble-cli/src/reference.rs)).
The catalog makes `script` a third, for free:

```text
nessemble reference script              # every entry, grouped by domain
nessemble reference script nes_shade    # signature, summary, docs link
```

Small, but it is the offline half of the same answer, and it is the cheapest
possible proof the catalog is well-formed data and not just a docs fixture.

## 5. LSP support for Rhai scripts

### 5.1 Document kinds come first

Everything else depends on this. `Document`
([`lib.rs:105`](../crates/nessemble-lsp/src/lib.rs)) gains a `kind: DocKind`
(`Asm` | `Rhai`), derived from the URI's extension at `didOpen` (cross-checked
against the notification's `languageId`, which `didChange` does not carry — so
the extension is the authority and the id is a tiebreak for extensionless
buffers).

Every request handler then branches on it, and — the part that matters most —
`analyze_and_publish` must **not** run the 6502 analyzer over a `Rhai` document,
project scans must not pull `.rhai` files into the include graph, and formatting,
semantic tokens, rename, inlay hints, and code actions must return nothing rather
than assembly-shaped answers. A regression here is worse than the missing
feature: it is confident nonsense in the editor.

### 5.2 Diagnostics: what Rhai can and cannot tell us

`engine.compile(source)` gives parse and syntax errors with a position, which
become diagnostics directly. That is the whole of what the compiler knows: Rhai
resolves function calls at run time, so **no amount of compiling finds
`decode_png_fil`**. Anything beyond syntax has to come from walking the AST
ourselves — which this workspace already does, in `purity::impurity`
([`purity.rs:60`](../crates/nessemble-script/src/purity.rs)), under the same
`internals` feature. The mechanism is proven; the lints are new.

### 5.3 Script-specific lints

Published like the assembly lints — gentle severity, `source = "nessemble-script"`,
a rule id in `code` — so they read as advice and can be filtered:

| Lint | Fires when | Why it matters |
| --- | --- | --- |
| `missing-custom` | A script mapped by a `pseudo.txt` in the workspace defines no `custom` function | Today: a build error, found on the first build that reaches the directive |
| `top-level-statement` | A statement sits outside any `fn` | It never executes ([§2.2](#22-the-editor-goes-dark-at-the-rhai-boundary)); a `const` written there is `Variable not found` at build time |
| `custom-arity` | `custom` is declared with other than two parameters | The host calls it with exactly `(ints, texts)` |
| `unknown-host-function` | A call resolves to no script-local `fn`, no catalog entry, **and** is within edit distance 2 of one that is | Catches the typo class Rhai defers to run time |

The last one is deliberately narrow. Flagging *every* unrecognized call would
need the complete set of Rhai built-ins the packages register, which is exactly
the set the `metadata` feature would have given us and does not
([§3.3](#33-keeping-it-honest)). Restricting the lint to near-misses of known
names needs no such set: `decode_png_fil` is flagged, `some_rhai_builtin_we_never_listed`
is not. False negatives, never false positives — the right side to be wrong on
for a lint that appears while someone is typing. Recorded in
[§11.3](#113-the-unknown-function-lint-only-flags-near-misses).

### 5.4 Completion, hover, and signature help

All three are the catalog, rendered three ways:

- **Completion** — every entry as a `CompletionItem`: `name` as the label,
  `signature` as the detail, `summary` as the documentation, `kind` mapped to
  `Function`/`Method`/`Property`. After a `.`, offer only members, narrowed by
  the receiver's inferred handle type when the receiver is a local whose
  assignment is a `decode_png*` or `parse_xml*` call — and fall back to all
  members when it is not, since Rhai is dynamically typed and a wrong guess that
  *hides* the right completion is worse than a broad list. Script-local `fn`
  names come from the AST.
- **Hover** — signature, summary, availability, and a link to the docs section,
  built from `anchor` against `DOCS_BASE_URL`
  ([`xtask/src/main.rs:26`](../xtask/src/main.rs)). This is the payoff for §4:
  the same sentence in the book and under the cursor.
- **Signature help** — parameters parsed from `signature`, with the active
  parameter tracked across commas. `parse_int_list` and `quantize` have two
  registered arities each; both are offered.

### 5.5 Symbols, definition, folding

Cheap, and all from the same AST: `documentSymbol` lists each `fn` with its
parameters (`custom` first); `definition` and `references` resolve script-local
functions; `foldingRange` folds function bodies and comment runs. Formatting and
semantic tokens are **out of scope** ([§7](#7-non-goals)) — nessemble has no
opinion about Rhai layout, and the Rhai community extension already highlights.

### 5.6 The link back to assembly

The one feature that only *this* server can offer, because only this server reads
both sides:

- **Go to definition on `.foo`** in an `.asm` buffer opens the script `pseudo.txt`
  maps it to. `custom_scripts` ([`lib.rs:369`](../crates/nessemble-lsp/src/lib.rs))
  already produces exactly that map for diagnostics; this is a second reader.
- **Hover on `.foo`** shows the script's path and the doc comment above its
  `custom` function — the same "comment run above the definition" convention hover
  already uses for labels ([plan 001](001-language-server.md), Phase 5).

### 5.7 What the client has to change

The VS Code extension activates on `onLanguage:nessemble` and contributes that
language for `.asm`/`.s`
([`editors/vscode/package.json`](../editors/vscode/package.json)). It gains
`onLanguage:rhai`, `.rhai` in the client's document selector, and a `rhai`
language contribution so the id exists when no other extension provides it —
VS Code merges contributions for a shared id, so a user who already has a Rhai
extension keeps its highlighting and gains our server. `editor.md` gets a
"Pseudo-op scripts" section, and other editors get a one-line note that the
selector needs `rhai` alongside `nessemble`.

### 5.8 Feature gating

`nessemble-lsp` gains a `scripting` feature: **off**, the catalog-driven half
(completion, hover, signature help, `.foo` → script navigation) still works,
because the catalog carries no Rhai; **on**, it depends on `nessemble-script`
and adds everything that needs a compiled AST (syntax diagnostics, the lints,
symbols, folding, script-local definitions). `nessemble-cli` wires
`scripting + lsp` → `nessemble-lsp/scripting`, so a default build gets
everything and `--features lsp` alone still builds and still helps.

## 6. Script coverage: what the report leaves out

### 6.1 A script that never runs is not in the report

The fix has a pleasant shape, because `FileHits`
([`coverage.rs:36`](../crates/nessemble-script/src/coverage.rs)) already keeps
*coverable* and *hit* as two independent sets that accumulate by union. Seeding a
script that later runs is therefore a no-op, and seeding one that never runs
gives it exactly the entry it should have had: every coverable line, zero hits.
Ordering does not matter, which means no new coupling to the assembly's lifecycle.

- `nessemble-script::coverage` gains
  `seed(source, script_path, cov) -> Result<(), String>`: compile, walk the AST
  for coverable lines (the existing walk, lifted out of `run_with_coverage` and
  shared), insert with an empty hit set.
- `nessemble-cli::custom` exposes the mapping's scripts —
  `mapped_scripts(pseudo_file) -> Vec<PathBuf>` over the existing `read_mapping`.
- `coverage::run` seeds every one of them before assembling.

Two traps, both worth stating because both produce a *duplicated* file in the
report rather than a visible error:

1. **The key must match.** `build_resolver_with_coverage`
   ([`custom.rs:296`](../crates/nessemble-cli/src/custom.rs)) keys by
   `path.canonicalize().unwrap_or(path)`. Seeding must resolve the mapping entry
   the same way `Resolver::locate` does (relative to the mapping file's own
   directory) and canonicalize identically.
2. **Two directives may map to one file.** Deduplicate by canonical path before
   seeding.

A mapped script that is missing or does not compile is reported as a warning on
stderr and skipped — a coverage run is never blocked by one bad mapping entry,
matching how `collect_ignores` already handles an unreadable source file.

### 6.2 `--scripts` without a CDL

`--cdl` becomes optional, with a clap group requiring at least one of
`--cdl` / `--scripts`; neither is an error naming both. When `--cdl` is absent:

- Assemble with plain `Options::default()` — no forced `nes: true`, no
  `source_map: true`. Both exist to satisfy the CDL half
  ([`coverage.rs:104`](../crates/nessemble-cli/src/coverage.rs)), and forcing NES
  mode would fail a non-NES project that has perfectly good scripts to measure.
- Skip CDL loading, the iNES header reads, the size guard, and the source-map
  requirement. The report contains script files only.
- `--format`, `--out`, `--no-ignore` behave exactly as they do today.

This is what makes the feature usable in CI: `nessemble coverage main.asm -p
pseudo.txt --scripts --format lcov --out scripts.lcov` needs no emulator and no
playthrough.

### 6.3 The report should say which number is which

With §6.1 in place a script-heavy project's percentage moves for reasons that have
nothing to do with the ROM, and the single summary line
([`coverage.rs:236`](../crates/nessemble-cli/src/coverage.rs)) cannot say so:

- **Summary** gains a split when both halves are present:
  `coverage: 812/900 lines (90.2%) — rom 780/840, scripts 32/60`.
- **JSON** gains `"kind": "rom" | "script"` per file — additive, so existing
  consumers are unaffected. LCOV is unchanged; it has nowhere to put this and
  `genhtml` groups by path perfectly well.
- **`--scripts` that instruments nothing** prints a warning naming the likely
  cause (no `-p` mapping given, or the mapping is empty) instead of returning a
  ROM-only report with no explanation.

### 6.4 Deliberately unchanged

- **Bundled `~/.nessemble/scripts` stay excluded.** They are nessemble's code,
  not the project's; their coverage is nessemble's test suite's problem
  ([plan 007](007-cdl-based-coverage.md) §8).
- **The cache stays bypassed** under `--scripts`. A cache hit executes nothing
  and would report a covered script as uncovered
  ([`custom.rs:284`](../crates/nessemble-cli/src/custom.rs),
  [plan 011](011-pseudo-op-caching.md)).
- **`SharedCoverage` stays `Rc<RefCell<_>>`**, and so stays out of the parallel
  prewarm path ([plan 013](013-structured-data-parsing.md) §13.4). Coverage runs
  are not the run you optimize.
- **The `custom()`-is-called-without-evaluating-the-body model stays.** §4.3
  documents it and §5.3 lints it; changing it would change what every existing
  script means.

## 7. Non-goals

- **A Rhai formatter or semantic-token provider.** Layout is the Rhai
  ecosystem's business.
- **Type checking scripts.** Rhai is dynamically typed; §5.3's lints are
  name-level and stop there.
- **Sandboxing.** Scripts still run with the process's full filesystem access,
  and the docs still say so.
- **Branch or expression coverage** for scripts. Line coverage is what the AST
  walk supports and what LCOV consumes.
- **Instrumenting bundled scripts** ([§6.4](#64-deliberately-unchanged)).
- **A `nessemble lint` mode for `.rhai`.** The lints live in the editor first; a
  CLI surface can follow once they have proven themselves.

## 8. Acceptance

1. `docs/src/extending.md` opens with a domain-grouped table of every host
   function, each linking to the section that explains it; `xtask script-api
   --check` passes in CI and fails when the catalog and the page disagree.
2. Adding a `register_fn` to `engine()` without a catalog entry fails
   `cargo test`.
3. `nessemble reference script nes_shade` prints its signature and summary.
4. Opening a `.rhai` file in VS Code with the extension installed yields: syntax
   diagnostics, the four lints of §5.3, host-API completion with documentation,
   hover matching the docs text, signature help, and an outline — and opening an
   `.asm` file still yields exactly what it does today.
5. Go-to-definition on a `.foo` directive opens its mapped script.
6. `nessemble coverage main.asm -p pseudo.txt --scripts` with **no `--cdl`**
   produces a report containing every mapped script, including one whose
   directive is never used, at 0%.
7. A script mapped under two directive names appears once.
8. `cargo build -p nessemble-cli --no-default-features --features lsp` builds,
   and its server still completes and documents the host API.

## 9. Phased plan

Phases 0–3 are the catalog track and are ordered; Phase 4 is independent and can
land at any point.

### Phase 0 — the catalog. **Shipped ([§12.1](#121-phase-0)).**

`nessemble-script-api` with the full table, `Domain`/`ApiKind`/`Availability`,
and the drift test in `nessemble-script` ([§3](#3-the-catalog-one-table-four-consumers)).
Consumed by nothing yet. Ships with `nessemble reference script`
([§4.4](#44-a-third-reference-category)) as its first reader, because a table with
no reader is a table nobody checks.

### Phase 1 — the docs TOC. **Shipped ([§12.2](#122-phase-1)).**

`xtask script-api` (`--write` / `--check`), the marked block in `extending.md`,
the CI wiring, and the prose repairs of [§4.3](#43-what-else-the-page-gains).
**Delivers goal 1 on its own.**

### Phase 2 — LSP, catalog half. **Shipped ([§12.3](#123-phase-2)).**

Document kinds ([§5.1](#51-document-kinds-come-first)) — including the
must-not-analyze-Rhai-as-assembly guards and their regression tests — then
completion, hover, and signature help from the catalog, plus `.foo` → script
navigation ([§5.6](#56-the-link-back-to-assembly)). No Rhai dependency yet.

### Phase 3 — LSP, compiled half. **Shipped ([§12.4](#124-phase-3)).**

`nessemble-lsp/scripting`: syntax diagnostics, the four lints, document symbols,
folding, script-local definition and references. VS Code manifest and
`editor.md` ([§5.7](#57-what-the-client-has-to-change)). **Completes goal 2.**

### Phase 4 — coverage. **Shipped ([§12.5](#125-phase-4)).**

`seed` and `mapped_scripts` ([§6.1](#61-a-script-that-never-runs-is-not-in-the-report)),
optional `--cdl` ([§6.2](#62---scripts-without-a-cdl)), the summary and JSON
split ([§6.3](#63-the-report-should-say-which-number-is-which)), and `usage.md`.
**Completes goal 3.** Independent of Phases 0–3.

## 10. Risks

| Risk | Mitigation |
| --- | --- |
| Document kinds are added but a handler is missed, and an assembly feature runs over a `.rhai` buffer | The kind branch is a single `match` at the top of each handler; a test opens a `.rhai` document and asserts every non-script request returns empty |
| The catalog drifts anyway, through a package-registered function the scan cannot see | Those entries are `Availability::Feature` and enumerated in one place, with a comment saying why the scan skips them |
| The generated TOC block conflicts on every branch that touches the API | It is one contiguous region at the top of one file; `--write` regenerates it, so the resolution is mechanical |
| `unknown-host-function` false-positives on a legitimate Rhai built-in | Near-miss-only by construction ([§5.3](#53-script-specific-lints)); the edit-distance threshold is a constant, tuned against the bundled `ease.rhai` and the docs' examples |
| Seeded scripts double-count under a different path spelling | One canonicalization helper shared by seeding and the resolver, with a test that maps one script under two directive names and asserts one file in the report |
| `--cdl` becoming optional silently changes an existing invocation's meaning | It cannot: every invocation valid today passes `--cdl` and takes the same path. Only the previously-rejected invocations become valid |

## 11. Decisions

### 11.1 A new crate rather than `nessemble-isa`

The language server must serve the API docs in a build with no scripting host
([§3.2](#32-where-it-lives)), so the catalog cannot live in `nessemble-script`.
Between a new data crate and `nessemble-isa`, the deciding argument is that
`DIRECTIVES` is already documented as a stretch of that crate's remit; a scripting
host's API would be a second, larger one, in a crate named for the instruction
set. A ten-line data crate is cheaper than the explanation.

### 11.2 The drift check scans source rather than asking the engine

Asking `Engine::gen_fn_signatures` is the better check and is unavailable: rhai's
`metadata` feature does not compile in this workspace
([§3.3](#33-keeping-it-honest)). The source scan catches the drift that actually
happens — a `register_fn` added without a doc entry — at zero dependency cost. If
`metadata` becomes viable, the scan is one test to replace.

### 11.3 The unknown-function lint only flags near-misses

Complete unknown-call detection needs the complete set of built-ins, which is the
same blocked feature. Near-miss-only detection needs nothing beyond the catalog
and catches the class of mistake that Rhai's runtime dispatch makes expensive
(a typo in an untaken branch). A lint that is quiet-but-right beats one that is
loud-and-sometimes-wrong in a pane the author did not ask to open.

### 11.4 Coverage is a separate track

The coverage defects ([§6](#6-script-coverage-what-the-report-leaves-out)) share
a subject with the rest of this plan but no code. Keeping Phase 4 independent
means the report can be fixed without waiting on the catalog, and reviewed by
someone reading only [plan 007](007-cdl-based-coverage.md).

### 11.5 Prose stays hand-written; only the TOC is generated

The generated region is an index. The teaching — "Macros or scripts?", the worked
examples, the warning that scripts are not sandboxed — is writing, and stays
writing ([§4.2](#42-generated-not-hand-written)).

## 12. As built

*(One subsection per phase, in the manner of
[plan 013](013-structured-data-parsing.md) §13, recording where the build
deviated from this document and why.)*

### 12.1 Phase 0

**42 entries, and the count is the interesting part.** The catalog holds 42
entries across the seven domains of [§4.1](#41-the-domains) — 32 registered by
`engine()`, 9 curated from the `rhai-fs` and `rhai-rand` packages, and `custom`
itself. Thirty-two is also exactly the number of distinct names `engine()`
registers, minus one. Nothing was discovered missing from the docs and nothing
was found documented that no longer exists: the prose was accurate. What it was
not was *enumerable*, which is the whole complaint.

**`Origin` is a field §3.1 did not have.** The plan's struct carried
`availability` alone and left the drift test to exempt "package-registered"
entries by feature — which conflates two different things. `read_blob(path)` is
`Feature("fs")` *and* registered by `engine()`; `file.read_blob()` is
`Feature("fs")` and registered by `rhai-fs`. Only the first can be held against
the engine's source. So origin (`Host` / `Package(name)` / `Script`) and
availability (`Always` / `Feature(name)`) are separate axes, and the drift test
keys on origin. The `Script` variant exists for `custom`, which is defined by the
script rather than the host and is otherwise a permanent false positive.

**The name collision is real and had to be designed for, not avoided.**
`read_blob` is two different functions — a method on an open file handle and a
one-call free function — and both are documented. So `lookup` returns an
iterator rather than an `Option`, `reference script read_blob` prints both, and
the third drift test needed a carve-out: a `Package` entry whose name *is*
registered is only misfiled when no `Host` entry shares that name. That was
found by the test failing, which is the right way round.

**The scan is generic over `.register_*`, not a list of methods.** §3.3 named
`register_fn` / `register_get` / `register_type_with_name`. Hard-coding three
method names means a future `register_indexer_get` silently escapes the gate, so
the scanner instead finds every `.register_`, walks to the opening paren, and
takes the first string literal if there is one. Calls with no name literal —
`FilesystemPackage::new().register_into_engine(&mut engine)` — contribute
nothing, which is correct: those are precisely the `Origin::Package` entries.
The scan is bounded to the source above `#[cfg(test)]` and skips comment lines.

Three guards keep the gate from going quiet: a floor on how many registrations
the scan must find (a scanner that matches nothing would otherwise pass
vacuously), a spot-check naming each registration *shape* the source uses, and a
negative control run by hand — renaming one catalog entry fails both directions
with the expected messages.

**`path` is the one registered name deliberately absent.** It is `rhai-fs`'s
path-conversion hook, redefined so relative paths reroot to the directive's
source directory; a script never calls it. It sits in a `NOT_SCRIPT_FACING`
constant in the test with that reasoning, rather than becoming a catalog entry
documenting an internal.

**`reference script` wraps its second column.** `DIRECTIVES` summaries are three
or four words and align in one pass; these are whole sentences against
signatures up to 44 characters wide, which ran to ~140 columns. The listing
wraps the summary to a 96-column target with a hanging indent. Twenty lines,
worth it.

**§3.2's argument was verified, not asserted.** `cargo build -p nessemble-cli
--no-default-features` — no `scripting`, no `lsp` — builds, and
`nessemble reference script nes_shade` answers from it. That is the property the
whole crate exists for, and Phase 2 depends on it.

### 12.2 Phase 1

**`--write` doesn't exist; bare `script-api` is the write mode.** §5's own body
already said this ("rendered by `cargo run -p xtask -- script-api`"), and the
top-of-plan summary calling it `--write` / `--check` was the stray. `xtask`'s
existing commands (`wasm`, `vsix`, `dist`) all default to *doing the thing*
with no flag, and a `--write` nobody would type differently from no flag at
all is a flag for its own sake. `script-api` takes zero or one argument:
nothing writes, `--check` diffs without writing and exits non-zero on drift.

**The TOC lives at the top of the page, not appended to it.** Placed as a new
`## Host API reference` section right after the trust-warning intro and before
`## Macros or scripts?` — the tutorial content §2.1 describes as reading top to
bottom starts exactly where it always did, just after a reference table a
returning author can stop at instead of scrolling past. The section heading and
its one-sentence lead are hand-written; the marked block holds only the
per-domain headings and tables the catalog renders.

**The splice is marker-relative, not line-relative.** `update_script_api_block`
finds the two marker strings and rewrites only the text between them,
preserving everything before and after byte-for-byte. That makes `--check`
trivially precise (an exact string comparison after re-rendering) and the
regeneration idempotent by construction — a property a dedicated test checks
directly, alongside one that fails a missing marker rather than silently
no-op-ing.

**Two of §4.3's three doc gaps were already closed.** Re-reading the shipped
page before editing: `read_blob(path)` already had its own bullet in
"Filesystem access", and `.attr`/`.find`/`.find_all`'s return conventions were
already spelled out (both landed with plan 013, after §4.3 was written against
an earlier draft of the page). `emit_source` having no listed signature is now
moot — the generated table lists every entry's signature, including its, so no
prose edit was needed there either. The one gap that was real: nothing said
`custom` runs without the script's top-level statements ever executing. That
became "Execution model", a short subsection under "Writing a script" with the
plan's own `SCALE` example, landed verbatim as the reproduction case.

**The `docs` CI job didn't exist yet.** §4.2 says the check "joins the `docs`
job in CI" as though one were already there; `ci.yml` had no such job. Phase 1
adds it — one step, `cargo run -p xtask -- script-api --check` — and extends
the Claude Code stop hook and its README table the same way `changeset check`
already was, so a local turn fails the same way CI would before either reaches
GitHub.

### 12.3 Phase 2

**§5.6's first bullet was already shipped, and not by this plan.** `goto_definition`
resolving a `.foo` directive to its mapped script (`custom_scripts()`, keyed off
the same `--pseudo` mapping the diagnostics path already reads) has been in
`nessemble-lsp` since 2.13.1 — long before plan 014 existed. §5.6 describes it as
new work ("this is a second reader"); it is really a *third* one, the other two
being the diagnostics pass and this. Phase 2's actual work on that bullet was
adding the other half: hovering a `.foo` now shows the script's path and the doc
comment above its `custom` function (`custom_directive_hover`), the one piece
that was genuinely missing.

**The catalog-driven half is two new files, not new branches sprinkled through
`lib.rs`.** `nessemble-lsp/src/api.rs` (completion, hover, signature help — no
`scripting` gate, per §5.8) and `nessemble-lsp/src/scripting.rs` (Phase 3's
compiled half) hold every Rhai-specific behavior; `lib.rs` gained a `DocKind`
enum, a `doc_kind()` classifier, and one `if doc.kind == DocKind::Rhai { … }`
branch at the top of each handler — `complete`, `hover`, `goto_definition`,
`references`, `document_symbols`, `folding_ranges`, `signature_help` (new) — plus
a bare "return nothing" guard in `format_document`, `semantic_tokens`,
`inlay_hints`, `rename`, `code_actions`, and `document_links`. The include-graph
candidate lists (`compute_diagnostics`, `definition_location`) were switched from
`self.documents.keys()` to a new `asm_document_paths()` filter, so an open
`.rhai` buffer can never become a node in the `.include` graph even though
nothing in its own scan would have matched it anyway (§5.1's guard is about the
*document store*, not just the file-extension filters that happened to already
exclude it).

**Signature help is a capability this server never advertised before.** Nothing
in the assembly side calls anything with an argument list, so `signatureHelp`
starts at zero here. It parses the catalog's `signature` field text — no AST,
matching the "no Rhai dependency" constraint — splitting on top-level commas
and widening a `[, name]`-bracketed optional tail into a second arity. That
turned out to be exactly what makes `parse_int_list(text, delim[, radix])` offer
two signatures (with and without `radix`) without any special-casing: the
bracket notation §3.1 already used for optional arguments *is* the arity split.
`quantize`'s two engine-level registrations (scalar and array) don't produce two
signatures this way — both have the same two-parameter shape in text — which
is a difference from §5.4's "`parse_int_list` and `quantize` have two registered
arities each; both are offered" and is fine: the two `quantize` forms read
identically at a call site, so there is nothing for a second signature to show.

**A real bug, caught by the test written for it:** the first signature-help
implementation counted top-level commas by depth alone, so
`parse_int_list(texts[0], ",", )` — the delimiter argument is itself a
comma — miscounted the active parameter by one. `string_literal_mask` marks
every char inside a `"…"` literal (honoring `\"`) so the comma/paren scanners
skip it; caught by `signature_help_offers_both_arities_of_an_optional_argument`
failing on first run, not by inspection.

**Member narrowing after `.` was simplified to the fallback the plan itself
names.** §5.4 asks for completions narrowed by the receiver's inferred handle
type "when the receiver is a local whose assignment is a `decode_png*` or
`parse_xml*` call — and fall back to all members when it is not." Full
assignment-tracking type inference was judged not worth its complexity for this
pass; the implementation always takes the stated fallback (every catalog entry
offered, unfiltered by kind or inferred type). That is a real instance of the
rule the plan wrote, not a skipped feature — a wrong narrowing that hides the
right completion is worse than a broad list, which is the reasoning §5.4 gives
for the fallback existing at all. Kind-based narrowing (methods/properties only
after a `.`) was considered and left out for the same reason: it is one more
guess that can be wrong.

### 12.4 Phase 3

**`nessemble-lsp/scripting` depends on `rhai` directly, not on `nessemble-script`.**
§5.8's own wording says the feature "depends on `nessemble-script`"; it doesn't,
and can't usefully: `nessemble-script`'s `engine()` is private, and the crate
does not re-export `rhai`'s types, so reaching `rhai::Engine`/`AST`/`ASTNode` at
all requires a direct dependency on `rhai` regardless. Diagnosing a script is
also a strictly smaller job than running one — `Engine::compile` never resolves
a function call, so it needs no registered host functions and therefore no
`rhai-fs`/`rhai-rand` packages either. The dependency line mirrors
`nessemble-script`'s own (`rhai = { version = "1", default-features = false,
features = ["std", "internals"] }`, the same `internals` gate `purity.rs` uses)
so both stay pinned to one resolved version in the workspace's single
`Cargo.lock`, which is the property §5.8 was actually protecting.

**The default optimizer would have erased the plan's own reproduction case.**
§2.2's motivating example — a top-level `const SCALE = 3;` used only inside
`custom` — is exactly what Rhai's `OptimizationLevel::Simple` (the crate
default) is designed to fold away: constant propagation inlines `SCALE` into
`custom`'s body and can then treat the now-unreferenced top-level declaration as
dead code, silently deleting the statement `top-level-statement` exists to find.
The diagnostics engine sets `OptimizationLevel::None`, keeping the compiled
`AST` a 1:1 parse tree. Found by writing the plan's own example as a test before
anything else, not by reasoning about the optimizer in the abstract.

**Positions come from a text scan almost everywhere, by necessity and then by
choice.** `rhai::ScriptFnMetadata` (from `AST::iter_functions()`) carries no
source position at all under `internals` — only under the `metadata` feature,
which §3.3 already recorded as not compiling in this workspace. So a lint or
hover that needs to point at `fn custom(` has no AST-native way to. A small
`fn NAME(` line scanner (`fn_def_range`/`fn_name_column`, word-bounded so `fn
customize(` doesn't match `custom`) fills the gap for `custom-arity`'s range,
document symbols, folding's per-`fn` brace matching, and local
definition/hover/references. Once that scanner existed, using it for symbols,
folding, and local navigation *instead of* a second AST pass was a choice, not
just an availability accident: a script with a syntax error elsewhere in the
buffer still gets an accurate outline and still lets you jump to a helper
function, because none of that machinery requires the file to compile. Only
`diagnostics()` (parse errors, the four lints, `top-level-statement`, and
`unknown-host-function`, which does need the compiled call graph) touches the
AST at all. `unknown-host-function`'s own range recovery is symmetric: a call
expression's `rhai::Position` is a point, not a span, so `call_name_range`
widens it to the callee's own length by searching near the reported
line/column — the same "narrow to the token" move `diagnostic_range` already
makes for assembler diagnostics.

**The near-miss threshold and edit distance are a plain Levenshtein, no
dependency.** `EDIT_DISTANCE_THRESHOLD = 2` against every distinct catalog name
(deduped, since `read_blob` appears twice) — `decode_png_fil` → `decode_png_file`
is distance 1 and is flagged with a suggestion; a Rhai built-in the catalog
never lists (`to_string`, `type_of`, …) is far enough from every catalog name
that it isn't, matching §11.3's "false negatives, never false positives."

**Six small dispatch functions live outside `impl Server`.** Each one
(`rhai_document_symbols`, `rhai_local_definition`, `rhai_local_references`,
`rhai_local_fn_hover`, `rhai_doc_comment_above_custom`, `rhai_folding_ranges`)
either calls into `scripting::` (feature on) or returns the empty value (feature
off) and touches no server state either way — `clippy::unused_self` said so
under `--no-default-features`, and free functions were the honest fix rather
than a blanket `#[allow]`.

**The VS Code manifest gained a second `languages` contribution, not a second
extension.** `package.json` adds `rhai` (`.rhai`, no grammar of its own —
VS Code merges contributions for a shared language id, so an installed Rhai
syntax-highlighting extension keeps its grammar and gains this server) alongside
the existing `nessemble` entry, plus `onLanguage:rhai` in `activationEvents`.
`extension.js`'s `documentSelector` gained the matching `{ scheme, language:
"rhai" }` pair (`file` and `untitled`), and the file-watcher glob picked up
`*.rhai`. `editor.md` gained a "Pseudo-op scripts (`.rhai`)" section, cross-linked
from the Extending page's own generated table
([§4](#4-the-docs-table-of-contents)) so the same host-API sentence is reachable
from either page.

**Regression coverage follows the Risks table's own prescription.** One test
(`rhai_document_is_never_analyzed_as_assembly`) opens a `.rhai` buffer whose text
is deliberately neither valid Rhai nor valid assembly and asserts every
assembly-only handler — formatting, semantic tokens, code actions, inlay hints,
rename, document links — answers with nothing, plus that no `source: "nessemble"`
or `"nessemble-lint"` diagnostic appears; that is the exact test
[§10](#10-risks)'s first row asks for. Catalog completion/hover/signature-help,
the four lints, `.rhai` document symbols/folding/local navigation, and the `.foo`
directive's new hover text each have their own focused test, run under both
`--features scripting` and `--no-default-features` so the catalog-only build's
promise (acceptance item 8) is checked, not assumed.

### 12.5 Phase 4

**`--cdl` becoming optional stayed a hand-rolled check, not a `clap::ArgGroup`.**
§6.2 describes the requirement as "a clap group requiring at least one of `--cdl`
/ `--scripts`"; `coverage.rs` had no `ArgGroup` usage anywhere to extend, and every
other cross-flag rule in this module (the iNES-header checks, the CDL size guard)
is already a plain `if` with an `eprintln!` and a return code. Matching that shape
— `if args.cdl.is_empty() && !args.scripts { … }` at the top of `run` — needed no
new dependency on clap's group derive attributes and reads the same as every
other validation already in the file.

**The ROM half became its own function, `build_rom_report`, returning
`Result<CoverageReport, u8>`.** Before this phase `run` had one straight-line
path from "assemble" to "write the report"; splitting the CDL loading, iNES
header reads, and `build_report_with_ignores` call out from `run` — rather than
threading an `if want_rom { … } else { … }` through the middle of the existing
function — keeps `run`'s new branch (`want_rom`) a single `if`/`else` producing a
`CoverageReport` either way, instead of every early `return RETURN_EPERM` inside
the ROM path also needing to skip the script-coverage folding and summary code
that now follows it unconditionally.

**Seeding reuses `coverable_lines`, extracted verbatim from `run_with_coverage`
as §6.1 asks — and that reuse surfaces a pre-existing asymmetry, not a new bug.**
A script that is only *seeded* (never invoked by a build) can report fewer
coverable lines than the same script would if it actually ran: Rhai's default
`OptimizationLevel::Simple` folds a trivial constant body (e.g. a bare `[1]`
return) into fewer AST nodes than `AST::walk` finds in an unfolded tree, while
`run_with_coverage`'s `hit` set comes from live debugger events at run time,
which surface a few extra line positions (a function's own signature and closing
brace) that the folded AST no longer carries. This asymmetry already existed for
every *executed* script before this phase — `coverable` and `hit` were always
unioned, and `hit` routinely contains lines `coverable` alone would have missed —
Phase 4 is only the first caller that can read `coverable` with no `hit` to
supplement it (a script whose directive the build never reaches). Confirmed with
a scratch reproduction, not fixed: the acceptance bar ([§8](#8-acceptance) item 6)
only asks that an unreached script appear in the report at 0%, which it does
regardless of exactly how many lines its folded AST exposes.

**Seeding's dedup key is the same canonicalized path the resolver's cache key
uses, per §6.1's first trap.** `seed_mapped_scripts` (in `nessemble-cli`) resolves
every mapping entry through `custom::mapped_scripts`, then canonicalizes each
path before both deduplicating and calling `nessemble_script::coverage::seed` —
the same `path.canonicalize().unwrap_or(path)` `build_resolver_with_coverage`
already keyed on. A script mapped under two directive names therefore seeds (and
later reports) as one file, checked directly by a test that maps `.a` and `.b` to
one script and asserts the JSON report names it exactly once.

**A silently-empty `--scripts` run now says why.** §6.3's third bullet is a single
`if cov.is_empty()` check after folding script coverage into the report,
distinguishing "no `-p`/`--pseudo` mapping was given" from "its mapping named no
script that could be read and compiled" — the two ways a `--scripts` run can end
up instrumenting nothing.

**The ROM/script summary split is a `CoverageReport::totals_for(FileKind)`
method, not an ad hoc filter at the call site.** `FileCoverage` gained a `kind:
FileKind` field (`Rom` | `Script`), threaded through every constructor
(`build_report_with_ignores` sets `Rom`; `FileCoverage::from_line_hits_with_ignores`
— script coverage's only constructor — sets `Script`) and into the JSON output as
an additive `"kind"` field per file; LCOV is unchanged, exactly as §6.3 specifies.
`totals_for` filters `self.files` by kind before summing, mirroring `totals`'s own
shape, so a future JSON/LCOV consumer that wants the same split has a typed
method to call rather than reimplementing the filter.
