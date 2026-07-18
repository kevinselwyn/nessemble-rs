# nessemble-rs: A Plan for an Idiomatic-Rust Refactor

> Status: **Planned — not started.** This document scopes a series of small,
> behavior-preserving refactors that make the codebase read more like idiomatic
> Rust, drawn from a staff-level review of the workspace (see
> [§2](#2-context--where-this-came-from)). The codebase is already in very good
> shape — `clippy::pedantic` is enabled workspace-wide and passes clean, `unsafe`
> is `forbid`-den, and every crate is thoroughly documented — so this is polish,
> not rescue. The work is split into **phases, each shipped as its own patch
> release** (a single `nessemble: patch` changeset per phase), so any one can land
> or be reverted independently. Phases 1–5 are the core cleanup; Phases 6–7 are
> **optional** and constrained by the byte-parity mandate.

---

## 1. Goal

Reduce copy-paste and C-isms in the Rust sources **without changing any observable
behavior**: the assembled ROM bytes, the CLI/LSP/wasm outputs, the diagnostics,
and the parity corpus (**122/122**) must be identical before and after every
phase. Each phase leaves the tree green (`cargo test` + `cargo clippy` +
`cargo run -p xtask -- parity`) and ships as one patch version.

Non-goals: no new features, no new user-facing options, no dependency changes, no
churn to the assembly language or its output. This plan touches only *how* the
Rust is written, never *what* the tool does.

## 2. Context — where this came from

A review of all nine crates (~15k lines) found the codebase overwhelmingly
idiomatic; most apparent non-idiomaticity is **deliberate parity** with the C
reference implementation (uniform `i64` arithmetic, sentinel bytes, fixed-size
nesting arrays), documented as such at each site. The review nonetheless
surfaced eleven concrete polish opportunities. None are correctness bugs; the
highest-value ones simply remove real duplication. This plan turns that list into
scheduled, individually-shippable work.

The eleven findings, with the phase that owns each:

| # | Finding | Severity | Phase |
|---|---------|----------|-------|
| 1 | `exec_instruction` repeats an opcode-resolve/error/bail dance ~8× | Med | 1 |
| 6 | `opcode_byte` collapses an `Option` to a `0xFF` C sentinel | Low-Med | 1 |
| 2 | ~22 `InesXxx(Expr)` variants triplicated across three files | Med | 2 |
| 4 | Conditional state as a fixed `[bool; N]` + manual depth guards | Med | 3 |
| 8 | Token-class → id mapping duplicated in 3 crates | Low | 4 |
| 5 | `consolidate_data` clones a `Vec` repeatedly just to read it | Low-Med | 5 |
| 7 | `RcOptions::apply` — 12× `if let Some(x) = … { base.x = x }` | Low | 5 |
| 10 | `&args[args.len().min(1)..]` obscure slice idiom (2 sites) | Low | 5 |
| 11 | Single-variant `AssembleError` enum forces irrefutable matches | Low | 5 |
| 3 | `Ines` models flags/bitfields as `i64` throughout | Med | 6 (opt) |
| 9 | `thread_local!` i18n catalog: locale is per-thread | Low | 7 (opt) |

## 3. Guardrails (every phase)

These are the invariants each phase must preserve; they are also the acceptance
tests for the phase.

- **Parity.** `cargo run -p xtask -- parity` stays **122/122** — the load-bearing
  safety net. The golden `.rom` corpus is byte-exact, so any change to encoding,
  header layout, or emission order is caught here.
- **Byte preservation.** The formatter's assemble-original-vs-formatted ROM
  equality test (`tooling.rs`) and the assembler's `tests/corpus.rs` continue to
  pass unchanged.
- **No new clippy noise.** `cargo clippy --workspace --all-targets` stays clean
  under the existing pedantic set (no new `#[allow]`s without justification).
- **Behavior-only-neutral diffs.** Public type *shapes* in `nessemble-core` may
  change (they are an internal library surface, not a stability contract — see
  [§7](#7-decisions)), but the *values* they carry and every user-facing output
  must be identical. If a phase is found mid-implementation to alter any
  CLI/LSP/wasm observable output, it **escalates from patch to minor** and the
  changeset says why.
- **One phase, one changeset, one PR.** Each phase adds a `nessemble: patch`
  changeset under `.changeset/` with an author-written changelog line, and lands
  as an independent PR.

## 4. Phased plan

Ordered highest-value-first, so the structural wins land while the code is fresh,
then the cross-cutting contract, then mechanical polish, then the two optional
byte-parity-adjacent items.

### Phase 1 — Instruction-encoding cleanup (#1, #6) · patch

**Files:** `crates/nessemble-core/src/assemble.rs`.

`exec_instruction` (`:1351`) repeats this shape in nearly every `Operand` arm:

```rust
let op = self.get_opcode(&mnem, MODE);
if op.is_none() && !self.mnemonic_exists(&mnem) { return; }
if op.is_none() { return; }
self.write_byte(Self::opcode_byte(op));
```

Introduce one resolver that records the right diagnostic and returns the opcode
byte or bails:

```rust
/// Resolve `mnem` in `mode`, recording the appropriate error and returning
/// `None` when it can't be emitted (unknown mnemonic, or wrong mode).
fn resolve_opcode(&mut self, mnem: &str, mode: AddressingMode) -> Option<u8> { … }
```

Each arm collapses to `let Some(op) = self.resolve_opcode(&mnem, mode) else {
return; };`. The register→mode selection (`if reg == 'X' { … } else { … }`,
repeated three times for zero-page/absolute/indirect indexing) folds into a small
helper too. Because the `Option` now flows all the way to `write_byte`, the
`opcode_byte(idx: Option<&Opcode>) -> u8` sentinel (`:1344`, "matches C's
`(unsigned int)(-1)` low byte") is deleted along with **#6** — no arm ever needs
the `0xFF` fallback because a `None` short-circuits before emission.

**Care:** the `ZeroPage` arm (`:1423`) deliberately emits *without* an existence
check (mirroring the reference); keep that asymmetry explicit rather than routing
it through the common resolver.

**Verify:** existing `assemble.rs` unit tests (unknown-opcode, invalid-mode,
zeropage) unchanged; parity 122/122; clippy clean.

**Changeset line (draft):** *Internal: fold the repeated opcode-resolution logic
in the instruction encoder into a single helper (no output change).*

### Phase 2 — iNES directive dedup (#2) · patch

**Files:** `crates/nessemble-core/src/ast.rs`, `parse.rs`, `assemble.rs`.

Today ~22 `InesXxx(Expr)` variants each need a declaration (`ast.rs:98`), a parser
arm (`parse.rs:342`), and an execution arm that reads `self.nes = true;
self.ines.field = self.eval(e);` (`assemble.rs:967`). Collapse the numeric setters
into **one parameterized variant** (decision confirmed with the maintainer):

```rust
// ast.rs
pub enum InesField { Prg, Chr, Map, Mir, Bat, FourScreen, PrgRam, Tv, Vs, Pc10,
                     SubMap, PrgNvRam, ChrRam, ChrNvRam, Console, VsPpu, VsHw,
                     MiscRom, Expansion }
// … Pseudo::Ines(InesField, Expr) replaces the 19 numeric InesXxx(Expr) variants.
```

- The **parser** maps the directive name → `InesField` via one table (`inesprg →
  Prg`, …), replacing 19 near-identical match arms with a lookup + a single
  `Pseudo::Ines(field, self.parse_expr()?)`.
- The **assembler** gets one arm: `Pseudo::Ines(field, e) => { self.nes = true;
  let v = self.eval(e); self.set_ines_field(field, v); }`, where `set_ines_field`
  is a single `match` assigning to the right `Ines` member.

**Special cases stay their own variants** (they don't fit the plain
`field = eval(e)` mold): `Ines2(Expr)` sets a `bool` (`!= 0`), `InesTiming(Expr)`
wraps `Some(…)`, and `InesTrn` is a bare marker spliced by the preprocessor.
Keep those three as-is so the collapse doesn't contort around them.

This is the largest structural change and touches the public `ast::Pseudo` enum;
under the internal-API posture that's a patch-level refactor.

**Verify:** the parse-layer tests (`parse.rs`) and the iNES header tests
(`tests/ines_header.rs`, `tests/nes2_header.rs`) exercise every field — they must
pass untouched; parity 122/122.

**Changeset line (draft):** *Internal: collapse the ~19 numeric `.inesXxx`
directive variants into one parameterized AST node (header bytes unchanged).*

### Phase 3 — Conditional-assembly stack (#4) · patch

**Files:** `crates/nessemble-core/src/assemble.rs`.

Replace `if_cond: [bool; MAX_NESTED_IFS]` + the separate `if_depth`/`if_active`
bookkeeping (`:205`, `:1063`) with a `Vec<bool>` used as a push/pop stack:
`.if`/`.ifdef`/`.ifndef` push, `.endif` pops, `.else` flips the top. This deletes
the scattered `if self.if_depth < MAX_NESTED_IFS` bounds guards and simplifies
`if_suppressed` (`:416`) — "is any enclosing level false?" becomes
`self.if_stack.iter().any(|&c| !c)` (or the exact reference semantics, see care
note). `MAX_NESTED_IFS` survives as a **validation cap** (a hard error / no-op
past depth 10) rather than an array length, so the deep-nesting test
(`lib.rs:551`) still can't index out of range.

**Care:** the current `if_suppressed` checks the current level *and its immediate
parent* (`:429`), not the whole stack — preserve that exact predicate so
suppression semantics match the reference byte-for-byte. `reset_state` (`:392`)
clears the stack instead of re-zeroing the array.

**Verify:** the `.if`/`.ifdef` selection tests (`lib.rs`), the unbalanced-nesting
guard test, and collect-mode diagnostics; parity 122/122.

**Changeset line (draft):** *Internal: model conditional-assembly nesting as a
stack instead of a fixed array (same suppression semantics).*

### Phase 4 — Shared token-class contract (#8) · patch

**Files:** `crates/nessemble-core/src/tooling.rs`, `crates/nessemble-lsp/src/lib.rs`,
`crates/nessemble-wasm/src/lib.rs`.

The `TokenClass → integer id` mapping is written three times: `lsp`'s
`token_type_index` (`lib.rs:1034`), `wasm`'s `token_class_id` (`lib.rs:178`), and
the parallel `token_classes()` name list. Centralize the numbering as an inherent
method on the core enum — e.g. `TokenClass::wire_id() -> u32` and
`TokenClass::wire_name() -> &'static str` — with a doc comment stating the ids are
a **stable wire contract** (the reason the mapping was duplicated in the first
place). The LSP keeps its own `SemanticTokenType` legend (that's a different,
LSP-specific numbering) but derives its index from the shared id; wasm's
`tokenize`/`token_classes` call the shared methods. This removes the "keep these
three in sync" hazard while preserving the exact wire numbers.

**Verify:** the wasm `token_classes_legend_aligns_with_ids` and
`tokenize_packs_class_triples` tests, and the LSP `semantic_tokens_classify_*`
tests, must produce identical ids/names; full workspace suite green.

**Changeset line (draft):** *Internal: define the highlight token-class wire ids
once in core instead of re-deriving them in the LSP and wasm crates.*

### Phase 5 — Low-risk polish batch (#5, #7, #10, #11) · patch

Four small, independent, mechanical cleanups in one PR (each is a self-contained
file-local diff):

- **#5 — `consolidate_data` clones (`tooling.rs:864`).** The pass repeatedly
  writes `if let Some(hs) = hint_strides.clone() { flush_hint(&hs, …) }`, cloning a
  `Vec<usize>` only to borrow it. Restructure so `flush_hint` borrows
  `hint_strides` directly (take `&Option<Vec<usize>>` or split the borrow), dropping
  the per-iteration allocations. Idempotency and the data-consolidation tests
  guard it.
- **#7 — `RcOptions::apply` (`cli/src/rc.rs:54`).** Twelve
  `if let Some(v) = self.field { base.field = v; }` blocks. A small local
  `macro_rules!` (`overlay!(base, self, field, …)`) collapses the plain scalar
  fields; the two enum-validated fields (`indent_style`, the two `Case`s) stay
  explicit. The rc mapping tests cover it.
- **#10 — argument slicing (`xtask/src/main.rs:40`, `changeset.rs:36`).** Replace
  `&args[args.len().min(1)..]` with the plain `args.get(1..).unwrap_or(&[])` at
  both sites — same result, obvious intent.
- **#11 — `AssembleError` (`core/src/lib.rs:49`).** The single-variant
  `enum AssembleError { Diagnostic(Diag) }` forces callers into irrefutable
  `let AssembleError::Diagnostic(d) = err;` matches (in `wasm`, `cli`, tests).
  Flatten it — either a newtype `struct AssembleError(pub Diag)` or expose the
  `Diag` directly — and update the handful of match sites. Internal-API posture
  makes this a patch.

**Verify:** each item's local tests; parity 122/122; clippy clean.

**Changeset line (draft):** *Internal: assorted readability cleanups — borrow
instead of clone in the data-consolidation pass, tidy config overlay and argv
slicing, and flatten the single-variant assemble error.*

### Phase 6 — Typed iNES header fields (#3) · patch · **optional**

**Files:** `crates/nessemble-core/src/assemble.rs`.

The `Ines` struct (`:103`) types every field as `i64`, including booleans
(`bat`, `fsc`, `vs`, `pc10`, `nes2`) and small bitfields (`submap` 0–15, `map`
0–4095). Idiomatic Rust would use `bool` and sized ints so invalid states are
unrepresentable and the `!= 0` / `& 0x0F` ceremony in header emission shrinks.

**This is the riskiest phase and is explicitly optional.** The uniform `i64` is a
deliberate parity choice — it keeps expression arithmetic and truncation
identical to the C reference. Retyping means auditing every read/write site for
truncation-order and sign differences, all behind the parity corpus. Recommended
approach if taken: convert **one field group at a time** (e.g. the booleans
first, which are the safest), each as its own commit with a parity run, rather
than a big-bang retype. If the maintainer prefers, this phase can be dropped
entirely — the `i64` modeling is defensible as-is.

**Verify:** parity 122/122 after *each* field-group commit; the full iNES/NES 2.0
header test matrices; a manual diff of the emitted header bytes on a
representative ROM.

**Changeset line (draft):** *Internal: type the iNES header fields as booleans and
sized integers instead of uniform `i64` (identical header output).*

### Phase 7 — Process-global i18n catalog (#9) · patch · **optional**

**Files:** `crates/nessemble-i18n/src/lib.rs`.

The catalog is `thread_local!` (`:93`), so `set_locale` / `register_locale` on one
thread don't reach others — a footgun for the multithreaded LSP (a worker that
assembles would silently fall back to `en-US`). Replace the `thread_local!
RefCell<Catalog>` with a process-global `OnceLock<RwLock<Catalog>>` (or
`Mutex`), so locale registration and selection are visible everywhere.

**Behavior note:** this *does* change semantics — locale becomes process-global
instead of per-thread. For the single-threaded CLI it's invisible; there's no
public API today that switches locale per-thread on purpose, so it's a
patch-level fix, not a feature. The alternative (do nothing but document the
per-thread caveat) is also acceptable if the maintainer would rather not add a
lock to the hot `t!` path.

**Verify:** the i18n unit tests (fallback, stub-locale override) — note the
existing tests mutate global locale, so confirm they still pass under a shared
lock (and add a cross-thread test asserting a registered locale is visible from a
second thread); full workspace suite green.

**Changeset line (draft):** *Internal: make the i18n locale catalog process-global
so a locale set on one thread is honored on all of them.*

## 5. Testing strategy

Every phase leans on the safety nets already in the repo — this plan adds almost
no new test infrastructure, by design:

- **Parity harness** (`cargo run -p xtask -- parity`, 122/122) after each phase —
  the primary gate for anything touching the assembler.
- **Byte-preservation** tests in `tooling.rs` and the `tests/corpus.rs` /
  `tests/*_header.rs` suites in `nessemble-core`.
- **Per-crate unit + integration tests** already covering the touched code
  (instruction encoding, parse arms, conditionals, formatter passes, rc mapping,
  LSP/wasm token classification) must pass **unchanged** — an unchanged test over
  refactored code is the proof the behavior didn't move.
- **New tests only where a phase creates a new seam:** a cross-thread locale test
  (Phase 7) and, if Phase 6 is taken, an explicit emitted-header-bytes assertion
  for a representative ROM.
- **`cargo clippy --workspace --all-targets`** clean under the pedantic set after
  each phase.

## 6. Risks & mitigations

- **A refactor silently changes output.** *Mitigation:* the parity corpus is
  byte-exact and runs per phase; the byte-preservation tests assemble
  original-vs-transformed. This is precisely what those harnesses exist to catch.
- **Phase 2 / Phase 6 touch public `nessemble-core` types.** *Mitigation:* the
  maintainer has confirmed the core Rust surface is internal (CLI/LSP/wasm are the
  product), so shape changes are patch-level; the crates are path-versioned within
  one workspace, not an external stability contract. Values and outputs are held
  fixed by the guardrails.
- **Phase 6 truncation/sign subtleties.** *Mitigation:* it's optional, done
  field-group-by-field-group with a parity run between commits, and can be dropped
  without affecting Phases 1–5.
- **Phase 7 adds a lock to the `t!` hot path.** *Mitigation:* `t!` is called only
  on diagnostics/warnings and CLI output (never per-byte), so an `RwLock` read is
  negligible; and the phase is optional.
- **Refactor fatigue / low payoff.** *Mitigation:* phases are ordered
  value-first; if attention runs out after Phases 1–3 (which remove the bulk of
  the duplication), the remaining phases are independent and can be deferred
  indefinitely without leaving the tree half-migrated.

## 7. Decisions

Settled with the maintainer before drafting:

1. **Public-API posture: internal → patch bumps.** `nessemble-core`'s `pub`
   surface (`ast::Pseudo`, `AssembleError`, `Options`, `tooling::*`) is not a
   stability contract — the CLI, LSP, and wasm crates are the product. So
   shape-changing refactors (Phase 2's enum collapse, Phase 5's error flatten,
   Phase 6's field retyping) are **patch**-level; only a change to actual
   CLI/LSP/wasm observable output would escalate a phase to **minor**.
2. **Finding #2 shape: collapse to `Pseudo::Ines(InesField, Expr)`.** Prefer the
   cleaner parameterized data model over a macro that regenerates the 19 separate
   variants. The three special cases (`Ines2`, `InesTiming`, `InesTrn`) stay their
   own variants.
3. **Optional phases: include both #3 and #9.** Scoped as Phases 6–7, explicitly
   optional, each behind the parity corpus / i18n tests, droppable without
   affecting the core phases.

## 8. What is intentionally *not* changing

For the record, so a future reader doesn't "fix" these:

- The uniform `i64` arithmetic in expression evaluation and location math is
  parity-driven and stays (Phase 6 only retypes the *header* struct, and only if
  taken).
- The two separate lexers (`lexer.rs` parity lexer, `tooling.rs` lossless lexer)
  stay separate — one is byte-for-byte tied to the reference flex grammar, the
  other is for tooling; merging them would risk parity.
- The `#[allow(...)]`s that carry a justifying comment (e.g. `implicit_hasher`,
  `type_complexity`, the wasm `needless_pass_by_value` for `register_fn`) are
  correct and stay.
- Deliberate reference-parity sentinels and messages that are *not* covered above
  (e.g. the `overflow-chr` warning using `prg_index` for message parity,
  `assemble.rs:817`) stay exactly as documented.
