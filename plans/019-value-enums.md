# nessemble-rs: A Plan for Value Enums (bare names in `.enum`)

> Status: **Shipped.** Built as designed; the deviations found while building
> are recorded in [As built](#as-built). Provides the enum registry consumed by
> [plan 020](020-index-keyed-tables.md) (`.table NAME`).

---

## 1. Summary

`.enum START[, STEP[, NAME]]` now accepts **bare names** as members. Each bare name takes the current counter value and advances it by `STEP`, exactly like `NAME .rs 1`. Inside an `.enum`, `NAME = expr` defines `NAME` and resets the counter to `expr + STEP`, so a run of 40 hand-numbered opcode constants becomes 40 lines of names. An optional third argument names the enum. It defines `NAME_COUNT` and records the enum in a pass-1 **enum registry** (name, members with values, start, step, contiguous, count), which the index-keyed `.table NAME` feature (plan 020) reads.

## 2. Semantics

### 2.1 Syntax

```text
.enum START[, STEP[, NAME]]
    members
.endenum
```

A member line is one of:

| Form | Value | Counter afterwards |
|---|---|---|
| `NAME` (bare, column 1) | counter | counter + STEP |
| `NAME .rs N` (unchanged) | counter | counter + N×STEP |
| `NAME = expr` | `expr` | `expr` + STEP |
| `NAME\i` (bare pasted name, inside a `.for`) | counter | counter + STEP |

All the forms can be mixed in one block. Trailing comments are allowed on every member line (`OP_DRAW ; redraw`). Blank and comment-only lines are ignored.

### 2.2 Bare names: decided, with two choices made here

- **A bare name must start in column 1, written flush left.** This matches the parser's rule for every other line: a name in column 1 defines something, and an indented word is an instruction. The parser already turns a column-1 bare name into `Stmt::Constant(name, None)`. The only new part is the assembler's handling while an `.enum` is open (§3.3). The decided example (`.enum` indented, members flush left) follows this rule.
- **An indented bare name inside an `.enum` is still an instruction.** That keeps `  NOP` and `  RTS` inside an enum byte-identical. For an unknown mnemonic with no operand, the error gets a hint (§5, E9). Indented `NAME .rs N` keeps working, because the parser checks for `.rs` before it looks at indentation.
- **Outside an `.enum`, a bare name is unchanged:** it is still a constant equal to `1` (`FLAG` lists as `0001 = FLAG`).

### 2.3 `NAME = expr` inside `.enum`: decided

This is C-style: the counter is reset to `expr + STEP`, and `NAME` becomes a member with value `expr`.

**Behaviour change.** Today (v2.27.0), `=` inside an `.enum` does **not** move the counter. For example:

```nessemble
.enum $10, 2
FOO .rs 1
BAR .rs 1
QQ = 5
ZZ .rs 1
.endenum
```

This lists `ZZ` as `$0014` today. Under this plan `ZZ` is `$0007`. This is the brief's decided design, so it counts as a named exception to "no valid program changes output", together with the bare-name fix. The reference corpus contains no `.enum` blocks at all, so it is unaffected. The test suite pins both values (§6).

### 2.4 The optional name (proposed, settled here as `.enum START, STEP, NAME`)

- **Spelling and position: third argument, a plain identifier** (`.enum $00, 1, OP`).
  - `START` is an expression that may itself begin with a symbol, so `.enum OP, 1` cannot tell a name from a start value. Putting the name last is unambiguous and leaves every existing one- and two-argument form untouched.
  - It is also the form plan 020's examples use.
  - The cost is that a named enum spells out `STEP` even when it is 1. One spelling keeps docs and completion simple, as with `.for`.
- **The name is a single `Text` token, parsed as text rather than as an expression.**
  - A number, string, register letter or `HIGH`/`LOW`/`BANK` in that position is error E5.
  - A pasted name (`T\i`) is not supported: it falls to plan 017's end-of-statement error.
- **What the name denotes: nothing in the symbol table.** It lives only in the enum registry, a separate namespace.
  - `OP` is not usable in expressions, `.ifdef OP` is false, and the name does not appear in the listing.
  - So an enum name **cannot collide with a constant or label**. `.enum 0, 1, OP` and `OP = 3` coexist.
  - The only symbol a named enum creates is `NAME_COUNT`.
  - Reason: the only consumers of the name are `.table NAME` and tooling. Giving it a symbol value (start? count?) would invent a meaning with no use and create a new kind of collision.
- **`NAME_COUNT`** is the literal concatenation `{NAME}_COUNT` (`op` gives `op_COUNT`). It is a constant equal to the number of recorded members, listed under `[constants]`. It is defined when the `.endenum` is processed on pass 1. A reference before that point behaves like any forward constant reference.
- **The member prefix is not enforced.** Members of `.enum $00, 1, OP` may be called anything. Some sets in the reference corpus share no common prefix, and enforcing one would also block mixing `=` aliases.

### 2.5 Registry fields for odd cases

| Case | start | contiguous | count |
|---|---|---|---|
| Empty enum | the evaluated `START` argument | `true` (nothing breaks the run) | 0; `NAME_COUNT = 0` |
| First member is `NAME = expr` | that member's value | judged from there | as usual |
| Any `.rs N` with N≠1 | first member's value | `false`, unless N×STEP lands back on the run, which happens only when STEP = 0 | as usual |
| A reset `=` to exactly the next counter value | — | stays `true` | — |

**Rule:** `contiguous` ⇔ for every i, `members[i].value == start + i × step`. It is defined on values, so `.rs N`, breaking resets and skipped members are covered by one rule. STEP may be zero or negative, as today. With step 0 every member has the same value and `contiguous` is true; §8 raises this for `.table`.

### 2.6 Conditionals, loops, macros, includes, nesting

- **Conditionals** follow the existing `.rs` model. Symbol definition is suppressed in a false `.if`, but counter arithmetic is not: today a `.rs` inside `.if 0` within an `.enum` still advances the counter.
  - Bare names and `=` resets do the same, since a bare name is exactly `.rs 1`.
  - A suppressed member is not recorded in the registry and is not counted in `NAME_COUNT`. It leaves a gap, so `contiguous = false`.
  - A named `.enum` that is itself inside a false conditional records nothing and defines no `NAME_COUNT`.
  - See §8, Q1: this is a footgun worth fixing separately.
- **`.for`** inside an `.enum` works. `SLOT\i` on its own line defines `SLOT0`, `SLOT1`, … as successive members. Today each is `1`: this is part of the bare-name behaviour change. A named `.enum` inside a `.for` body triggers E4 on its second iteration, reported once through the existing per-loop dedup.
- **Macros**: a column-1 bare name in a `.macrodef` body becomes a member when the macro is expanded inside an `.enum`. Diagnostics name the call site, as they do today.
- **Includes**: the preprocessor splices `.include`d files into one token stream, so an `.enum` in an earlier-included file is in the registry before a later `.table`.
- **Nesting**: unnamed nesting keeps today's behaviour, which is that an inner `.enum` resets the counter and the first `.endenum` closes everything. Opening any `.enum` while a **named** one is open, or opening a named one while any `.enum` is open, is error E7. No existing program uses named enums, so this changes no valid output.
- **Stray `.endenum`** stays silently accepted, as today. An unterminated **named** `.enum` is error E8. An unterminated unnamed one stays silent, as today.

### 2.7 Passes

- Symbols are written only on pass 1 (`define_symbol` returns early on pass 2). The registry follows the same rule: it is built on pass 1 and read-only on pass 2. Pass 2 re-runs only the counter arithmetic, which gives the same values because `.for` bounds are replayed (plan 016 §12).
- **Contract with `.table` (plan 020):** on pass 1, the registry holds every named enum whose `.endenum` came earlier in the token stream. On pass 2 it is complete. `.table` validates in the shared final-pass hook (plan 022 §3), which allows forward references and reports each diagnostic once.
- Registry diagnostics (E4, E7, E8, and the E3 warning) are raised on pass 1 only, so they are not duplicated.

### 2.8 Listing (`-l`, `--mlist`)

- Every member is a constant (`SymType::Enum`, listed under `[constants]`). `=` members also become `SymType::Enum`, which lists identically to `Constant`.
- `NAME_COUNT` is `SymType::Constant`.
- `--mlist` filters only labels, so constants created inside macros are listed either way, as today.

## 3. Code changes

Every change to shared code is marked **[shared]**.

### 3.1 AST: `crates/nessemble-core/src/ast.rs` [shared]

- `Pseudo::Enum(Expr, Option<Expr>)` becomes `Pseudo::Enum { start: Expr, step: Option<Expr>, name: Option<String> }`.
- Only `parse.rs` and `assemble.rs` match on it (checked with grep).

### 3.2 Statement parser: `crates/nessemble-core/src/parse.rs` [shared]

- **`"enum"` arm of `parse_pseudo`.** After `STEP`, an optional `, NAME`:
  - `Tok::Text(n)` becomes `Some(n)`.
  - Any other token is E5.
  - A comma without `STEP` (`.enum $00, , OP`) is a normal expression error.
- **`Parser.enum_depth: u32`.** Incremented on `.enum`, reset to 0 on `.endenum`, mirroring the assembler's flat model. It is used only for diagnostics: in `parse_stmt`, when `enum_depth > 0` and an unindented statement starts with a token that cannot begin a name (`Number`, `CharReg`, `QuotString`, `High`/`Low`/`Bank`, an operator), emit E2 instead of `unexpected token at start of statement`.
  - This only replaces one error message with another: those lines are errors today.
  - Emitted custom pseudo-op source is parsed separately and does not see `enum_depth`. It then gets the generic message, which is acceptable.
- **No change to how bare names parse.** `Stmt::Constant(Name, None)` already covers both plain and pasted bare names.

### 3.3 Assembler: `crates/nessemble-core/src/assemble.rs` [shared: symbol table]

**New types (public in the crate, re-exported from `lib.rs`):**

```rust
/// One named `.enum` (plan 019 §2.4). Recorded on pass 1; read-only on pass 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumInfo {
    pub name: String,
    /// Where the `.enum` was written (for diagnostics that point at it).
    pub file: u32,
    pub line: u32,
    /// Value of the first member, or the evaluated START when empty.
    pub start: i64,
    pub step: i64,
    /// Recorded members in declaration order.
    pub members: Vec<EnumMember>,
    /// `members[i].value == start + i*step` for every i.
    pub contiguous: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumMember {
    pub name: String,
    pub value: i64,
    /// Counter units consumed: 1 for a bare name, N for `.rs N`, 1 for `=`.
    pub size: i64,
    pub file: u32,
    pub line: u32,
}

impl EnumInfo {
    pub fn count(&self) -> usize { self.members.len() }
}
```

**Assembler fields:**

- `open_enum: Option<OpenEnum>` records the name, file, line, `from_macro`, whether it was suppressed, the start, step and members collected so far, plus a `HashSet` of member names for the duplicate check.
- `enums: Vec<EnumInfo>` and `enum_index: HashMap<String, usize>` hold the registry. They are created in `new`, **not** cleared in `reset_state` (like `symbols`), and pushed only when `pass == 1`.
- Lookup is `pub(crate) fn enum_info(&self, name: &str) -> Option<&EnumInfo>`. This is the function `.table NAME` calls.
- `enum_active`, `enum_value` and `enum_inc` stay as they are. `reset_state` also clears `open_enum`.

**Execution:**

- **`Pseudo::Enum`:**
  - Apply E7 if a named enum is involved and one is already open.
  - On pass 1, apply E4 against `enum_index`.
  - Set the counter state as today, then start a fresh `OpenEnum`. An unnamed nested `.enum` replaces the open frame, matching today's reset.
- **`Pseudo::Endenum`:** if the open frame is named, on pass 1, and not suppressed:
  - compute `start` and `contiguous`;
  - push it to the registry;
  - define `NAME_COUNT` through `define_symbol(..., SymType::Constant, from_macro_of_enum_line)`, with `cur_file`/`cur_line` temporarily set to the `.enum` line, so plan 017's `duplicate-symbol` warning (E3) points where `NAME` is written;
  - then clear the frame.
- **`Stmt::Constant(name, None)` while `enum_active`:** handled exactly as `Pseudo::Rs(name, 1)`, through a shared helper `enum_member(name: &Name, value, size)`. That helper:
  - calls `add_symbol(SymType::Enum)`, or `add_pasted_symbol` for a pasted name;
  - records the member in the open frame on pass 1 when the line is not suppressed;
  - runs the duplicate check E1;
  - advances the counter by size × step. The counter advances on both passes, regardless of suppression, as `.rs` does today.
- **`Stmt::Constant(name, Some(e))` while `enum_active`:** evaluate, call `enum_member` with `value = e`, then set `enum_value = value + enum_inc`.
- **`Pseudo::Rs` while `enum_active`:** routed through the same `enum_member` helper, so `.rs` members are recorded too. Values and counter arithmetic stay byte-identical.
- **E1 (duplicate member) fires only when the enum is named, or when either definition is a bare name.** A duplicate `.rs` or `=` member in an unnamed enum is valid today (the last one wins), so it gets only plan 017's `duplicate-symbol` warning.
- **End of pass 1 in `run()`:** if `open_enum` is named, raise E8 at its stored line.
- **New `fn error_at(&mut self, file: u32, line: u32, msg)`** for E4, E7 and E8, which point at a stored line rather than the current one. (Plan 022's final-pass queue needs the same helper; whichever lands first adds it.)
- **`exec_instruction` unknown-opcode path:** when `enum_active` and the operand is `Operand::Implied`, use E9 instead of `unknown-opcode`.

### 3.4 Public API: `crates/nessemble-core/src/lib.rs`

- Add `pub enums: Vec<EnumInfo>` to `Assembly`. It is constructed in one place (`lib.rs` near line 637), so the change is safe. Tests and the LSP use it.
- Re-export `EnumInfo` and `EnumMember`.

### 3.5 Messages: `crates/nessemble-i18n/locales/en-US.ftl` [shared]

- Add a `## Enums (.enum / .endenum)` section with the keys in §5.

### 3.6 Formatter: `crates/nessemble-core/src/tooling.rs` [shared]

- `is_indented`: a line whose significant lexemes are exactly one name (plain, or pasted per `name_lexemes`), plus an optional comment, **keeps the indentation it was written with**: column 1 stays in column 1, and an indented line gets the indent unit.
- Today the formatter indents a column-1 `FLAG`, which turns a constant into an `Unknown opcode` error, as observed on v2.27.0. It would do the same to every bare member. The rule mirrors the parser exactly, so `format` never changes the bytes.
- No other formatter change. `.rs` lines are still indented, which is harmless.

### 3.7 LSP: `crates/nessemble-lsp/src/lib.rs` [shared]

- **`definitions()`**: track `.enum` … `.endenum` with a flat flag, as the assembler does.
  - In a body, a line-initial identifier (or pasted name) with nothing after it but a comment, or followed by `.rs`, is a `DefKind::EnumMember`.
  - On the `.enum` line, a third-argument identifier yields `DefKind::Enum` (named `NAME`) and a constant `NAME_COUNT` whose range is the `NAME` token.
- **`DefKind`**: add `Enum` and `EnumMember`.
  - `document_symbol` maps them to `SymbolKind::ENUM` and `SymbolKind::ENUM_MEMBER` ("enum", "enum member").
  - Matches that accept `Label | Constant` (around line 2355, and the pasted-hover "constant" wording) also accept `EnumMember`.
- **Hover and completion** values already come from `ListSymbol`, so members and `NAME_COUNT` show their values without any change.
- **Rename**: `rename` / `prepareRename` on a `NAME_COUNT` occurrence is rejected ("`OP_COUNT` is generated by `.enum OP`; rename the enum instead"). Renaming the enum does not rewrite `NAME_COUNT` references in v1 (§8).
- **Semantic tokens**: no change.

### 3.8 Not changed

- Lexer, expression parser, preprocessor, `.nessemblerc` schema (`nessemble-rc`), coverage, the scripting crates and wasm.

## 4. Tooling parity

| Tool | Own parser? | Change |
|---|---|---|
| assemble / listing `-l`, `--mlist` | core parser | §3.2–3.3; members and `NAME_COUNT` under `[constants]` |
| `format` | yes (lexeme-based, `tooling.rs`) | §3.6. Idempotent; the plan's example formats to itself |
| `lint` | yes (lexeme-based) | **No new rule.** The clobber verifier already treats `.enum` as transparent and a non-mnemonic column-1 word as "not fully understood", so bare members only make a body more conservative. A member that shares a mnemonic's name (`TAX` in column 1) inside a routine body would be misread; see §8 |
| `coverage` | source map | None: member lines emit no bytes, so they record no spans. A test pins this |
| LSP | yes (`definitions()` scanner) plus the assembler's symbols | §3.7 |
| `nessemble reference directives enum` | static table | §7 |

## 5. Diagnostics

All diagnostics use the usual framing, ``Error in `f.asm` on line N: …``. For a macro expansion, N is the call site.

| # | Key | Text | Line | When |
|---|---|---|---|---|
| E1 | `enum-member-twice` | ``Enum member `{name}` is defined twice in the same `.enum` `` | second definition | Named enum, or either definition is bare (§3.3). Pass 1 |
| E2 | `enum-member-invalid` | ``Expected an `.enum` member name (two or more letters, digits or `_`, not starting with a digit)`` | the member line | Parse time |
| E3 | (plan 017) `duplicate-symbol` | **Warning**, in plan 017's wording (its §3: ``… is already defined at f.asm line N (…); this definition replaces it  [duplicate-symbol]``) | the `.enum` line | `NAME_COUNT` collides with an earlier symbol. A later redefinition warns at the later line |
| E4 | `enum-name-twice` | ``Enum `{name}` is already defined on line {line} of `{file}` `` | second `.enum` | Pass 1 |
| E5 | `enum-name-invalid` | ``The `.enum` name must be a name, as in `.enum $00, 1, OP` `` | `.enum` line | Parse time |
| E7 | `enum-nested-named` | ``An `.enum` cannot be opened inside another `.enum` when either is named; close the `.enum` on line {line} with `.endenum` first`` | inner `.enum` | Pass 1 |
| E8 | `enum-unterminated` | ``.enum {name}` has no matching `.endenum` `` | `.enum` line | End of pass 1 |
| E9 | `enum-member-indented` | ``Unknown opcode `{mnemonic}` (an `.enum` member must start in the first column)`` | member line | Replaces `unknown-opcode` inside `.enum` for an implied-operand line |

Notes:

- E3 is plan 017's `duplicate-symbol` warning ("treat as duplicate symbol"), at whatever severity the project sets.
- A fourth argument (`.enum 0, 1, OP, X`) is caught by plan 017's end-of-statement error, so no new key is needed.
- There is no E6.

## 6. Tests (written first)

### `crates/nessemble-core/tests/value_enums.rs` (new)

This file uses the same helpers as `for_loops.rs` (`bytes`, `assembly`, `error`, `sym`), plus a `listing(src)` helper built on `render_list_file`.

1. **Bare names.** `.enum $00, 1, OP` with `OP_YIELD` / `OP_DRAW` / `OP_FRAME` gives 0, 1, 2 and `OP_COUNT = 3`. The listing is exactly `[constants]\n0000 = OP_YIELD\n0001 = OP_DRAW\n0002 = OP_FRAME\n0003 = OP_COUNT\n`.
2. **Mixed bare and `.rs`.** `.enum $00` with `AA` / `BB .rs 2` / `CC` gives 0, 1, 3.
3. **Step 2.** `.enum $10, 2` with `AA` / `BB` / `CC .rs 2` / `DD` gives $10, $12, $14, $18.
4. **Counter reset.**
   - `.enum 0, 1, E` with `AA` / `BB` / `CC = $10` / `DD` gives 0, 1, $10, $11; `E_COUNT = 4`; registry `contiguous == false`.
   - A reset to the next value (`CC = 2`) keeps `contiguous == true`.
   - With step 2, `CC = $10` gives `DD = $12`.
5. **The `=` behaviour change**, with the v2.27.0 value quoted in a comment: the §2.3 program gives `ZZ = $07` (it was `$14`).
6. **`NAME_COUNT`.**
   - `.db OP_COUNT` placed before and after the enum both emit the count.
   - The listing contains `NAME_COUNT`.
   - `op` gives `op_COUNT`.
   - The name is not a symbol: `.ifdef OP` is false, and `OP = 3` alongside `.enum 0, 1, OP` assembles.
7. **Empty enum.** `.enum 5, 1, E` / `.endenum` gives `E_COUNT = 0`, `start == 5`, `contiguous`, no members.
8. **Comments on member lines.** `OP_A ; c` and `OP_B;c` are both members, with correct values.
9. **Registry contract** via `Assembly::enums`: name, member names and values in order, start, step, contiguous, count, for a `.rs 2` member (contiguous false), and for an enum in an `.include`d file followed by code in the including file (use `assemble_file` and a temp dir, as `project_root.rs` does).
10. **`.for` inside an enum.** `SL\i` over `0, 2` gives `SL0..SL2` = 0, 1, 2 (v2.27.0 gave all `1`).
11. **Macro.** A `.macrodef` body with bare names expanded inside an enum gives members. An E1 raised from an expansion reports the `.macro` call line.
12. **Conditionals pinned (§2.6).** A suppressed bare member advances the counter, is not counted, and sets `contiguous = false`.
13. **Unchanged behaviour.**
    - A bare `FLAG` outside an enum is still `0001`.
    - Indented `  NOP` inside an enum still emits `$EA`.
    - A stray `.endenum` is accepted.
    - Unnamed nested `.enum` keeps today's values (`DD = 9` and `EE` as label `0000` from the v2.27.0 probe).
    - A duplicate `.rs` member in an unnamed enum is not an error.
14. **Diagnostics E1, E2, E4, E5, E7, E8, E9**: exact message and line for each. E3: plan 017's warning text and line.
15. **Byte- and listing-identical `.rs` regressions**, using today's values:
    - `.enum $10, 2` with `FOO .rs 1` / `BAR .rs 1` gives `$10` and `$12`; `.db FOO, BAR` emits `10 12`; listing `0010 = FOO`, `0012 = BAR`.
    - `.enum $00` with `FOO .rs 2` / `BAR .rs 1` gives `$00` and `$02`.
    - Committed golden: `tests/corpus/examples/enum` keeps passing in `corpus.rs`, unchanged.

### Other test locations

- **Parser unit tests** (`parse.rs` `mod tests`): `.enum $00, 1, OP` produces `Pseudo::Enum { name: Some("OP") }`, the two-argument form produces `name: None`, and E5 and E2 are emitted.
- **Formatter** (`tooling.rs` tests):
  - A column-1 lone name stays in column 1, and `  RTS` stays indented.
  - The plan example is a fixed point.
  - Formatting is idempotent.
  - Assembling before and after formatting gives identical bytes.
- **LSP** (`nessemble-lsp` tests):
  - Go-to-definition on `OP_DRAW` lands on the member line; on `OP_COUNT` it lands on the `OP` token.
  - Document symbols have ENUM and ENUM_MEMBER kinds.
  - Hover shows a member's value.
  - Rename of `OP_COUNT` is rejected.
- **Coverage**: a source-map test asserts no spans for enum lines.
- **CLI** (`crates/nessemble-cli/tests/cli.rs`): `-l` output for test 1, and the framed E8 message with exit code 1.

## 7. Docs and CHANGELOG

- **`docs/src/syntax.md`**:
  - Rewrite `### .enum`: usage `.enum START[, STEP[, NAME]]`, the member-forms table from §2.1, the column-1 rule, `=` resets, `NAME_COUNT`, and a short note on what the name is and is not. Use the OP_YIELD example.
  - Update the `.endenum` example.
  - The directive-table summary becomes "Start a block of sequential constants".
- **`docs/src/upgrading.md`**: a note for the release that ships this, covering the two behaviour changes (bare names in an `.enum`; `=` inside an `.enum` resets the counter), with before and after values.
- **`nessemble reference`**: in `crates/nessemble-isa/src/lib.rs` `DIRECTIVES`, change the `.enum / .endenum` summary to "assign sequential constants (bare names, `.rs`, `=` resets); a named enum defines NAME_COUNT".
- **Plan file** and an "As built" section once shipped.
- **Changeset** (`cargo run -p xtask -- changeset add minor "…"`), never `CHANGELOG.md`. This is a **behaviour change**:
  > `.enum` members can now be bare names, each taking the next value, and `NAME = expr` inside an `.enum` resets the counter. `.enum START, STEP, NAME` defines `NAME_COUNT`. **Behaviour change:** a bare name inside an `.enum` was silently `1`, and `=` inside an `.enum` did not move the counter.

## 8. Risks and open questions

- **Q1 (footgun): suppressed members still advance the counter.** This follows today's `.rs` behaviour and the decided "bare name ≡ `.rs 1`". A `.ifdef DEBUG` member therefore leaves a gap in a named enum (`contiguous = false`), which `.table` will report.
  - Recommendation: a follow-up plan that makes all three forms skip counter effects when suppressed.
  - That plan changes output for `.rs` too, so it needs its own named exception.
- **Q2: the indented-member rule.** Some authors indent everything. E9 makes the mistake obvious, but the alternative is "any lone non-mnemonic word in an enum is a member". That needs mnemonic knowledge in the parser and tooling, including `-u` undocumented opcodes, so it was rejected for v1.
- **Q3: the `=` reset is a second output change.** Settled: it ships as a separate release-notes item. No known user is affected; the reference corpus has no `.enum`.
- **Q4: contract additions for `.table` (plan 020).**
  - Added `size`, `file` and `line` per member, plus `file`/`line` for the enum.
  - `contiguous` is defined on values (§2.5), not on "no resets".
  - Step 0 makes every member the same value while `contiguous` stays true. Plan 020 rejects step 0 for a key set, and reads any other step as keys `FIRST + n·step`.
  - The registry is crate-internal (`enum_info`) plus `Assembly::enums`.
- **Q5: renaming an enum.** Rename does not rewrite `NAME_COUNT` references, and rename of `NAME_COUNT` is refused. Find-references on `OP` (an enum name) is out of scope until `.table` gives it references.
- **Pre-existing problems, out of scope:**
  - A `START` that depends on a forward symbol gets its pass-1 value. On v2.27.0, `.enum WW` before `WW = 7` lists its member as `0001`, and the value is never corrected, because symbols are written only on pass 1. A "must be known on pass 1" error like `.for`'s would help, but it changes behaviour.
  - A column-1 non-definition word outside an enum, such as `LDA FOO` in column 1, silently became a constant on v2.27.0; plan 017's end-of-statement error now covers it.
  - LSP `definitions()` does not see `.rs` reservations outside an enum.
- **Lint verifier.** A member named like a mnemonic in column 1 inside a routine body is misread as an instruction. Making `.enum` bodies transparent would change findings for existing code, so it is deferred.
- **Relies on plan 017** (shipped) for E3 (`duplicate-symbol`) and for the fourth-argument error.

### Critical files for implementation

- `crates/nessemble-core/src/assemble.rs`
- `crates/nessemble-core/src/parse.rs`
- `crates/nessemble-core/src/ast.rs`
- `crates/nessemble-core/src/tooling.rs`
- `crates/nessemble-lsp/src/lib.rs`

---

## Decisions settled with the maintainer

1. `NAME = expr` inside `.enum` resets the counter in every enum, named or not, and ships as **its own release-notes item**, separate from the bare-name change, in both the changeset and `upgrading.md` (§2.3, Q3).

---

## As built

Shipped as §3 describes. `Pseudo::Enum { start, step, name }` in the AST; the
parser takes the optional third argument and tracks `enum_depth` for E2; the
assembler keeps an `OpenEnum` frame plus the `enums` / `enum_index` registry,
routes bare names, `=` members and `.rs` members through one `enum_member`
helper, and exposes the registry as `Assembly::enums` (and
`Assembler::enums()`), with `enum_info` ready for plan 020. The §8 proposed
points were taken as written.

Deviations and findings:

- **Macro diagnostics point into the body.** §2.6 and test 11 say a member
  diagnostic from a macro expansion names the call site. Since plan 017,
  every diagnostic on an expanded line names the macro body's line, with a
  note for each `.macro` call (`in expansion of macro `ONE` at t.asm line 6`).
  The enum diagnostics follow that rule like any other, and the test pins the
  body line plus the note.
- **A one-letter enum name is E5.** A single letter lexes as a register token,
  not a name, so `.enum 0, 1, E` reports E5, which matches the E2 wording
  ("two or more letters"). The tests use two-letter names.
- **Suppressed named enums raise nothing.** A named `.enum` inside a false
  conditional skips E4 and E7 as well as recording, and an unterminated one
  there does not raise E8.
- **E9 applies in every `.enum`**, named or not: any implied-operand line whose
  mnemonic exists in no mode.
- **The E4 location** is resolved from the stored `.enum` file and line with no
  macro expansion, so for an enum written inside a macro body it names the
  expanded line rather than the body line. No test depends on this.
- **Formatter tests.** Two final-newline tests formatted a column-1 `nop`,
  which the assembler reads as a constant. They now format `  nop`, since a
  column-1 lone name now keeps its column (§3.6).
- **LSP.** There is no `prepareRename` handler, so the `NAME_COUNT` refusal is
  a `RequestFailed` error response to `textDocument/rename` carrying the §3.7
  message. Go-to-definition prefers a symbol over an enum name of the same
  spelling, so `OP = 3` beside `.enum 0, 1, OP` resolves to the constant.
- **`upgrading.md`** had no per-release section, so the two behaviour changes
  went into a new "`.enum` blocks (value enums)" section, each with before and
  after values. They also ship as two separate changesets, per the maintainer
  decision.
- **Tests.** Integration tests are in `crates/nessemble-core/tests/value_enums.rs`
  (which includes the no-source-map-spans check). Parser and formatter unit
  tests are in their modules, LSP tests are in `nessemble-lsp`, and CLI tests
  for `-l` and E8 are in `cli.rs`. The corpus goldens pass unchanged. The
  maintainer still needs to run the reference-corpus ROM-hash check before
  merge.
