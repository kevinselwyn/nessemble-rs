# nessemble-rs: A Plan for CDL-Based Runtime Coverage

> Status: **Proposed — planning only.** This document specifies a new
> `nessemble coverage` subcommand that reports **runtime execution coverage** of
> an assembled ROM against a **CDL (Code/Data Logger)** capture from an emulator,
> and **retires the existing `-C`/`--coverage` write-coverage flag** it supersedes
> (§10). The feature is adapted from a `generate-coverage` utility that lives in
> an external NES disassembly project, but re-founded on the assembler's own
> byte-exact source map rather than a regex byte-count estimator (§4). v1 parses
> **FCEUX**
> and **Mesen** NES CDL files and emits **JSON** and **LCOV** reports; **BizHawk**
> and an HTML report are deferred follow-ups (§5, §7). Coverage also extends to
> **Rhai pseudo-op scripts** so unexecuted script branches are visible (§8). No
> code is written yet — this PR is the plan.

---

## 1. Goal

Ship a first-class **runtime coverage** command:

```sh
nessemble coverage path/to/main.asm --cdl capture.cdl            # → coverage.json + coverage.lcov
nessemble coverage path/to/main.asm --cdl capture.cdl --format lcov --out cov.lcov
nessemble coverage path/to/main.asm --cdl capture.cdl --scripts  # include .rhai coverage
```

Given (a) an assembly project and (b) a CDL file an emulator wrote after the ROM
ran, report **which source lines were actually executed or read at runtime** and
which were never touched. The output is machine-readable (JSON and LCOV) so it
drops into CI and coverage services.

Two requirements from the request:

1. **Promote** a `generate-coverage` utility (from an external NES disassembly
   project) directly into nessemble as a built-in command (§4).
2. **Supplant** the current `-C`/`--coverage` flag, which is little-used and of
   limited value (§10).

Plus two scope decisions settled up front (§11): the **CDL format is
abstracted** so FCEUX and Mesen are covered in v1 and BizHawk can follow (§5);
and coverage **extends to Rhai scripts** to surface never-executed script code
(§8).

## 2. Two different things both called "coverage"

The word already means something in this codebase, and the new feature means
something else. Being precise about the difference is the whole design.

| | **`-C` write coverage (today)** | **CDL runtime coverage (this plan)** |
|---|---|---|
| Question answered | How many bytes of each bank did the **assembler emit**? | Which source lines did the **running game** touch? |
| Input | The source alone | The source **plus** an emulator CDL capture |
| When | Assemble time, static | Post-run, dynamic |
| Granularity | Per bank (`covered/total` byte counts) | Per source line (code / data / mixed / unaccessed) |
| Origin | C-reference `get_coverage` parity | external `generate-coverage` utility |
| Fate | **Removed** (§10) | **Added** as `nessemble coverage` |

The `-C` metric is really *disassembly progress* — "how full is the ROM image" —
and duplicates what a disassembly project tracks elsewhere. It says nothing about
whether emitted code ever runs. The new metric is the one people actually want
when they ask "is this routine dead?": it needs a real execution trace, which the
CDL provides.

## 3. Why nessemble is the right home

The external utility lives outside the assembler and pays for it. To attribute
CDL flags to source lines it must **guess byte counts from source text**: it
re-derives each instruction's length from operand syntax, counts `.db`/`.dw`/
`.color` commas, and recomputes `.incbin` ranges with file-size arithmetic. Every
one of those is a re-implementation of logic the assembler already owns exactly, and
each is a place the estimate can drift from the truth (macros, conditional
assembly, custom pseudo-ops, `.align`, and `.incbin` third-argument semantics are
all invisible to a regex).

nessemble is the assembler. It already:

- tracks a **per-ROM-byte write bitmap** (`Assembler.coverage: Vec<bool>`,
  `crates/nessemble-core/src/assemble.rs`) recording exactly which output offset
  each emission wrote, and
- tracks the **current source line** (`Assembler.cur_line: u32`) as it walks the
  program.

Joining those two facts as bytes are emitted yields a **byte-exact source map**
— `(file, line) → (rom_offset, len)` — for free, over the real post-macro,
post-conditional program. That map is the foundation the estimator was
approximating, and it is the compelling reason to build the feature in the
assembler rather than lean on an external heuristic.

