# nessemble-rs: A Plan for a WASM Build & Assembler Web Component

> Status: **Complete — all phases (0–4) done.** Decisions in
> [§6](#6-decisions) are settled. The `nessemble-wasm` crate compiles to `wasm32`
> and the `<nessemble-assembler>` Web Component is embedded in the mdBook docs and
> powers the marketing homepage's live assemble-and-play demo (all verified in
> headless Chromium). The release pipeline packages the wasm bundle as a GitHub
> release asset, and the workspace version is now `2.6.0`.

---

## 1. Goal

Bring back the interactive, in-browser assembler that the original C project
shipped in its docs, rebuilt on the Rust assembler:

1. **A WASM build** of the assembler (`nessemble-core` + scripting), produced
   with `wasm-pack`.
2. **An assembler Web Component** — a vanilla custom element
   `<nessemble-assembler>` — modeled on the [previous React
   component](https://github.com/kevinselwyn/nessemble/blob/master/docs/js/components/assembler.tsx),
   that assembles a source buffer entirely client-side and shows the result.
3. **Re-embed it in the public documentation**, the way the [old docs
   did](https://raw.githubusercontent.com/kevinselwyn/nessemble/refs/heads/master/docs/pages/syntax.md)
   (inline interactive examples throughout `syntax.md` and friends), plus a
   playground on the marketing homepage.

## 2. Reference — what the old one did, and what changes

**Old component** (`docs/js/components/assembler.tsx`): a React class that shelled
out to an **Emscripten**-compiled C assembler via `assembler.callMain(args)`,
rendering a hex dump, a downloadable `.rom`, and stderr. It also offered
run/disassemble modes and a custom-pseudo playground. Docs embedded it as
`<div class="nessemble-assembler">…code…</div>` with an optional
`data-opts='{"pseudo":{"ease":true}}'` JSON attribute, ~20+ inline instances.

**What changes here:**

- **Rust → WASM via `wasm-pack`** (not Emscripten): a new `nessemble-wasm` crate
  exposes an `assemble` function over `wasm-bindgen`.
- **A real Web Component** (vanilla `customElements.define`), not React — it drops
  straight into static mdBook output with no framework or bundler runtime.
- **Assemble + custom pseudo-ops**, but **no run/disassemble** — the Rust core has
  no simulator or disassembler, so those modes are dropped.

## 3. Current state (grounding)

- **Assembler API:** `nessemble_core::assemble(source: &str, &Options) ->
  Result<Assembly, AssembleError>` operates on in-memory source and needs no
  filesystem for plain code — an ideal WASM entry point. `Assembly` carries
  `rom: Vec<u8>`, `warnings: Vec<Diag>`, `symbols`, `coverage`.
  `assemble_with(source, options, custom)` injects a custom pseudo-op resolver.
- **Scripting:** `nessemble-script` hosts Rhai. It **hard-depends on `rhai-fs`**
  (filesystem access for scripts) — unavailable in the browser, so the WASM build
  must **feature-gate `rhai-fs` off**. `decode_png` (via `nessemble-media`, the
  `png` crate) *does* build for wasm. Bundled scripts (`ease.rhai`, …) are
  `include_str!`'d into the **CLI** crate (`crates/nessemble-cli/src/data/scripts/`).
- **Docs pipeline:** mdBook (`docs/src/*.md` → `docs/book/`, config in
  `docs/book.toml`). `xtask dist` builds the site into `site/`: it copies the
  marketing site from `website/` to the root and the built book to `site/docs/`.
  `.github/workflows/pages.yml` runs `xtask dist` and deploys `site/` to GitHub
  Pages. Both `site/` and `docs/book/` are generated (gitignored).
- **Filesystem-dependent directives** (`.include`, `.incbin`, `.incpng`, …) read
  from disk; in-browser they simply error. That's an accepted limitation
  (documented), not a blocker for the interactive snippets.

## 4. Proposed architecture

### 4.1 `nessemble-wasm` crate

- New workspace crate `crates/nessemble-wasm`, `crate-type = ["cdylib"]`, built
  with **`wasm-pack`** (→ `wasm-bindgen`). Depends on `nessemble-core` and
  `nessemble-script` (scripting **without** `rhai-fs`).
- Public surface (shape, not final signatures):
  - `assemble(source: &str, opts: JsValue) -> JsValue` returning
    `{ rom: Uint8Array, warnings: string[], errors: string[] }` (a structured
    result, never a panic — errors are data).
  - `opts` mirrors the old `data-opts`: `{ format: "nes"|"raw", undocumented,
    empty_byte, pseudo: { <name>: true | "<inline rhai source>" } }`.
- **Custom pseudo-ops in the browser:** the crate embeds the built-in scripts
  (`include_str!`, shared with or copied from the CLI's `data/scripts/`) so
  `pseudo: { ease: true }` resolves by name; a string value supplies inline script
  source. A `wasm`-side resolver runs them through `nessemble-script` and is
  passed to `assemble_with`. Scripts that need `rhai-fs` (file I/O) are
  unsupported in-browser and surface a clear error.
- **Panic safety:** set a panic hook that converts panics into returned errors so
  a bad input never tears down the module.

### 4.2 The Web Component

- A single hand-written JS file defining `class NessembleAssembler extends
  HTMLElement` and `customElements.define('nessemble-assembler', …)`, plus a small
  CSS file. No framework, no build step.
- **Lazy, shared WASM init:** the module is fetched and instantiated once (a shared
  promise) the first time any component is interacted with, so a page with 20
  embedded examples loads one `.wasm`.
- **UX (from the old component, assemble-only):**
  - An editable `<textarea>` seeded from the element's text content / a slot (so
    markdown authors write the example inline).
  - **Assemble** button → hex dump of the ROM in a `<pre>`, byte count, and a
    **Download `.rom`** link; **Reset** (restore original) and **Clear**.
  - Errors/warnings rendered distinctly (the old component's red-styled output).
  - Options via attributes / a `data-opts` JSON attribute for parity, including
    `pseudo`.
- **`assembled` event:** on each run the element dispatches a custom DOM event
  (e.g. `nessemble:assembled`) carrying the ROM bytes and any errors in
  `detail`, so an embedding page can react to a fresh build — notably to hand the
  ROM to an emulator (see Phase 3). This is the page-facing hook beyond the
  built-in hex dump / download.
- **Legacy parity:** an optional upgrader that also enhances
  `<div class="nessemble-assembler" data-opts="…">` elements, so the old docs'
  embedding syntax keeps working alongside the new tag.

### 4.3 Delivery

- **New `xtask wasm`** step: runs `wasm-pack build` for `nessemble-wasm` (target
  chosen for classic `<script>` loading — `--target no-modules` or `web` with a
  tiny loader) and stages the `.wasm` + JS glue + the component JS/CSS into the
  docs assets.
- **mdBook wiring:** reference the component + wasm loader via
  `[output.html] additional-js` / `additional-css` in `docs/book.toml`; ensure the
  `.wasm` and glue land in `docs/book/` (mdBook copies non-markdown assets) so
  they deploy under `site/docs/`.
- **`xtask dist`** gains the `wasm` step before the mdBook build; **`pages.yml`**
  installs the wasm toolchain (`wasm-pack`, `wasm32-unknown-unknown`).
- **Marketing homepage:** replace the Asciinema recording with the component,
  seeded with the demo program and wired to **play its freshly-assembled ROM in
  the existing JSNES emulator** (see Phase 3).
- **Release asset:** `release.yml` builds the wasm bundle and attaches it to the
  GitHub release for the version.

## 5. Phased plan

Each phase is independently shippable; parity is unaffected throughout (a new
crate + tooling; the `assemble`/`assemble_file` path is untouched — **122/122**
must stay green).

### Phase 0 — WASM crate & build — ✅ done
- Scaffolded `crates/nessemble-wasm` (cdylib + rlib, `wasm-bindgen`); confirmed
  `nessemble-core` + Rhai compile to `wasm32-unknown-unknown`. Exposes
  `assemble(source, opts_json) -> AssembleResult { rom, errors, warnings, ok }`
  (errors returned as **data**, never a throw) plus a `start()` panic hook. ✅
- Made `rhai-fs` **optional** in `nessemble-script` (`fs` feature, on by default);
  the wasm crate takes `nessemble-script` as a direct path dep with
  `default-features = false` so `fs` is genuinely dropped from the wasm graph.
  Built-in scripts are embedded (`ease`, shared via `include_str!` from the CLI
  crate); `opts.pseudo` enables built-ins by name or supplies inline Rhai. ✅
- Added `xtask wasm` (runs `wasm-pack build … --target web`). ✅
- **Done when:** ✅ six host unit tests (`cargo test -p nessemble-wasm`, no wasm
  toolchain needed) plus a Node smoke test over the real `wasm-bindgen` bundle
  cover: `lda #$00` → `A9 00`; a bad program → error (no panic); an inline
  script (`.double 5` → `0A`); the built-in `ease` script by name; and an
  `open_file` script erroring cleanly (no filesystem). Workspace tests +
  **parity 122/122** unaffected.
- **Size note:** the raw `wasm-bindgen` output is ~3.0 MB (Rhai dominates);
  `wasm-opt -Oz` + gzip (via `wasm-pack`) will cut that substantially — a budget
  input for Phase 2, where an assemble-only fallback stays in reserve if needed.

### Phase 1 — Assembler Web Component — ✅ done
- Implemented the vanilla `<nessemble-assembler>` element (`web/`): editor,
  Assemble, hex dump + byte count, download, reset/clear, error/warning display,
  a shared lazy wasm init (one module per page), `data-opts` options (incl.
  `pseudo`), the `nessemble:assembled` event, and the legacy
  `.nessemble-assembler` div upgrader. Indentation is preserved (nessemble is
  column-sensitive: col 0 = label, indented = instruction). ✅
- **Done when:** ✅ verified in headless Chromium on a standalone page — plain
  assembly (correct column semantics), an inline custom pseudo-op, legacy-div
  upgrade, download, error styling, and the assembled event, loading the wasm
  once.

### Phase 2 — Embed in the mdBook docs — ✅ done
- `docs/theme/head.hbs` loads the component on every page via `{{ path_to_root }}`
  (chosen over `additional-js`, which loads classic scripts — the wasm glue is an
  ES module the component `import()`s). `xtask dist` builds the wasm and stages
  the component + wasm into `docs/src/nessemble/` (mdBook copies it into the book)
  and `website/static/nessemble/`; `pages.yml` installs the wasm target +
  `wasm-bindgen-cli`. ✅
- Added interactive examples to `docs/src/syntax.md`: a labels/branch demo, a
  `.ascii` demo, and a `.ease` custom-pseudo-op demo (Rhai scripting in the
  browser). ✅
- **Done when:** ✅ a local `xtask dist` produces a `site/docs/` whose pages run
  the assembler in-browser (verified in headless Chromium, all three examples
  incl. the scripted one).

### Phase 3 — Homepage: live assemble-and-play (replaces the Asciinema demo) — ✅ done
`website/index.html` showed a **recorded** Asciinema session next to a **JSNES**
emulator that played a *pre-built* `static/data/roms/example.nes`. Replaced the
recording with a live demo: edit → assemble → play. **Verified in headless
Chromium** — the page seeds `example.asm`, assembles it to the exact 24592-byte
iNES ROM (byte-for-byte identical to the old `example.nes`), and JSNES renders a
non-blank frame. One caveat: the Asciinema *code* is bundled inside
`website.js`/`website.css`, so only the standalone leftover files and the
`<asciinema-player>` usage were removed; the dead bundled code was left in place.

- Swap the `<asciinema-player>` for a `<nessemble-assembler>`, **pre-populated
  with the demo program** (`website/static/data/recording/example.asm`, a full
  iNES program) and configured to assemble as iNES (`format: nes`).
- **Wire assemble → play.** On a successful assemble, feed the resulting ROM
  bytes into the **existing JSNES emulator** (via the component's `assembled`
  event; §4.2) instead of loading the static `example.nes`. Visitors edit,
  assemble, and run the program in the browser — a far better demo than a
  recording. (The ROM is *played* by JSNES, the existing JS NES emulator; the
  Rust core still only assembles — consistent with the §8 non-goals.)
- **Remove the now-unused assets:** the Asciinema player + CSS
  (`static/js/asciinema-player.js`, `static/css/asciinema-player.css`), the
  recording data (`static/data/recording/*`), and the pre-built
  `static/data/roms/example.nes` (assembled live now). Keep `jsnes.js`.
- **Done when:** the homepage loads with the demo program in the editor; clicking
  **Assemble** produces the ROM and the JSNES emulator plays it; edits reassemble
  and replay; assembly errors surface in the component without breaking the page.

### Phase 4 — Release artifact ✅
- Build the wasm bundle in `release.yml` and attach it to the GitHub release.
- **Done when:** cutting a release attaches a downloadable wasm bundle.
- **Done:** a `wasm` job builds the bundle (`cargo run -p xtask -- wasm`) and
  packages the glue + `.wasm`, the Web Component (JS + CSS), and a usage README
  into `nessemble_<v>_wasm.tar.gz`; `release` depends on it and uploads the
  tarball (covered by the existing `artifacts/**/*.tar.gz` glob). The workspace
  version dropped its `-dev` suffix to `2.6.0` to cut the release.

## 6. Decisions

**Made:**

1. **Toolchain — `wasm-bindgen` (`--target web`).** *Refined from the original
   `wasm-pack` choice during implementation:* `wasm-pack` just orchestrates
   `cargo build` + `wasm-bindgen` (+ optional `wasm-opt`), and `xtask wasm`
   invokes those directly — no extra tool to install, the bindings match the
   `wasm-bindgen` version pinned in `Cargo.lock`, and CI only needs the wasm
   target + `wasm-bindgen-cli`. (`wasm-opt` size optimization can be added later.)
2. **Component — a vanilla-JS custom element** (`customElements.define`), no
   framework, no JS build step.
3. **Scope — assemble *plus* custom pseudo-ops** (Rhai compiled to wasm with
   `rhai-fs` gated off); **no** run/disassemble.
4. **Delivery targets — (a) embedded in the mdBook docs, (b) a marketing-homepage
   playground, (c) a GitHub release asset.** npm publishing is **out of scope**.

## 7. Risks & open constraints

- **`rhai-fs` in wasm.** Must be feature-gated off; scripts using file I/O won't
  run in-browser (clear error). Requires making it optional in `nessemble-script`
  without disturbing the CLI's behavior.
- **WASM size.** Rhai adds meaningful size; use `wasm-opt`/release + measure. If
  it's too heavy, a fallback is an **assemble-only** wasm feature that drops
  scripting (kept in reserve, not the default given the decision above).
- **No filesystem.** `.include` / `.incbin` / media directives error in-browser;
  documented as a limitation of the interactive snippets.
- **Classic-script loading & CSP.** mdBook `additional-js` are classic scripts;
  pick a `wasm-pack` target that fits (`no-modules`/`web`). GitHub Pages must serve
  `.wasm` as `application/wasm`, and some browsers need
  `script-src 'wasm-unsafe-eval'` — verify on Pages.
- **Bundled-script duplication.** The built-in scripts live in the CLI crate;
  decide whether the wasm crate shares them (move to a common location) or keeps a
  copy, to avoid drift.
- **Versioning.** The wasm build tracks the workspace version. **Done:** the
  phases rode the pre-release **`2.6.0-dev`** suffix (as the LSP work did) so the
  release workflow's pre-release guard suppressed a release while they landed;
  Phase 4 dropped the `-dev` suffix to **`2.6.0`** to cut the release.

## 8. Non-goals

- Run/simulate and disassemble modes (no simulator/disassembler in the Rust core).
- npm publishing of the wasm/component.
- A full offline/PWA playground, project persistence, or multi-file/include
  support in the browser.
