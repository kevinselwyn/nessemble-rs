# nessemble-rs: A Multi-Phase Plan to Reimplement `nessemble` in Rust

> Status: **Draft for review.** This document is a proposal produced from a
> read-only analysis of the upstream C project
> [`kevinselwyn/nessemble`](https://github.com/kevinselwyn/nessemble) (v1.1.1).
> It defines scope, target architecture, and a phased migration path. See
> [Open Questions](#12-open-questions--decisions-needed) at the end — several
> answers there will reshape scope and priorities.

---

## 1. Executive Summary

`nessemble` is a 6502 assembler / disassembler / simulator targeting the
Nintendo Entertainment System (NES), written in C. It is a mature, feature-rich
CLI tool (v1.1.1) that also ships as a WebAssembly module and integrates with a
package registry, embedded scripting engines, and image/audio importers.

This plan proposes reimplementing the tool in Rust as **`nessemble-rs`**, a
Cargo workspace of focused crates, delivered in **10 phases**. The strategy
prioritizes:

1. **Behavioral parity first** for the core assembler (the most-used path),
   validated by *differential testing* against the original C binary and the
   existing golden-ROM test corpus.
2. **Incremental, independently shippable phases**, each with its own tests and
   acceptance criteria.
3. **Replacing bespoke C machinery** (hand-rolled HTTP, JSON, deflate, hashing,
   flex/bison) with well-maintained Rust crates where semantics allow.
4. **Deferring or re-scoping** the heaviest, lowest-core-value subsystems
   (three embedded scripting languages, the web/registry server stack) pending
   product decisions.

**Scope note:** the **disassembler/reassembler**, the **simulator/debugger**,
and the **package-registry functionality** (install/publish/search + user auth)
are **explicitly out of scope** for this reimplementation. `nessemble-rs`
targets the **assembler** (plus its media importers, CLI, config, and WASM
build). The sections below retain a full inventory of the original C tool for
context, but out-of-scope subsystems are marked as such and are **not** part of
any delivery phase.

The core first-party C code is **~12.7k LOC** plus **~770 lines** of flex/bison
grammar. Third-party vendored code is **~105k LOC** (mostly the Duktape JS
engine, Lua 5.1.5, and TinyScheme), almost all of which maps to existing Rust
crates or is out of scope.

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

### 2.5 Build targets

- Native Linux/macOS/Windows (mingw) binaries.
- **WebAssembly / JS** module via Emscripten (`nessemble.js`, used by the
  docs "playground").
- Distribution packaging: `.deb`, macOS `.pkg`, Windows `.msi`, npm package.

> Note: the repository also contains **Python/Flask server components** (docs,
> registry, website, CDN) and a **TypeScript docs frontend**. These are
> *server-side/website* code, not part of the CLI, and are considered **out of
> scope** for the Rust reimplementation unless explicitly requested.

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
  → `cfg!`/target features + `wasm32` target.

---

## 4. Third-Party Dependency Mapping (C → Rust crates)

Vendored C (~105k LOC) and system deps mapped to the Rust ecosystem:

| C dependency | Purpose | Proposed Rust replacement |
|--------------|---------|---------------------------|
| flex / bison | lexer / parser generators | `logos` (lexer) + hand-written parser, or `lalrpop` |
| `getopt_long` | CLI parsing | `clap` (derive) |
| stb_image / stb_image_write | PNG decode/encode | `image` (or `png`) |
| gettext (`i18n.c`) | translations | `fluent`/`gettext` crate, or defer |
| QR code (`reference.c`) | terminal QR | `qrcode` crate |
| pager (`pager.c`) | `$PAGER`/less | small shell-out, or `minus` |
| Emscripten | WASM build | native `wasm32-unknown-unknown` + `wasm-bindgen` |
| Duktape | embedded JavaScript (custom pseudo-ops) | `boa_engine` / `rquickjs` — *or drop* (see Q) |
| Lua 5.1.5 | embedded Lua (custom pseudo-ops) | `mlua` (Lua 5.1 mode) — *or drop* |
| TinyScheme | embedded Scheme (custom pseudo-ops) | `steel` / custom — *or drop* |
| shared-object (`so.c`) | native plugin pseudo-ops | `libloading` — *or drop* |

**Out-of-scope dependencies** (only needed by the excluded package-registry
subsystem — listed for completeness):

| C dependency | Purpose | Rust equivalent (if ever re-scoped) |
|--------------|---------|-------------------------------------|
| jsmn | JSON parsing (registry) | `serde` + `serde_json` |
| udeflate `deflate.c` | gzip/inflate for tar.gz (registry) | `flate2` |
| tar handling (`zip.c`) | untar registry packages | `tar` crate |
| hand-rolled SHA/HMAC (`hash.c`) | auth signing (registry) | `sha2` + `hmac` |
| raw-socket HTTP (`http.c`) | registry/user API client | `ureq` / `reqwest` |

---

## 5. Goals, Non-Goals & Guiding Principles

### 5.1 Goals

- **G1 — Assembler parity:** byte-for-byte identical ROM output vs C v1.1.1 for
  the in-scope test corpus (`test/examples`, `test/opcodes`, `test/nerdy-nights`,
  `test/errors`) and for the same CLI surface.
- **G2 — Memory safety & maintainability:** idiomatic Rust, no global mutable
  state, structured error/diagnostics, thorough tests.
- **G3 — Cross-platform + WASM:** Linux/macOS/Windows binaries and a
  `wasm32` library retaining the (assembler) playground use case.
- **G4 — CLI compatibility:** same flags, subcommands, exit codes, and
  primary stdout/stderr contract for the in-scope surface so existing scripts
  keep working. Out-of-scope subcommands/flags are either omitted or emit a
  clear "not supported" message (decision pending — see Q).

### 5.2 Non-Goals / Out of Scope (unless requested)

- The **disassembler / reassembler** (`-d`, `-R`; `disassemble.c`).
- The **simulator / debugger** (`-s`; `simulate.c`, `simulate/opcode.c`,
  `simulate/illegal.c`, and the REPL).
- The **package-registry functionality**: the package manager
  (`install`/`uninstall`/`publish`/`info`/`ls`/`search`), the `registry`
  get/set command, user/auth commands (`adduser`/`login`/`logout`/
  `forgotpassword`/`resetpassword`), and the underlying HTTP client, JSON,
  tar/gzip, and HMAC machinery that exist solely to serve them
  (`registry.c`, `api.c`, `user.c`, `http.c`, `json.c`, `zip.c`, `hash.c`).
- Reimplementing the **Python Flask registry/website/docs/CDN servers**.
- Reimplementing the **TypeScript docs frontend**.
- Bug-for-bug replication of *internal* quirks that no test observes (we will
  match observable behavior, and flag intentional deviations).

> These exclusions are intentional design decisions for this effort, not
> oversights. If any is later re-scoped, the mapping tables above and the phase
> list below can be extended without disturbing the assembler core.

### 5.3 Principles

- **Differential testing is the source of truth.** Keep a pinned build of C
  `nessemble` and compare outputs continuously.
- **Vertical slices over horizontal layers** where possible: get a minimal
  end-to-end assemble path working early, then widen.
- **One behavioral change per PR**, each green against the corpus.
- **Preserve the file/CLI contract** even when the internals change.

---

## 6. Target Rust Architecture

### 6.1 Workspace layout (proposed)

```text
nessemble-rs/
├─ Cargo.toml                 # workspace
├─ crates/
│  ├─ nessemble-core/         # lexer, parser, AST, assembler, symbol table,
│  │                          #   iNES/banking, pseudo-ops, expressions
│  ├─ nessemble-isa/          # 6502 opcode tables (from opcodes.csv), modes
│  ├─ nessemble-media/        # PNG/CHR, palette, RLE, WAV/DPCM importers
│  ├─ nessemble-script/       # custom pseudo-op scripting host (feature-gated)
│  ├─ nessemble-cli/          # clap CLI, dispatch, i18n, pager, reference, init
│  └─ nessemble-wasm/         # wasm-bindgen wrapper for the (assembler) playground
└─ tests/                     # differential + golden-ROM harness
```

Rationale: the ISA tables and core types anchor the assembler, so they live in a
leaf crate. Scripting is optional (`--features`) so the default build is small
and dependency-light. The `disasm`, `sim`, and `registry` crates are
**intentionally absent** — those subsystems are out of scope (§5.2); the layout
leaves clean seams to add them later if re-scoped.

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

---

## 7. Phased Migration Plan

Each phase lists scope, key deliverables, and acceptance criteria. Phases are
ordered so that the highest-value core lands first and each builds on the last.

### Phase 0 — Foundations & test harness
- **Scope:** Cargo workspace skeleton; crate stubs; CI (fmt, clippy, test);
  pin & build reference C `nessemble` v1.1.1 in CI for differential testing;
  port the existing test corpus into a Rust harness that shells out to both
  binaries and diffs outputs; import `opcodes.csv` as data (build script or
  committed generated table) into `nessemble-isa`.
- **Deliverables:** green empty workspace; `xtask`/harness that can run the
  corpus against the C binary and record golden outputs.
- **Acceptance:** CI runs; harness produces baseline golden ROMs/listings from
  the C tool.

### Phase 1 — Lexer + expression/number evaluation
- **Scope:** `logos` lexer covering all tokens in `nessemble.l`; number bases,
  char/defchr literals, macro-arg tokens; expression parser (Pratt) with the
  full operator set, `HIGH/LOW/BANK`, parens; standalone evaluator.
- **Acceptance:** unit tests for tokenization + expression results matching C
  semantics (including integer division/`pow` behavior and truncation).

### Phase 2 — Core assembler: instructions, symbols, two-pass, raw output
- **Scope:** addressing-mode selection & opcode emission (`instructions.c`),
  symbol table (constants, labels, local/anonymous labels, `->` scoping),
  two-pass driver, ROM/offset/coverage buffers, `.org`, raw (non-iNES) output,
  `-c`/check mode, error/exit-code parity.
- **Acceptance:** byte-identical output for the non-iNES / opcode subset of
  `test/opcodes` and simple `test/examples`; `test/errors` cases produce
  matching failures.

### Phase 3 — iNES, banking, segments & data/core directives
- **Scope:** iNES header + trainer, PRG/CHR banking, `.segment`/`.prg`/`.chr`,
  `.db`/`.dw`/`.ascii`/`.fill`/`.hibytes`/`.lobytes`/`.checksum`/`.random`/
  `.color`/`.enum`/`.rs`/`.rsset`, `.inesprg/chr/map/mir/trn`.
- **Acceptance:** full `test/examples` (excluding asset/scripting/macro/include
  cases) and `test/nerdy-nights` produce byte-identical `.nes` ROMs.

### Phase 4 — Macros, conditionals, includes
- **Scope:** `.macro`/`.endm`, `.macrodef` text macros, macro args (`\1`,`\#`,`\@`),
  `.if`/`.ifdef`/`.ifndef`/`.else`/`.endif` (nested), `.include` (nested, depth
  limit), stdin/piped input, list-file output (`-l`).
- **Acceptance:** macro/include/conditional examples byte-identical; list files
  match.

### Phase 5 — Asset importers (media)
- **Scope:** `nessemble-media`: `.incbin`, `.incpng` (+palette matching),
  `.incpal`, `.incrle`, `.incwav` (DPCM), `.font`, `.defchr`, `.chr`.
- **Acceptance:** `incpng/incpal/incrle/incwav/font/defchr` examples
  byte-identical; PNG/WAV edge cases covered.

### Phase 6 — CLI completeness, config, i18n, reference, init
- **Scope:** in-scope `clap` CLI surface & exit codes (assemble/check/coverage +
  `init`, `config`, `reference`, `scripts`, `--version`/`--license`/`--help`);
  `~/.nessemble` layout; `reference` (+QR); pager; i18n framework (strings can be
  ported incrementally). Out-of-scope subcommands/flags (`-d`/`-R`/`-s`,
  `registry`, package-manager, user/auth) are omitted or emit a clear
  "not supported in this build" message (decision pending — see Q).
- **Acceptance:** CLI help/usage/exit-code parity for the in-scope surface;
  `init` output matches; config round-trips.

### Phase 7 — WASM build, scripting host, packaging & cutover
- **Scope:** `wasm32` library + `wasm-bindgen` bindings for the (assembler)
  playground; scripting host (`nessemble-script`) — scope per product decision
  (drop / JS-only / all three); custom pseudo-op resolution & `scripts` install;
  distribution packaging (`.deb`/`.pkg`/`.msi`/npm); docs update.
- **Acceptance:** the assembler playground works against the WASM build; chosen
  scripting path passes `test/examples/custom`/`ease`; release artifacts build.

> **Removed from scope:** the earlier drafts included a disassembler/reassemble
> phase, a simulator/debugger phase, and a registry/package-manager phase. These
> are now out of scope (§5.2) and have been dropped from the plan.

---

## 8. Testing & Validation Strategy

- **Differential (oracle) testing:** for every in-scope corpus input, run both
  the pinned C binary and `nessemble-rs`, and assert byte-identical ROMs /
  identical exit codes & key stderr.
- **Golden files:** commit C-generated outputs as goldens so CI does not require
  rebuilding the C tool every run (but a scheduled job re-verifies against it).
- **Existing corpus reuse (in scope):** `test/opcodes` (343 files),
  `test/examples` (157), `test/nerdy-nights` (32), `test/errors` (62) — port the
  Python drivers into the Rust harness. *(`test/integration` targets the
  simulator and `test/registry` targets the package registry; both are out of
  scope and excluded.)*
- **Unit tests** per crate (lexer, expression eval, addressing modes,
  media importers).
- **Property tests** (`proptest`) for expression evaluation and iNES/banking
  offset math.
- **Fuzzing** (`cargo-fuzz`) on the parser and PNG/WAV asset loaders.
- **Fixed-cap behaviors:** explicitly test the observable limits/errors the C
  tool enforces (include depth, symbol/macro caps) so we match or consciously
  change them.

---

## 9. Risk Register

| Risk | Impact | Mitigation |
|------|--------|-----------|
| Undocumented assembler quirks not covered by tests | Silent output divergence | Broad differential testing beyond the shipped corpus; fuzz-generated inputs run through both tools |
| flex/bison edge cases (start-conditions, greedy rules) | Parser mismatch | Hand-written parser mirrored against grammar; targeted lexer tests |
| Scripting engines (Duktape/Lua/TinyScheme) are huge | Scope blow-up | Feature-gate & likely re-scope (see Q); Lua via `mlua` cheapest to retain |
| Floating-point in expressions (`pow`, `/`) | Off-by-one divergence | Match C integer-cast semantics exactly; property tests |
| WASM parity (Emscripten FS/EM_ASM hooks) | Playground breakage | Redesign JS interop via `wasm-bindgen`; keep the assembler API used by playground |
| i18n/gettext catalogs | Localization gaps | Framework early, translate strings incrementally |
| Out-of-scope flags/subcommands still invoked by users/scripts | Confusing failures | Decide (Q) between omitting them vs. a clear "not supported" message + non-zero exit |

---

## 10. Suggested Sequencing & Parallelism

- **Critical path:** Phase 0 → 1 → 2 → 3 → 4 (the assembler) delivers the bulk
  of user value and unblocks everything else.
- **Parallelizable after Phase 2/3:** `nessemble-media` (Phase 5) shares only the
  core/ISA crates and can proceed independently of the CLI work.
- **Independent track:** packaging/WASM/scripting (Phase 7) depends mostly on the
  CLI shell (Phase 6), not on assembler internals.

---

## 11. Success Criteria (Definition of Done for the migration)

1. `nessemble-rs` **assembles** the entire in-scope test corpus with byte parity
   vs C v1.1.1 (assemble/check/coverage, incl. media importers).
2. In-scope CLI flags, subcommands, and exit codes match documented behavior;
   out-of-scope commands behave per the agreed decision (omitted vs. clear
   "not supported").
3. Cross-platform native builds + a working `wasm32` assembler playground module.
4. Clean `cargo fmt`/`clippy`; documented crates; CI differential suite green.
5. Scripting scope delivered per the agreed product decision.

---

## 12. Open Questions / Decisions Needed

These materially affect scope, sequencing, and effort. Grouped by priority.

### A. Scope & priorities
1. **Confirmed scope:** the effort targets the **assembler** (assemble / check /
   coverage + media importers + CLI/config/init/reference + WASM). The
   **disassembler/reassembler**, **simulator/debugger**, and **package-registry
   functionality** are **out of scope** (§5.2). Please confirm, or flag any of
   these you actually want re-included.
2. **Out-of-scope CLI flags/commands:** for `-d`/`-R`/`-s`, `registry`, the
   package manager, and user/auth — should `nessemble-rs` **omit them entirely**
   (unknown-flag error) or **recognize them and print a clear "not supported in
   this build" message** with a non-zero exit? (Matters for scripts that call
   the old binary.)
3. **Parity bar:** Is **byte-for-byte ROM parity** with C v1.1.1 a hard
   requirement, or is "correct + documented behavior" acceptable where the C
   tool has quirks?
4. **Version pin:** Should we target the current `master` of upstream, the
   v1.1.1 release, or the latest published binary? (Analysis here is v1.1.1.)

### B. Scripting subsystem (largest remaining scope lever)
5. **Custom pseudo-op scripting:** Keep it at all? If so, which engines —
   **all three** (JS/Lua/Scheme), **JS-only**, **Lua-only**, or replace with a
   single modern embedded language? (This decides whether we pull in
   `boa`/`rquickjs`, `mlua`, and/or `steel`.)
6. **Native `.so` plugins** (`scripting/so.c`): retain via `libloading`, or drop
   for safety/portability?

### C. Ecosystem
7. **Server components** (Python Flask registry/website/docs/CDN + TS frontend):
   confirmed **out of scope**? *(The client-side package-registry commands are
   already out of scope per §5.2; this confirms the server side too.)*

### D. Platforms & distribution
8. **Target platforms:** Which must ship — Linux, macOS (Intel/ARM), Windows,
   WASM? Any 32-bit or specific-MSRV requirement?
9. **WASM/playground:** Must the docs "playground" keep working (assembler-only,
   now that the simulator/disassembler are out of scope)? If so, is a
   `wasm-bindgen` API redesign acceptable, or must the existing JS module API
   be preserved exactly?
10. **Packaging:** Do we need the same artifacts (`.deb`, `.pkg`, `.msi`, npm),
    or is `cargo install` / GitHub releases sufficient initially?

### E. Compatibility & process
11. **i18n:** Retain gettext-style translations (any locales beyond `en-US`
    actually used?), or defer localization?
12. **CLI contract:** Must every in-scope flag/exit-code/stdout string match
    exactly (for downstream scripts), or is a cleaned-up but documented CLI
    acceptable?
13. **Repo strategy:** Build `nessemble-rs` in this repo alongside/replacing the
    C tree, or as a fresh tree? Any commit/PR granularity or licensing
    (GPL — `COPYING`) constraints to preserve?
14. **Reference-tool availability:** Can CI build the C `nessemble` (flex, bison,
    Emscripten toolchains) for differential testing, or should we rely
    solely on committed golden files?

---

*Prepared as a planning artifact; no application code changed in this PR.*
