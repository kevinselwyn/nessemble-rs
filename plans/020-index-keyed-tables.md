# nessemble-rs: A Plan for Index-Keyed Tables (`.table` / `.endtable`)

> Status: **Proposed.** The decided points from the brief are settled. Every
> point marked *(proposed)* is this plan's choice and is collected in §11 for
> the maintainer to confirm. Phases 1–2 ship on their own; Phase 3 (the enum
> form) consumes the registry from [plan 019](019-value-enums.md). The
> final-pass hook in §5.2 is shared with [plan 022](022-assertions.md).

---

## 1. Summary

`.table [COUNT | ENUM][, FIRST]` … `.endtable` wraps an indexed data table. Each entry starts with a `[KEY]` tag, and the assembler checks that entry *n* is tagged `FIRST + n`, that every entry is the same size, and (when a count or enum is given) that the table covers exactly that many keys. Tags emit nothing and entries are never reordered, so converting a table's hand-kept `; $NN` comments to tags gives byte-identical output, and the build fails as soon as a key and its entry stop lining up.

## 2. Syntax

```nessemble
opcode_table:
    .table OP                  ; or: .table, .table 40, .table OP_COUNT, .table 24, 1
    [OP_YIELD]  .dw op_yield
    [OP_DRAW]   .dw op_draw
    [OP_FRAME]  .dw op_frame
    .endtable
```

### 2.1 The tag

- **Where it goes.** A tag is `[` *expression* `]` as the first thing in a statement: at column 0, or after the line's indentation. A statement never starts with `[` today: `[1] .db 1` is rejected on v2.27.0 with "unexpected token at start of statement". So no valid program changes meaning. The indirect operand `JMP [ptr]` is unaffected, because its `[` follows a mnemonic and is parsed by `parse_operand`, which never sees statement-start position. `[K] JMP [ptr]` is a tag followed by an indirect jump.
- **What the key can be.** Any expression: a constant, a number (`[$05]`), a sum (`[OBJ_FIRST + 2]`), a loop counter (`[\i]`), or a forward reference. The existing one-level, right-associative precedence applies, so `[A*2+1]` is `A*(2+1)`. The docs say so, as plan 016 §12 does for `.for`.
- **What a tag can prefix.** Any statement: `.db`, `.dw`, `.lobytes`, `.fill`, an instruction, a `.macro` call, a custom pseudo-instruction, `.include`, `.for`, `.if`.
- **A tag on a line of its own** *(proposed)* is allowed. The entry is then whatever follows, up to the next tag. This is needed anyway (see the macro case in §4.2), and it makes multi-line records read naturally.
- **Rejected after a tag:**
  - a label or constant definition (`[K] foo:`, `[K] X = 1`) on the same source line;
  - `.table` or `.endtable`;
  - `.macrodef`;
  - a second tag on the same source line.

### 2.2 Argument forms *(proposed)*

| Form | `FIRST` | Count checked | Ships in |
|---|---|---|---|
| `.table` | 0 | no | Phase 1 |
| `.table COUNT` (any expression) | 0 | exactly `COUNT` entries | Phase 1 |
| `.table COUNT, FIRST` | `FIRST` | exactly `COUNT` | Phase 1 |
| `.table ENUM` | enum's `start` | one entry per member | Phase 3 |
| `.table ENUM, FIRST` | `FIRST` | one entry per member whose value is `FIRST` or later in declaration order | Phase 3 |

**Telling `COUNT` from `ENUM`.** When the first argument is a bare identifier (`Expr::Symbol(name)`) and `name` is a registered named enum (§5.3), it is the enum form. Otherwise the argument is evaluated as an ordinary expression, so `.table OP_COUNT` and `.table 40` are the numeric form. Plan 019 keeps enum names out of the symbol table, so a name can be both an enum and a constant; the enum wins, and the docs say so. A bare identifier that is neither gets its own error rather than "symbol not defined" (§7).

**No order-only form with a base.** `.table , 1` is not syntax. A 1-based table states its count (`.table 24, 1`) or uses its enum. See §11 Q3.

**`FIRST` with an enum** selects a suffix of the key set. This covers the common case of a type enum whose member 0 is "none", with a table read as `table-1, X` that starts at member 1. `FIRST` must be the value of a member.

### 2.3 Entries

