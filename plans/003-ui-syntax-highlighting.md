# nessemble-rs: A Plan for Syntax Highlighting in the Web Component

> Status: **Proposed — not started.** Approach decided (Option B below): reuse the
> assembler's own lexer, exposed to the browser via the existing `nessemble-wasm`
> bundle, and render a highlight overlay behind the `<nessemble-assembler>`
> editor **and** the docs' static code blocks (both reuse the one lexer). No
> language server runs in the browser. Surfaces were confirmed per request to
> include static blocks; the remaining scoping choices in §6 (granularity, theming,
> version) are **default answers open to review** — redirect any before Phase 0.

---

## 1. Goal

Give nessemble assembly **syntax highlighting** on the web surfaces — live in the
in-browser `<nessemble-assembler>` editor as the user types, and in the docs'
static code blocks — colored by the *same* tokenizer the assembler and the
Language Server already use, so highlighting can never drift from a second,
parallel grammar.

## 2. Approach — why the lexer, not the LSP

Three options were weighed (see the discussion that produced this plan):

- **A — run the LSP in the browser.** The classifier the LSP uses for
  `textDocument/semanticTokens/full` is valuable, but the *server* is the wrong
  delivery vehicle: `nessemble-lsp` runs on `lsp-server` over **stdio + threads**,
  neither of which exists on `wasm32-unknown-unknown`. Standing up a wasm LSP plus
  a JSON-RPC client/transport shim to drive an **async round-trip** — for what is a
  synchronous lex — is disproportionate. Semantic tokens are also designed to
  *augment* a base grammar inside a full editor, not to be the sole highlighter of
  a small code box. **Rejected.**
- **C — a static grammar (TextMate/Prism/highlight.js) vs. build-time
  pre-rendering.** Two very different things:
  - A hand-written **regex grammar** is a *second source of truth* that drifts from
    the real lexer and handles nessemble's quirks poorly (mnemonic-vs-label, the
    `<` zero-page prefix, `[ ]` indirect addressing, `.dw`/directive families).
    **Rejected** for our surfaces; it remains the only option for GitHub Linguist,
    which stays a non-goal (§8).
  - **Build-time pre-rendering of the docs' static code fences** — running the same
    `tooling::highlight` in an **mdBook preprocessor** — is *not* a second grammar:
    it bakes the real lexer's output into HTML at build time, so it can't drift.
    **In scope** (added per request; see §4.6 and Phase 3). Only works for *fixed*
    code, which is exactly what a docs code fence is.
- **B — expose the lexer to wasm (this plan).** The highlighting logic is already
  a **pure function over text**; the browser build already links `nessemble-core`.
  Add one small wasm export and an overlay renderer. Reuses existing
  infrastructure, single source of truth. **Chosen.**

## 3. Current state (grounding)

- **The lexer is public and pure.** `nessemble_core::tooling::lex(source: &str) ->
  Vec<Lexeme>` (`crates/nessemble-core/src/tooling.rs`) returns `Lexeme { start,
  end, kind }` with `LexKind` = `Directive | Ident | Number | String | Char |
  Comment | Punct | Whitespace | Newline`. Byte offsets into the source.
- **The classifier already exists — in the wrong crate.** `nessemble-lsp`'s
  `semantic_tokens(text)` and `token_type(kind, piece, &mnemonics)`
  (`crates/nessemble-lsp/src/lib.rs`) map each lexeme to one of **7** classes
  (`TOKEN_TYPES`): directive→keyword, mnemonic→function, other ident→variable,
  number, string/char→string, comment, punct→operator. Mnemonic detection uses
  `nessemble_isa::OPCODES`. This function takes only `&str` — **no server state** —
  so it is trivially reusable. It also does the UTF-16 column bookkeeping
  (`utf16_len`) that a browser consumer needs.
- **Core already re-exports the ISA.** `nessemble-core` depends on `nessemble-isa`
  and does `pub use nessemble_isa as isa`, so `OPCODES`/`DIRECTIVES` are reachable
  from core — the classifier can live there with no new dependency.
- **The wasm crate already links core.** `nessemble-wasm`
  (`crates/nessemble-wasm/src/lib.rs`) depends on `nessemble-core` and returns data
  to JS via `#[wasm_bindgen]` getters (`AssembleResult { rom, errors, warnings, ok
  }`). A `tokenize` export mirrors `assemble`; **no new build step** — it rides the
  existing `xtask wasm` bundle.
