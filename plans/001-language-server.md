# nessemble-rs: A Plan for a Language Server

> Status: **Draft — key decisions made, ready to refine.** Direction is settled
> (see [§9 Decisions](#9-decisions)); a few sub-choices remain inside the
> formatting phase. No implementation until explicitly greenlit.

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
  document's directory (from disk for now; an open-buffer overlay can come later).

## 4. Proposed architecture

- **New crate `nessemble-lsp`** in the workspace, depending on `nessemble-core`
  and `nessemble-isa`. It owns the protocol loop, the in-memory document store,
  and the mapping from core analysis to LSP types. A library crate is
  unit-testable without spawning a process.
- **CLI entry point:** `nessemble lsp` (a subcommand, mirroring `init`/`scripts`),
  speaking LSP over **stdio**. `nessemble-cli` depends on `nessemble-lsp`,
  **feature-gated** (`lsp`) like `scripting`, so `--no-default-features` stays
  lean and the heavier LSP deps are opt-out.
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
- **Shared metadata:** promote the `DIRECTIVES` catalog into a shared location
  (e.g. `nessemble-isa` or a small `nessemble-lang` module) so the CLI
  `reference` command and the LSP consume one source of truth.

## 5. Phased plan

Ordered by your priorities: **diagnostics → completion → formatting/highlighting**,
with precise spans and navigation deferred. Each phase is independently
shippable, and each shipped phase is a **minor version bump**.

### Phase 0 — Scaffold & transport
- New `nessemble-lsp` crate; `nessemble lsp` subcommand (stdio, `lsp-server`);
  LSP lifecycle (`initialize` → advertise capabilities → `initialized` →
  `shutdown`/`exit`); `textDocument` open/change/close into a document store. No
  analysis yet.
- **Done when:** an LSP client connects, completes the handshake, and the server
  tracks open documents (verified by a protocol-level test).

### Phase 1 — Diagnostics (line-level) — *priority*
- On open/change (debounced), assemble the in-memory buffer (base dir = the
  document's directory; includes/media from disk). Map the error + `warnings` to
  `publishDiagnostics` with **whole-line ranges**.
- **Done when:** a syntax/opcode error shows a squiggle on the right line and
  clears when fixed; warnings appear as warnings.

### Phase 2 — Completion — *priority*
- `textDocument/completion` for: mnemonics (from `nessemble-isa`), directives
  (shared catalog), in-scope labels/constants (names from the symbol table), and
  macro names; snippet completions for common instruction/directive forms.
  Optional: label vs. mnemonic context-awareness based on the current line.
- **Done when:** typing offers relevant mnemonics/directives/labels with docs
  (opcode modes/cycles, directive descriptions) in the completion detail.

### Phase 3 — Formatting & highlighting — *priority*
- **Formatting** (`textDocument/formatting`, optionally `rangeFormatting`): a
  normalizer for nessemble asm — consistent indentation, operand spacing,
  comment alignment, and case. Sub-decision (see §9): the current lexer discards
  comments/whitespace, so formatting needs either a comment-preserving
  ("lossless") lex pass or a lighter line-based normalizer; we'll pick during
  this phase.
- **Highlighting** (`textDocument/semanticTokens`): classify each token
  (mnemonic, register, number, string, label, directive, comment, …). Requires
  **per-token columns**, so this phase includes a focused lexer enhancement to
  record each token's start column and length. This is LSP-native, so it works in
  any semantic-tokens-capable client with no editor grammar to ship.
- **Done when:** "Format Document" tidies a file deterministically (idempotent),
  and tokens are colorized via semantic tokens in VS Code/Cursor.

### Phase 4 — Precise spans (deferred core refactor)
- Extend the Phase-3 column work into full start/end **spans** through parser and
  assembler; upgrade diagnostics from line-level to token-accurate ranges and add
  parse-level **error recovery** so multiple problems report at once. Must keep
  ROM output identical — `xtask parity` stays 122/122.
- **Done when:** diagnostics highlight exact ranges; multiple errors surface
  together; parity + all existing tests green.

### Phase 5 — Navigation, symbols & hover (deferred)
- Track symbol **definition** (and ideally reference) positions; implement
  `documentSymbol` (outline), `definition`, `references`, and `hover`
  (symbol value/kind; opcode/addressing details; directive descriptions).
- **Done when:** outline lists labels/constants/macros; go-to-definition jumps to
  a label; hover shows opcode and symbol info.

### Phase 6 — Advanced (optional / later)
- Folding ranges, rename, code actions (quick-fixes), open-buffer include overlay.
  Scope TBD after Phase 5.

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
- **Formatter + trivia.** The lexer drops comments/whitespace; mitigated by
  choosing the lossless-lex vs. line-based approach up front (§9).
- **Dependency weight.** Addressed by choosing `lsp-server` (no async runtime)
  and feature-gating the `lsp` feature.
- **In-memory vs. disk includes.** Start disk-resolved; add an open-buffer overlay
  only if needed (Phase 6).
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

**Still open (small, decide when we reach them):**

- **A. Formatter approach** — comment-preserving ("lossless") lex pass vs. a
  line-based normalizer (Phase 3).
- **B. Feature default** — is the `lsp` cargo feature **on or off by default**?
  (Affects the default binary size / dependency set.)
- **C. Shared catalog home** — put `DIRECTIVES` in `nessemble-isa` vs. a new
  `nessemble-lang` module.