- **Extent.** An entry runs from its tag to the next executed tag, or to `.endtable`.
- **Size.** Size is the number of bytes emitted (not suppressed) between those points. That equals the byte distance, because layout directives are banned inside a table (§3.4). Entry 0's size sets the table's size. A one-entry table has no size check.
- **Index check.** Entry *n* (from 0) must carry a tag whose value is `FIRST + n × STEP`. `STEP` is 1, except in the enum form where it is the enum's step (§5.3).
- **Untagged bytes are an error.** Bytes emitted inside a table before its first tag are an error. Later bytes always belong to the current entry, so the error can only arise before the first tag. Comments, blank lines, labels, constants, `.rs`, `.enum`, `.phase`/`.dephase`, `.if` and anything else that emits nothing may appear anywhere. A label does not start an entry.
- **Bytes stay in source order.** Tags emit nothing and nothing is reordered.

## 3. Semantics and edge cases

### 3.1 When values are read

Tag keys, `COUNT` and `FIRST` are evaluated on **pass 2**, which is the final pass:

- Labels and constants are fixed on pass 1 (plan 016 §12: "Constants are only written on pass 1").
- Label values already include any `.phase` delta.

So keys may forward-reference, and a phased label used as a key reads its run-time address. Sizes and counts don't move later labels, so unlike `.for` bounds nothing has to be known on pass 1. Nothing is recorded on pass 1. All checks that need the whole table run once, in the final-pass hook (§5.2).

### 3.2 Conditionals and loops

- **Inside a false `.if`.** A `.table` inside a false `.if` opens nothing. Its tags and bytes are skipped through the same `if_suppressed()` test that `write_byte` uses, so the two always agree.
- **`.if` inside a table** can drop entries. The index check then sees only the entries that were assembled, which is the point.
- **`.for` inside a table** is supported: `.for 0, 7` / `[\i] .dw row\i` / `.endfor`. Each iteration's tag is its own entry. The tag's file and line are the same for every iteration, so each recorded entry keeps the `.for` iteration suffix captured when it was recorded ("… (in `.for` iteration `\i` = 3)"). Errors are de-duplicated by (file, line, message without suffix), as `Assembler::error` already does.
- **Overlapping blocks.** A `.table` opened inside a `.for` body or `.if` block must close inside it, and vice versa. This is checked at parse time.

### 3.3 Nesting and pairing

Checked at parse time, once, beside `validate_loops`:

- A nested `.table` is an error.
- `.endtable` with no open `.table` is an error.
- A `.table` with no `.endtable` is an error.
- A tag outside any `.table` is an error.

The check runs on the preprocessed flat stream, so a table may contain `.include`d lines and macro expansions.

### 3.4 Layout directives *(proposed)*

`.org`, `.prg`, `.chr`, `.segment` and `.inestrn` inside an active table are errors at that line. They move the location counter or send bytes into the trainer, which would make "size" and "distance" disagree. `.phase` and `.dephase` are allowed: they change only label addresses, not ROM placement.

### 3.5 Error cascades *(proposed)*

- **Index mismatches.** Only the **first** mismatched entry in a table is reported. Inserting one constant shifts every later entry, and the first is where the author needs to look.
- **Size mismatches.** Every wrong-size entry is reported, because each is independent. An entry that emits 0 bytes gets its own message.
- **Enum form.** Missing members and a count mismatch are reported on the `.table` line, in addition to the first index error. A duplicate tag needs no separate message: it shows up as an index mismatch on the second copy.

### 3.6 How values appear in messages