- **The editor is a plain textarea.** The Web Component (`web/nessemble-assembler.js`)
  builds a `<textarea class="na-editor">` and calls `mod.assemble(...)`; styling is
  in `web/nessemble-assembler.css`. Both are staged into the docs/site by
  `xtask dist`. There is **no** highlighting today (mdBook uses its stock
  highlight.js, which doesn't know the dialect).

## 4. Proposed architecture

### 4.1 Hoist the classifier into `nessemble-core` (single source of truth)

- Add `nessemble_core::tooling::highlight(source: &str) -> Vec<HlToken>` where
  `HlToken { start: u32, len: u32, class: TokenClass }` and
  `pub enum TokenClass { Directive, Instruction, Identifier, Number, String,
  Comment, Operator }` (the LSP's 7 classes, named for humans rather than LSP token
  types). Offsets/lengths in **UTF-16 code units** so a JS consumer's
  `string.slice` lines up (reuse the LSP's `utf16_len` logic); whitespace/newlines
  are consumed for positioning but not emitted.
- **Refactor `nessemble-lsp` to consume it.** `semantic_tokens` becomes: call
  `tooling::highlight`, then map `TokenClass` → the LSP legend index (`TT_*`) and
  produce the delta-encoded `SemanticToken`s. The LSP keeps its LSP-specific
  encoding; the *classification* is shared. No change to the LSP's advertised
  capabilities or output.

### 4.2 `tokenize` wasm export

- `#[wasm_bindgen] pub fn tokenize(source: &str) -> Vec<u32>` returning a **flat,
  triple-packed** `[start, len, class, start, len, class, …]` (→ `Uint32Array` in
  JS), `class` being the `TokenClass` discriminant. Flat typed array = cheapest
  boundary crossing and trivial to iterate.
- Document the class legend (0=directive … 6=operator) in the crate docs so JS maps
  ids → CSS classes without guessing. (A tiny exported `token_classes() ->
  Vec<String>` legend is optional; a comment + constants in the component suffice.)
- Panic-safe like `assemble` (the module's existing `start()` panic hook covers
  it); malformed input just yields best-effort tokens, never a throw.

### 4.3 The highlight overlay (Web Component)

Keep the existing `<textarea>` — it owns input, undo, IME, and nessemble's
**column semantics** (col 0 = label, indented = instruction) — and paint colors on
a layer *behind* it (the CodeJar / "highlight-within-textarea" technique):

- Wrap the editor in a positioned container with a sibling
  `<pre class="na-highlight" aria-hidden="true">` **exactly** matching the
  textarea's box model: font, size, line-height, letter-spacing, tab-size, padding,
  `white-space: pre-wrap`, wrapping, and scroll region.
- Make the textarea's **text transparent** (`color: transparent`) with a visible
  `caret-color`; the `<pre>` underneath shows the colored tokens.
- On `input`: `tokenize(value)` → build the `<pre>` content as colored `<span>`s.
  **Escape token text** (set each span's `textContent`, or HTML-escape) so source
  can never inject markup. Preserve a trailing newline so the last line renders.
- On `scroll`: sync `pre.scrollTop/scrollLeft` to the textarea.
- **Debounce** re-tokenizing with `requestAnimationFrame` for large buffers; wasm
  init is already lazy/shared (one module per page) and `tokenize` is synchronous
  and fast.
- The overlay is purely visual: it never mutates the textarea value or whitespace,
  so assemble behavior, the `nessemble:assembled` event, and column semantics are
  unchanged.

### 4.4 Colors / theming

- Add `.na-tok-directive|instruction|identifier|number|string|comment|operator`
  classes in `web/nessemble-assembler.css`, driven by CSS custom properties
  (`--na-tok-*`).
- **One dedicated, overridable palette** (decision §6) — a single deliberate set
  of token colors used on every surface, with **light and dark variants** selected
  by `prefers-color-scheme` and mdBook's theme class, so the editor looks
  consistent across the docs (themes: light/rust/coal/navy/ayu) and the marketing
  site while staying legible in both modes. Because the colors are CSS variables, a
  surface that wants to match its own theme can still override `--na-tok-*` without
  a code change.

### 4.5 Delivery

- **No new toolchain.** `tokenize` ships in the existing wasm bundle built by
  `xtask wasm`; the component JS/CSS are already staged by `xtask dist` into the
  book and `website/static/`. `pages.yml` is unchanged.
- Highlighting is **progressive enhancement**: if wasm hasn't finished loading, the
  textarea is a normal (uncolored) editor; the overlay activates once `tokenize` is
  available.

### 4.6 Static docs code blocks (mdBook preprocessor)

The docs' **non-interactive** code fences (the `code` examples in `docs/src/*.md`)
get the same highlighting, baked at build time — sharing the classifier and the
CSS with the editor, not a separate grammar.

- **An mdBook preprocessor** that reads the book JSON on stdin, walks each
  chapter's Markdown, and for every fenced code block tagged as nessemble assembly
  (info string TBD — e.g. ` ```asm `; a small decision, §7) runs
  `nessemble_core::tooling::highlight` and replaces the fence with raw HTML:
  `<pre class="na-code"><code>` + HTML-escaped `<span class="na-tok-…">`s. Emitting
  HTML (rather than a fenced block) means mdBook's stock highlight.js leaves it
  alone — no double-highlighting, no per-language JS grammar.
- **Where it lives:** a build-time-only tool that reuses the workspace lexer. Add
  it as an `xtask` subcommand (`xtask mdbook-highlight`) wired into `docs/book.toml`
  as `[preprocessor.nessemble-highlight] command = "cargo run -q -p xtask --
  mdbook-highlight"` (xtask is already the build/dist orchestrator; no shipped
  crate, no drift). A standalone `mdbook-nessemble` binary on `PATH` is the
  alternative if per-invocation `cargo run` latency matters.
- **Shared CSS, zero runtime cost.** The `--na-tok-*` classes from §4.4 are already
  loaded on every docs page (via `docs/theme/head.hbs`, which pulls in the
  component CSS), so the static blocks are themed by the *same* stylesheet as the
  editor. Highlighting is baked into the HTML, so these blocks need **no wasm or JS
  at runtime** and render even with scripting disabled — only the interactive
  `<nessemble-assembler>` examples use the wasm `tokenize`.
- **Marketing site** code snippets, if any, can reuse the same preprocessor output
  or the shared CSS; the homepage's *interactive* demo stays on the wasm path.

## 5. Phased plan

Each phase is independently shippable. The **assemble path is untouched**, so
parity stays **122/122** throughout; the only Rust behavior touched is a refactor
of the LSP's semantic-tokens *source* (guarded by its existing tests), plus a new
build-time mdBook preprocessor (dev tooling, not on the assemble path). The
interactive track (Phases 1–2) and the static-docs track (Phase 3) both depend
only on the shared classifier from Phase 0 and are otherwise independent.

### Phase 0 — Shared classifier in core
- Add `tooling::highlight` + `TokenClass` to `nessemble-core`; unit-test the
  classification (directive, mnemonic vs label, number/string/char, comment,
  operator; UTF-16 offsets on a multi-byte line).
- Refactor `nessemble-lsp::semantic_tokens` to call it and map to `TT_*`.
- **Done when:** `cargo test -p nessemble-core -p nessemble-lsp` green (LSP
  semantic-token output byte-for-byte unchanged vs current, pinned by a test);
  `cargo clippy --workspace` clean; parity **122/122**.

### Phase 1 — `tokenize` wasm export
- Add `tokenize(source) -> Vec<u32>` to `nessemble-wasm`; host unit tests plus a
  Node smoke test over the real `wasm-bindgen` bundle (e.g. `lda #$00 ; c` yields
  instruction/number/comment classes at the right offsets).
- **Done when:** `cargo test -p nessemble-wasm` green and the Node smoke test
  passes; workspace tests + parity unaffected.

### Phase 2 — Overlay renderer in the component
- Implement the transparent-textarea overlay in `web/nessemble-assembler.js` +
  base CSS: tokenize-on-input (rAF-debounced), scroll sync, transparent text /
  visible caret, HTML-escaped spans, trailing-newline handling, and the legacy
  `.nessemble-assembler` div path.
- **Done when:** verified in **headless Chromium** on a standalone page — typing
  updates colors live, the caret and colored text stay aligned while scrolling and
  wrapping, `<` (source) never injects markup, and assemble + the assembled event
  still work.

### Phase 3 — Static docs code blocks (mdBook preprocessor)
- Add the `xtask mdbook-highlight` preprocessor (§4.6): lex nessemble code fences
  with `tooling::highlight`, emit HTML-escaped `na-tok-*` spans, register it in
  `docs/book.toml`. Depends only on Phase 0 (independent of the wasm track).
- **Done when:** `xtask dist` produces docs whose static nessemble code fences are
  highlighted with the shared classes (verified in headless Chromium), stock
  highlight.js no longer touches them, and non-nessemble fences are unchanged.

### Phase 4 — Theming across both surfaces + site
- One overridable `--na-tok-*` light/dark palette (§4.4) applied to the editor
  overlay *and* the static blocks, across mdBook themes and the marketing site.
- **Done when:** a local `xtask dist` produces a `site/` where both the docs'
  editors and the static code blocks are legible in light and dark (headless
  Chromium across at least a light and a dark mdBook theme).

### Phase 5 — Release
- Roll up under the workspace version and cut the release (see §6/§7).
- **Done when:** the version's release ships the updated wasm bundle + component.

## 6. Decisions

Architectural (settled):

1. **Reuse the assembler's lexer** (`tooling::highlight`) as the single source of
   truth — no TextMate/Prism/highlight.js grammar for the editor.
2. **Overlay a transparent `<textarea>`** rather than switch to a
   `contenteditable` editor — keeps native input/undo/IME and nessemble's column
   semantics, and avoids reworking the existing component and its events.
3. **Hoist the classifier down into `nessemble-core`**, shared by the LSP and the
   wasm build, so highlighting stays identical across the CLI's LSP and the
   browser.

Scoping (default answers to the open questions — flagged here so a reviewer can
redirect any of them before Phase 0):

4. **Granularity — the LSP's current 7 classes** (directive, instruction,
   identifier, number, string, comment, operator), exposed to JS as a flat
   `Uint32Array` of `[start, len, class]` triples in UTF-16 units. Kept a superset
   later if a richer look (registers, label-vs-constant, opcode-vs-pseudo) is
   wanted — the LSP would then collapse the extras back to its 7.
5. **Surfaces — the interactive editor *and* the docs' static code blocks**
   (updated per request). The editor uses the wasm `tokenize` at runtime; the
   static blocks use the same `tooling::highlight` in a build-time mdBook
   preprocessor (§4.6) — one classifier, one stylesheet, no grammar. **GitHub
   Linguist** highlighting stays out of scope (§8) — it needs a TextMate grammar.
6. **Theming — one dedicated, overridable palette** with light/dark variants
   (§4.4), not a per-theme match, for a consistent look; `--na-tok-*` variables
   leave per-surface overrides open.
7. **Version — minor feature: `2.8.0-dev` while landing, `2.8.0` to ship**
   (main is `2.7.0`).

## 7. Risks & open constraints

- **Caret/glyph alignment** — the classic overlay pitfall. Mitigate by driving the
  textarea and `<pre>` from *one* shared box-model CSS block and testing wrapping,
  `tab-size`, and scrolling in headless Chromium.
- **Theme legibility** across mdBook's five themes + the site — use overridable CSS
  variables; verify light and dark.
- **HTML injection** — token text must be escaped (set `textContent`, never raw
  string `innerHTML`).
- **Large-buffer performance** — rAF-debounce tokenizing; keep the DOM update a
  single `<pre>` replacement.
- **UTF-16 vs byte offsets** — emit UTF-16 units from `highlight` so JS slicing is
  correct on non-ASCII (reuse the LSP's `utf16_len`).
- **Wasm size** — negligible; `tokenize` reuses the lexer already in the bundle.
- **Preprocessor info string** — decide which fenced-code tag marks nessemble
  assembly (` ```asm `, ` ```6502 `, or a dedicated tag) and confirm existing docs
  fences use it consistently, so the preprocessor highlights the right blocks and
  leaves the rest to stock highlight.js.
- **Versioning** — land the feature under a pre-release `-dev` version (as the LSP
  and wasm work did) so intermediate merges don't cut a release; drop the suffix in
  Phase 5. Main is currently `2.7.0`; a minor feature → `2.8.0-dev` while landing,
  `2.8.0` to ship.

## 8. Non-goals

- Running the Language Server (or any of its non-highlight features — completion,
  hover, diagnostics-as-you-type) in the browser.
- A TextMate/Prism grammar or **GitHub Linguist** highlighting — GitHub only
  accepts a TextMate grammar (a *separate* artifact that would drift from the
  lexer); a later plan could generate one from the same lexer. (The docs' own code
  blocks are in scope, handled by the build-time preprocessor in §4.6 — not a
  grammar.)
- Semantic (scope-aware) highlighting beyond the lexer — e.g. resolving whether an
  identifier is a defined label/constant vs a register. v1 is purely lexical, like
  the LSP's semantic-token pass.
- Editing niceties (autocomplete, bracket matching, auto-indent) in the component.
