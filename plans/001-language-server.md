# nessemble-rs: A Plan for a Language Server

> Status: **Phases 0–7 complete** (0–6 merged; 7 in review); the server ships
> behind the default-on `lsp` feature and is run with `nessemble lsp`. Phase 7
> adds **workspace-aware analysis** (auto include-graph discovery + the Phase-6
> overlay), fixing cross-file "symbol not defined" false positives. Only the
> optional **Phase 8** (folding/rename/code actions) remains; once it's done (or
> declared out of scope), the `-dev` suffix drops to `2.5.0` for the release.
> All planning decisions are settled (see [§9 Decisions](#9-decisions)).

---

## 1. Goal

Provide a **Language Server Protocol (LSP)** implementation for nessemble's
flavor of 6502/NES assembly, so editors like **VS Code / Cursor** (and any
LSP-capable editor: Neovim, Helix, Emacs, …) get live diagnostics, completion,
and highlighting while writing `.asm` sources.

Two hard requirements from the request:

1. The server is **launchable from the existing CLI** — a `nessemble lsp`
   subcommand (§4).
2. It is **usable in VS Code / Cursor** — delivered as the server plus setup
   docs (no bespoke extension in this repo; §6).

## 2. Why this is a good fit

nessemble-rs already has the analysis engine an LSP needs: a hand-written lexer,
a recursive-descent parser, a two-pass assembler, an opcode/addressing-mode
table (`nessemble-isa`), and a directive catalog. A language server is largely a
matter of (a) exposing that analysis incrementally over a protocol and (b)
enriching it with **source positions** it does not currently track.

## 3. Current state — what we have and what's missing

Grounded in the current code (not aspirational):

**Available today**

- `nessemble-core`: `assemble(source, &Options) -> Result<Assembly, AssembleError>`
  and `assemble_file(...)`. `Assembly` carries `rom`, `warnings: Vec<Diag>`,
  `symbols: Vec<ListSymbol>`, and `coverage`.
- `nessemble-isa`: `OPCODES` table, `Opcode`, `AddressingMode`, `find(mnemonic, mode)`
  — material for completion/hover of instructions (bytes, cycles, modes).
- A `DIRECTIVES` catalog (name + description) — currently private in the CLI
  crate (`reference.rs`).
- Clean CLI subcommand dispatch (`init`/`scripts`/`reference`/`config`), so a new
  `lsp` entry point drops in easily.

**Gaps that shape the plan** (the honest part)

- **Diagnostics are line-only.** `Diag { file, line, message }` — no column, no
  end position. `Token { tok, line, file }` carries no column or byte offset.
  We ship line-level diagnostics first (whole-line ranges) and defer precise
  spans (§5, Phase 4).
- **Highlighting needs columns.** LSP-native highlighting is **semantic tokens**,
  which require a line+character range *per token*. Since the lexer has no column
  today, the highlighting phase carries a **focused lexer-column addition** — a
  narrow first slice of the deferred span work. (Diagnostics remain line-level
  until the full refactor.)
- **First-error abort.** The assembler stops at the first hard error
  (`AssembleError` is a single diagnostic). `warnings` are already a `Vec`, so the
  warning path is fine; surfacing *all* errors at once needs recovery (Phase 4).
- **Symbols have no source position.** `ListSymbol` has no definition line and
  references aren't tracked. This blocks go-to-definition / outline / hover-on-
  symbol, which is why **navigation is deferred** (Phase 5). Completion only
  needs symbol *names*, which are available.
- **Batch, disk-oriented.** The assembler reads the file (and its `.include` /
  `.inc*` targets) from disk. Editors hold **unsaved, in-memory** buffers, so the
  server analyzes the editor's current text, resolving includes relative to the
  document's directory (from disk for now; an open-buffer overlay arrives in
  Phase 6, used by Phase 7's project analysis).

## 4. Proposed architecture

- **New crate `nessemble-lsp`** in the workspace, depending on `nessemble-core`
  and `nessemble-isa`. It owns the protocol loop, the in-memory document store,
  and the mapping from core analysis to LSP types. A library crate is
  unit-testable without spawning a process.
- **CLI entry point:** `nessemble lsp` (a subcommand, mirroring `init`/`scripts`),
  speaking LSP over **stdio**. `nessemble-cli` depends on `nessemble-lsp`,
  **feature-gated** (`lsp`, **on by default**) like `scripting`, so a stock build
  ships `nessemble lsp` while `--no-default-features` drops it and its deps.
- **Transport — recommendation: `lsp-server` (sync, minimal).** It matches the
  project's pure-Rust, minimal-dependency, clean-cross-compile ethos (no tokio /
  async runtime), and a synchronous request loop is a fine fit for analyzing one
  document at a time. The cost is a little more manual message routing. The
  alternative, `tower-lsp`, is more ergonomic but pulls an async stack; we'd
  reach for it only if we later need heavy concurrency. **Decided: lsp-server.**
- **Document store:** an in-memory map of `Uri -> (text, version)` kept in sync
  via `textDocument/didOpen|didChange|didClose`; analysis runs against this text.
- **Analysis bridge:** a thin adapter that runs core analysis on a buffer and
  translates results to LSP diagnostics/completions/tokens. Where the core lacks
  positions, fall back to whole-line ranges (diagnostics) until Phase 4.
- **Shared metadata:** promote the `DIRECTIVES` catalog into **`nessemble-isa`**
  (alongside the opcode table) so the CLI `reference` command and the LSP consume
  one source of truth.

## 5. Phased plan

Ordered by your priorities: **diagnostics → completion → formatting/highlighting**,
with precise spans and navigation deferred. Each phase is independently
shippable to `main`.

> **Versioning across phases.** To avoid cutting a release per phase, the
> workspace version stays at the pre-release **`2.5.0-dev`** while phases land;
> the release workflow skips pre-release versions. Only once **all** phases are
> complete does the final phase drop the `-dev` suffix to **`2.5.0`**, cutting a
> single release containing all the language-server work.

### Phase 0 — Scaffold & transport — ✅ done
- New `nessemble-lsp` crate; `nessemble lsp` subcommand (stdio, `lsp-server`,
  feature `lsp` on by default); LSP lifecycle (`initialize` → advertise
  capabilities → `initialized` → `shutdown`/`exit`); `textDocument`
  open/change/close into an in-memory document store. Full-text sync advertised;
  no analysis yet.
- **Done when:** an LSP client connects, completes the handshake, and the server
  tracks open documents (verified by an in-memory protocol test and an
  end-to-end stdio smoke test). ✅

### Phase 1 — Diagnostics (line-level) — *priority* — ✅ done
- On open/change, assemble the in-memory buffer (via `nessemble_core::
  assemble_source_as`: base dir = the document's directory, includes/media from
  disk) and publish the error + `warnings` as `publishDiagnostics` with
  **whole-line ranges** (UTF-16 line length); `didClose` clears them. Errors map
  to their own line; include-originated diagnostics anchor at the top with their
  origin noted. (Debounce and *all-errors-at-once* recovery come in Phase 4.)
- **Done when:** a syntax/opcode error shows a squiggle on the right line and
  clears when fixed; warnings appear as warnings. ✅ (in-memory protocol test +
  `analyze` unit tests + an end-to-end stdio check.)

### Phase 2 — Completion — *priority* — ✅ done
- `textDocument/completion` offers: mnemonics (from `nessemble-isa`, with their
  addressing modes as detail), directives (the shared catalog, `.`-triggered,
  with descriptions), in-scope labels/constants (from the symbol table, cached
  per document so they survive transient errors), and macro names (scanned from
  `.macrodef`). Client-side prefix filtering; context-awareness and snippets are
  future polish.
- Moved the `DIRECTIVES` catalog and an `AddressingMode::label()` helper into
  `nessemble-isa` (decision C), so the `reference` command and the LSP share one
  source of truth.
- **Done when:** typing offers relevant mnemonics/directives/labels with docs in
  the completion detail. ✅ (completion unit test + lifecycle protocol test +
  an end-to-end stdio check.)

### Phase 3 — Formatting & highlighting — *priority* — ✅ done
- **Lossless tooling lexer** (`nessemble_core::tooling`): a new, position-tracking
  scanner that segments the *entire* input — whitespace and comments included —
  into gap-free `Lexeme`s with byte ranges. It is deliberately **separate** from
  the parity lexer (which stays byte-for-byte untouched, parity 122/122), and is
  the shared base for the two features below (and reusable by the Phase-4 span
  work).
- **Formatting** (`textDocument/formatting`): `tooling::format` normalizes leading
  indentation (instructions indent 4; labels/directives/constants at column 0),
  tidies spacing around commas, and trims trailing whitespace, while **preserving
  comments, other internal spacing, blank lines, and identifier case**. It is
  idempotent. Deliberately conservative — broader operand-spacing reflow,
  comment-column alignment, and case-forcing are deferred (the lossless
  foundation enables them later) to avoid mangling files.
- **Highlighting** (`textDocument/semanticTokens/full`): classifies each lexeme
  (directive→keyword, mnemonic→function, ident→variable, number, string, comment,
  punct→operator; mnemonics detected via `nessemble-isa`) into delta-encoded
  semantic tokens. LSP-native, so it works in any semantic-tokens-capable client
  with no editor grammar to ship.

> **Shared foundation.** The lossless, position-tracking lex pass built here is
> the base that Phase 4 extends into full parser/assembler spans — so the lexer
> work is done once and reused, not thrown away.
- **Done when:** "Format Document" tidies a file deterministically (idempotent),
  and tokens are colorized via semantic tokens. ✅ (formatter unit tests incl.
  idempotence; lexer tests; LSP unit tests for both requests; end-to-end stdio.)

### Phase 4 — Precise diagnostics & multi-error — ✅ done
- **Multiple errors at once** (core recovery): the parser gained a recovering
  variant (`parse_recovering`: record the error, skip to the next line, continue)
  and the assembler a collect mode (`hard_error` records without aborting; a
  defensive `if_depth` guard makes continuing panic-safe). A new
  `diagnose_source_as` orchestrates preprocess → recovering parse → collect-mode
  assemble and returns every deduplicated error/warning plus symbols. The parity
  `assemble`/`assemble_file` path is **untouched** (still first-error), so the
  error-corpus tests and parity (122/122) are unchanged.
- **Token-accurate ranges** (LSP): instead of threading byte-spans through the
  whole assembler (high parity risk for little extra benefit), the LSP narrows
  each diagnostic's range to the backtick-quoted subject of its message located
  on the reported line (reusing the source text; the messages already quote the
  offending symbol/opcode), falling back to the line's trimmed content span. This
  achieves the visible outcome — exact squiggles on the offending token — with no
  risk to ROM output.
- **Done when:** diagnostics highlight exact ranges; multiple errors surface
  together; parity + all existing tests green. ✅ (core recovery/no-panic tests;
  LSP multi-error + range-narrowing tests; end-to-end stdio.)

> **Note on approach.** The plan originally envisioned threading spans through the
> parser and assembler. In practice the two visible outcomes (exact ranges +
> multi-error) are delivered with far less parity risk by *recovery in the core*
> plus *range narrowing in the LSP* (reusing Phase-3's tooling lexer/text). Full
> span threading remains a possible future refinement.

### Phase 5 — Navigation, symbols & hover — ✅ done
- Track symbol **definition** (and reference) positions; implement
  `documentSymbol` (outline), `definition`, `references`, and `hover`
  (symbol value/kind; opcode/addressing details; directive descriptions). ✅
  Positions come from a positioned pass over the lossless tooling lexer
  (`located_lexemes`), keeping the parity path untouched.
- **Done when:** outline lists labels/constants/macros; go-to-definition jumps to
  a label; hover shows opcode and symbol info. ✅ (documentSymbol/definition/
  references/hover unit tests + a hover round-trip in the lifecycle protocol
  test; the `editor.md` docs page documents setup.)

### Phase 6 — File-content overlay (core seam) — ✅ done

Foundational for Phase 7; no LSP behavior change on its own.

- **Problem it unblocks.** `preprocess::do_include` read each `.include`d file
  straight from disk (`std::fs::read_to_string`). To analyze the *project* while
  honoring the editor's unsaved edits, the preprocessor must be able to read an
  open buffer's current text instead of the on-disk copy.
- Added an **opt-in file-content provider**: `pub type FileOverlay =
  dyn Fn(&Path) -> Option<String>` — consulted before disk in `do_include`; on
  `None` it falls back to `read_to_string` exactly as today (a closure, so the
  caller owns path-matching/normalization). ✅ Threaded via
  `preprocess::preprocess_with` and the public
  `diagnose_source_with_overlay(path, source, options, overlay)`; the existing
  `preprocess` / `diagnose_source_as` delegate with `None`.
- The `assemble` / `assemble_file` (CLI) path is **untouched** — it never builds
  an overlay — so it stays byte-for-byte identical.
- **Done when:** an overlay entry substitutes buffer text for an included file
  during preprocessing (even one absent from disk), and takes precedence over
  disk; the default (no-overlay) path is unchanged. ✅ (core unit tests
  `overlay_supplies_an_include_absent_from_disk` and
  `overlay_takes_precedence_over_the_on_disk_file`; **parity 122/122**.)

### Phase 7 — Workspace-aware analysis (project diagnostics) — ✅ done

Fixes cross-file **"symbol `xxx` was not defined"** false positives: nessemble
symbols are global across the whole `.include` graph, but the server analyzed one
buffer in isolation, so a symbol defined in a sibling/parent file looked
undefined. The fix analyzes each open file *in the context of its project*.

- **Entry-point discovery — auto include-graph scan (zero config).** ✅ Captures
  `workspaceFolders` (then legacy `rootUri`/`rootPath`) at `initialize`.
  Enumerates `*.asm` / `*.s` under the workspace (skipping hidden dirs incl.
  `.git`, plus `target/` / `node_modules`; bounded by `MAX_SCAN_FILES`),
  extracts each file's `.include` / `.inestrn` targets (resolved
  **file-relative**, matching the assembler) into an `IncludeGraph`, and takes
  **roots** = files nobody includes. For the open file it assembles the root(s)
  whose closure contains it, over the Phase-6 overlay (built from all open
  buffers) so unsaved edits are reflected.
- **Multi-root handling — intersect undefined sets.** ✅ `intersect_diag_sets`
  keeps only the diagnostics common to *every* root that includes a file, so a
  symbol defined under *any* root is never flagged.
- **Multi-file diagnostics.** ✅ Core gained `Preprocessed.paths` + a
  `diagnose_project` returning the flattened file table (name ↔ resolved path),
  so each diagnostic maps back to a `Url`. Diagnostics are published per open
  document; a `published` set tracks non-empty files so they are explicitly
  **cleared** when fixed.
- **Fallback.** ✅ If the workspace is unknown or the file isn't in any scanned
  root's closure, it falls back to single-file analysis — today's behavior.
- **Config override** (auto + explicit) remains a later addition layered on this.
- **Done when:** opening a fragment no longer flags symbols defined in a
  sibling/parent file; single-file behavior is preserved when no root is found;
  fixing an error clears it. ✅ (four LSP unit tests over temp workspaces +
  an end-to-end stdio check through a real `workspaceFolders` handshake; core
  `overlay`/paths tests; **parity 122/122**.)

### Phase 8 — Advanced (optional / later)
- Folding ranges, rename, code actions (quick-fixes). Scope TBD.

## 6. Editor integration (server + docs)

Deliverable is the **server plus setup documentation**, not a bespoke extension:

- **Neovim / Helix / Emacs (eglot):** point the client at the `nessemble` binary
  with `lsp` as the argument and associate it with `.asm`. Copy-paste snippets in
  the docs.
- **VS Code / Cursor:** these need *some* extension to register a language server;
  since we're not shipping one, document the pragmatic path — configure a generic
  "LSP client" extension to spawn `nessemble lsp` for `.asm`. Semantic-token
  highlighting then arrives over LSP (no TextMate grammar required). A dedicated,
  one-click VS Code/Cursor extension is explicitly **out of scope for now** and
  noted as a possible future `editors/` addition.
- Docs live under `docs/src/` (e.g. an "Editor support" page) and link from the
  README.

## 7. Testing strategy

- **Protocol tests** in `nessemble-lsp`: drive the server with scripted JSON-RPC
  (initialize, didOpen, expect diagnostics/completions/tokens) — no editor needed.
- **Analysis unit tests:** feed buffers to the analysis bridge, assert payloads.
- **Formatter tests:** golden input→output pairs; assert idempotence.
- **Parity guard:** the Phase-3 lexer-column work and the Phase-4 span refactor
  must keep `xtask parity` at 122/122 and all existing tests green (additive, not
  a rewrite).

## 8. Risks & mitigations

- **Highlighting vs. deferred spans.** Semantic tokens need columns; mitigated by
  scoping a *narrow* lexer-column pass in Phase 3, leaving full diagnostic spans
  to Phase 4.
- **Formatter + trivia.** The lexer drops comments/whitespace today; the Phase-3
  lossless lex pass adds trivia + positions. Mitigated by making it an additive
  mode (the existing token stream is unchanged) and guarding on parity + tests.
- **Dependency weight.** Addressed by choosing `lsp-server` (no async runtime)
  and feature-gating the `lsp` feature.
- **In-memory vs. disk includes.** Start disk-resolved; add an open-buffer overlay
  (Phase 6) so project analysis (Phase 7) reflects unsaved edits.
- **Scope creep.** Phase boundaries are the throttle; ship Phase 1 before 3+.

## 9. Decisions

**Made:**

1. **CLI surface** — `nessemble lsp` subcommand over stdio (feature-gated `lsp`).
2. **Transport** — **`lsp-server`** (sync, minimal deps), matching the project's
   ethos; `tower-lsp` only if concurrency demands it.
3. **Editor deliverable** — **server + docs only**; no in-repo extension; a
   dedicated VS Code/Cursor extension is deferred/optional.
4. **Priorities / phase order** — diagnostics, then completion, then
   formatting + highlighting; navigation/hover deferred to Phase 5.
5. **Diagnostic precision** — **line-level first**; precise spans + multi-error
   recovery deferred to Phase 4.
6. **Feature default** — the `lsp` cargo feature is **on by default** (opt out
   with `--no-default-features`).
7. **Shared catalog home** — the `DIRECTIVES` catalog moves into `nessemble-isa`,
   next to the opcode table.
8. **Formatter approach (Phase 3)** — a **comment-preserving ("lossless") lex
   pass**: the lexer emits whitespace/comment trivia with positions and the
   formatter re-emits from that full token stream. Chosen over a line-based
   normalizer for robustness and because its position-tracking foundation is
   reused by highlighting and the Phase-4 span refactor.
9. **Entry-point discovery (Phase 7)** — **auto include-graph scan, zero
   config**: derive entry roots from the workspace's `.include` graph rather than
   a project file. Structured so an explicit-config override can be layered on
   later.
10. **Multi-root reporting (Phase 7)** — **intersect the undefined-symbol sets**
    across every root that includes a fragment, so a symbol defined under any
    root is never flagged.
11. **Overlay is opt-in (Phase 6)** — the file-content provider defaults to disk;
    only the LSP passes an overlay, keeping the CLI/assembler path (and ROM
    parity) byte-for-byte unchanged.

All planning decisions are settled; remaining choices are implementation details
within each phase.