- **Index and key values** print as `$NN` for 0–255 and `$NNNN` above that. Negative values print in decimal.
- **The key's text** is what the author wrote, quoted from token spans with the parser's `render` (plan 017's mechanism), and stored on the tag as `key_text` at parse time. A numeric-literal key drops the redundant "(value)". Plan 022 quotes `.assert` expressions the same way.

### 3.7 Emitted source *(proposed)*

A custom pseudo-instruction's `emit_source` may not contain `.table`, `.endtable` or tags in v1; this is rejected through the existing `emit-source-unsupported-directive`. The tag goes on the invoking line: `[OP_X] .my_pseudo …`. The bytes it emits count toward that entry through `write_all` → `write_byte`.

### 3.8 What is unchanged

- **ROM output for every existing source.** `[` at statement start and `.table`/`.endtable` are errors on v2.27.0. `.table` is currently "Unknown custom pseudo-instruction `.table`".
- **One exception to flag.** A project with a script that defines a custom pseudo-instruction named `table` or `endtable` would be shadowed, because built-ins match first in `parse_pseudo`. See §10.
- **Symbols, the list file (`-l`, `--mlist`) and the source map.** Tags define no symbols and emit nothing.

## 4. Code changes

**[shared]** marks shared code: lexer/token, expression parser, statement parser, symbol table, listing writer, lint engine, `.nessemblerc` schema, LSP.

### 4.1 AST — `crates/nessemble-core/src/ast.rs` [shared]

- `Stmt::Tag(Expr, String)` (the key and its source text) is a **marker statement**: one `Line` of its own, executed before the statement that shares its source line. This is better than wrapping (`Stmt::Tagged(Expr, Box<Stmt>)`), because a macro call's tag and the macro's expanded lines are separate token runs (§4.2). With a marker, "entry runs from tag to next tag" is literal.
- `Pseudo::Table(Option<Expr>, Option<Expr>)` and `Pseudo::EndTable`.
- No new `Line` field: `Line.expansion` (plan 017) already says which `.macro` expansion a tagged line came from.

### 4.2 Statement parser — `crates/nessemble-core/src/parse.rs` [shared]

**Tag prefix.** In `parse_program` and `parse_program_recovering`, after the optional `Tok::Indent`, the parser checks for a tag:

1. If the next token is `Tok::OpenBrack`, parse `[` `parse_expr()` `]` (`expect(&Tok::CloseBrack)`). Push `Line { stmt: Stmt::Tag(e, text), line, file, from_macro, expansion }`, where `text` is `self.render(start, end)` over the key's tokens.
2. Skip one optional `Tok::Indent`.
3. If at `Endl`/EOF, the line is done.
4. If the next token is another `OpenBrack` **on the same source line**, error `tag-twice`. If it is on a different line, that is the empty-macro case below: loop and parse the next tag.
5. Otherwise call `parse_stmt(indented = true)`. After a tag the statement is never a column-0 constant, so `[K] LDA #1` at column 0 is an instruction.
6. If the result is `Stmt::Label` or `Stmt::Constant` on the tag's own source line (and not `from_macro`), error `tag-before-definition`. If it is `Pseudo::Table`/`EndTable`, error `tag-before-block`.

**Why the indent and line checks matter.** For `    [K] .macro row, 1`, the preprocessor leaves `Indent [ K ]` followed by the macro body's own tokens. A body line normally begins with `Indent`, which step 2 skips. An empty or blank-first body leaves the tag at end of line or directly before the next line's tokens, which steps 3 and 4 handle.

**Fit with plan 017's end-of-statement check.** The tag is consumed as a prefix *before* `parse_stmt`, and `Parser::expect_end` then checks the statement after the tag, so `[KEY] stmt` passes and `[KEY] .db 1 junk` still fails on `junk`. A bare `[K]` line is complete. `expect_end` needs no change; its "statement" quote (`render(start, …)`) should start after the tag.

**`.table` arguments** are parsed in `parse_pseudo` like `.for`: zero, one or two comma-separated expressions. Three or more is error `table-arg-count`. `"endtable"` → `Pseudo::EndTable`.

**`validate_tables(&[Line]) -> Vec<ParseError>`** is called from `parse` and `parse_recovering` beside `validate_loops`. One pass, tracking:

- the open table (line, file, `.for` depth and `.if` depth at open);
- reports for `endtable-unmatched`, `table-unterminated`, `table-nested` (naming the outer `.table`'s line), `tag-outside-table`, `table-for-unbalanced` and `table-if-unbalanced`.

**`parse_fragment`** is used by `exec_emitted_source`. It is `parse` without `validate_tables`, so emitted source gets the clean unsupported-directive error (§3.7) rather than "tag outside `.table`".

### 4.3 Token and preprocessor — `lexer.rs`, `preprocess.rs` [shared]

- **Lexer.** No change to lexing rules. `[`, `]` and `.table` already lex as `OpenBrack`, `CloseBrack` and `Pseudo("table")`.
- **Token.** No change. Plan 017 already stamps every token with a `span` and an `expansion` id, and records each expansion (name, definition site, call site, parent) in `Preprocessed.expansions`.
- **Where macro diagnostics go.** This plan follows plan 017's house rule: a diagnostic on a macro-expanded line is reported at the **macro body line**, in the file holding the body, with one `note: in expansion of macro …` per enclosing call, through `diagnostic::locate`. The notes name the call site, which is what the brief asks for.
- **Tagged `.macrodef`.** In `handle_directive`, if anything other than `Endl`/`Indent` was emitted for the current line before `.macrodef`, report the existing preprocessor diagnostic style with `tag-before-block` naming `.macrodef`.

### 4.4 Assembler — `crates/nessemble-core/src/assemble.rs`

New state:

```rust
struct OpenTable {
    file: u32, line: u32,                 // the `.table` line (author line)
    spec: TableSpec,                      // resolved at `.table` on pass 2
    entries: Vec<EntryRec>,
    untagged_reported: bool,
}
enum TableSpec {
    Order { first: i64 },
    Count { count: i64, first: i64 },
    KeySet { name: String, first: i64 },  // Phase 3; looked up via `enum_info` (§5.3)
}
struct EntryRec { file: u32, line: u32, key_text: String, key: i64, bytes: usize, suffix: Option<String> }
table: Option<OpenTable>,                 // `None` when no table is open, or it is suppressed
deferred: Vec<Deferred>,                  // §5.2
table_entries: Vec<TableEntryInfo>,       // for the LSP (§6)
```

**`.table`** (in `exec_pseudo`), only when `pass == 2 && !if_suppressed()`:

- resolve `TableSpec` as in §2.2, reporting `table-arg-unknown`, `table-count-invalid` and the enum errors at this line;
- set `self.table = Some(..)`.

On pass 1, and when suppressed, `.table` does nothing; parse-time pairing guarantees `.endtable` matches.

**`Stmt::Tag(e, text)`** (in `exec_stmt`): if a table is open and not suppressed, evaluate `e` and push an `EntryRec` with `self.origin()` (file, line, `cur_expansion` and the `.for` counters).

**`write_byte`**: after the `if_suppressed` early return, if a table is open:

- with no entries yet, report `table-untagged` once per table;
- otherwise `entries.last_mut().bytes += 1`.

This is one branch on the hot path, and only when a table is open.

**`.endtable`**: if a table is open, move it into `self.defer(origin_of_table_line, DeferredCheck::Table(t))`. If it has no entries, report `table-empty`.

**Layout directives.** `Pseudo::Org`, `Prg`, `Chr`, `Segment` and `InesTrn` with a table open report `table-layout`.

**`run_lines`.** `exec_for` and `exec_endfor` need no change. The only additions are the new `Stmt` and `Pseudo` arms. `reset_state` clears `table`.

**`exec_emitted_source`.** The unsupported scan rejects `Pseudo::Table`, `Pseudo::EndTable` and `Stmt::Tag` (§3.7), and parsing switches to `parse_fragment`.

**Symbol table**: no change.

### 4.5 Public API — `crates/nessemble-core/src/lib.rs` [shared]

- `Diagnostics` and `ProjectDiagnostics` gain `tables: Vec<TableEntryInfo>`, where `TableEntryInfo { file: String, line: u32, table_line: u32, key: i64, index: i64, size: usize, ok: bool }`.
- `Assembly` does not gain it: the CLI doesn't need it.
- `render_list_file` (listing writer) [shared]: **no change**. A test pins this.

### 4.6 Directive catalog — `crates/nessemble-isa/src/lib.rs`

Add the `DIRECTIVES` entry `(".table / .endtable", "check an indexed table's entries against their keys")`. This feeds `nessemble reference .table` and LSP completion and hover.

## 5. Final-pass checks and the enum contract

### 5.1 What runs at `.endtable`'s deferred check

Given the `EntryRec`s, `FIRST`, `STEP` and the spec:

1. **Index.** For each n, the expected key is `FIRST + n·STEP`. On the first mismatch, report `table-tag-index` at that entry's line and stop index checking for this table.
2. **Size.** `size = entries[0].bytes` when there are at least 2 entries. Each entry with `bytes == 0` gets `table-entry-empty`. Each entry with `bytes != size` gets `table-entry-size`.
3. **Count.**
   - `Count`: if `len != count`, report `table-count` at the `.table` line.
   - `KeySet`: for each member at or after `FIRST` whose value no entry carries, report `table-missing-member` at the `.table` line. If `len` is greater than the number of members covered, report `table-count-enum`.
4. Append a `TableEntryInfo` per entry for the LSP.

### 5.2 The final-pass hook, shared with `.assert` (plan 022)

This is the **reconciled interface** agreed with plan 022. This plan merges first, so it introduces the hook; plan 022 adds its variants.

```rust
/// Where a deferred check reports: the line the author wrote.
pub(crate) struct Origin {
    pub file: u32,
    pub line: u32,
    pub expansion: u32,         // `Line.expansion`; `locate` turns it into body file + notes
    pub suffix: Option<String>, // `.for` iteration suffix captured at record time
}
/// A check that needs settled symbol values and runs exactly once per build.
pub(crate) struct Deferred {
    pub origin: Origin,
    pub check: DeferredCheck,
}
pub(crate) enum DeferredCheck {
    Table(OpenTable),
    // Plan 022 adds `Assert { .. }` and `Error { .. }`.
}
impl Assembler {
    /// The current origin: `cur_file`, `cur_line`, `cur_expansion`, plus `loop_counters()`.
    fn origin(&self) -> Origin;
    /// Queue a check. No-op unless `self.pass == FINAL_PASS` (2), so recording happens once per
    /// build. The *caller* applies its own suppression rule before calling: `.table` uses
    /// `if_suppressed()` so it agrees with `write_byte`; `.assert`/`.error` use the stricter
    /// all-levels test (plan 022 §2.3).
    fn defer(&mut self, origin: Origin, check: DeferredCheck);
    /// Drain `deferred` in queue order, reporting via `report_at`. Called exactly once:
    /// in `run()` after pass 2 + `validate_ines()` when pass 2 produced no errors, and in
    /// `diagnostics()` after pass 2 unconditionally. In non-collect mode it stops at the first
    /// error, so `run()`'s `errors.last()` is that first error.
    fn run_deferred(&mut self);
    /// Report at an explicit origin through `diagnostic::locate` (so an expanded
    /// line names its body file and the calls), appending its suffix,
    /// de-duplicated by (file, line, message without suffix) like `error`.
    fn report_at(&mut self, origin: &Origin, message: String);
}
```

Plan 019's `error_at` for pass-1 enum diagnostics is `report_at` with an `Origin` captured at the `.enum` line; whichever plan lands first adds it.

Contract for both users:

- A record's values are taken at record time on pass 2. They are final, and after `.phase` remapping, because pass 2 reads pass-1 symbols and phased label addresses.
- A check may instead carry an `Expr` to evaluate in `run_deferred`. The symbol table is complete there, but loop counters are not available, so anything using `\i` must be evaluated at record time.
- Nothing is queued on pass 1. The queue is cleared at the start of each `run()` and `diagnostics()`.

Entries and the table record are evaluated at record time on pass 2, so `.table` needs no drain-time evaluator; plan 022 adds the side-effect-free `eval_final` for assertions.

### 5.3 Key sets: consuming plan 019's enum registry

Plan 019 records each named `.enum` as an `EnumInfo` on pass 1 and exposes `Assembler::enum_info(&self, name) -> Option<&EnumInfo>`:

```rust
// Provided by plan 019 (crates/nessemble-core/src/assemble.rs).
pub struct EnumInfo {
    pub name: String,
    pub file: u32, pub line: u32,
    pub start: i64,
    pub step: i64,
    pub members: Vec<EnumMember>,  // declaration order; each has name, value, size, file, line
    pub contiguous: bool,          // members[i].value == start + i*step for every i
}
impl EnumInfo { pub fn count(&self) -> usize { self.members.len() } }
// Also defined as the constant NAME_COUNT.
```

This plan defines no registry of its own. Phase 1 compiles without plan 019 because nothing calls `enum_info` until Phase 3; `.table NAME` in Phase 1 falls through to the numeric form, or to `table-arg-unknown`.

- **When it is read.** `.table` resolves its spec on pass 2, when plan 019's registry is complete (it is filled on pass 1 and read-only on pass 2). An `.enum` in an earlier- or later-included file both work.
- **What `.table ENUM` requires.**
  - `contiguous` must be true and `step` must not be 0; otherwise report `table-not-contiguous` at the `.table` line.
  - `STEP = step` *(proposed)*. A step of 2 gives keys 0, 2, 4…, the pre-doubled index of a `.dw` table read with `ASL` / `TAX`.
  - `FIRST` defaults to `start`. An explicit `FIRST` must be a member's value, else `table-first-not-member`. The members checked are those from that member onward.

## 6. Tooling parity

| Tool | Own parser? | Change |
|---|---|---|
| **`format`** (`tooling::format_with`) | yes (`tooling::lex`) | New pass `align_table_tags`, run after `consolidate_data` (details below). |
| **`lint`** (`tooling::lint`) [shared] | yes | No new rule. `require-block-comment` / `block_label` unchanged; a test pins it. Routine effects see through tags. |
| **`coverage`** | no (source map) | **No change**; tests only. |
| **LSP** (`crates/nessemble-lsp/src/lib.rs`) [shared] | partly (text scans) | Hover on tags and on `.table`, folding, completion (details below). |
| **Listing `-l` / `--mlist`** | no | No change; tags define no symbols. Test pins it. |
| **wasm `highlight()`** | shares `tooling::lex` | No change. |
| **`.nessemblerc` schema** (`nessemble-rc`) [shared] | — | **No change**: alignment is always on inside tables. |

**`format` details.**

- **Tag rule.** A shared helper `tag_lexemes(source, sig) -> usize` returns the number of lexemes in a leading `[…]` tag (0 if none). It sits beside `name_lexemes` and is also used by lint and the LSP.
- **Tagged lines.** Inside each `.table` … `.endtable` (matched by directive text), every tagged line is written as `lead + tag + pad + rest`. `pad` brings `rest` to the column of the widest tag in that table plus one space. The tag's inner whitespace next to the brackets is trimmed (`[ OP_X ]` → `[OP_X]`); the expression itself is verbatim.
- **Untagged lines inside the table** (continuation lines of multi-line records, including directives) are padded to the same statement column. Comment-only lines keep their indentation. Labels and constants stay at column 0.
- **`lead`** is what the statement would get untagged: the indent unit for instructions, and for directives when `indentDirectives` is set.
- **Data consolidation.** Tagged lines never merge, because `parse_data_line` requires a directive as the first lexeme. Consecutive untagged `.db` lines within one entry may still merge, which keeps both the bytes and the entry boundaries.
- **Format hints.** A `@nessemble-format` hint above a tagged run binds to nothing, so it is reported as `ineffective-comment-directive` as usual.
- **Idempotent.** The pass is idempotent.

**`lint` details.** In `routine_effects_inner` and `operand_text`, skip `tag_lexemes`, so `[K] JMP handler` (a table of JMP instructions) is analyzed as a `JMP`; today it would be "unknown". Add `table` and `endtable` to `is_transparent_directive`, since they emit nothing. (Plan 023 replaces the text-based routine walk with an emitted-stream walk; there, tags are invisible because they emit nothing.)

**`coverage` details.** An entry's bytes are attributed to the line that emits them (the tag line for one-line entries) through the existing `record_span`. A table is classified as data by the CDL like any other data.

**LSP details.**

- **Hover on a tag** (cursor inside `[…]`, including on the key identifier, appended to the symbol hover): `` `OP_SFX` = $05 · entry $05 of the table on line 12 · 2 bytes ``. On a mismatch it adds "expected tag $06". On a tag inside a `.for` it shows "8 entries from this line ($00–$07)". This comes from `Diagnostics::tables`, matched by (file, line).
- **Hover on `.table`**: the resolved form, e.g. "40 entries keyed by `OP` ($00–$27), 2 bytes each".
- **Folding**: `.table`…`.endtable` as `BlockTag::Table` in `folding_ranges`.
- **Completion and hover** of the directive come from `DIRECTIVES`.
- **Diagnostics** arrive through the assembler with no further change. Messages backtick their subject, so `diagnostic_range`/`quoted_subject` underline the key.

## 7. Diagnostics

These are new keys under `## Tables` in `crates/nessemble-i18n/locales/en-US.ftl`, rendered as ``Error in `f.asm` on line N: …``. Wording *(proposed)* follows the file's existing style: capitalized, with backticked subjects. The brief's lowercase wording maps one-to-one.

| Key | Text | Line |
|---|---|---|
| `table-tag-index` | ``Entry tagged `{ $key }` ({ $value }) is at index { $index }`` (literal key: `Entry tagged { $value } is at index { $index }`) | the tag (the body line, with expansion notes, if from a macro) |
| `table-missing-member` | ``Table `{ $set }` is missing `{ $member }` ({ $value })`` | `.table` |
| `table-count-enum` | ``Table has { $count } entries; `{ $set }` has { $expected }`` | `.table` |
| `table-count` | ``Table has { $count } entries; `.table` expects { $expected }`` | `.table` |
| `table-entry-size` | `Entry at index { $index } is { $size } bytes; entries in this table are { $expected }` | the entry's tag |
| `table-entry-empty` | `Entry at index { $index } emits no bytes` | the entry's tag |
| `table-untagged` | ``Untagged data inside `.table`; start each entry with a `[KEY]` tag`` | first untagged byte's author line |
| `table-not-contiguous` | ``{ $set }` is not a contiguous enum`` | `.table` |
| `table-first-not-member` | ``{ $set }` has no member with value { $value }`` | `.table` |
| `table-arg-unknown` | ``{ $name }` is neither a constant nor a named enum`` | `.table` |
| `table-arg-count` | ``.table` takes at most two arguments (`.table [COUNT \| ENUM][, FIRST]`), but { $count } were given`` | `.table` |
| `table-count-invalid` | ``.table` expects at least 1 entry, not { $value }`` | `.table` |
| `table-empty` | ``.table` has no entries`` | `.endtable` |
| `table-unterminated` | ``.table` has no matching `.endtable` `` | `.table` |
| `endtable-unmatched` | ``.endtable` has no open `.table` `` | `.endtable` |
| `table-nested` | ``.table` inside another `.table` (opened on line { $line })`` | inner `.table` |
| `table-for-unbalanced` / `table-if-unbalanced` | ``A `.table` and a `.for` body (an `.if` block) must not overlap; close the inner one first`` | the closing directive |
| `tag-outside-table` | ``A `[KEY]` tag must be inside a `.table` `` | the tag |
| `tag-before-definition` | ``A `[KEY]` tag can't prefix a label or constant definition`` | the tag |
| `tag-before-block` | ``A `[KEY]` tag can't prefix `{ $directive }` `` (`.table`, `.endtable`, `.macrodef`) | the tag |
| `tag-twice` | ``Only one `[KEY]` tag may start a line`` | the tag |
| `table-layout` | ``{ $directive }` can't be used inside `.table`; it moves the location counter`` | the directive |
| *(existing)* `emit-source-unsupported-directive` | directive = `.table`, `.endtable` or `[KEY] tag` | the custom pseudo-instruction's line |

Inside a `.for`, each message gets the existing `for-iteration-suffix`. The first index error per table is the only index error (§3.5).

## 8. Tests (written first)

**`crates/nessemble-core/tests/tables.rs`** (new; helpers copied from `for_loops.rs`: `raw()`, `bytes`, `error`):

- **Byte-identical conversion.** A 40-entry `.dw` table with `; $NN` comments and 40 `OP_* = $NN` constants assembles to the same bytes as its `.table OP_COUNT` tagged conversion. Also assert `-l` and `--mlist` output is identical (`render_list_file` over both symbol lists).
- **Two-byte, one-byte and multi-line entries.** `.dw`; `.db`; and a 3-line record (tag + `.db` + `.db` + `.lobytes`), plus a bare-tag-line record.
- **`.macro`-built entry.** `[K] .macro entry, x`, including a macro whose body starts with a blank line. Also a macro whose body carries the tag (`[\1] .dw \2`): an error inside it reports the body line with an `in expansion of macro` note naming the call.
- **Tagged custom pseudo-instruction** (resolver returning `CustomOutput::Bytes`, and one returning `Source`). Also: `emit_source` that contains a tag is rejected.
- **Bases and counts.**
  - `FIRST ≠ 0`: `.table 24, 1` with tags 1…24 passes, and with 0…23 fails on entry 0.
  - `.table N` and `.table COUNT_CONST`: exact count passes; one short and one extra each fail with `table-count`.
- **Forward references.** Tag keys defined below the table; a phased label as a key reads its `.phase` address.
- **Loops and conditionals.**
  - `.for` inside a table with `[\i]` keys passes. A wrong-size iteration reports once, with the iteration suffix.
  - `.if 0` inside a table drops an entry, and the index check sees that.
  - A `.table` inside a false `.if` is ignored.
- **Every §7 diagnostic, with its line.** Includes:
  - the first-mismatch-only rule (insert one constant → exactly one `table-tag-index`);
  - zero-byte entry;
  - untagged bytes before the first tag (direct, and from a macro → body line plus expansion note);
  - `.org` inside a table;
  - nested, unclosed and unmatched tables;
  - a tag outside a table;
  - `[K] foo:`; `[K] .table`; `[K] [L] .db 1`;
  - `.table 1, 2, 3`;
  - `.table NOPE`.
- **Run-once guarantee.** Assert each table error appears exactly once in `diagnose_source_as` output, i.e. not once per pass.
- **Phase 3 only (enum form).** Behind plan 019, using `.enum START, STEP, NAME`:
  - complete table passes;
  - missing member → `table-missing-member` naming the member;
  - extra entry → `table-count-enum`;
  - non-contiguous enum → `table-not-contiguous`;
  - step 2 enum with pre-doubled keys;
  - `, FIRST` suffix;
  - `FIRST` not a member;
  - the `.enum` in an earlier-included file.

**`parse.rs` unit tests:**

- `JMP [ptr]`, `JMP [ptr, X]` and `LDA [zp], Y` parse unchanged.
- `[K] JMP [ptr]` gives Tag + Instruction.
- `[K] LDA #1` at column 0 is an instruction.
- With plan 017's end-of-statement check: `[K] .db 1` is accepted and `[K] .db 1 junk` is rejected.

**`tooling.rs` tests:**

- Format alignment: mixed tag widths, `indentDirectives` on and off, continuation lines.
- Idempotence.
- Consolidation never merges across tags.
- `[ K ]` → `[K]`.
- `require-block-comment` behaves the same with tags present: a label inside a table is still treated as following code.
- `[K] JMP x` in routine effects is analyzed as a `JMP`.

**LSP tests:** tag hover (ok, mismatched, in a `.for`); `.table` hover; folding range.

**CLI test (`crates/nessemble-cli/tests/cli.rs`):** the exact ``Error in `f.asm` on line N: Entry tagged `OP_SFX` ($05) is at index $06`` output and exit code. Also `nessemble reference .table`.

**Byte-identical regressions:** `tests/corpus.rs` must stay green, with no golden changes. Before merging, the maintainer confirms the reference corpus still builds to its reference hash both unconverted and with one opcode handler table converted to `.table`.

## 9. Docs and changeset

- **Syntax reference.** `docs/src/syntax.md`: `### .table` and `### .endtable` in the alphabetical Pseudo-Instructions list. They cover:
  - the §2 example with its hex output and a `<nessemble-assembler>` "Try it" block;
  - the argument forms table;
  - multi-line records;
  - tagging a `.macro` call or custom pseudo-instruction;
  - `.for` inside a table;
  - the precedence note for keys;
  - the "bytes never move" guarantee;
  - converting a comment-indexed table.
- **Docs for Phase 3.** The enum form is documented once Phase 3 lands.
- **`nessemble reference`** comes free from the `DIRECTIVES` entry.
- **Changesets** (never edit `CHANGELOG.md`). All additive, none a behaviour change:
  - Phase 1: `cargo run -p xtask -- changeset add minor "Add .table/.endtable: tag each entry of an indexed table with [KEY] and the assembler checks its index, size and count."`
  - Phase 2: `minor`, for the editor features.
  - Phase 3: `minor`, for the enum-keyed tables.

**Phasing:**

1. **Phase 1 — core.** AST, parser, `validate_tables`, assembler, the deferred hook, order-only and numeric forms, every non-enum diagnostic, the `DIRECTIVES` entry, docs.
2. **Phase 2 — tooling.** Formatter alignment, the `tag_lexemes` helper, lint see-through, LSP hover and folding, `Diagnostics::tables`.
3. **Phase 3 — enum form.** Depends on plan 019's `enum_info` (§5.3). It adds only the `TableSpec::KeySet` resolution and the §5.1 step 3 enum branch.

## 10. Risks and open questions

- **Name collision.** A user script for a custom pseudo-instruction named `table` or `endtable` would be shadowed by the built-in. Consider having `nessemble-rc`/the LSP warn when a configured custom name matches a built-in; that is out of scope here.
- **Shapes the reference corpus can't express yet.**
  - Several comment-indexed lines hold **two entries on one line** (`.dw a, b ; type 5, 6`). Converting them means splitting into two tagged lines; the bytes are unchanged.
  - Some tables **overlap**: the last entry is cut short because the next table's first byte doubles as its high byte. Its size check would fail. Such a table ends `.endtable` before the partial entry, or stays comment-indexed. A "short last entry" allowance is not in v1.
- **Run-once vs `run()` semantics.** `run()` returns `errors.last()`. `run_deferred` stops at the first error in non-collect mode to keep that equal to the first table error. `.assert` must follow the same rule.
- **First-mismatch-only** hides later independent ordering errors in the same table. The fix-and-rebuild loop finds them, and the LSP lists all entries' status in hovers.
- **Order-only with a base** (`.table , 1`) is unsupported (§2.2). Revisit if 1-based tables can't name a count.
- **Not in v1 (outside this brief):**
  - tags and `.table` inside `emit_source`;
  - a step argument for the non-enum forms;
  - a lint rule that suggests converting `; $NN`-commented tables to tags;
  - a code action that does that conversion;
  - checks of what the entries contain (e.g. each `.dw` pointing at a routine).

## 11. Decisions to confirm (proposed points)

1. A bare `[K]` line is allowed (§2.1).
2. A named enum wins over a same-named constant; an unknown bare name gets `table-arg-unknown` (§2.2).
3. There is no order-only form with a base; `, FIRST` needs a count or an enum (§2.2).
4. `, FIRST` with an enum selects a suffix of its members and must be a member value (§2.2, §5.3).
5. An enum with step ≠ 1 means keys `FIRST + n·step`; step 0 or gaps → `table-not-contiguous` (§5.3).
6. `.org`/`.prg`/`.chr`/`.segment`/`.inestrn` are banned inside a table; `.phase`/`.dephase` are allowed (§3.4).
7. Only the first index mismatch is reported per table; every size mismatch is reported (§3.5).
8. Messages use the capitalized, backticked style of `en-US.ftl` (§7).
9. Emitted source may not contain tags or tables in v1 (§3.7).
10. Formatter alignment is always on inside tables, with no `.nessemblerc` option (§6).

### Critical files for implementation

- `crates/nessemble-core/src/parse.rs`
- `crates/nessemble-core/src/assemble.rs`
- `crates/nessemble-core/src/ast.rs`
- `crates/nessemble-core/src/tooling.rs`
- `crates/nessemble-lsp/src/lib.rs`
- also `crates/nessemble-core/src/preprocess.rs`, `crates/nessemble-core/src/lexer.rs`, `crates/nessemble-core/src/lib.rs`, `crates/nessemble-i18n/locales/en-US.ftl`, `crates/nessemble-isa/src/lib.rs`, `docs/src/syntax.md`

---

## Decisions settled with the maintainer

1. A table indexed from a base other than 0 must state its count or use an enum (`.table 24, 1`); there is no `.table , FIRST` form (§2.2, §11 Q3).
