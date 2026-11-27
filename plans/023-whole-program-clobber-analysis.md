# nessemble-rs: A Plan for Whole-Program Clobber Analysis

> Status: **Implemented** (Phases 0–6; the reference-corpus acceptance run in
> §6 is the maintainer's, and its counts are not recorded yet). Builds on [plan 008](008-linting-rules.md) (the lint
> engine), [plan 009](009-comment-directives.md) (the directive registry and
> `@nessemble-lint-ignore…`) and [plan 010](010-routine-signatures.md)
> (signatures, `undeclared-clobber`, `overdeclared-clobber`). It replaces plan
> 010 §8.1's "body = text block" with "body = code reachable from the label, in
> the assembled program". Interactions with plans 018–022 are in §2.10.
>
> Before writing this, every "today" claim was checked against v2.27.0: the lint engine in `tooling.rs`, `nessemble lint` on the reference corpus (43 warnings, all `require-block-comment`), and the corpus's 38 clobber suppressions, each run through today's single-file analysis with the suppression stripped (via in-memory LSP buffers; nothing was written to disk). §8.3 has the counts.

---

## 1. Summary

`nessemble lint` gains a **program mode**. When it can assemble the project from an entry file (the `.nessemblerc` `"entry"` key, `--entry`, or a file argument that assembles on its own), the clobber rules run over the instruction stream the assembler actually emitted. Each routine is walked from its label: through branches (both arms), fall-through, `JMP` and `JSR`, across files and banks. The walk trusts each callee's contract, pairs push/pull and store/reload saves, and treats an indirect or computed transfer as a new `unverifiable-clobbers` warning. That removes all four blind spots: cross-file callees, fall-through, save/restore and unreachable fragments. A new `unused-lint-ignore` warning then reports every suppression the better analysis no longer needs. Without a program, lint falls back to today's single-file analysis, byte-for-byte unchanged, and assembled output never changes.

---

## 2. Semantics

### 2.1 Modes: program mode and file mode

**Program mode** applies to a linted file that belongs to the include closure of an *entry* that assembled without errors.
- In program mode, `undeclared-clobber`, `overdeclared-clobber`, `unverifiable-clobbers` and `require-routine-doc` use the whole-program analysis (§2.3–2.8).
- **File mode** is everything else. Those rules behave exactly as in v2.27.0 (text-block body, same-file calls), and `unverifiable-clobbers` does not run.
- The text rules (`require-block-comment`, the three directive rules, `invalid-routine-signature`) are identical in both modes.

**Where the entry comes from** (proposed, in precedence order):
1. **`--entry <file>`** on the command line (repeatable).
2. **`.nessemblerc` `"entry"`**: a string, or an array of strings for projects that build several ROMs. Resolved against the directory of the `.nessemblerc` that declares it. This is not `Config.root`, which `.nessembleignore` can move.
3. **Opportunistic**: with neither of the above, each *file* named on the command line (not files found by walking a directory) is tried as its own entry. If it assembles, it is the entry; if not, its files stay in file mode, silently. This is today's behaviour for fragments.

**What gets reported:**
- `nessemble lint main.asm` (main.asm is an entry, by any route): every file in main.asm's include closure, each under its own header. `.nessembleignore`d files are analysed but not reported. Included files are reported whatever their extension.
- `nessemble lint src/` or `nessemble lint src/prg/07.asm` with an `entry` configured: only the named or walked files, in program mode when they are in the closure. A walked file that the entry never includes stays in file mode.