## 4. What we are promoting

The external `generate-coverage` utility does three things:

1. **Source map** — walk each PRG `.asm`, follow `.include`, estimate the byte
   count of every emitting line, and record `line → (bank, addr, bytes)`.
2. **Classify** — for each line's byte range, OR together the CDL flags and label
   the line `code` / `data` / `mixed` / `unaccessed`.
3. **Report** — render an Istanbul/NYC-style HTML index + per-file pages.

We keep **step 2's classification model verbatim** (it is the useful part),
**replace step 1** with the assembler's exact source map (§3), and **re-target
step 3** at JSON + LCOV (§7), leaving the HTML renderer as a later port (§7.3).
The CDL bit semantics we adopt unchanged, taken from FCEUX's documented
`xPdcAADC` PRG bit layout:

```
code  ⇐ PRG_CODE (0x01) | PRG_INDIRECT_C (0x10)
data  ⇐ PRG_DATA (0x02) | PRG_INDIRECT_D (0x20) | PRG_PCM (0x40)
```

## 5. The CDL format landscape

The request asks to primarily support FCEUX and investigate Mesen and BizHawk.
Findings, and what they imply for the design:

### 5.1 FCEUX — flat ROM mask (v1)

A CDL file is exactly ROM-sized, one flag byte per ROM byte, **PRG section then
CHR section**, mirroring iNES layout (no header in the CDL). PRG bit layout
`xPdcAADC`; CHR bit layout `xxxxxxRD`. Multiple CDLs merge by bitwise OR. This is
the format the external utility already consumes.

### 5.2 Mesen — flat ROM mask, but bit-incompatible with FCEUX (v1)

