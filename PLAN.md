# nessemble-rs: A Multi-Phase Plan to Reimplement `nessemble` in Rust

> Status: **Scope confirmed.** This document is a plan produced from a
> read-only analysis of the upstream C project
> [`kevinselwyn/nessemble`](https://github.com/kevinselwyn/nessemble) pinned at
> **v1.1.1**. All initial open questions have been answered by the project owner;
> see the [Decisions Log](#12-decisions-log) for the resolved directions that
> shape this plan.

---

## 1. Executive Summary

`nessemble` is a 6502 assembler / disassembler / simulator targeting the
Nintendo Entertainment System (NES), written in C. The upstream v1.1.1 CLI tool
also ships a WebAssembly module and integrates with a package registry, three
embedded scripting engines, and image/audio importers.

This plan reimplements the tool in Rust as **`nessemble-rs`**, a Cargo workspace
of focused crates, delivered in **10 phases**. Per the owner's decisions
([§12](#12-decisions-log)), the effort targets the **assembler only** and is a
**fresh Rust codebase** — **no C source is vendored into this repository.** The
strategy prioritizes:

1. **Assembler parity first** (the only in-scope runtime path): **byte-for-byte
   ROM output parity** with the official v1.1.1 release binaries, validated by
   *differential testing* against those binaries. We do **not** replicate C
   quirks/bugs — where the C tool is clearly wrong, we do the right thing and
   document the deviation.
2. **Incremental, independently shippable phases**, each with tests and
   acceptance criteria.
3. **Replacing bespoke C machinery** (flex/bison, gettext, image codecs) with
   well-maintained, ideally **pure-Rust** crates that cross-compile cleanly to
   all five target platforms.
4. **A single embedded scripting language** ([Rhai](https://rhai.rs)) for custom
   pseudo-ops, replacing the JS/Lua/Scheme trio.

**Scope note.** In scope: the **assembler** (assemble / check / coverage), its
**media importers**, the **CLI/config/init/reference** surface, **i18n**, custom
pseudo-op **scripting**, **documentation + website generation**, and **release
packaging** for all five platforms. Out of scope: the **disassembler/
reassembler**, the **simulator/debugger**, the **package-registry functionality**
(install/publish/search + user auth), native **`.so` plugins**, and the Python
**server** components. A **WASM build/playground** is deferred (a possible future
addition, not required now). The inventory below describes the original C tool
for context; out-of-scope pieces are marked and belong to no delivery phase.

The relevant (in-scope) first-party C code is a subset of the ~12.7k LOC + ~770
lines of flex/bison grammar; it is used purely as a behavioral reference, not
copied.

---

## 2. What `nessemble` Does (Feature Inventory)

Derived from `src/main.c`, `src/usage.c`, the grammar, and `docs/pages/*.md`.

### 2.1 Primary modes (per-invocation)

| Mode | Flag / command | Description | Scope |
|------|----------------|-------------|-------|
| Assemble | *(default)* | Assemble `.asm` → raw binary or iNES `.nes` ROM | **In scope** |
| Check | `-c` / `--check` | Parse + validate only, no output | **In scope** |
| Coverage | `-C` / `--coverage` | Emit code-coverage data for a ROM | **In scope** |
| Disassemble | `-d` / `--disassemble` | ROM/binary → assembly listing | **Out of scope** |
| Reassemble | `-R` / `--reassemble` | Disassemble then re-assemble (round-trip) | **Out of scope** |
| Simulate | `-s` / `--simulate` | Interactive 6502 CPU simulator / debugger (REPL) | **Out of scope** |

> `-C`/coverage output describes a ROM; the assembler already tracks coverage
> during emission, so it stays in scope even though disassembly does not.

### 2.2 Assembler feature set

- **Two-pass assembly** (symbol resolution then emission).
- **6502 instruction set** incl. **undocumented/illegal opcodes** (`-u`).
- **Addressing modes**: implied, accumulator, immediate, relative, zero-page
  (+X/+Y), absolute (+X/+Y), indirect (+X/+Y).
- **Number formats**: hex (`$..`/`..h`), binary (`%..`/`..b`), octal, decimal,
  char literals, defchr tiles, macro args (`\1`, `\#`, `\@`).
- **Expressions**: `+ - * / ** % & | ^ << >> == != < > <= >=`, parens,
  `HIGH()`, `LOW()`, `BANK()`.
- **Symbols**: constants, labels, local/anonymous labels (`:` with `+`/`-`),
  `.rs`/`.rsset` struct offsets, `.enum`.
- **Macros**: `.macro`/`.endm` invocation macros and `.macrodef` text macros.
- **Conditional assembly**: `.if`/`.ifdef`/`.ifndef`/`.else`/`.endif` (nested).
- **Includes**: `.include` (nested up to depth 10).
- **iNES header** control: `.inesprg`, `.ineschr`, `.inesmap`, `.inesmir`,
  `.inestrn` (trainer), PRG/CHR banking, `.segment`, `.org`, `.prg`, `.chr`.
- **Data directives**: `.db`/`.byte`, `.dw`/`.word`, `.ascii`, `.fill`,
  `.hibytes`, `.lobytes`, `.checksum`, `.random`, `.color`, `.font`, `.defchr`.
- **Asset importers**: `.incbin`, `.incpng` (PNG→CHR), `.incpal`, `.incrle`,
  `.incwav` (WAV→DPCM), `.incchr`.
- **Custom pseudo-ops** (`.foo`) resolved via external scripts (`-p`).
- **List file** output (`-l`), symbol/label tables.
- Reads from a file, stdin (piped), or (in the JS build) an in-memory FS.

### 2.3 Simulator / debugger — **OUT OF SCOPE**

*(Retained here for context only; not part of any delivery phase.)*

- Full documented + illegal 6502 opcode execution, cycle counting.
- REPL commands: registers/flags inspection & set, step/steps, goto, memory
  read/fill, disassemble-at, breakpoints (add/remove/list), record to file,
  recipe-file scripted sessions, quit.

### 2.4 Tooling / ecosystem commands

**In scope:**

- `init` — scaffold a new project.
- `config` (get/set/list).
- `reference` (opcode/pseudo reference lookup, incl. QR code output),
  `scripts` (install bundled custom-pseudo scripts).
- `--version`, `--license`, `--help`, man-style usage; **i18n** via gettext.

**Out of scope** (package-registry functionality):

- `registry` (get/set) — configures the registry endpoint.
- Package manager: `install`, `uninstall`, `publish`, `info`, `ls`, `search`
  against an HTTP registry (`http://www.nessemble.com/registry` by default).
- User/auth: `adduser`, `login`, `logout`, `forgotpassword`, `resetpassword`.

### 2.5 Build targets (original C tool)

- Native Linux/macOS/Windows (mingw) binaries.
- **WebAssembly / JS** module via Emscripten (`nessemble.js`, used by the
  docs "playground").
- Distribution packaging: `.deb`, macOS `.pkg`, Windows `.msi`.

> **Our targets (resolved):** we reproduce the **v1.1.1 native release
> artifacts** for macOS, Linux amd64/i386, and Windows 32/64-bit ([§5.4](#54-target-platforms--artifacts)).
> The **WASM/JS** build is **deferred** (D9). The **Python/Flask servers** and
> the **TS frontend runtime** stay **out of scope**, but we **do** generate the
> static **documentation site and website** as build artifacts (C7,
> [§6.6](#66-documentation--website)).

---

## 3. Current Architecture (C) — Subsystem Map

First-party sources under `src/` (~12.7k LOC) plus grammar (~770 LOC):

| Subsystem | Files | Notes |
|-----------|-------|-------|
| CLI & dispatch | `main.c`, `usage.c` | `getopt_long`, subcommand routing, 2-pass driver |
| Lexer | `nessemble.l` (flex) | 244 lines; include/macro start-conditions, global mutable stack |
| Parser | `nessemble.y` (bison) | 526 lines; expression grammar + directives + instructions |
| Assembler core | `assemble.c`, `instructions.c`, `macro.c`, `math.c` | symbol table, ROM/coverage buffers, addressing-mode emit, iNES banking |
| Pseudo-ops | `pseudo/*.c` (38 files) | one file per directive |
| Opcode tables | `static/opcodes.csv` → generated `opcodes.c` | 256 rows; mnemonic, mode, opcode, length, timing, meta |
| Disassembler | `disassemble.c` | 711 LOC — **OUT OF SCOPE** |
| Simulator | `simulate.c`, `simulate/opcode.c`, `simulate/illegal.c` | ~2.1k LOC CPU + REPL — **OUT OF SCOPE** |
| Media/format | `png.c`, `wav.c`, `zip.c`, `hash.c`, `json.c` | PNG (stb), WAV needed by importers; tar/gzip (udeflate), SHA/HMAC, JSON (jsmn) used only by the **out-of-scope** registry |
| Config/home | `config.c`, `home.c` | `~/.nessemble/` config & paths |
| Registry/net | `registry.c`, `api.c`, `user.c`, `http.c` | **hand-rolled raw-socket HTTP client** (no TLS lib) — **OUT OF SCOPE** |
| Scripting | `scripting/{js,lua,scm,cmd,so}.c` | Duktape / Lua / TinyScheme / shell / shared-object custom pseudo-ops |
| Output | `list.c`, `coverage.c` | list file, coverage report |
| Misc | `i18n.c`, `pager.c`, `reference.c`, `scripts.c`, `init.c`, `error.c`, `utils.c` | gettext, pager, reference, scaffolding, `setjmp`-based error handling |

### 3.1 Notable design characteristics (and porting implications)

- **Heavy global mutable state.** `nessemble.h` declares dozens of globals
  (symbol table `symbols[65536]`, `rom`, `coverage`, `ines`, offsets, if/macro
  stacks, `yyin`, etc.). The C code threads state implicitly through globals.
  → In Rust this becomes an explicit `Assembler`/`Context` struct passed
  through the pipeline; this is the single biggest structural change.
- **flex/bison** with custom start-conditions and a global include stack.
  → Replace with a hand-written lexer (`logos`) + recursive-descent/Pratt
  parser, *or* `lalrpop`/`pest`. See [§6.2](#62-lexer--parser-strategy).
- **`setjmp`/`longjmp` error handling** and `error_exit()`/two-pass reset.
  → Replace with `Result<_, AsmError>` propagation + diagnostic accumulation.
- **Fixed-size arrays / caps** (`MAX_SYMBOLS`, `MAX_MACROS`, `MAX_BANKS`,
  `MAX_INCLUDE_DEPTH`, …). → Rust uses growable collections; caps become
  configurable or removed, but **must preserve observable limits/errors** where
  tests depend on them.
- **Platform `#ifdef`s** (`IS_WINDOWS`/`IS_LINUX`/`IS_MAC`/`IS_JAVASCRIPT`).
  → `cfg!`/target features. Five required targets (§5.4): macOS, Linux
  amd64, Linux i386, Windows 32-bit, Windows 64-bit. This is a strong reason to
  favour **pure-Rust** dependencies that cross-compile without a C toolchain
  (a key factor in the scripting choice — see [§6.4](#64-scripting-language-choice)).

---

## 4. Dependency Mapping (C → Rust crates)

In-scope C machinery and system deps mapped to the Rust ecosystem. Preference is
for **pure-Rust** crates so the five-platform matrix cross-compiles without a C
toolchain.

| C dependency | Purpose | Chosen Rust replacement |
|--------------|---------|-------------------------|
| flex / bison | lexer / parser generators | `logos` (lexer) + hand-written recursive-descent/Pratt parser |
| `getopt_long` | CLI parsing | `clap` (derive) |
| stb_image / stb_image_write | PNG decode/encode | `image` (pure Rust) |
| gettext (`i18n.c`) | translations | **Project Fluent** (`fluent`/`fluent-bundle`, pure Rust) — see [§6.5](#65-i18n-approach) |
| QR code (`reference.c`) | terminal QR | `qrcode` crate |
| pager (`pager.c`) | `$PAGER`/less | shell out to `$PAGER`, or `minus` |
| Duktape / Lua / TinyScheme | embedded scripting (custom pseudo-ops) | **Rhai** — single pure-Rust engine (replaces all three) — see [§6.4](#64-scripting-language-choice) |
| docs (`mkdocs`, Python) + website (webpack/TS) | documentation & website | **mdBook** (docs) + a static site build — see [§6.6](#66-documentation--website) |
| distribution (`dpkg`, `pkgbuild`, `wixl`) | release artifacts | `cargo-deb`, `cargo-bundle`/`pkgbuild`, `cargo-wix` — see Phase 9 |

**Dropped (owner decision):**

| C dependency | Purpose | Disposition |
|--------------|---------|-------------|
| shared-object (`so.c`) | native `.so` plugin pseudo-ops | **Dropped** (portability/safety; B6) |
| Emscripten | WASM build | **Deferred** — not required now; possible future addition (D9) |

**Out-of-scope dependencies** (belong only to the excluded disassembler /
simulator / package-registry subsystems — listed for completeness, not used):

| C dependency | Purpose | Rust equivalent (only if ever re-scoped) |
|--------------|---------|------------------------------------------|
| jsmn | JSON parsing (registry) | `serde` + `serde_json` |
| udeflate `deflate.c` | gzip/inflate for tar.gz (registry) | `flate2` |
| tar handling (`zip.c`) | untar registry packages | `tar` crate |
| hand-rolled SHA/HMAC (`hash.c`) | auth signing (registry) | `sha2` + `hmac` |
| raw-socket HTTP (`http.c`) | registry/user API client | `ureq` / `reqwest` |

---

## 5. Goals, Non-Goals & Guiding Principles

### 5.1 Goals

- **G1 — Assembler ROM parity:** **byte-for-byte identical ROM output** vs the
  official **v1.1.1 release binaries** for the in-scope corpus (`test/examples`,
  `test/opcodes`, `test/nerdy-nights`, `test/errors`). We **do not** reproduce
  odd/buggy C behavior: where the C tool is demonstrably wrong, `nessemble-rs`
  does the correct thing and the deviation is documented (A3).
- **G2 — CLI fidelity:** the in-scope CLI (flags, subcommands, exit codes,
  primary stdout/stderr) is **as close to the C v1.1.1 tool as possible** (E12).
  Out-of-scope options (`-d`/`--disassemble`, `-R`/`--reassemble`,
  `-s`/`--simulate`, and the registry/user commands) are **omitted entirely** —
  not parsed, not listed in help/usage, not documented, no "not supported"
  message. To the user they do not exist.
- **G3 — Memory safety & maintainability:** idiomatic Rust, no global mutable
  state, structured error/diagnostics, thorough tests.
- **G4 — Five-platform releases:** produce the same release artifacts as v1.1.1
  for macOS, Linux amd64, Linux i386, Windows 32-bit, and Windows 64-bit
  (see [§5.4](#54-target-platforms--artifacts)).
- **G5 — i18n retained:** translation support remains a first-class feature
  (E11), implemented the idiomatic-Rust way (Project Fluent — [§6.5](#65-i18n-approach)).
- **G6 — Docs & website:** generate the documentation site (clean mdBook theme)
  and reproduce the existing project **website look as-is**, both as static
  artifacts deployable to **GitHub Pages** (C7, Q-b) — no server required.
- **G7 — Custom pseudo-ops:** retain scriptable custom pseudo-ops via a single
  embedded language ([Rhai](https://rhai.rs)).

### 5.2 Non-Goals / Out of Scope

- The **disassembler / reassembler** (`-d`, `-R`; `disassemble.c`). (A1)
- The **simulator / debugger** (`-s`; `simulate.c`, `simulate/opcode.c`,
  `simulate/illegal.c`, and the REPL). (A1)
- The **package-registry functionality**: the package manager
  (`install`/`uninstall`/`publish`/`info`/`ls`/`search`), the `registry`
  get/set command, user/auth commands (`adduser`/`login`/`logout`/
  `forgotpassword`/`resetpassword`), and the underlying HTTP client, JSON,
  tar/gzip, and HMAC machinery that serve them
  (`registry.c`, `api.c`, `user.c`, `http.c`, `json.c`, `zip.c`, `hash.c`). (A1)
- Native **`.so` plugin** pseudo-ops (`scripting/so.c`). (B6)
- The **Python Flask server** components (registry/website/docs/CDN back ends)
  and the **TypeScript docs frontend runtime**. *(We still generate the static
  docs + website content — C7 — just not the servers.)*
- A **WASM build / interactive playground** — **deferred**, not required now;
  candidate for a future phase once the native build is solid. (D9)
- Bug-for-bug replication of C quirks (A3).

> These exclusions are deliberate. The architecture leaves clean seams so any of
> them could be re-scoped later without disturbing the assembler core.

### 5.3 Principles

- **Differential testing is the source of truth.** Compare against the pinned
  **v1.1.1 release binaries** (no C build in this repo — E13/E14).
- **Vertical slices over horizontal layers**: get a minimal end-to-end assemble
  path working early, then widen.
- **Pure-Rust dependencies preferred** so all five targets cross-compile cleanly.
- **One behavioral change per PR**, each green against the corpus.

### 5.4 Target platforms & artifacts

Match the v1.1.1 release exactly (D8/D10). The official
[v1.1.1 release](https://github.com/kevinselwyn/nessemble/releases/tag/v1.1.1)
ships these assets (note: **no** JS/npm artifact, consistent with WASM being
deferred):

| Platform | Rust target triple | Release artifact(s) |
|----------|--------------------|---------------------|
| macOS | `x86_64-apple-darwin` | `nessemble_<v>.pkg` |
| Linux amd64 | `x86_64-unknown-linux-gnu` | `nessemble_<v>_amd64.deb` |
| Linux i386 | `i686-unknown-linux-gnu` | `nessemble_<v>_i386.deb` |
| Windows 32-bit | `i686-pc-windows-*` | `nessemble_<v>_win32.exe`, `nessemble_<v>_win32.msi` |
| Windows 64-bit | `x86_64-pc-windows-*` | `nessemble_<v>_win64.exe`, `nessemble_<v>_win64.msi` |

The standalone `.exe`s are the raw CLI binaries; the `.msi`s are installers.

---

## 6. Target Rust Architecture

### 6.1 Workspace layout (proposed)

```text
nessemble-rs/
├─ Cargo.toml                 # workspace
├─ crates/
│  ├─ nessemble-isa/          # 6502 opcode tables (from opcodes.csv), modes
│  ├─ nessemble-core/         # lexer, parser, AST, assembler, symbol table,
│  │                          #   iNES/banking, pseudo-ops, expressions
│  ├─ nessemble-media/        # PNG/CHR, palette, RLE, WAV/DPCM importers
│  ├─ nessemble-script/       # Rhai-based custom pseudo-op host (feature-gated)
│  ├─ nessemble-i18n/          # Fluent bundles + string catalog
│  └─ nessemble-cli/          # clap CLI, dispatch, reference, init, config, pager
├─ docs/                      # mdBook source (documentation site)
├─ website/                   # static website source + generator
├─ locales/                   # *.ftl translation files (Fluent)
├─ xtask/                     # dev tooling: parity harness, packaging drivers
└─ tests/                     # differential + golden-ROM harness
```

Rationale: `nessemble-isa` is a leaf crate shared by core (and any future
disasm/sim). Scripting is feature-gated so the default build stays lean. The
`disasm`, `sim`, and `registry` crates are **intentionally absent** (out of
scope, §5.2). A `nessemble-wasm` crate is **deferred** (D9) but the workspace is
shaped so it can wrap `nessemble-core` later with no core changes.

### 6.2 Lexer / parser strategy

Two viable options; **recommendation: hand-written `logos` lexer + recursive
descent + Pratt expression parser.**

- *Pros:* full control over the context-sensitive bits the flex grammar relies
  on (indentation → instruction, `include`/`macro` start-conditions, `.macrodef`
  raw-text capture), better error messages, no build-time codegen tool, easy
  two-pass reuse.
- *Alternative:* `lalrpop` mirrors the bison grammar closely, which could ease
  the initial translation, but the lexer's stateful modes and the two-pass model
  are awkward to express in an LR generator.

The grammar itself is small (expressions + ~40 directives + instruction forms),
so a hand-written parser is very tractable and is the more maintainable
long-term choice.

### 6.3 State model

Replace globals with an owned pipeline:

- `SourceManager` (files, include stack, line tracking).
- `Assembler { symbols, macros, segments, banks, ines, rom, coverage, if_stack,
  pass, diagnostics, flags, options }`.
- Pseudo-ops implemented as methods / a dispatch table on `Assembler` rather
  than 38 free functions over globals.
- Errors via `thiserror` + a `Diagnostics` collector that reproduces the C
  tool's messages and exit codes.

### 6.4 Scripting language choice

The C tool embeds **three** engines (Duktape JS, Lua 5.1.5, TinyScheme) purely
to let custom pseudo-ops (`.foo`) run user code that consumes assembler
arguments and emits bytes/values. Per B5 we consolidate to **one** language.

**Recommendation: [Rhai](https://rhai.rs).** Rationale:

- **Pure Rust, zero C deps.** Critical for the five-target matrix (D8) — no C
  toolchain, no `.a` linking, cross-compiles to Linux i386 / win32 / win64 /
  macOS (and later wasm32) trivially. `mlua` (Lua) and `rquickjs`/Duktape link C
  and complicate i386/Windows cross-builds; `boa` (JS) is heavier and less
  embedding-focused; `steel` (Scheme) is younger and niche.
- **Purpose-built for embedding**: trivially register host functions/types, pass
  the pseudo-op's numeric/string args in, collect emitted bytes out.
- **Sandboxed & bounded**: no ambient filesystem/network; operation and
  complexity limits guard against runaway scripts — a good fit for an assembler
  directive.
- **Small, actively maintained, permissive (MIT/Apache-2.0)** license.

*Migration:* the only bundled script that must exist is `ease` (today
`src/static/scripts/ease.lua`); per Q-a there is **no external/user script
ecosystem** to preserve. Adopting Rhai means **porting `ease` to `.rhai`** and
defining a stable Rhai host API for custom pseudo-ops — a small, self-contained
task.

### 6.5 i18n approach

i18n stays first-class (E11). Gettext (`.po`/`.mo`) is workable in Rust but leans
on C tooling; the idiomatic, pure-Rust choice that achieves the same result is
**Project Fluent** (`fluent`/`fluent-bundle`, Mozilla):

- Translation files are `locales/<lang>/*.ftl`, embedded at build time.
- A thin `nessemble-i18n` wrapper exposes a `t!("id", args)` equivalent used
  throughout the CLI/diagnostics, mirroring the C `_()` macro call sites.
- Fluent adds proper plural/gender/number formatting that gettext handled awkwardly.
- The existing catalog is effectively `en-US` only; we seed `en-US.ftl` from the
  C source strings and add locales as needed. *(A lighter alternative,
  `rust-i18n` with TOML/YAML, is available if Fluent feels heavyweight; Fluent is
  the recommendation.)*

### 6.6 Documentation & website

C7 requires generating the documentation and website (but not the servers).
Owner direction (Q-b): **the marketing website should look the same** as the
existing one; **the docs site may use a clean theme**; both deploy to
**GitHub Pages**.

- **Website — reproduce as-is.** The upstream landing page
  ([`website/static`](https://github.com/kevinselwyn/nessemble/tree/master/website/static)
  + `templates/index.html`) is a Bootstrap/"grayscale"-themed single page. Its
  only Flask templating is a handful of config substitutions (documentation URL,
  analytics IDs); its demo plays a **pre-built `example.nes` via JSNES** plus an
  asciinema recording — **no WASM/assembler runtime needed**, so it works fully
  even with the WASM build deferred. Plan: carry over the exact
  CSS/JS/img/font/data assets and template, render `index.html` to static output
  at build time, and adjust only the download links (point at the new releases)
  and the config values. **Copy change (Q-f):** the current page describes
  nessemble as an "assembler, disassembler, and simulator" — this is updated to
  **"assembler"** only (no mention of disassembler/simulator), keeping the
  layout/theme identical.
- **Docs — clean theme.** Author content in Markdown and build with **mdBook**
  (pure Rust). Port the in-scope `docs/pages/*.md` (installation, syntax, usage,
  building, extending, packages, etc.), **omitting** the out-of-scope
  simulator/disassembler/registry/playground pages.
- Both are produced by an `xtask`/CI step and published to **GitHub Pages**;
  where possible, `reference` data and usage text are generated from the same
  source of truth as the CLI so docs cannot drift.

---

## 7. Phased Migration Plan

Each phase lists scope, key deliverables, and acceptance criteria. Phases are
ordered so that the highest-value core lands first and each builds on the last.

### Phase 0 — Foundations & parity harness
- **Scope:** Cargo workspace skeleton; crate stubs; local checks (fmt, clippy,
  test); import the 256-row opcode table (from upstream `opcodes.csv`) into
  `nessemble-isa` as committed Rust data. **No C build in this repo (E13/E14):**
  the differential oracle is the **official v1.1.1 release binary** — the harness
  downloads/extracts it (on Linux, unpack `nessemble_1.1.1_amd64.deb` /
  `_i386.deb`) and records **golden ROM outputs** from the pinned reference test
  corpus (see §8). Golden files are committed so day-to-day runs need no network.
- **Deliverables:** building workspace; `xtask` parity harness that runs an input
  through both the release binary and `nessemble-rs` and diffs bytes.
- **Acceptance:** harness produces baseline golden ROMs from the v1.1.1 binary and
  a diff report scaffold.
- **Status: ✅ complete.** Workspace + crate seams build clean (`fmt`/`clippy`/
  `test` green); `nessemble-isa` generates the 256-entry opcode table from
  `opcodes.csv` (with tests); minimal `nessemble` CLI runs; 122 assemble fixtures
  imported to `tests/corpus/`; `xtask` implements `fetch-oracle`, `verify-goldens`,
  and `parity`. `verify-goldens` confirms the v1.1.1 oracle reproduces **all 119**
  non-scripting goldens (3 scripting cases deferred to Phase 8).

### Phase 1 — Lexer + expression/number evaluation
- **Scope:** `logos` lexer covering all tokens in `nessemble.l`; number bases,
  char/defchr literals, macro-arg tokens; expression parser (Pratt) with the
  full operator set, `HIGH/LOW/BANK`, parens; standalone evaluator.
- **Acceptance:** unit tests for tokenization + expression results matching C
  semantics (including integer division/`pow` behavior and truncation).
- **Status: ✅ complete** (folded into Phase 2). Implemented as a hand-written
  lexer (`nessemble-core::lexer`, mirroring flex longest-match/rule-order) plus
  a recursive-descent/Pratt expression parser. Verified that operators are a
  single precedence level and **right-associative** (matching the reference
  bison grammar's default shift resolution).

### Phase 2 — Core assembler: instructions, symbols, two-pass, raw output
- **Scope:** addressing-mode selection & opcode emission (`instructions.c`),
  symbol table (constants, labels, local/anonymous labels, `->` scoping),
  two-pass driver, ROM/offset/coverage buffers, `.org`, raw (non-iNES) output,
  `-c`/check mode, error/exit-code parity.
- **Acceptance:** byte-identical output for the non-iNES / opcode subset of
  `test/opcodes` and simple `test/examples`; `test/errors` cases produce
  matching failures.
- **Status: ✅ complete.** `nessemble-core` now lexes, parses, and assembles via
  a two-pass engine (symbol table, expression eval, syntactic addressing-mode
  selection, `.org` + non-iNES data directives, reference-matching error text).
  Parity harness: **78/119** goldens reproduced byte-for-byte — **all** opcode
  cases (documented + `-u` undocumented), the non-iNES simple examples, and the
  Phase-2 error cases (undefined symbol, unknown opcode, invalid mode, branch
  out of range). The remaining failures are Phase 3+ features (iNES output,
  banking, conditionals, includes, media, and the `enum`/`rs`/`checksum`/etc.
  directives). Hermetic regression tests live in
  `crates/nessemble-core/tests/corpus.rs`.

### Phase 3 — iNES, banking, segments & data/core directives
- **Scope:** iNES header + trainer, PRG/CHR banking, `.segment`/`.prg`/`.chr`,
  `.db`/`.dw`/`.ascii`/`.fill`/`.hibytes`/`.lobytes`/`.checksum`/`.random`/
  `.color`/`.enum`/`.rs`/`.rsset`, `.inesprg/chr/map/mir/trn`.
- **Acceptance:** full `test/examples` (excluding asset/scripting/macro/include
  cases) and `test/nerdy-nights` produce byte-identical `.nes` ROMs.
- **Status: ✅ complete.** Full iNES output (16-byte header + PRG/CHR bank
  layout with `empty_byte` fill), PRG/CHR banking and `.segment`/`.prg`/`.chr`,
  and the directives `.checksum` (CRC-32), `.random` (reference LCG + `str2hash`
  seed), `.color` (NES-palette matching), `.enum`/`.endenum`, `.rs`/`.rsset`,
  and `.inesprg/chr/map/mir`. Overflowing-bank **warnings** are emitted to
  stderr like the reference. Parity: **93/119** goldens byte-for-byte. The
  remaining failures are Phase 4 (macros/conditionals/includes) and Phase 5
  (media importers: `.incbin/.incpng/.incpal/.incrle/.incwav/.font/.defchr`, and
  the nerdy-nights programs that use them); the `.inestrn` trainer is deferred
  to Phase 4 (it performs an include).

### Phase 4 — Macros, conditionals, includes
- **Scope:** `.macro`/`.endm`, `.macrodef` text macros, macro args (`\1`,`\#`,`\@`),
  `.if`/`.ifdef`/`.ifndef`/`.else`/`.endif` (nested), `.include` (nested, depth
  limit), stdin/piped input, list-file output (`-l`).
- **Acceptance:** macro/include/conditional examples byte-identical; list files
  match.
- **Status: ✅ complete.** A token-stream **preprocessor** (`nessemble-core::
  preprocess`) resolves `.include` (nested, resolved relative to the top-level
  file's directory, with the depth-10 limit → `Too many nested includes`) and
  expands `.macrodef`/`.macro` text macros — substituting `\N` (parenthesised
  argument tokens), `\#`, and `\@` — the token-level analogue of the reference's
  re-entrant flex buffers. Conditionals (`.if`/`.ifdef`/`.ifndef`/`.else`/
  `.endif`, nested) are evaluated by the assembler, gating byte/symbol emission
  exactly as the reference does (suppressed bytes do not advance the location
  counter). `.inestrn` splices the trainer file into the 512-byte trainer region
  (emitted between header and PRG/CHR data). Diagnostics now carry the offending
  file's basename (top-level or included). The `-l` **list file** is produced and
  verified byte-for-byte against the v1.1.1 oracle across the corpus (`.rs` lists
  as a label, `.enum`/constants as constants, anonymous labels included). Parity:
  **101/119** goldens byte-for-byte; the remaining failures are all Phase 5 media
  importers (`.incbin/.incpng/.incpal/.incrle/.incwav/.font/.defchr` and the
  nerdy-nights programs that use them).

### Phase 5 — Asset importers (media)
- **Scope:** `nessemble-media`: `.incbin`, `.incpng` (+palette matching),
  `.incpal`, `.incrle`, `.incwav` (DPCM), `.font`, `.defchr`, `.chr`.
- **Acceptance:** `incpng/incpal/incrle/incwav/font/defchr` examples
  byte-identical; PNG/WAV edge cases covered.

### Phase 6 — CLI completeness, config, reference, init
- **Scope:** in-scope `clap` CLI surface & exit codes (assemble/check/coverage +
  `init`, `config` get/set/list, `reference` incl. QR, `scripts`,
  `--version`/`--license`/`--help`); `~/.nessemble` layout; pager. Match the C
  v1.1.1 CLI as closely as possible (E12). `-d`/`-R`/`-s` and the
  registry/user commands are **omitted entirely** (no parser entry, no
  help/usage line, no docs). `reference` is backed by **locally bundled data**
  (opcodes + bundled reference text), not a network call.
- **Acceptance:** CLI help/usage/exit-code parity for the in-scope surface;
  help/usage text contains **no reference** to disassemble/reassemble/simulate or
  the registry; `init` output matches; config round-trips.

### Phase 7 — i18n (Project Fluent)
- **Scope:** `nessemble-i18n` crate; wire a `t!`-style lookup through all CLI /
  diagnostic strings (mirroring the C `_()` call sites); seed `en-US.ftl` from the
  C strings; document how translators add a locale.
- **Acceptance:** all user-facing strings resolve through Fluent; `en-US` output
  matches the C tool's English messages; adding a stub locale works end-to-end.

### Phase 8 — Custom pseudo-op scripting (Rhai)
- **Scope:** `nessemble-script` (feature-gated) hosting **Rhai**; define the host
  API custom pseudo-ops use (receive args, emit bytes/values); `.custom`
  (`PSEUDO_CUSTOM`) dispatch; the `-p` pseudo file and the `scripts` install
  command; **port the bundled `ease` script** and any `scripts.txt` entries to
  `.rhai`.
- **Acceptance:** `test/examples/custom` and `ease` behavior reproduced with the
  Rhai host; scripts install to `~/.nessemble/scripts` and resolve at assemble time.

### Phase 9 — Documentation, website & release packaging
- **Scope:** mdBook documentation (clean theme, in-scope `docs/pages/*.md`) + the
  **existing website reproduced as-is** (§6.6), both published to GitHub Pages;
  release pipeline producing the **v1.1.1-matching artifacts** for all five
  platforms — `.pkg` (macOS), `amd64.deb`, `i386.deb`, and **both**
  `win32.exe`+`win32.msi` and `win64.exe`+`win64.msi` (Q-e) — via `cargo-deb`,
  `cargo-wix`, `pkgbuild`/`cargo-bundle`, and raw target-triple builds.
- **Acceptance:** docs + website build to static output deployable to GitHub
  Pages; all **seven** release artifacts are produced for the five targets and
  the CLI binaries run on each.

> **A WASM build/playground is deferred (D9)** — a candidate follow-up phase once
> the native build is complete; not required for this plan.
>
> **Removed from scope entirely:** disassembler/reassemble, simulator/debugger,
> and the package registry (§5.2).

---

## 8. Testing & Validation Strategy

- **Oracle = the official v1.1.1 release binary (E14).** We do **not** build the
  C tool in this repo (E13). The parity harness runs each input through the
  released binary and through `nessemble-rs` and asserts byte-identical ROMs and
  matching exit codes / key stderr. On Linux the oracle binary is unpacked from
  `nessemble_1.1.1_amd64.deb` (or `_i386.deb`); macOS/Windows parity can be
  spot-checked from `.pkg`/`.exe` when those hosts are available, but the
  assembler is platform-independent so Linux parity is authoritative.
- **Golden files:** commit oracle-generated ROMs as goldens so routine runs need
  no download; regenerate deliberately when the pinned version changes.
- **Reference corpus (in scope):** the upstream `test/opcodes` (343),
  `test/examples` (157), `test/nerdy-nights` (32), `test/errors` (62) inputs —
  copied in as **test fixtures/data only** (not C code). *(`test/integration`
  (simulator) and `test/registry` (registry) are out of scope and excluded.)*
- **No-quirk policy (A3):** when `nessemble-rs` intentionally diverges from a
  buggy C behavior, the case is moved from "must match" to a documented
  "known deviation" list with a rationale, rather than being silently skipped.
- **Unit tests** per crate (lexer, expression eval, addressing modes, media
  importers, Rhai host, Fluent lookups).
- **Property tests** (`proptest`) for expression evaluation and iNES/banking
  offset math.
- **Fuzzing** (`cargo-fuzz`) on the parser and PNG/WAV asset loaders.
- **Fixed-cap behaviors:** explicitly test observable limits/errors (include
  depth, symbol/macro caps) so we match them or consciously, documentedly change
  them.

---

## 9. Risk Register

| Risk | Impact | Mitigation |
|------|--------|-----------|
| Undocumented assembler quirks not covered by fixtures | Silent output divergence | Broad differential testing vs the v1.1.1 binary beyond the shipped corpus; fuzz-generated inputs run through both |
| Distinguishing "quirk to drop" from "behavior to match" (A3) | Wrong deviation | Every intentional divergence goes on a reviewed "known deviations" list with rationale |
| flex/bison edge cases (start-conditions, greedy rules) | Parser mismatch | Hand-written parser mirrored against grammar; targeted lexer tests |
| Cross-compiling to Linux i386 / win32 / win64 | Broken/absent artifacts | Choose **pure-Rust** deps (Rhai, `image`, Fluent); exercise all five target triples in the release pipeline early |
| Rhai host API differs from the JS/Lua/Scheme model | `ease` script behavior changes | Port the single bundled `ease` script; define & document a stable Rhai pseudo-op API (no external scripts to preserve, per Q-a) |
| Floating-point in expressions (`pow`, `/`) | Off-by-one divergence | Match C integer-cast semantics exactly; property tests |
| Fluent migration from gettext strings | Localization gaps | Seed `en-US.ftl` directly from C strings; verify English output matches the oracle |
| Packaging tools (`cargo-wix`/`cargo-deb`/`pkgbuild`) per platform | Release friction | Stand up the packaging pipeline in Phase 9 with a smoke-install check per target |

---

## 10. Suggested Sequencing & Parallelism

- **Critical path:** Phase 0 → 1 → 2 → 3 → 4 (the assembler) delivers the bulk
  of user value and unblocks everything else.
- **Parallelizable after Phase 2/3:** `nessemble-media` (Phase 5) shares only the
  core/ISA crates and can proceed independently of the CLI work.
- **Independent tracks after the CLI shell (Phase 6):** i18n (Phase 7), scripting
  (Phase 8), and docs/website + packaging (Phase 9) have few interdependencies
  and can proceed in parallel. Packaging can be scaffolded early (empty binary)
  to de-risk the five-target matrix.
- **Deferred:** the optional WASM build wraps `nessemble-core` and can start any
  time after Phase 3 if/when it is greenlit.

---

## 11. Success Criteria (Definition of Done for the migration)

1. `nessemble-rs` **assembles** the entire in-scope reference corpus with
   **byte-for-byte ROM parity** vs the v1.1.1 release binary (assemble / check /
   coverage, incl. media importers), except for a short, documented list of
   intentional no-quirk deviations (A3).
2. In-scope CLI (flags, subcommands, exit codes, primary output) is as close to
   C v1.1.1 as possible; disassemble/reassemble/simulate and the registry/user
   commands appear nowhere in the parser, help, usage, or docs.
3. Custom pseudo-ops work via the single embedded **Rhai** engine; bundled
   scripts are ported.
4. i18n via **Project Fluent**; English output matches the oracle.
5. Documentation site + website generate to static output.
6. **All v1.1.1 release artifacts** build for the five platforms (macOS,
   Linux amd64, Linux i386, win32, win64).
7. Clean `cargo fmt`/`clippy`; documented crates; parity harness green against
   the release binary.
8. *(Deferred)* WASM build/playground — not required for done.

---

## 12. Decisions Log

All initial open questions have been resolved by the project owner. Recorded here
as the authoritative directions for this plan.

| # | Topic | Decision |
|---|-------|----------|
| A1 | Scope | **Assembler only.** Disassembler/reassembler, simulator/debugger, and the package registry are out of scope. |
| A2 | Out-of-scope CLI options | Disassemble/reassemble/simulate options are **omitted entirely** — not parsed, not in help/usage, not documented, no "not supported" message. |
| A3 | Parity bar | **ROM output parity is required**, but **do not replicate odd C quirks** — where C is wrong, do the right thing and document the deviation. |
| A4 | Version | Target **`nessemble@1.1.1`**. |
| B5 | Scripting | **A single embedded language.** Recommendation: **Rhai** (pure-Rust, embeddable, sandboxed) — replaces the JS/Lua/Scheme trio. See [§6.4](#64-scripting-language-choice). |
| B6 | `.so` plugins | **Dropped.** |
| C7 | Servers vs docs | **No server components**, but **generating the website/documentation is required** (static output). See [§6.6](#66-documentation--website). |
| D8 | Platforms | Same as `nessemble`: **macOS, Linux amd64, Linux i386, win32, win64.** See [§5.4](#54-target-platforms--artifacts). |
| D9 | WASM/playground | **Not required now**; may be added later once a functioning WASM build exists. **Deferred.** |
| D10 | Artifacts | Produce the **same artifacts as the v1.1.1 release** (`.pkg`, `amd64.deb`, `i386.deb`, `win32.exe`+`.msi`, `win64.exe`+`.msi`). |
| E11 | i18n | **Still important.** Use the idiomatic-Rust equivalent — **Project Fluent** — to achieve the same result. See [§6.5](#65-i18n-approach). |
| E12 | CLI contract | Stay **as close to the C v1.1.1 CLI as possible.** |
| E13 | Repo | **Fresh start** — `nessemble-rs` contains **no C code**. |
| E14 | Parity source | **No CI build of C.** Parity is checked against the **v1.1.1 release binaries**. |

### Follow-up decisions (resolved)

| # | Topic | Decision |
|---|-------|----------|
| Q-a | Scripting migration | Only the **default `ease` script** needs to exist; port it to the chosen language (**Rhai**). No external/user script ecosystem to preserve. |
| Q-b | Docs/website | The **marketing website is reproduced the same** (from `website/static`); the **docs site uses a clean theme** (mdBook). **Both deploy to GitHub Pages.** |
| Q-c | Locales | Ship **`en-US` only**, but make adding locales easy (the Fluent layout does this). |
| Q-d | Parity host | **Linux parity is authoritative**; macOS/Windows are build/packaging targets. |
| Q-e | Windows artifacts | Reproduce **both** the `.exe` and the `.msi` for each Windows arch. |
| Q-f | Website copy | Landing-page copy mentions **assembler only** — no disassembler/simulator — while keeping the layout/theme identical. |

**All questions are resolved.** The plan is fully specified and ready to execute
from Phase 0.

---

*Prepared as a planning artifact; this repository contains no C source — the
upstream project is used only as a behavioral reference.*