**Failure:**
- If an explicit entry (`--entry` or rc) fails to assemble, lint prints the assembler's errors exactly as the build would and skips the program-dependent rules for that program. It does not fall back to text analysis, because those findings would be the blind spots again. Text rules still run, and the exit code is non-zero. Failing `.assert`/`.error` checks (plan 022) count as assembly errors here.
- Assembler *warnings* (bank overflow, and plan 017's `duplicate-symbol` / `duplicate-symbol-same-value` / `value-out-of-range`) are not printed by lint. They belong to the build. `.out` lines (plan 022) are not printed either.

**Assembly inputs:**
- Custom pseudo-instructions come from `-p/--pseudo`, falling back to `.nessemblerc` `assemble.pseudo`. The persistent cache from plan 011 is used, so a lint after a build is warm.
- Lint uses the build's own project-root discovery; `--root` is passed through.
- `-u` and the empty byte do not affect control flow and are out of scope (§8).

**Several entries** (proposed): a file in more than one program is analysed once per program, and the results are merged:
- `undeclared-clobber` and `unverifiable-clobbers`: union. A contract must hold in every build it ships in.
- `overdeclared-clobber`: intersection. A register one build needs is not over-declared.
- `unused-lint-ignore`: a suppression is unused only if it is unused in every program.

### 2.2 What the assembler records (program facts)

Recording is off by default. It is switched on by `Options::record_program`, the way `source_map` is today, and it reads state only, so bytes are identical with it on or off (a test pins this). It is collected on **pass 2** only, when addresses are final. The new types live in `crates/nessemble-core/src/program.rs`:

- **`Emit`**: one per executed statement that wrote at least one ROM byte.
  - `rom_offset`, `len`: header-less PRG/CHR coordinates, the same space as `SourceSpan` and the CDL.
  - `bank: u16`: `prg_index` at emission.
  - `cpu_addr: u16`: `address_offset()`, so phased (the run-time address labels receive).
  - `segment`: PRG only. CHR and trainer bytes are not recorded, because they never run on the CPU.
  - `site: Site { file, line }`: **the author's line**. For a macro expansion this is the outermost `.macro` call site; for code from a custom pseudo-op's `emit_source` it is the pseudo-op's line.
  - `kind`: `Instruction { target: Option<Target> }`, or `Data { directive }` (for `.db`/`.dw`/`.incbin`/`.fill`/…, and custom pseudo-op `Bytes`, named `.foo`).
- **`Target`**, recorded for `JMP abs`, `JSR` and branches:
  - `addr`: the operand value.
  - `anchor: Option<(rom_offset, delta)>`: when the operand expression names a label (or `:+`/`:-`, or an `@local`), the ROM offset where that label was defined, plus `operand − label value`. The anchor pins the target to an exact ROM location, which is how "the same address in different banks" and overlapping `.phase` blocks are told apart.
- **`LabelDef`**: one per label definition executed on pass 2: `name` (the fully qualified key once plan 021 lands, e.g. `p1@loop`), `cpu_addr`, `bank`, `rom_offset` (the load position at definition), `site`, `in_expansion` (whether the line came from a macro or emitted source), `local` (`@name`).
- **`Unmodelled { site, rom_offset, cpu_addr, bank }`**: LSP only (§4.4). It marks a custom pseudo-op the editor did not run.
- **`files: Vec<PathBuf>`** (resolved paths, indexed by file id) and **`image: Vec<u8>`** (the header-less ROM).

**Resolving a control transfer to a ROM location** (the walk's identity is a ROM offset, never a bare address):
1. **Anchored** (the operand names a label): `anchor.rom_offset + delta`. It is accepted if an emit covers that offset and its `cpu_addr` agrees with `addr`.
2. **Unanchored** (a numeric operand, or bytes decoded from data): the emit covering `addr` in the *current bank*. If there is none, the unique bank that covers `addr`. If several do, the transfer is unverifiable (ambiguous). If none does, it is unverifiable (nothing assembled there).
3. **Fall-through**: the emit at `rom_offset + len` if its `cpu_addr` continues the run. Otherwise (after `.org` or `.phase` moved) it is resolved like rule 2.

Within one bank, two emits at the same `cpu_addr` with different ROM offsets (overlapping `.phase` blocks) make an unanchored lookup ambiguous. The same ROM offset written twice resolves to the last emit, which is what the ROM holds.

### 2.3 Decoding

At a walk position the opcode is `image[off]`, looked up with `nessemble_isa::by_byte`; operands come from the following bytes. The source statement is used only for the anchor and the site. This handles each case uniformly:

- **Instruction emits**: exact.
- **A jump into the middle of an instruction** (the `.db $2C` "BIT skip" idiom): decoded as the CPU would.
- **Data emits reached by the walk**: decoded as code. This is what makes a hand-encoded branch split across two `.db` lines verifiable: `$B0` followed by an offset byte on the next line decodes to `BCS rel`, and both arms are followed. The path becomes **unverifiable** if:
  - the opcode has no recorded effects (unknown, or a JAM/KIL byte);
  - an undocumented opcode is reached and the program was assembled without `-u`;
  - any byte of the instruction was never written, or lies across a bank boundary;
  - **a `JSR`'s return site is a Data emit**. This is the inline-arguments-after-`JSR` idiom: the bytes after the call are a parameter table, not code. Decoding them would invent writes.

### 2.4 The walk (body = code reachable from the label)

For each signature that declares a clobber list, the walk starts at the `LabelDef` with the same file and line, not in an expansion. A label assembled more than once (a file included twice, a pasted `row\i:`) is walked once per definition and the results unioned. A signature whose label was never assembled (an `.if` assembled it out) is not analysed.

The walk is **path-sensitive** over states `(rom_offset, save stack, memory saves, previous instruction, push depth)`, with states de-duplicated. The rules, in the order they are checked at each position:

1. **Reaching a contracted routine's entry** by fall-through, `JMP` or branch: any other label (not the walk's own entry, and not any label at the same address as it) whose signature declares clobbers.
   - With no save pending, its clobbers plus returns are added and the path ends.
   - With a save pending, the walk continues into it, because the restore can only come further along. This matches the external contracts checker.
   - `@local` labels are never contracts and never stop a walk. A local belongs to its parent routine.
2. **`RTS` / `RTI`**:
   - If the routine's own push depth on this path is above zero, the path ends **unverifiable**: the return goes through values the routine pushed, which is the RTS-dispatch computed jump.
   - Likewise after a `TXS` on this path.
   - Otherwise the path ends, and any open memory save commits its pending writes.
3. **`BRK`**: the path ends (the handler is not followed).
4. **`JMP abs`**: continue at the resolved target.
5. **`JMP (ind)`**: the path ends **unverifiable**.
6. **`JSR`**:
   - **Into a contracted label**: its clobbers plus returns are written (through the caller's pending saves), then continue at the return site.
   - **Into anything else**: the callee's *summary* is absorbed and the walk continues, unless the summary says the callee pulls its own return address (a `PLA`/`PLP` at depth 0, or a `TXS`). Then the path ends **unverifiable**.
   - Summaries are computed with a fresh save state and solved as a **monotone fixpoint** over the call graph, so recursion is exact rather than "visit once". A summary is `{ writes, first-write origins, unverifiable reasons, pulls_return, absolute stores }`.
7. **Conditional branch**: follow both arms, except where the previous instruction on this path fixes the flag:
   - `CLC`+`BCC`/`BCS`, `SEC`+`BCS`/`BCC`, `CLV`+`BVC`/`BVS`;
   - `LDA`/`LDX`/`LDY #imm` followed by `BEQ`/`BNE`/`BMI`/`BPL`.
   - The arm that is never taken is pruned. This keeps a never-taken fall-through from running into a table.
8. **Anything else**: apply its writes (§2.5), then fall through. A position with no emit makes the path **unverifiable** ("runs past the last byte assembled").

**What this covers:**
- **Unreachable code** after an unconditional exit is never visited (blind spot 4).
- **`.if`, `.for`, macros and custom pseudo-ops** are seen as the bytes they produced (they are just emits).
- **Fall-through and always-taken branches** into the next block are followed (blind spot 2).

### 2.5 Writes and saves

**Writes:**
- They come from `nessemble-isa`'s per-opcode effects, unchanged: `S` is written only by `TXS` (plan 010 §16.4), and flags and memory are not verified.
- A write commits immediately, unless the register is currently saved, in which case it is held as *pending* against the innermost save of that register.
- **May-clobber**: a write on any path counts, so a branch where only one arm writes `X` clobbers `X`.

**Stack saves (Phase 2):**
- `PHA` saves `X` if the previous instruction on this path was `TXA`, `Y` after `TYA`, and `A` otherwise. The `TXA`/`TYA` itself still writes `A`, so `TXA PHA … PLA TAX` preserves `X` but clobbers `A`, which is the truth.
- `PHP` pushes flags (depth only).
- `PLA` pops:
  - an `A` save is restored, and its pending writes are discarded;
  - an `X`/`Y` save is restored when the next instruction on the path is the matching `TAX`/`TAY`. The pending writes are discarded and that transfer is not a write, but `PLA`'s write to `A` counts;
  - a mismatch commits the pending writes and `PLA` writes `A`;
  - at depth 0 it sets `pulls_return` and writes `A`.
- `PLP` pops and commits the popped save's pending writes.
- `TXS` commits every stack save and marks the path (rule 2).

"Pushed and pulled back on every path to the exit" falls out of path sensitivity: a path that exits without the restore has committed its writes. Hence the required test, "a save on one path but not the other must report".

**Memory saves (Phase 4, stretch, droppable):**
- `STA`/`STX`/`STY` to a non-indexed zero-page or absolute address *M* (an exact value known from the bytes) opens a save `{reg, M}`, replacing and committing any earlier save of *M*.
- A later `LDA`/`LDX`/`LDY` from *M* into the same register restores it. Pending writes are discarded and the load is not a write.
- The save is *released* (pending commits) by:
  - any other store to *M*;
  - `INC`/`DEC`/`ASL`/`LSR`/`ROL`/`ROR` on *M*;
  - a contracted callee whose clobbers include a memory slot resolving to *M* (the symbol is resolved through the program's labels and constants);
  - an uncontracted callee whose summary stores to *M*;
  - exiting with the save still open.
- Indexed and indirect stores are not treated as aliasing *M*; that is a risk, recorded in §8.

Two of the corpus's 38 suppressions need this phase (§8.3).

### 2.6 The rules in program mode

| Rule | Fires when |
| --- | --- |
| `undeclared-clobber` | The routine declares a clobber list and writes a register in `A`/`X`/`Y`/`S` that is neither clobbered nor returned. It fires even if the routine is also unverifiable: a definite write is definite. |
| `overdeclared-clobber` | The routine declares a clobber list, the walk found **no** unverifiable reason, and a declared clobber register (returns excluded, as today) is written on no path. A preserved (saved and restored) register counts as not written. |
| `unverifiable-clobbers` *(new, default `warn`)* | The routine declares a clobber list and at least one path ended unverifiable, including through an uncontracted callee. It does not run in file mode. |
| `require-routine-doc` | A block-opening label is the resolved target of a `JSR` anywhere in the program, or of a `JMP` from a different text block (a tail call, not a loop back to the top), and has no signature. Targets reached through macros and `emit_source` count. `@local` labels are never flagged. |

Labels matching `lint.ignore` are still walked as callees (their contracts are trusted) but get no findings of their own.

### 2.7 `unused-lint-ignore` *(new, default `warn`)*

**The principle: a suppression naming rule *R* is *judged* at a line only if *R* ran there with full information. It is *unused* if *R* produced no finding there before suppression.**

**When a rule counts as having run:**
- **Text rules** run on every line of every linted file, in both modes.
- **Program rules** (`undeclared-clobber`, `overdeclared-clobber`, `unverifiable-clobbers`, `require-routine-doc`) run only in program mode, at lines whose label the program assembled. A routine with no clobber list still counts: the rule ran and could not fire, so a suppression on it is unused. That covers eight of the corpus's suppressions.

**Configuration:**
- A rule configured `off` for the file is **not judged**. Switching a rule off for a while should not demand deleting the suppressions you will want back.
- If every rule a suppression names is off, there is no finding.
- `unused-lint-ignore` itself `off` means no findings.

**Partial runs:**
- In file mode (a fragment with no entry), suppressions of program rules are not judged.
- A bare suppression (no rule list) is judged only where every rule ran, which in practice means program mode.
- A suppression whose target label was assembled out in every program is not judged.

**Granularity:**
- **Per named rule.** A two-rule suppression where only one rule fired is reported naming only the unneeded one.
- **Regions** are judged over their lines. An unclosed region runs to end of file, as today.

**Suppressing the rule itself:**
- `unused-lint-ignore` findings are themselves filtered by suppressions, in one pass with no fixpoint. A region naming `unused-lint-ignore` that silences one counts as used.
- A suppression never silences the finding about itself.

**Multiple programs**: unused only if unused in all of them (§2.1).

### 2.8 Coexistence with the local-label feature (plan 021)

- An `@name` label never opens a block (`block_label` already returns `None` for `@`, and plan 021 pins it), and no annotation binds to one.
- The walk follows branches to locals like any target, and they belong to their parent routine for every rule.
- A qualified `parent@x` that another routine `JSR`s is an uncontracted callee and is walked.

### 2.9 What does not change

- **Emitted bytes, for every program.** Recording only reads state. A test asserts identical ROMs with recording on and off over `tests/corpus`, and §6 adds the same check on the reference corpus.
- **File-mode output**, byte-for-byte (the message texts too).
- **`format`**: it ignores the new keys.
- **Coverage and `SourceMap`**: untouched.
- **The `-l` list file**: untouched.

### 2.10 Interactions with plans 018–022

- **018 (unary operators):** none. The walk reads bytes, not expressions.
- **019 (value enums):** none; enum members emit nothing.
- **020 (`.table`):** tags emit nothing, and table entries are `Data` emits. A walk that reaches a table is decoding data (§2.3), which is the intended signal. A `[K] JMP handler` entry is an `Instruction` emit and is decoded normally. `.table`/`.endtable` lines do not open blocks.
- **021 (`@local` labels):** §2.8. `LabelDef.name` is the qualified key.
- **022 (`.assert`/`.error`/`.out`):** they emit nothing, so they never appear in the walk. Plan 022's `is_block_entry` rule (an assertion line between a blank line and a label does not stop that label opening a block) must be kept by this plan's block detection.
- **Call sites.** This plan's `Site { file, line }` is the outermost `.macro` call, read from plan 017's expansion table (§3.2). It does not add its own token field.

---

## 3. Code changes

**Shared code** is marked ⚑.

### 3.1 `nessemble-rc`: one coordinated schema change ⚑ `.nessemblerc` schema

Plan 017 shipped the `assemble` block (`RcAssemble { rules }`, root-level only, `deny_unknown_fields`) and made assembling read `.nessemblerc`. This plan extends that same block rather than adding a second one:

```json
{
  "entry": "main.asm",                       // or ["main.asm", "demo.asm"]; root only (this plan)
  "assemble": {
    "pseudo": "scripts/pseudo.txt",          // root only (this plan)
    "rules": { "duplicate-symbol": "warn" }  // plan 017
  },
  "lint": { … }
}
```

- **`entry` goes on `RcConfig`**, the root document, not on the flattened `RcOptions`. `deny_unknown_fields` then rejects it inside `overrides` automatically.
- **`pseudo` is a new field of plan 017's `RcAssemble`**, which is already root-only, so it is rejected inside `overrides` with plan 017's existing message.
- **`Config`** gains `rc_dir: Option<PathBuf>`, plus `pub fn entries(&self) -> Vec<PathBuf>` and `pub fn pseudo(&self) -> Option<PathBuf>`, beside plan 017's `assemble_rules`.
- **Errors use the existing style.** A missing entry file is reported where the entry is used, not when the config is loaded, so a fresh checkout without generated files still formats (§5).

### 3.2 Recording: preprocessor, parser, assembler

- **Call sites.** No new token or line field: plan 017's `Line.expansion` and `Preprocessed.expansions` already record every expansion's call site and parent. `Site` is the outermost call (follow `parent` to the root expansion and take its `call_file`/`call_line`), or `(Line.file, Line.line)` for a line not from a macro.
- **⚑ symbol table** (`assemble.rs`, `Symbol`): gains `rom_offset: usize`, set in `define_symbol`. Plan 017 already records each definition's site (file, line, expansion) on the symbol. Neither is read by expression evaluation or the list file; they exist only so a `Target` anchor can come from `eval`.
- **`assemble.rs`** (Assembler):
  - New fields: `record_program: bool` and `program: ProgramBuilder`. The current site comes from `cur_expansion` and `cur_file`/`cur_line`.
  - `exec_stmt`/`exec_instruction`/`exec_pseudo`/`exec_custom` push an `Emit` after a statement wrote bytes on pass 2. They compare `rom_index()` and `address_offset()` before and after, and skip statements suppressed by `.if` and the trainer.
  - `exec_instruction` computes the `Target` anchor with a helper, `operand_anchor(&Expr)`: the first `Symbol`/`PastedSymbol` of kind `Label`, or a local reference, gives that symbol's `rom_offset` plus `value − symbol value`.
  - `add_label` and the pasted-label path push a `LabelDef`.
- **New module `program.rs`**: the §2.2 types, the resolution functions of §2.2, and:

  ```rust
  pub fn build_program(path: &Path, source: &str, options: &Options,
                       overlay: &FileOverlay, custom: CustomResolver) -> ProgramBuild
  // ProgramBuild { program: Option<ProgramMap>, errors, warnings, files, paths }
  ```

  It is built on the `diagnose_project_with` path (collect-all), so the CLI prints every error and the LSP reuses it. It is re-exported from `lib.rs`. `ProjectDiagnostics` gains `program: Option<ProgramMap>` when `Options::record_program` is set.
- **`nessemble-isa`**: no change. `by_byte`, `writes`, `effects_known` and `is_undocumented` already exist.

### 3.3 Analysis: new module `crates/nessemble-core/src/clobbers.rs`

`tooling.rs` is already 5,400 lines, so the analysis gets its own module:

- `Contracts`: the signatures of every program file, keyed by `(path, label line)` and mapped onto `LabelDef`s, with a reverse index from ROM offset to contract.
- `walk(program, contracts, entry) -> RoutineEffect { writes: RegSet, origins: [Origin; 4], reasons: Vec<Reason>, pulls_return }`.
- `summaries`: the fixpoint of §2.4, rule 6.
- The memory-save machinery from §2.5, behind an internal flag until Phase 4.
- `Origin` (instruction, site, and the "via" step: a `JSR` site, or a contract name) and `Reason` (an enum with one variant per §5.1 text). (`Origin` here is the clobber-analysis type, distinct from plan 020's deferred-check `Origin`; name it `WriteOrigin` if both live in scope.)

### 3.4 Lint engine ⚑ lint engine (`tooling.rs`)

- **`RuleId`**: add `UnverifiableClobbers` (`unverifiable-clobbers`) and `UnusedLintIgnore` (`unused-lint-ignore`), both defaulting to `Warn`. `ALL` becomes 10 entries, with `index` updated. `needs_signatures` adds `UnverifiableClobbers`. A new `fn needs_program(self)` covers the four program rules.
- **`LintSuppressions`**: `suppresses` becomes `consume(rule, line) -> bool`, which records which `(range, rule)` pairs fired. `resolve` keeps each directive's line and column, and the bound for regions.
- **`lint(source, opts)`**: the signature is unchanged and it remains file mode. It now also produces `unused-lint-ignore` findings for text rules, as in §2.7.
- **New:**

  ```rust
  pub struct ProgramFile<'a> { pub path: &'a Path, pub source: &'a str,
                               pub options: &'a LintOptions<'a>, pub report: bool }
  pub fn lint_programs(programs: &[ProgramMap], files: &[ProgramFile]) -> ProgramLint
  // ProgramLint: per-file findings (parallel to `files`) + per-routine effects
  pub fn missing_clobbers_in(lint: &ProgramLint, path: &Path, name: &str) -> Option<Vec<Slot>>
  ```

  It runs the text rules per file (sharing `LintCtx`), the program rules once, the merge from §2.1, then suppression and unused-suppression handling per file using that file's `LintOptions` (so per-glob overrides and `ignore` still apply).
- **`missing_clobbers`** (text) stays for file mode, together with the plan 020/021/022 text-walk fixes (tags, `@x:` lines, `.assert` lines are transparent).

### 3.5 CLI (`crates/nessemble-cli/src/lint.rs`)

- New flags: `--entry <file>` (repeatable), `-p/--pseudo <pseudo.txt>`, `--root <dir>`, and `--no-cache` (as for the build).
- `run`:
  1. Resolve configs, as today.
  2. Collect entries, by the precedence in §2.1.
  3. Call `build_program` per entry, reading from disk. The resolver is `custom::build_resolver(pseudo, caching, None)`, where `pseudo` is the flag or `config.pseudo()`.
  4. Print `Diag`s for explicit entries that fail, with the build's formatter (`Error in …`).
  5. Partition the files to report into program mode and file mode.
  6. Call `lint_programs` and `lint`.
  7. Print `report` unchanged.
- The exit code also fails on an explicit entry that does not assemble.

### 3.6 LSP ⚑ LSP (`nessemble-lsp`, `nessemble-cli/src/lsp.rs`)

- **`nessemble_lsp::run_with(hooks: ServerHooks)`**, where `ServerHooks { custom_resolver: Box<dyn Fn(Option<&Path>) -> CustomResolver> }`. The CLI injects a **cache-only resolver**: `custom::build_cache_reader(pseudo)` returns `Bytes` on a plan-011 cache hit and *never runs a script*. On a miss it falls back to the lenient empty output and records an `Unmodelled` marker. `run()` keeps the lenient resolver. Plan 022 proposes a `CustomOutput::Unmodeled` variant for the same miss; use one variant for both, so the `Unmodelled` marker and plan 022's taint flag come from the same place.
- **Which roots are assembled** (`compute_diagnostics`): when the discovered `.nessemblerc` has `entry`, the configured entries whose closure contains the file, rather than the include graph's roots. It is assembled with `record_program` and the hook resolver, so lint reuses the same `Run` and costs **no extra assembly**.
- **Lint** (`with_lint`): documents covered by a `Run` with a program get their findings from one `lint_programs` call per round. All others keep `lint_diagnostics`, which is file mode.
- **Code actions**:
  - "Add `X` to clobbers" reads `missing_clobbers_in` when in program mode.
  - New: "Remove unused suppression" on `unused-lint-ignore`. It deletes the directive line, or just the named rule when others remain.
- **Rule: the editor never shows a program finding the CLI would not.** A routine whose walk crosses an `Unmodelled` marker gets the reason `editor-not-run`. That reason does not produce `unverifiable-clobbers`, but it still suppresses `overdeclared-clobber` and makes the routine's suppressions not judged.

### 3.7 Listing writer

No change (the `-l` symbol list is unaffected).

---

## 4. Tooling parity

| Surface | Behaviour |
| --- | --- |
| **`lint` CLI** | Program mode or file mode (§2.1). Report format, `--quiet`, `--max-warnings` and the exit code are unchanged, plus the entry-failure rule. |
| **LSP** | The same `entry` key and the same `lint_programs`, so with a warm cache the editor matches the CLI. Unsaved buffers are overlaid, as for assembler diagnostics. With a cold cache, or `emit_source` pseudo-ops inside routine code, the editor reports a subset (§3.6). Lint findings keep their gentle severities. The new rule ids appear in `code`. |
| **`format`** | No change. `entry` and `assemble` are ignored. A test pins that the formatter leaves `@nessemble-lint-ignore…` lines alone, as today. |
| **`coverage`** | No change. It may later read `site` from `ProgramMap` for macro call-site attribution (a non-goal here). |
| **Listing (`-l`)** | No change. `Symbol.rom_offset` and `site` are not listed. |
| **`reference` / directive catalog** | No new directives. |
| **WASM playground** | Gets file-mode lint only. There is no filesystem entry; the bundle is rebuilt only. |

---

## 5. Diagnostics

### 5.1 Lint findings (ESLint rows, `LINE:COL severity message rule-id`)

These keep `lint`'s existing ESLint row format rather than `Error in …`, because it is the shipped lint report. **Every finding sits at the author's line**: the routine's label line, or the directive's line, never a macro body. Lines named *inside* a message are sites, i.e. call sites for macros and custom pseudo-ops. "of `F`" is added only when the file differs from the routine's own file, and `F` is the path as the report's file header prints it. Rule-id style matches plan 017's `duplicate-symbol` / `value-out-of-range` (lowercase, hyphenated).

**`undeclared-clobber`** (program mode, at the label line and column). The v2.27 prefix is kept, and the origin is appended per register:
- One register:
  `routine `NAME` writes X but does not declare it clobbered (X: `LDX` on line 14 of `util.asm`)`
- Through a contracted callee:
  `… (Y: `JSR helper` on line 9, whose contract clobbers Y)`
- Through an uncontracted callee:
  `… (X: `INX` on line 31 of `util.asm`, reached through `JSR helper` on line 9)`
- Falling into a contracted routine:
  `… (Y: falls through into `next_routine` on line 40, whose contract clobbers Y)`
- From a macro or custom pseudo-op:
  `… (A: `LDA` in `.macro SET_BANK` on line 22)` / `… (A: `LDA` in `.foo` on line 22)`
- From hand-encoded bytes:
  `… (A: hand-encoded `LDA` in `.db` on line 7)`
- Several registers, joined with `; `:
  `routine `NAME` writes X, Y but does not declare them clobbered (X: …; Y: …)`

**`overdeclared-clobber`** (program mode). The text is unchanged from today:
`routine `NAME` declares X clobbered but never writes it`
When the register is preserved by a save, it adds:
` (it is saved on line 3 and restored on line 18)`

**`unverifiable-clobbers`** (at the label line):
`routine `NAME` cannot be fully verified: REASON`
followed by ` (and N more)` when there are several reasons. The reasons:
- `indirect `JMP [vec_ptr]` on line 169 of `main.asm` ends a path`
- `` `RTS` on line 12 returns through values the routine pushed (a computed jump)``
- `` `RTS` on line 30 follows `TXS` on line 4, so its return address is unknown``
- `` `JSR helper` on line 9 enters `helper`, which pulls its own return address``
- `` `JMP $8000` on line 9 targets $8000, which banks 3 and 5 both assemble code at``
- `` `JSR $1234` on line 9 targets $1234, where nothing was assembled``
- `the path reaches data on line 40 that is not a known instruction (`$02`)`
- `` `JSR helper` on line 9 returns into data on line 10``
- `the path runs past the last byte assembled in bank 3 after line 88`
- A reason inside a callee is prefixed with `through `JSR helper` on line 9, `.

**`unused-lint-ignore`** (at the directive's line, at the `@` column):
- `` `@nessemble-lint-ignore-next-line` suppresses `undeclared-clobber`, which reports nothing on line 41``
- `` `@nessemble-lint-ignore-next-line` suppresses `overdeclared-clobber`, `undeclared-clobber`, which report nothing on line 41``
- `` `@nessemble-lint-ignore-next-line` suppresses nothing on line 41``
- `` `@nessemble-lint-ignore start` suppresses `require-block-comment`, which reports nothing on lines 3–90`` (or `on lines 3 to the end of the file`)
- `` `@nessemble-lint-ignore start` suppresses nothing on lines 3–90``

The first backticked text is the directive token, which is where `diagnostic_range` narrows.

**`require-routine-doc`**: the text is unchanged: `called routine `NAME` has no signature annotations`.

### 5.2 Assembly and config errors (stderr)

- **An explicit entry that fails to assemble**: each error is printed exactly as the build prints it (``Error in `main.asm` on line 12: Symbol `x` was not defined``), then:
  ``nessemble: lint: `main.asm` did not assemble, so routine rules were skipped for it``
  The exit code is non-zero.
- **A missing entry**:
  ``nessemble: entry `main.asm` (set in `src/.nessemblerc`) does not exist``
- **`entry` inside an override**: serde's existing message, `invalid config `…`: unknown field `entry`…`.
- **`assemble.pseudo` inside an override**:
  ``in `….nessemblerc` overrides for `GLOB`: `assemble.pseudo` may only be set at the top level``

The LSP silently degrades to file mode on all of these, as today with a bad config.

---

## 6. Tests

Tests are written first in each phase, in the existing layout:
- unit tests in `program.rs`, `clobbers.rs` and `tooling.rs`'s `mod tests`;
- integration tests in a new `crates/nessemble-core/tests/clobber_analysis.rs`, which builds multi-file programs with an in-memory `FileOverlay` via `build_program`;
- CLI tests in `crates/nessemble-cli/tests/cli.rs`, using `lint_dir`;
- LSP tests in `nessemble-lsp/src/lib.rs`'s tests;
- rc tests in `nessemble-rc/src/lib.rs`.

**Recording (Phase 1)**
- **Byte identity**: every `tests/corpus` golden assembles identically with `record_program` on and off.
- `Emit` fields: bank, phased `cpu_addr`, ROM offset, and `site` (top level, a nested macro naming the outermost call, `emit_source` naming the pseudo-op line, the `.for` body line per iteration).
- `.if`-suppressed statements and the trainer make no emits.
- Anchors: a label, `label+2`, `:+`/`:-`, a pasted `row\i`, and a constant (no anchor).
- Two `.prg` banks both `.org $8000`: a label in bank 1 resolves by anchor; numeric `JMP $8000` is ambiguous.
- Two overlapping `.phase` blocks in one bank.
- `Site` for a nested macro expansion is the outermost call, derived from plan 017's expansion chain.

**Blind spots, each a minimal two-file program (`main.asm` includes `util.asm`)**
1. **Cross-file callee**: the caller declares `A`, and `JSR helper` goes to `util.asm`'s helper, which declares `A, X`. Program mode reports X with the origin `whose contract clobbers X`. File mode does not, which pins today's behaviour. The reverse direction is also tested: a verified cross-file call where the caller over-declares now gets `overdeclared-clobber`.
2. **Fall-through and always-taken branch**: `gated:` in `main.asm` falls through an `.include` into the contracted `ungated:` (X, Y) in `util.asm`. The truthful list `A, X, Y` gives no finding, and leaving out Y gives undeclared Y. A variant uses `LDX #0 / BEQ ungated`: the branch is pruned and not reported.
3. **Save/restore**:
   - `PHA … JSR util_eats_a … PLA` preserves A;
   - `TXA PHA … PLA TAX` preserves X and clobbers A;
   - the `TYA` form;
   - all with the callee in `util.asm`.
4. **Unreachable fragment**: `r: JSR far_routine / RTS` followed by unlabeled `LDX #0 / RTS`. Not counted.

**Required cases**
- Callee **with** a contract is trusted. Its own extra write is reported on the callee, not on the caller.
- Callee **without** a contract is walked: its X write reaches the caller with a "reached through" origin.
- **A branch where only one arm writes X** reports X.
- **A save on one path but not the other**: `BEQ :+ / TXA / PHA … PLA / TAX / RTS` against the other arm `LDX #1 / RTS` must report X.
- **Indirect `JMP`** gives `unverifiable-clobbers`. Overdeclared is suppressed, and a definite undeclared write is still reported.
- **Unreachable code after `RTS`** is not counted.
- **A `.for`-unrolled body**: a write is counted, and a write under `.if \i == 2` is counted.
- **An `.if`-guarded write assembled out** is not counted. The same source with the guard true is counted.
- **`unused-lint-ignore`**:
  - it fires for a no-longer-needed suppression, both next-line and region;
  - it does not fire for a needed one;
  - per-rule granularity on a two-rule list;
  - a rule configured `off` is not judged;
  - file mode does not judge program rules;
  - a bare suppression in file mode is not judged;
  - an assembled-out label is not judged;
  - a region naming `unused-lint-ignore` counts as used;
  - a suppression on a routine with no clobber list is flagged.

**Further cases**
- The split hand-encoded branch (`.db $B0` / `br_off:` / `.db target-(br_off+1)`) is verified: both arms are followed and it is not unverifiable.
- The `.db $2C` skip.
- An unknown opcode reached gives unverifiable.
- `JSR` followed by an inline `.dw` gives "returns into data".
- RTS dispatch (`PHA PHA RTS`) gives a computed jump.
- A callee that does `PLA PLA` gives "pulls its own return address".
- Recursion: mutual recursion between two uncontracted routines reaches the fixpoint.
- Falling through into a contracted routine with a save pending walks into it.
- Stacked labels at one address do not trust each other's contract.
- `@local` targets are followed and never stop the walk (written once plan 021 lands; gated until then).
- `.assert` lines inside a routine (plan 022) and a routine that falls into a `.table` (plan 020): the first is invisible, the second reaches data.
- A macro-body write names the call site in the message.
- `emit_source` instructions are seen, via a test resolver returning `CustomOutput::Source`.
- `require-routine-doc` sees a cross-file `JSR` and a tail `JMP`, and ignores a loop-back `JMP`.
- `lint.ignore` names are trusted as callees with no findings of their own.
- **Memory saves (Phase 4)**:
  - `STX tmp_x … LDX tmp_x` preserves X;
  - an intervening `INC tmp_x` releases it;
  - a contracted callee listing `[tmp_x]` releases it;
  - exiting before the reload commits;
  - an indexed store does not release (the documented limitation).

**CLI**
- `lint main.asm` reports a finding in `util.asm` under its own header.
- `lint src/` with `entry` in the rc.
- `lint frag.asm` with no entry gives file-mode output identical to v2.27 (golden string).
- An entry that fails to assemble gives the `Error in` text and exit 1.
- `--entry` and `-p` work.
- `.nessembleignore`d included files are analysed but not reported.
- Two entries test the merge rules.

**rc**
- `entry` as a string and as an array.
- `entry` in an override is an error.
- `assemble.pseudo` at the root, and in an override (an error).
- An unknown `assemble` key is an error.
- `assemble.pseudo` coexists with plan 017's `assemble.rules` in one block.

**LSP**
- A two-file workspace with an rc entry gives the same lint diagnostics as `lint_programs`.
- An unsaved edit in the callee changes the caller's finding.
- A cache-miss custom pseudo-op inside a routine produces no `unverifiable-clobbers`.
- The quick fixes (add clobber, remove suppression).

**Acceptance on the reference corpus** (manual, run by the maintainer and recorded as counts only in the "As built" section). First add `entry` and `assemble.pseudo` to its `.nessemblerc`. Then:
1. Check the ROM bytes, with and without recording, against the reference ROM.
2. Run `nessemble lint <entry>`.
3. Run again with the 38 suppressions deleted.

---

## 7. Docs and CHANGELOG

**Docs**
- **`docs/src/usage.md` § lint**:
  - Explain program mode and file mode, and the new flags.
  - Add the two new rules to the rules table, and update `overdeclared-clobber`'s "unknowns" text to "unverifiable paths".
  - Replace the "two known blind spots" subsection under *Suppressing a finding* with what the analysis follows and the remaining limits (indirect jumps, RTS dispatch, inline data after `JSR`, indexed aliasing).
  - Replace that subsection's two worked examples with neutral identifiers.
  - Document `unused-lint-ignore`.
- **`usage.md` § .nessemblerc**: `entry` and `assemble.pseudo`, beside plan 017's `assemble.rules`.
- **`docs/src/editor.md`**: program-mode lint in the editor, the cache-only rule, and the new quick fix.
- **`docs/src/upgrading.md`**:
  - `unused-lint-ignore` is on by default;
  - adding `entry` changes clobber findings;
  - how to clean up suppressions (run the CLI and apply the quick fix).
- **Plans**: in `plans/010-routine-signatures.md`, add an "As built" note at §8.1 pointing here. This plan gets its own "As built" section when it ships.

**Changesets** (one per phase PR, via `cargo run -p xtask -- changeset add …`; `CHANGELOG.md` is never edited):

| Phase | Type | Text |
| --- | --- | --- |
| 0 | `minor` | "`.nessemblerc` accepts `entry` and `assemble.pseudo`, read by `nessemble lint` and the language server." |
| 1 | `none` | (internal recording) |
| 2 | `minor`, **behaviour change** when `entry` is set | "`nessemble lint` analyses the whole program from its entry file: routine clobber checks follow calls across files, fall-through, branches and save/restore pairs, see macros, `.for` and custom pseudo-instructions as the code they emit, and report indirect jumps as the new `unverifiable-clobbers` warning." |
| 3 | `minor`, **behaviour change** for every project (a new default-`warn` rule) | "New lint rule `unused-lint-ignore` reports suppressions that no longer suppress anything." |
| 4 | `minor` | "Clobber checks treat a register stored to memory and reloaded before every exit as preserved." |
| 5 | `minor` | "The language server reports the same whole-program clobber findings as `nessemble lint`, with a quick fix to remove an unused suppression." |

---

## 8. Risks, open questions, phasing

### 8.1 Phases (each separately mergeable and revertible)

0. **Schema**: `entry`, `assemble.pseudo` and the CLI flags (parsed, validated, not yet used), extending plan 017's `RcAssemble`.
1. **Recording**: `Symbol.rom_offset`, `program.rs`, `build_program`, and the byte-identity tests (reusing plan 017's expansion table).
2. **Analysis and program mode**: `clobbers.rs` with stack saves, `lint_programs`, the three clobber rules and `require-routine-doc` in program mode, and the CLI. *The phase to review hardest.*
3. **`unused-lint-ignore`**, in both modes.
4. **Memory saves** (stretch). Droppable, but full acceptance needs it for two routines.
5. **LSP**: hooks, the cache-only resolver, entry-driven roots, and the quick fixes.
6. **Docs, and the corpus acceptance run**, recorded in "As built".

### 8.2 Risks

- **Decoding data as code.** Data bytes reached by a never-taken arm or after a non-returning call would invent writes, which are false undeclared findings. Mitigations: constant-flag pruning, "returns into data", "pulls its own return address", and messages that say "hand-encoded … in `.db`", so the cause is visible. If the corpus shows any, the next step is to restrict decoding to data reached by fall-through or branch from instruction emits.
- **New genuine findings.** Macros and custom pseudo-ops were invisible to both today's lint and the external checker. If they reveal real undeclared writes, acceptance item 1 is met by fixing the corpus, not nessemble. Each such finding names its macro or pseudo-op site.
- **The RTS-dispatch policy** adds one `unverifiable-clobbers` on the corpus that the external checker treats as a plain return. Chosen: call it what it is, a computed jump through the stack, an indirect jump in all but opcode.
- **Indexed and indirect stores** can alias a memory save (Phase 4) without releasing it. This is documented, and acceptable for a warn-level lint.
- **Macro attribution.** Build diagnostics report a macro-expanded line at its body line with expansion notes (plan 017). Lint findings instead sit at the routine's label line and name the outermost call site inside the message (§5.1), because a routine is written where it is called from, not in the macro body.
- **Annotated labels inside a macro body** are not analysed: they bind per expansion, with no text line of their own. Documented.
- **LSP cost.** One extra walk per round over an assembly the LSP already does; it should be milliseconds. Recording adds per-statement bookkeeping, and only while `record_program` is on.
- **LSP vs CLI with a cold cache.** The editor reports a subset until a build has run. `emit_source` output is never cached (plan 011), so routines containing it stay partly unmodelled in the editor.
- **Multiple entries** (array) are proposed and more than the brief strictly needs. They could be cut to a single string with no other changes.

### 8.3 The corpus's 38 clobber suppressions, classified (counts only)

Each suppression was classified by its reason comment, then checked against v2.27 single-file lint with the suppression stripped. **26 are live today** (removing one brings back a finding). **12 are already inert.**

| Cause (from the reason comment) | Rule | Count | Live today |
| --- | --- | ---: | ---: |
| Blind spot 2: fall-through into the next block | overdeclared (one also undeclared) | 13 | 11 |
| Blind spot 2: branch into, or always-taken branch into, the next block | overdeclared | 2 | 2 |
| Blind spot 3: push/pull save (A ×2, X ×1, X+Y ×2) | undeclared | 5 | 5 |
| Blind spot 3: memory save (X in a temp, Y in a temp) | undeclared | 2 | 2 (Phase 4) |
| Blind spot 4: unreachable fragment folded into the routine (one via a callee's block) | undeclared | 7 | 6 |
| No `@nessemble-clobbers` on the routine at all (undeclared-clobber cannot fire) | undeclared | 8 | 0 |
| RTS-through-pushed-address dispatch (computed jump) | overdeclared | 1 | 0 |
| Blind spot 1 cited directly | — | 0 | — |
| **Total** | 22 undeclared, 15 overdeclared, 1 both | **38** | **26** |

The three inert entries in otherwise live categories (two fall-through, one unreachable fragment) are most likely inert because of blind spot 1: the routine also calls out of file, which already suppresses `overdeclared-clobber` and hides a callee's writes.

**Expected results with this plan:**
- **Suppressions deleted**: all 26 live findings disappear. 24 of them need Phases 2–3; the 2 memory-save ones need Phase 4.
- **Suppressions kept**: `unused-lint-ignore` flags all 38. The 12 inert ones are flagged as soon as program mode runs.
- **Anything not flagged** is a real disagreement to fix in the corpus.
- **`unverifiable-clobbers`**: about 10 routines. The 9 indirect `JMP [ptr]` cases plus the one RTS dispatch; the split hand-encoded branch is now verified. (The acceptance criterion "unverifiable only for indirect jumps" holds if the RTS-dispatch policy above is accepted as an indirect jump.)
- **The 43 `require-block-comment` warnings** are unchanged. `require-routine-doc` (enabled at `warn` there) may gain findings for undocumented cross-file call targets, which the acceptance criteria allow. All six of the corpus's `@nessemble-lint-ignore start require-block-comment` regions were verified to be in use, so `unused-lint-ignore` adds nothing there.

### 8.4 Open questions

1. **Whether `unused-lint-ignore` should default to `warn`.** The brief says it does. It is a behaviour change for every project with stale suppressions.
2. **The `JMP` part of `require-routine-doc`.** Proposed: only a tail `JMP` from a different text block. The alternative is every `JMP` target, which fires on loop-back labels.
3. **Whether `assemble.pseudo` belongs in this schema change or should be a lint-only `-p`.** Proposed: in the schema, because the LSP needs it.
4. **Whether the plain build (`nessemble main.asm`) should also read `entry`/`assemble.pseudo`.** Settled: not in this plan. Plan 017 already makes the build read `.nessemblerc` (for `assemble.rules`), so it would be a small follow-up.
5. **The `entry` key name** (owner's open point). Proposed: `"entry"`, root-only, string or array.

### 8.5 Out of brief (not done)

- Caller-side preservation checking.
- Verifying memory clobbers.
- Verifying flags.
- Resolving RTS-dispatch tables.
- Modelling the inline-arguments-after-`JSR` idiom.
- Fixing macro diagnostic attribution in build errors.
- `-u` / undocumented-opcode programs in lint.
- File-system lint for WASM.

---

### Critical files for implementation

- `crates/nessemble-core/src/tooling.rs`
- `crates/nessemble-core/src/assemble.rs`
- `crates/nessemble-cli/src/lint.rs`
- `crates/nessemble-rc/src/lib.rs`
- `crates/nessemble-lsp/src/lib.rs`

Also touched: `crates/nessemble-core/src/preprocess.rs`, `crates/nessemble-core/src/lexer.rs`, `crates/nessemble-core/src/ast.rs`, `crates/nessemble-core/src/parse.rs`, `crates/nessemble-core/src/lib.rs`, `crates/nessemble-cli/src/custom.rs`, `crates/nessemble-cli/src/lsp.rs`, `docs/src/usage.md`, `docs/src/editor.md`, `docs/src/upgrading.md`, and the new files `crates/nessemble-core/src/program.rs`, `crates/nessemble-core/src/clobbers.rs` and `crates/nessemble-core/tests/clobber_analysis.rs`.

---

## As built

Where the implementation differs in detail from the sections above:

- **One PR, all phases.** Phases 0–6 shipped together, one commit and one changeset per phase (Phase 1's is `none`). Phase 4 (memory saves) is included, not dropped.
- **`ProgramMap` carries more.** Besides §2.2's fields it has `written` (the write bitmap: an instruction byte that was never written ends the path), `undocumented` (`-u`), and `symbols` (every defined name and value, for resolving a contract's `[symbol]` memory slots). `Emit` has an `operand` field: the instruction's operand rendered back to source (`helper`, `[vec_ptr]`, `:+`), so messages can say `` `JSR helper` `` and `` `JMP [vec_ptr]` ``. `ProgramMap::resolve`, `resolve_addr` and `fall_through` implement §2.2's three resolution rules; `Resolution` is `At`, `Ambiguous(banks)` or `Nothing`.
- **Where recording hooks in.** `exec_stmt` wraps the old body (`exec_stmt_inner`): on pass 2 it notes the operand anchor and the author's site, and `write_byte` accumulates the statement's bytes. A custom pseudo-op's `emit_source` lines record their own emits (they run through `exec_stmt`), so the enclosing statement records none. `LabelDef`s come from `add_label` and the pasted-label path. `Symbol.rom_offset` is set in `define_symbol` from `rom_index()`.
- **The fixpoint joins.** Each round of the callee-summary fixpoint joins into the last (writes, reasons, stores and `pulls_return` only grow), so it is monotone by construction and stops when nothing changes; 64 rounds is a backstop. A single walk that exceeds 200,000 distinct states ends with the reason "it has too many paths to follow".
- **A routine that pulls its own return address.** A contracted routine with a `PLA`/`PLP` at depth 0 is `unverifiable-clobbers`: `` `PLA` on line 7 pulls the routine's own return address ``. A `TXS` alone is not (a reset handler does `LDX #$FF / TXS`), but an `RTS` after it is, as §2.4 says.
- **Open saves at an exit commit.** Any path end (including an unverifiable one) commits every save still open, so an `RTS` with a pushed register still on the stack reports the write.
- **Memory saves**, as §2.5, with every non-indexed zero-page or absolute `STA`/`STX`/`STY` opening one. A reload that restores one is recorded, like a `PLA`, for `overdeclared-clobber`'s "(it is saved on line 4 and restored on line 6)".
- **Message details.** Contract origins say `whose contract clobbers Y` for a `JMP` or branch into a contract too (`` `JMP next` on line 9, whose contract clobbers Y ``). An undocumented or unknown opcode inside an instruction emit reads `` the path reaches `$02` on line 40, which is not a known instruction ``; the "data" wording is kept for data emits. Overlapping `.phase` blocks in one bank read `which bank 0 assembles code at more than once`. "of `F`" uses the path the report prints: in the CLI the header path (closure files are shown relative to the current directory), in the editor the absolute path.
- **`unused-lint-ignore`'s merge.** Findings from several programs are merged before suppressions are applied, so a suppression is used if its rule fired in any program — which is §2.1's "unused only if unused in all of them". A bare suppression is judged where every rule that is not `off` ran.
- **LSP hooks.** `ServerHooks { custom_resolver: ResolverFactory }`, where `ResolverFactory` is `Box<dyn Fn(Option<&Path>) -> CustomResolver + Send + Sync>` (the server is sent across a thread in tests). The CLI's factory is `custom::build_cache_reader`. A name the hook does not know but a workspace mapping file declares still resolves to `Unmodeled`, so project pseudo-ops are never reported as unknown. `run()` keeps the lenient resolver. Program mode in the editor applies only when the discovered `.nessemblerc` has an `entry` whose closure holds the file; otherwise the include graph's roots are used, as before.
- **The CLI's entry config** is the first path argument's `.nessemblerc` for building (`assemble.rules`, `assemble.pseudo`); every path's `entry` list is collected.
- **Not done here:** the §6 acceptance run on the reference corpus (it is not in this repository).

## Decisions settled with the maintainer

1. `unused-lint-ignore` defaults to `warn` (§8.4 Q1).
2. Returning through a pushed address (`RTS` dispatch) is reported as `unverifiable-clobbers`, the same as an indirect jump (§8.2).
3. The key is `"entry"`, root-level only, taking a string or an array (§8.4 Q5).
4. The plain build does not read `entry` or `assemble.pseudo` in this plan (§8.4 Q4), although plan 017 already makes it read `.nessemblerc`.
