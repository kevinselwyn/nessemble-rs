# nessemble-rs: A Plan for Syntax Highlighting in the Web Component

> Status: **Phases 0–4 done + live-site bug fixes; Phase 5 (release) pending.**
> Approach (Option B below): reuse the assembler's own lexer, exposed to the
> browser via the existing `nessemble-wasm` bundle, and render a highlight overlay
> behind the `<nessemble-assembler>` editor **and** the docs' static code blocks
> (both reuse the one lexer). No
> language server runs in the browser. All scoping choices in §6 are **settled**:
> 7 lexical classes, both surfaces (static blocks opt in via a ` ```nessemble `
> fence tag + re-tag sweep), one shared light/dark palette, shipped as a minor
> release `2.8.0`. **Phases 0–4 are done** — shared classifier in core; the
> `tokenize` wasm export; the editor overlay; static docs-block highlighting +
> re-tag sweep; theming across surfaces — plus **live-site bug fixes** (left-align,
> highlight-on-load, asset cache-busting) landing ahead of the release. The
> workspace stays on `2.8.0-dev`; **Phase 5 (drop the suffix → `2.8.0`) is the
> only remaining step.**

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

- Add two things to `nessemble_core::tooling`:
  - `classify(kind, piece) -> TokenClass` — the shared **per-lexeme
    classification** (mnemonic-aware via the ISA opcode set), where
    `pub enum TokenClass { Directive, Instruction, Identifier, Number, String,
    Comment, Operator }` (the LSP's 7 classes, named for humans, not LSP token
    types). This is the anti-drift core: the *decision of what a token is*.
  - `highlight(source) -> Vec<HlToken>` with `HlToken { start: u32, len: u32,
    class: TokenClass }` — the **flat-offset convenience** the wasm/editor
    highlighter consumes; offsets in **UTF-16 code units** so a JS consumer's
    `string.slice` lines up. Whitespace/newlines are dropped.
- **Refactor `nessemble-lsp` to share `classify`.** `semantic_tokens` keeps its
  own line/column delta walk (it needs LSP `(deltaLine, deltaChar)`, not flat
  offsets) but sources each token's type from `tooling::classify`, mapping
  `TokenClass` → the legend index `TT_*`. So the LSP and the browser classify
  identically while each keeps its own geometry. No change to the LSP's advertised
  capabilities or output — pinned by its existing semantic-token test.

### 4.2 `tokenize` wasm export

- `#[wasm_bindgen] pub fn tokenize(source: &str) -> Vec<u32>` returning a **flat,
  triple-packed** `[start, len, class, start, len, class, …]` (→ `Uint32Array` in
  JS). Flat typed array = cheapest boundary crossing and trivial to iterate. `class`
  is an explicit id (not the enum discriminant) so the wire format is stable.
- A self-describing legend is exported as `token_classes() -> Vec<String>`
  (`["directive", "instruction", … , "operator"]`, indexed by class id), so JS
  turns an id into a CSS class (`na-tok-<name>`) without hard-coding the mapping.
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

### 4.6 Static docs code blocks (build-time transform)

The docs' **non-interactive** code fences (the `code` examples in `docs/src/*.md`)
get the same highlighting, baked at build time — sharing the classifier and the
CSS with the editor, not a separate grammar.

> **Implemented (Phase 3):** as a `dist`-time markdown transform rather than a
> registered mdBook preprocessor — `xtask dist` builds the book from a *copy* and
> `highlight_fences` rewrites each ` ```nessemble ` fence via the shared
> `tooling::lex`/`classify` before `mdbook build`. This avoids a JSON-preprocessor
> dependency in the std-lean xtask and keeps the committed sources clean, while
> the site (always built via `dist`) still gets fully-highlighted static blocks.
> The bullets below describe the original preprocessor sketch; the emitted HTML
> (`<pre class="na-code">` with `na-tok-*` spans, no `<code>`) is the same.

- **An mdBook preprocessor** that reads the book JSON on stdin, walks each
  chapter's Markdown, and for every code fence tagged **` ```nessemble `** runs
  `nessemble_core::tooling::highlight` and replaces the fence with raw HTML:
  `<pre class="na-code"><code>` + HTML-escaped `<span class="na-tok-…">`s. Emitting
  HTML (rather than a fenced block) means mdBook's stock highlight.js leaves it
  alone — no double-highlighting, no per-language JS grammar.
- **Opt-in re-tag sweep.** The docs currently fence assembly as ` ```text ` (~135
  fences, mixed with non-asm like directory trees, tables, and command output), so
  the dedicated ` ```nessemble ` tag is an explicit opt-in: a one-time docs pass
  reclassifies the genuinely-assembly ` ```text ` blocks to ` ```nessemble `. Only
  re-tagged blocks are highlighted; everything else is untouched. (This also lets
  the preprocessor be strict — an unlexable ` ```nessemble ` block is an authoring
  error worth surfacing, rather than a silent guess.)
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

### Phase 0 — Shared classifier in core — ✅ done
- Added `tooling::classify` + `TokenClass` and `tooling::highlight` + `HlToken`
  (UTF-16 offsets) to `nessemble-core`, with unit tests for the classification
  (directive, mnemonic vs label incl. case, number/string/char, comment, operator)
  and for `highlight` (significant-tokens-only, and UTF-16 offsets on a multi-byte
  line). ✅
- Refactored `nessemble-lsp::semantic_tokens` to source classification from
  `tooling::classify` (dropping its local mnemonic set + `token_type`), keeping its
  delta encoding. ✅
- **Done:** `cargo test -p nessemble-core -p nessemble-lsp` green (the LSP's
  existing semantic-token test pins output unchanged); `cargo clippy` clean; parity
  **122/122**. The workspace version moved to the pre-release `2.8.0-dev` so this
  and later phases can land without cutting a release.

### Phase 1 — `tokenize` wasm export — ✅ done
- Added `tokenize(source) -> Vec<u32>` (flat `[start, len, class]` triples, UTF-16
  offsets) and a self-describing `token_classes() -> Vec<String>` legend to
  `nessemble-wasm`, over `tooling::highlight`. ✅
- **Done:** `cargo test -p nessemble-wasm` green (host unit tests: triple packing,
  UTF-16 offsets, legend alignment); clippy/fmt clean; a Node smoke test over the
  real `wasm-bindgen` bundle confirms `tokenize` returns a `Uint32Array`
  (`lda #$00 ; c` → `[0,3,1, 4,1,6, 5,3,3, 9,3,5]`), the legend maps ids → names,
  and `assemble` still works. Parity **122/122** unaffected.

### Phase 2 — Overlay renderer in the component — ✅ done
- Implemented the transparent-textarea overlay in `web/nessemble-assembler.js` +
  CSS: a `<pre class="na-highlight">` backdrop behind the (transparent-text)
  textarea, `tokenize`-on-input (rAF-debounced), scroll sync, `--na-fg` caret,
  HTML-escaped spans, zero-width-space trailing-newline handling, a base
  `--na-tok-*` light/dark palette, and lazy wasm load on first focus (so untouched
  editors don't fetch the module; the legacy `.nessemble-assembler` div path is
  unchanged). ✅
- **Done:** verified in **headless Chromium** (playwright-core + the real wasm
  bundle) — mnemonics/comment/label colored correctly, typing updates colors live,
  a literal `<` / `<script>` is escaped and never injects an element, the backdrop
  scroll stays locked to the textarea, and assemble + the `nessemble:assembled`
  event still work. Column semantics preserved (the textarea still owns the text).

### Phase 3 — Static docs code blocks — ✅ done
- **Implemented as a `dist`-time markdown transform**, not a registered mdBook
  preprocessor: `xtask dist` builds the book from a *copy* and `highlight_fences`
  rewrites each ` ```nessemble ` fence into `<pre class="na-code">` with
  `na-tok-*` spans, via the shared `tooling::lex`/`classify` (xtask's only crate
  dep is `nessemble-core`). Chosen over the mdBook-preprocessor/JSON route because
  the site is always built via `dist`; it keeps xtask lean and the committed
  sources clean (the transform runs on `target/docs-build`). No `<code>` element,
  so mdBook's highlight.js/copy-button leave the block alone. ✅
- **Re-tag sweep:** reclassified the genuinely-assembly ` ```text ` fences in
  `docs/src/*.md` to ` ```nessemble ` (98 blocks), leaving hex-dump output, trees,
  bit-layout diagrams, the `pseudo.txt` mapping, and other languages as-is. ✅
- **Done:** `xtask dist` produces docs whose ` ```nessemble ` fences are
  highlighted with the shared `na-tok-*` classes (verified in headless Chromium),
  highlight.js doesn't touch them, normal fences still render, and the committed
  `docs/src` gains only the re-tag edits.

### Phase 4 — Theming across both surfaces + site — ✅ done
- Palette moved to `:root` (so `.na-host` **and** `.na-code` inherit it), with a
  light default, a `prefers-color-scheme` dark, and — the fix for the OS-vs-page
  mismatch — explicit **mdBook theme overrides** (`html.light`/`.rust` → light,
  `html.coal`/`.navy`/`.ayu` → dark) that win over the media query, plus a
  `.na-force-dark` opt-in the (dark) marketing site sets on `<body>`. ✅
- **Done:** a local `xtask dist` site is legible in light and dark for **both**
  the editor and the static blocks; verified in headless Chromium that switching
  the mdBook theme (light ↔ coal) re-colors the shared `na-tok-*` classes
  (instruction `rgb(130,80,223)` ↔ `rgb(210,168,255)`).

### Live-site bug fixes (landed ahead of the release) — ✅ done
Three bugs spotted on the deployed site, fixed as a **bugfix** change (no version
bump); merging redeploys the site via `pages.yml` without cutting a release:
- **Left-align** the component (`text-align: left` on `.na-host`/`.na-editor`/
  `.na-highlight`/`.na-code`), so a centered host page (the marketing hero) no
  longer centers the source.
- **Highlight on page load:** the editor now loads the wasm highlighter eagerly on
  connect (shared module, one fetch per page) instead of lazily on first focus, so
  colors appear immediately — not only after an interaction.
- **Cache-busting:** `xtask dist` appends `?v=<version>` to the component/wasm
  asset URLs (CSS/JS/glue/wasm), so the CDN-fronted deploy stops serving stale
  `nessemble-assembler.css`/`.js` — the root cause of the static blocks and editor
  appearing unstyled on the live site despite a successful deploy.
- **Done:** re-verified in headless Chromium — editor highlights on load, source is
  left-aligned, static blocks are colored (light + dark), and assets carry
  `?v=<version>`.

### Phase 5 — Release — pending
- Drop the pre-release suffix (`2.8.0-dev` → `2.8.0`) and merge to cut the release
  (see §6/§7).
- **Done when:** the `2.8.0` release ships the updated wasm bundle + component.

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

Scoping (confirmed):

4. **Granularity — the LSP's current 7 classes** (directive, instruction,
   identifier, number, string, comment, operator), exposed to JS as a flat
   `Uint32Array` of `[start, len, class]` triples in UTF-16 units. Kept a superset
   later if a richer look (registers, label-vs-constant, opcode-vs-pseudo) is
   wanted — the LSP would then collapse the extras back to its 7.
5. **Surfaces — the interactive editor *and* the docs' static code blocks.** The
   editor uses the wasm `tokenize` at runtime; the static blocks use the same
   `tooling::highlight` in a build-time mdBook preprocessor (§4.6) — one
   classifier, one stylesheet, no grammar. Static blocks opt in via a dedicated
   **` ```nessemble `** fence tag, applied by a one-time re-tag sweep of the docs'
   assembly ` ```text ` fences (§4.6, Phase 3). **GitHub Linguist** highlighting
   stays out of scope (§8) — it needs a TextMate grammar.
6. **Theming — one dedicated, overridable palette** with light/dark variants
   (§4.4), not a per-theme match, for a consistent look; `--na-tok-*` variables
   leave per-surface overrides open.
7. **Version — minor release: `2.8.0-dev` while landing, `2.8.0` to ship**
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
- **Re-tag accuracy** — the ` ```nessemble ` sweep must catch the assembly
  ` ```text ` fences without sweeping in look-alikes (register dumps, memory maps,
  command output). A block that doesn't lex cleanly as assembly should be caught in
  review (the preprocessor can warn), not silently mis-highlighted.
- **Versioning** — land the feature under a pre-release `-dev` version (as the LSP
  and wasm work did) so intermediate merges don't cut a release; drop the suffix in
  Phase 5. Main is currently `2.7.0`; shipped as a minor release → `2.8.0-dev`
  while landing, `2.8.0` to ship.

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
