# nessemble-rs: A Plan for Evaluating a Lit Conversion of the Web Component

> Status: **Exploration — recommendation only; nothing shipped.** This document
> evaluates rebuilding the hand-spun `<nessemble-assembler>` custom element on
> [Lit](https://lit.dev). It concludes that a full, idiomatic (Shadow DOM) Lit
> conversion **works against two deliberate architectural pillars** — a
> **zero-JS-build runtime** and a **light-DOM component that shares one global
> `na-tok-*` palette with the static docs code blocks** — and is **not
> recommended** at this time. If a conversion is nonetheless pursued, the only
> variant that respects those pillars is **Lit rendering into light DOM, shipped
> as a second vendored bundle** (Option B in [§5](#5-the-three-options)); a
> phased plan for it is sketched in [§7](#7-phased-plan-if-option-b-is-chosen) so
> the work is ready to pick up. Decisions are collected in
> [§9](#9-decisions--open-questions). Workspace version at time of writing:
> **2.15.0**.

---

## 1. Goal

Answer a concrete question: should the `<nessemble-assembler>` Web Component —
today a ~530-line hand-written vanilla custom element (`web/nessemble-assembler.js`)
— be rebuilt on Lit, a 6-KB templating/reactivity library for Web Components?

"Should" here means weighing what Lit *buys* (declarative templates, reactive
properties, scoped styles) against what it *costs* in this specific project,
whose delivery model was chosen precisely to avoid a client-side framework and
a JavaScript build step (see plan 002, §6). This is an evaluation, not an
implementation; no behavior changes ship from this document.

## 2. Current state (grounding)

The component, as built by plans 002 (wasm + component) and 003 (highlighting):

- **A vanilla custom element**, `customElements.define("nessemble-assembler", …)`,
  defined inside an IIFE in `web/nessemble-assembler.js`. **No framework, no
  bundler, no build step for the component itself.**
- **Light DOM.** `connectedCallback` appends the editor wrapper, toolbar, and
  output `<pre>` directly to `this` (`this.append(...)`). There is **no shadow
  root.** This is load-bearing (see below).
- **A shared, external stylesheet**, `web/nessemble-assembler.css`, scoped to
  `.na-*` classes. Crucially, its `--na-fg` / `--na-tok-*` custom properties and
  its `.na-tok-directive` / `.na-tok-instruction` / … rules are consumed by
  **two** surfaces: the live editor's CodeMirror decoration spans *and* the
  **static docs code blocks** (`.na-code`) that `xtask dist` emits from
  ` ```nessemble ` fences (`highlight_markdown` → `highlight_code` in
  `xtask/src/main.rs`). One palette, two renderers — that's the whole point of
  plan 003 §4.4.
- **A vendored, pre-built CodeMirror 6 ESM bundle** (`web/vendor/codemirror.js`),
  produced out-of-band by esbuild from `web/build/codemirror.entry.mjs` and
  **committed** to the repo. The README there is explicit: *"The runtime and
  `xtask dist` never run a JavaScript toolchain — they just copy the file."*
- **Loaded as a classic script.** Both surfaces include it as
  `<script src="…/nessemble-assembler.js" defer>` (`docs/theme/head.hbs`) /
  `<script src="static/nessemble/nessemble-assembler.js">`
  (`website/index.html`) — **not** `type="module"`. `ASSET_BASE` is derived from
  `document.currentScript.src`, then the wasm glue and the CodeMirror bundle are
  pulled in with dynamic `import()`.
- **The real work is not UI plumbing.** Of the ~530 lines, the reactive-UI
  surface is small; the bulk is the wasm loader, `hexdump`, the Feather SVG icon
  strings, `tokenize`-driven CodeMirror decorations (`computeDecorations`,
  `makeHighlighter`), and the CodeMirror theme (`makeTheme`). None of that is
  templating.
- **Reactive state is tiny.** Effectively: `collapsed`, the output text, an
  error flag, the download object-URL, and the Assemble button's label/disabled
  state. Everything else is derived or one-shot.
- **Distribution.** `xtask` `stage_web_assets` copies the four files
  (`…-assembler.js`, `…-assembler.css`, `nessemble.js`, `nessemble_bg.wasm`) plus
  `vendor/codemirror.js` next to each other, then `cache_bust` appends
  `?v=<version>` to the asset URLs. Adding assets means touching both lists.

## 3. What Lit would buy

Lit's value is real and worth stating fairly:

1. **Declarative `render()`.** The imperative DOM assembly — `el()` helpers,
   `bar.append(...)`, `innerHTML = svgIcon(...)`, and the scattered
   `node.hidden = true/false` show/hide logic — collapses into one
   HTML-template `render()` driven by reactive properties. The toolbar + output
   region is the part that would read noticeably better.
2. **Reactive properties.** `@state()` fields for `collapsed`, `outputText`,
   `error`, `downloadUrl`, `assembling` replace the hand-managed `.hidden` /
   `.textContent` / `.disabled` mutations and the `_updateToggle()` bookkeeping.
3. **Scoped styles** (only if Shadow DOM is used) — encapsulation so the
   `.na-*` prefix discipline is no longer required.
4. **Attribute/property reflection** for `data-opts` / `collapsed`, handled by
   the framework instead of `getAttribute` / `hasAttribute`.
5. **Lifecycle ergonomics** — `firstUpdated()` is a natural home for the
   imperative CodeMirror mount.

## 4. What Lit costs here

Each of these is a direct tension with a *chosen* property of the current
design, not an incidental hurdle.

### 4.1 It reintroduces a JavaScript build step (the biggest one)

Lit is an npm ESM package; it is not something you hand-author. To use it you
must either (a) load it from a CDN at runtime — rejected project-wide for these
assets, which are copied locally and cache-busted — or (b) **vendor a bundle**,
exactly like `web/vendor/codemirror.js`. Option (b) is the only project-consistent
answer, but it means the *component itself* now depends on a bundling toolchain,
partially eroding the "hand-written, no build" property that made the component
a plain committed `.js` file. It's a manageable cost (the CodeMirror precedent
exists), but it is a genuine reversal of a deliberate decision.

### 4.2 Light DOM vs Shadow DOM — the palette-sharing problem

This is the subtle one. Lit defaults to **Shadow DOM**. Two consequences:

- **CSS custom properties inherit through the shadow boundary**, so
  `--na-fg` / `--na-tok-*` set on `:root` / `html.coal` / `.na-force-dark`
  would still reach a shadowed editor. *That part is fine.*
- **Bare class selectors do not.** The highlighter adds
  `class="na-tok-directive"` spans, and their colors live in the **external**
  `nessemble-assembler.css`. Inside a shadow root those `.na-tok-*` rules simply
  don't apply. To go Shadow DOM you must **relocate every `na-tok-*` rule into
  the component's own `static styles`** (or a CodeMirror theme that reads the
  custom properties directly) — and now the editor's palette and the **static
  docs `.na-code` blocks'** palette are two copies that must be kept in sync,
  undoing plan 003 §4.4's single-source-of-truth. CodeMirror *does* support
  living in a shadow root (it injects its `StyleModule` into the shadow root
  node), so CM would function — but the shared-stylesheet contract breaks.

The escape hatch is to make Lit render into **light DOM** by overriding
`createRenderRoot() { return this; }`. Then `.na-tok-*` and the whole existing
stylesheet keep working untouched — but you've **given up Lit's scoped styles,
its single most-cited advantage**, and are using Lit purely as a
template/reactivity layer over the existing global CSS.

### 4.3 Classic script → module

Lit is delivered as ES modules, so the component becomes `type="module"`. That
requires:

- Changing `docs/theme/head.hbs` and `website/index.html` from
  `<script src … defer>` to `<script type="module" src …>`.
- Replacing `document.currentScript.src` (which is **`null` in a module**) with
  `import.meta.url` for `ASSET_BASE`. Minor, but it's a real edit and a
  re-verify of asset resolution at every page depth.
- Confirming the legacy-`<div class="nessemble-assembler">` upgrade path and the
  `nessemble:assembled` event still fire with module timing.

### 4.4 Lit doesn't touch the actual complexity

The wasm loader, `hexdump`, the icon SVGs, `tokenize`-driven decorations, and
the CodeMirror mount are all **imperative regardless of framework**. Lit
improves the ~30% of the file that builds the toolbar/output; the ~70% that is
the genuinely hard integration is unchanged (or, for the CM mount, moves from
`_mountEditor()` to `firstUpdated()` with no simplification). The
effort-to-benefit ratio is therefore lower than a line count alone suggests.

### 4.5 Small additional weight and a dependency to track

Lit is ~6 KB min+gzip — negligible next to the wasm binary and the CodeMirror
bundle — but it is one more vendored artifact to rebuild/track alongside
`package.json`/`package-lock.json`, and one more version to bump.

## 5. The three options

| | A. Stay vanilla | B. Lit, light DOM | C. Lit, shadow DOM |
|---|---|---|---|
| JS build step for component | none | vendored Lit bundle | vendored Lit bundle |
| `na-tok-*` shared with docs `.na-code` | ✅ unchanged | ✅ unchanged | ❌ must duplicate into component |
| Scoped styles (Lit's main draw) | n/a | ❌ forgone | ✅ gained |
| Script tag | classic `defer` | `type="module"` | `type="module"` |
| Declarative `render()` / reactive state | ❌ | ✅ | ✅ |
| CodeMirror integration effort | (exists) | same, moved to `firstUpdated` | same + re-theme into shadow root |
| Churn to `xtask` / HTML / CSS | none | moderate | high |
| Net verdict | status quo | **only conversion that respects the pillars** | most idiomatic Lit, worst fit here |

**Option B** is the only conversion that keeps the single shared palette *and*
delivers Lit's templating win. It buys declarative rendering at the price of a
vendored Lit bundle and a module-script switch, while explicitly forgoing the
scoped-styles feature (because that feature is what breaks palette sharing).
**Option C** is what "convert to Lit" usually means, and it is the worst fit:
it duplicates the token palette and re-plumbs CodeMirror theming for the
smallest incremental gain.

## 6. Recommendation

**Do not convert now (Option A).** The component is stable, framework-free by
design, and its real complexity is orthogonal to what Lit improves. A Lit
rewrite would trade a deliberate zero-build, light-DOM, single-palette design
for a modest readability gain confined to the toolbar/output markup, while
reintroducing a build dependency the project consciously avoided.

**If** a maintainer still wants the declarative-template ergonomics — e.g. the
UI is about to grow (run/disassemble modes, a settings panel, tabs) and the
imperative `el()`/`.hidden` plumbing is becoming the bottleneck — then pursue
**Option B only**, never Option C, so the shared `na-tok-*` palette survives.
The trigger to revisit is *UI growth*, not code aesthetics: at today's surface
area the vanilla element is the right tool.

## 7. Phased plan (if Option B is chosen)

Kept small and reversible so it can be abandoned cheaply.

### Phase 0 — Vendor Lit, mirroring the CodeMirror precedent
- Add `lit` to `web/build/package.json`; add a `lit.entry.mjs` re-exporting
  `LitElement`, `html`, `css`/`nothing` as needed; extend `build-vendor.mjs`
  (or add a sibling) to emit `web/vendor/lit.js`. Commit the bundle.
- Update `web/build/README.md` to document the second bundle.

### Phase 1 — Reimplement the element on `LitElement`, light DOM
- `class NessembleAssembler extends LitElement` with
  `createRenderRoot() { return this; }` (light DOM → existing stylesheet and
  `.na-tok-*` keep working with **zero CSS changes**).
- `@state()` for `collapsed`, `outputText`, `error`, `downloadUrl`,
  `assembling`; `render()` builds the toolbar + `<pre class="na-output">`
  declaratively (icons via `html`+`unsafeSVG` or static template parts).
- Keep `hexdump`, the wasm loader, `computeDecorations`, `makeHighlighter`,
  `makeTheme`, and the `nessemble:assembled` dispatch **verbatim** — they are
  framework-agnostic.
- Move the CodeMirror mount into `firstUpdated()`; keep the plain-`<pre>`
  placeholder-before-bundle behavior.
- Preserve the legacy `<div class="nessemble-assembler">` → element upgrade and
  the `data-opts` / `collapsed` attribute reads.

### Phase 2 — Delivery wiring
- Switch `docs/theme/head.hbs` and `website/index.html` script tags to
  `type="module"`; swap `document.currentScript.src` → `import.meta.url`.
- Add `web/vendor/lit.js` to `stage_web_assets` in `xtask/src/main.rs` (and, if
  its URL should be cache-busted, to the `cache_bust` asset list).

### Phase 3 — Verify (headless Chromium, per plan 002/003 practice)
- Editor mounts, highlights (colors match `.na-code`), Assemble → hexdump +
  warnings + download, Reset/Clear/Format, output toggle, `collapsed`, the
  legacy `<div>` upgrade, and `nessemble:assembled` all work on **both** the
  docs and the marketing site, at multiple page depths, in light and dark
  themes.

### Phase 4 — Docs / changeset / release
- Note the dependency in `web/build/README.md`; changeset; release per
  `RELEASING.md`.

## 8. Risks & constraints (Option B)

- **Module timing.** `type="module"` scripts are deferred and async; re-verify
  the legacy-upgrade `DOMContentLoaded`/immediate branch and first-paint
  placeholder still behave.
- **Asset resolution.** `import.meta.url` must resolve correctly at every page
  depth the docs produce; this is the one behavior most likely to regress.
- **Two vendored bundles** (CodeMirror + Lit) to rebuild and keep pinned.
- **No scoped-styles payoff.** Accept up front that Option B uses Lit as
  templating only; if scoped styles are the actual goal, the palette-duplication
  cost of Option C must be paid instead — reopen the decision then.

## 9. Decisions & open questions

- **D1 — Recommend Option A (no conversion) at current scope.** The gain is
  confined to toolbar/output markup; the cost touches three deliberate design
  pillars.
- **D2 — If converting, Option B (light-DOM Lit), never Option C.** Preserving
  the single `na-tok-*` palette shared with the static docs blocks outranks
  gaining Shadow DOM style encapsulation.
- **D3 — Vendor Lit like CodeMirror**, not a CDN, to honor the "copy files, run
  no JS toolchain at dist/runtime" contract.
- **Open — the revisit trigger is UI growth.** Run/disassemble modes, a
  settings panel, or tabs would raise Option B's benefit enough to reconsider;
  pure refactoring appetite is not sufficient.

## 10. Non-goals

- Rewriting the CodeMirror integration, the wasm loader, `hexdump`, or the
  tokenizer-driven highlighting — all framework-independent and out of scope.
- Adopting a heavier framework (React/Vue/Svelte) or any client-side router.
- Changing the `.na-code` static-docs highlighting pipeline in `xtask`.
- Introducing a runtime CDN dependency for component assets.