Mesen's NES CDL is **also a flat ROM-sized mask** (PRG then CHR), so one reader
handles the container for both. But the two are **not bit-compatible above bit 3**:
they share bit 0 (code), bit 1 (data), and bits 2–3 (bank), then diverge —
FCEUX's bits 4/5/6 are indirect-code / indirect-data / PCM, whereas Mesen's are
indirect-read / DMC / JSR-target. Folding a Mesen file through FCEUX's masks would
therefore **misclassify** (e.g. Mesen's indirect-read would be counted as code via
FCEUX's indirect-code bit). Because nothing in either file's size or content
distinguishes them, the format **cannot be auto-detected** — the emulator must be
stated (§6.4). Each emulator gets its own `code_mask`/`data_mask` on the shared
`FlatMaskCdl` reader. **Action for v1:** confirm Mesen's exact bit positions
against its `CodeDataLogger.h` before shipping the `Mesen` masks.

### 5.3 BizHawk — named-block container (deferred)

BizHawk's CDL is a **different, self-describing container**, not a flat mask: a
header with a magic string and a platform sub-type (e.g. padded `"NES"`), then a
sequence of length-prefixed named blocks (`"PRG ROM"`, `"CHR VROM"`, WRAM, …).
Per-byte flags differ too — `ExecFirst=0x01`, `ExecOperand=0x02`, `Data=0x04`,
`Write=0x08` — so `code ⇐ ExecFirst|ExecOperand`, `data ⇐ Data` (a `Write` bit
distinguishes writes from reads). Supporting it means parsing the container and
selecting the `PRG ROM` block, which is a self-contained follow-up phase behind
the same abstraction (§6.2). **Deferred to a later release.**

### 5.4 Format abstraction

All three reduce to the same interface once parsed: *given a PRG ROM byte offset,
return its `code`/`data` classification.* v1 implements `Fceux` and `Mesen`
(sharing a flat-mask reader with per-emulator masks); the `CdlSource` trait (§6.2)
leaves a clean seam for `BizHawk`.

| Emulator | Container | PRG flags (code / data) | v1? |
|---|---|---|---|
| FCEUX | flat ROM mask | `0x01\|0x10` / `0x02\|0x20\|0x40` | ✅ |
| Mesen | flat ROM mask | shares bits 0–3; diverges 4–6 (verify) | ✅ |
| BizHawk | named-block header | `0x01\|0x02` / `0x04` | ⏳ later |

Because FCEUX and Mesen are indistinguishable by inspection (§5.2), the emulator
is selected explicitly (`--emulator`, default `fceux`); there is no `auto` in v1
(it only becomes meaningful once BizHawk's detectable header lands).

## 6. Architecture

Three new pieces in core plus a thin CLI command; nothing in the hot assemble
path changes unless coverage is requested.

### 6.1 Byte-exact source map (core)

Add an **opt-in** source-map recorder to the assembler, off by default so normal
assembly is untouched:

```rust
// nessemble-core
pub struct SourceSpan { pub file: Arc<str>, pub line: u32,
                        pub rom_offset: usize, pub len: usize }
pub struct SourceMap { pub spans: Vec<SourceSpan> }   // in emission order
```

- Gate recording behind an `Options` flag (e.g. `Options::source_map: bool`);
  when set, every byte-emitting site pushes a `SourceSpan` keyed by the current
  `(file, cur_line)` and the write's `rom_offset`/length — the same sites that
  already flip `coverage[offset] = true`.
- Expose it on the `Assembly` result next to `coverage`
  (`pub source_map: Option<SourceMap>`), mirroring how `coverage` is exposed
  today.
- **Offset reconciliation (verify):** the assembler's internal ROM buffer is
  `prg*BANK_PRG + chr*BANK_CHR` bytes — PRG then CHR, **no iNES header** — which
  is already the CDL's coordinate space, so `rom_offset` maps to a CDL index
  directly. Confirm header/trainer handling (`offset_trainer`) so the mapping
  holds when a trainer is present.

This makes the source map a property of the real assembled program (macros,
conditionals, custom pseudo-ops, `.incbin` all resolved), which is precisely what
the external estimator could not see.

### 6.2 CDL source + classifier (core)

```rust
pub enum CdlCls { Code, Data, Mixed, Unaccessed }

pub trait CdlSource {                       // one impl per emulator format
    fn prg_class(&self, prg_offset: usize) -> (bool /*code*/, bool /*data*/);
    fn prg_len(&self) -> usize;
}

pub struct FlatMaskCdl { bytes: Vec<u8>, code_mask: u8, data_mask: u8 } // FCEUX + Mesen
// pub struct BizHawkCdl { ... }            // later phase

pub fn classify_span(cdl: &dyn CdlSource, span: &SourceSpan) -> CdlCls;
```

`classify_span` ORs the CDL flags across `span`'s byte range and returns the
4-way class — a direct port of the external utility's range classifier.
`FlatMaskCdl` constructed with FCEUX or Mesen masks covers §5.1–5.2.

**Stale-CDL guard.** A CDL is a capture of one specific build and carries no ROM
identity, so a report against drifted source silently misaligns. nessemble knows
the true PRG size from the assembled header, so the command **hard-errors when the
CDL's PRG section size does not equal the assembled PRG size** — the strongest
check the format permits. No hash check is attempted (a CDL has nothing to hash
against); the error message notes that equal sizes still do not guarantee the same
build, so the CDL must come from the ROM this source assembles to.

### 6.3 Report model + emitters (core)

Aggregate per file and per line into a neutral model, then serialize:

```rust
pub struct LineCoverage { pub line: u32, pub cls: CdlCls }
pub struct FileCoverage { pub path: String, pub lines: Vec<LineCoverage>,
                          pub code: u32, pub data: u32, pub mixed: u32, pub unaccessed: u32 }
pub struct CoverageReport { pub files: Vec<FileCoverage> }   // asm and .rhai alike
```

- **JSON** (`--format json`, default alongside lcov): the full model — every file,
  every classified line, per-file and total rollups. Richest form; keeps the
  4-way class the CDL affords.
- **LCOV** (`--format lcov`): standard `SF:` / `DA:line,hits` / `LF` / `LH`
  records. LCOV is boolean per line, so map **hit = 1 when `cls ∈ {Code, Data,
  Mixed}`**, **0 when `Unaccessed`**; non-emitting lines (labels, comments,
  directives) are omitted, exactly as they carry no source span. Language-agnostic,
  so `.rhai` files (§8) slot in with no format change.
- A one-line **stdout summary** (covered/total lines, overall %) is printed for
  humans regardless of `--format`; it is not the deliverable, just a convenience.

### 6.4 CLI surface

New subcommand in `crates/nessemble-cli` (joining `init`, `scripts`, `reference`,
`lsp`, `format`):

```text
nessemble coverage <infile.asm> --cdl <file.cdl> [options]

  --cdl <file>          CDL capture to read (required; repeatable → OR-merge)
  --emulator <name>     fceux | mesen         (default: fceux; bizhawk later)
  --format <fmt>        json | lcov | all      (default: all)
  --out <path|dir>      output file, or directory for multi-format (default: cwd)
  --scripts             also report coverage for .rhai pseudo-op scripts (§8)
```

`coverage` assembles `<infile.asm>` with `Options::source_map = true` (and NES
format), loads and merges the CDL(s), classifies, and writes the report(s). It is
a read/report command: it never writes a ROM. `--emulator` is explicit (default
`fceux`) because FCEUX and Mesen flat masks are indistinguishable by inspection
(§5.2); no `auto` mode ships in v1. When BizHawk lands, its detectable container
header enables an `auto` that separates container from flat mask.

## 7. Report formats & the HTML question

- **v1 emits JSON + LCOV** (§6.3), the machine-readable pair chosen for CI and
  coverage-service ingestion (§11).
- **HTML is deferred.** The external utility's HTML renderer (sortable index,
  per-file colour-coded pages) is good and can be ported later as a third
  emitter over the same `CoverageReport` model — but it is not v1. Keeping the
  model emitter-agnostic (§6.3) is what makes that port additive.
- The web/wasm build (`crates/nessemble-wasm`) is out of scope for v1; the report
  model is plain data and could back a browser view later.

## 8. Rhai script coverage

The request extends coverage to Rhai pseudo-op scripts (`crates/nessemble-script`,
built on Rhai) to reveal script code that never runs during assembly. Runtime CDL
cannot see this — scripts execute *inside the assembler*, not on the NES — so this
is a distinct instrumentation path that lands in the same report.

**Approach — Rhai's debugger interface:**

- Rhai's `debugging` feature exposes `Engine::register_debugger`, whose callback
  fires when stepping into/over each statement and expression with the node's
  source `Position` (line/column). That position stream is the coverage
  numerator.
- Build the **coverable-line denominator** by statically walking the compiled
  `AST` once (enumerate statement positions per script) — so a script that is
  registered but whose `custom()` is never invoked still shows 0 % rather than
  vanishing.
- During a coverage run, every `custom(ints, texts)` invocation the assembly
  triggers (`nessemble_script::run`) executes on an **instrumented engine** that
  records hit positions; hits are unioned across all invocations of that script.
- Emit each script as a `FileCoverage` (line → hit/not-hit) into the same
  `CoverageReport`, so JSON and LCOV carry asm and script coverage together.

**Scope — user scripts only.** Coverage reports **only on-disk `.rhai` files the
project supplies** via `-p pseudo.txt`, keyed by absolute path. The **bundled**
scripts installed to `~/.nessemble/scripts` (e.g. `ease.rhai`) are **excluded** —
they are nessemble's own, not the project's code under test. Inline script
snippets carried directly in a mapping file (no standalone path) are **out of
scope for v1**, since LCOV/JSON key each entry by file path; this is a noted
limitation, not a blocker.

**Cost containment:** the `debugging` feature adds per-node overhead and is only
wanted under `--scripts`. Plan: put it behind a Cargo feature and construct the
instrumented engine **only** on the coverage path (the normal assemble engine in
`nessemble_script::engine` is unchanged), so default builds and hot assembly pay
nothing. Confirm the `debugging` feature composes with the existing
`default-features = false` Rhai setup and the `fs`/wasm feature matrix before
committing to it; if the matrix is awkward, a fallback is a lighter
`on_progress`-based line sampler, noted here as plan B.

## 9. Phasing

Each phase leaves the tree green (`cargo test` + `cargo clippy` + `xtask parity`)
and ships as its own changeset, consistent with prior plans.

- **Phase 0 — source map seam.** Add `Options::source_map`, `SourceMap`/
  `SourceSpan`, and recording at the emission sites; expose on `Assembly`.
  No CLI yet. Guardrail: with the flag off, byte output and parity are identical
  (§11); with it on, spans reconstruct the exact written ranges (test asserts the
  union of spans equals the write bitmap).
- **Phase 1 — CDL core.** `CdlSource` trait, `FlatMaskCdl` (FCEUX masks),
  `classify_span`, `CoverageReport` model. Unit-tested against the ported
  range-classification cases.
- **Phase 2 — `coverage` command + JSON/LCOV.** Wire the subcommand, assemble
  with the source map, classify, emit JSON and LCOV; stdout summary. Mesen masks
  and `--emulator` selection.
- **Phase 3 — remove `-C`.** Delete the flag, `render_coverage`, and the
  write-`CoverageReport` type per §10, and drop the one CLI test that exercises it.
  Small and self-contained (the parity harness does not touch `-C`).
- **Phase 4 — Rhai script coverage.** `--scripts`, debugger-based instrumentation
  behind a feature; scripts join the report (§8).
- **Later (own releases):** BizHawk container reader (§5.3); HTML report port
  (§7.3).

Phases 0–2 are the minimum shippable feature; 3 completes the "supplant" ask; 4
completes the "extend to scripts" ask.

## 10. Retiring `-C`/`--coverage`

Per the settled decision (§11), **the old flag is removed, not merely hidden.**

- Delete the `-C`/`--coverage` CLI flag and its guarded print block
  (`crates/nessemble-cli/src/main.rs`), the `render_coverage` renderer and the
  write-`CoverageReport` type + `coverage_report()` producer
  (`crates/nessemble-core`), and the `-C` usage doc entry
  (`docs/src/usage.md §-C`).
- **Parity impact — none.** Although `-C` mirrors the C reference's
  `get_coverage`, the parity harness never invokes it: the golden-ROM comparison
  in `xtask` passes no `--coverage` flag to the oracle (the only `-C` in `xtask`
  is an unrelated `tar` flag). The **sole** coupling is one self-contained CLI
  integration test, `coverage_reports_per_bank_for_ines_file_output`
  (`crates/nessemble-cli/tests/cli.rs`), which is deleted with the feature.
  Removal is still its own phase for a clean, revertible changeset, but it is a
  one-test change, not a fixture migration.
- The internal per-byte **write bitmap** (`Assembler.coverage`) **stays** — the
  new source map is built from the very same emission sites, so this is shared
  plumbing, not the removed feature.

## 11. Decisions

Settled with the requester before drafting:

1. **Fate of `-C`:** **removed entirely** (§10). Confirmed low-risk — the parity
   harness does not exercise it; removal drops one CLI test.
2. **Format scope:** **FCEUX + Mesen in v1** (one flat-mask reader, per-emulator
   masks); **BizHawk deferred** to a later phase behind `CdlSource` (§5).
3. **Report output:** **machine-readable JSON + LCOV** in v1 (§6.3); HTML port
   deferred (§7.3).
4. **Emulator selection:** **explicit `--emulator`, default `fceux`**, no `auto`
   in v1 — FCEUX and Mesen flat masks are indistinguishable and bit-incompatible
   above bit 3 (§5.2, §6.4).
5. **Stale-CDL guard:** **hard-error on a PRG size mismatch**; no hash check
   (a CDL has no ROM identity to hash) (§6.2).
6. **Rhai scope:** **user-supplied `-p` `.rhai` files only**, keyed by absolute
   path; bundled `~/.nessemble` scripts and inline snippets excluded in v1 (§8).

Open items to resolve during implementation (not blockers to the plan):

- Exact Mesen PRG bit positions vs. FCEUX, verified against Mesen source (§5.2).
- Rhai `debugging` feature vs. the `default-features = false` + `fs`/wasm matrix;
  `on_progress` sampler as plan B (§8).
- iNES header/trainer offset reconciliation between the internal ROM buffer and
  the CDL coordinate space (§6.1).

## 12. Out of scope

- **CHR coverage.** CDLs carry CHR draw/read flags, but the disassembly source
  map is PRG-only; CHR is ignored, matching the external util. Could be a later
  addition (CHR banks → tile usage) but not here.
- **Capturing** CDLs. nessemble consumes an emulator's CDL; it does not run or
  emulate the ROM.
- **BizHawk** and **HTML** in v1 (both explicitly deferred, §5.3/§7.3).
- **wasm/web** coverage UI (§7).
- Any change to assembled ROM bytes, existing diagnostics, or the parity corpus.
  The `-C` removal (§10) is CLI/doc-only and does not touch the parity path.
