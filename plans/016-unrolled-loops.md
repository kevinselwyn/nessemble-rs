# nessemble-rs: A Plan for Unrolled Loops (`.for` / `.endfor`)

> Status: **Approved design, not yet built.** No code has been written. Every
> design fork was settled with the maintainer and is recorded in
> [§11](#11-decisions); those answers are final. The one answer that departs
> from the first draft is per-iteration label pasting
> ([§2.2](#22-per-iteration-names)), so a body may define a named label
> once per iteration.

---

## 1. Goal

NES code unrolls loops constantly: copying a palette or a nametable row to
`PPUDATA`, clearing OAM, writing a fixed run of APU registers. A loop that runs
a known number of times at build time is cheaper (in cycles, and often in
bytes for very short runs) unrolled, and vblank time is the scarcest resource
an NES program has. Today the only ways to write one are by hand, or with a
`.macro` per iteration — both repeat the body in the source and drift the
moment one copy is edited.

This plan adds a block pseudo-instruction that repeats its body once per value
of a counter, with the counter's value available inside the body as `\i`:

```nessemble
.for 0, 2
  LDA src_label+\i
  STA PPUDATA
.endfor
```

assembles exactly as if it had been written:

```nessemble
  LDA src_label+0
  STA PPUDATA
  LDA src_label+1
  STA PPUDATA
  LDA src_label+2
  STA PPUDATA
```

## 2. Syntax

```text
.for START, END
  body
.endfor
```

- **`START` and `END` are both inclusive** — `.for 0, 2` runs three times, with
  `\i` = 0, 1, 2, matching the example above. This reads the way the bounds of
  a table are usually written (`0, LEN-1`), and agrees with the request's
  example ([§11, Q2](#q2-inclusive-end-bound)).
- **The bounds are expressions**, not just literals: `.for 0, PALETTE_LEN-1`,
  `.for FIRST_SPRITE, FIRST_SPRITE+7`. They must resolve on the first pass —
  see [§4](#4-where-the-loop-runs-the-assembler-not-the-preprocessor) and
  [§5.1](#51-bounds-must-be-known-on-pass-1).
- **`\i` is the counter**, usable anywhere an expression is: `LDA table+\i`,
  `LDA #\i*4`, `.db \i`, `.if \i % 2`. It evaluates to a plain integer, so
  `\i*2` gives a step-of-two sequence without any extra syntax.
- **`START > END` counts down**: `.for 7, 0` runs with `\i` = 7, 6, … 0
  ([§11, Q3](#q3-start--end)). Copying a buffer backwards to the PPU, or
  pushing a table onto the stack in reverse, is common enough that making the
  author write `7-\i` is a needless indirection.
- **`.for` and `.endfor` are case-insensitive** and may be indented, like
  every other directive.

### 2.1 Nesting

Loops nest. `\i` always names the **innermost** enclosing loop's counter; the
counters of enclosing loops are `\j` (one level out) and `\k` (two levels out):

```nessemble
.for 0, 3          ; row
  .for 0, 7        ; column
    LDA tiles+\j*8+\i
    STA PPUDATA
  .endfor
.endfor
```

Three levels is the ceiling, which covers every real unrolling case (a 2D
nametable region is two levels). A `\j`/`\k` with no loop that many levels out
is an error ([§6](#6-diagnostics)). See
[§11, Q1](#q1-how-nested-loops-name-their-counters) for the named-counter
alternative that was rejected.

### 2.2 Per-iteration names

A symbol name directly followed by a counter is **pasted**: the counter's
current value is appended to the name in decimal. This works for label
definitions, constant definitions and references alike:

```nessemble
.for 0, 2
row\i:              ; defines row0, row1, row2
  LDA #\i
  STA $0200+\i
  RTS
.endfor

jump_table:
.for 0, 2
  .dw row\i         ; .dw row0, row1, row2
.endfor

  JSR row1          ; outside the loop, the names are ordinary symbols
```

- **The rule matches `label\@`.** The preprocessor already treats a name
  followed by `\@` as one pasted name. Tokens carry no column, so it can't tell
  `row\i` from `row \i`; neither can this rule. That costs nothing: `row \i`
  (a name, then a bare counter with no operator between them) is not a valid
  expression anyway.
- **A name may paste one counter, as a suffix.** `cell\j\i` is an error: with
  `\j` = 1 and `\i` = 12 it would be `cell112`, the same as `\j` = 11 and
  `\i` = 2. Pasting in the middle of a name (`row\i_end`) is not supported in
  v1 ([§10](#10-non-goals)). A 2D table can still be named: the inner loop
  pastes `\i` onto a constant base set by the outer loop, or the table uses
  one flat loop.
- **A negative counter can't be pasted.** `row-1` isn't a name, so pasting a
  negative value is an error naming the iteration.
- **A pasted name that already exists is an error.** Ordinary redefinition is
  silent today (§3), but a pasted `row2` colliding with a hand-written `row2:`,
  or with another loop's `row\i`, is almost certainly a mistake. Pasted names
  are checked on pass 1 and an existing name is reported.
- **Pasted symbols are flagged `from_macro`**, like a macro's `label\@`. The
  name `row2` does not appear in the file's text, so the symbol exports that
  leave out macro labels leave these out too.
- **`\i` in a macro body pastes too.** A macro that defines `entry\i:` and is
  invoked inside a `.for` body gets one label per iteration. This is how a
  macro that needs a label can be used in a loop, since its `\@` label would
  collide.

## 3. What a loop body may contain

Everything that may appear at top level, with the semantics it would have if
the body were written out N times in place:

- **Instructions and data directives** — the point of the feature.
- **`.if` / `.ifdef` / `.else` / `.endif`**, including conditions on `\i`
  (`.if \i == 0` to special-case the first iteration). A conditional opened in
  a body must close in the same body ([§6](#6-diagnostics)).
- **Constant assignment** (`X = …`) — reassigned every iteration, as a constant
  redefined on consecutive lines already is. `X\i = …` defines one constant per
  iteration (§2.2).
- **Pasted labels** (`row\i:`), one per iteration (§2.2).
- **Anonymous labels** (`:` with `:-` / `:+` references) — these already
  resolve to the nearest definition, so a body with an internal branch works
  in every iteration unchanged:

  ```nessemble
  .for 0, 3
    LDA status+\i
    BEQ :+
    JSR handle
  : NOP
  .endfor
  ```

- **`.macro` invocations and `.include`** — expanded once by the preprocessor
  (which has already run before the loop executes) and then executed every
  iteration with the rest of the body. `\i` may appear in a macro *argument*
  (`.macro SET_PPU, \i`), since the argument's tokens are passed through
  verbatim and evaluated per iteration. `\i` in a macro *body* refers to
  whichever loop the expansion lands in.
- **Nested `.for` blocks** (§2.1).

What it may **not** contain:

- **Named labels without a counter.** A plain `x:` inside a body would be
  defined once per iteration. Redefining a label is *not* an error today
  (`add_symbol` silently overwrites it), so every reference would quietly
  resolve to the last iteration's address. That is a wrong-code bug with no
  diagnostic, so a plain named label in a body is an error that says *why* and
  points at the fix ("label `x` is defined inside a `.for` body, so it would be
  defined once per iteration; write `x\i:` for one label per iteration"). The
  same applies to a `label\@` from a macro expanded inside a loop: the
  preprocessor gave it one unique suffix, not one per iteration
  ([§11, Q4](#q4-named-labels-in-a-loop-body)).
- **`.macrodef` / `.endm`.** A definition inside a loop is defined once by the
  preprocessor regardless of the loop, which is surprising enough to reject
  outright.

## 4. Where the loop runs: the assembler, not the preprocessor

There are two places this could be implemented, and the choice decides what
the bounds may be.

**The preprocessor** (`crates/nessemble-core/src/preprocess.rs`) already
splices token streams for `.include` and `.macro`, and a loop could be one
more splice: capture the body like `capture_macro` does, re-emit it N times
with `\i` replaced by a `Tok::Number` like `substitute` does for `\N`. It is
the smallest change. But the preprocessor runs before any symbol exists, so
the bounds could only be integer literals — `.for 0, PALETTE_LEN-1` would be
an error, and that is the form real code wants.

**The assembler** (`crates/nessemble-core/src/assemble.rs`) evaluates
expressions, so bounds may reference constants. Its passes already walk a flat
`&[Line]` and handle `.if` blocks by keeping a stack (`if_stack`); a loop is
the same shape with one addition — on `.endfor`, jump back to the line after
`.for` instead of falling through. `\i` becomes an expression node evaluated
against a loop stack rather than a token substituted into a copy of the body,
so no tokens are duplicated and every executed line is still the real line in
the real file.

**This plan takes the assembler route** ([§11, Q5](#q5-assembler-or-preprocessor)).
It also makes pasting (§2.2) simple: the name is built from the counter's
current value as each line executes, not from copied tokens.
Concretely:

- **Lexer** (`lexer.rs`): `\i`, `\j`, `\k` lex as a new `Tok::LoopIndex(depth)`
  (0, 1, 2) alongside the existing `\N` / `\#` / `\@` rules. `.for` and
  `.endfor` lex as `Tok::Pseudo` with no lexer change.
- **AST** (`ast.rs`): `Expr::LoopIndex(u8)`; `Expr::PastedSymbol(String, u8)`
  for a pasted reference; a pasted form of `Stmt::Label` and
  `Stmt::Constant` (a `Name` type, `Plain(String)` or `Pasted(String, u8)`,
  shared by both); `Pseudo::For(Expr, Expr)` and `Pseudo::EndFor`.
- **Parser** (`parse.rs`): parse the two pseudo-ops; parse `Tok::LoopIndex` as a
  primary expression, and a `Tok::Text` followed by `Tok::LoopIndex` as a
  pasted name (§2.2) wherever a symbol name may appear. Matching `.for`/`.endfor` and checking what a body may
  contain (§3) is done in one pass over the parsed lines, before assembly, so
  an unterminated loop is reported once at parse time and not once per pass.
- **Assembler**: `run_pass` becomes an index-based walk. A `loop_stack` holds
  one frame per open loop — `{ body_start, current, end, step }`. `.for`
  evaluates its bounds and pushes a frame; `.endfor` either advances
  `current` and jumps to `body_start`, or pops. `Expr::LoopIndex(d)` reads
  `loop_stack[len - 1 - d].current`, and a pasted name formats that value
  onto its base before the symbol table is consulted. Inside a suppressed `.if`, `.for` pushes
  a zero-iteration frame so its `.endfor` still pairs up.
- **`exec_emitted_source`** (a custom pseudo-op's `emit_source`): `.for` in
  emitted source just works, since it is executed by the same machinery. A
  custom pseudo-op invoked inside a loop body is re-run every iteration, which
  is correct (and its cache key already includes its arguments, so an
  argument containing `\i` re-keys per iteration).

## 5. Semantics

### 5.1 Bounds must be known on pass 1

The iteration count decides how many bytes the loop emits, which decides every
later label's address. A bound that depends on a symbol not yet defined on the
first pass (a forward reference) is therefore an error, rather than a value
that silently changes between passes and shifts every label after it. (`.fill`
has the same hazard today and does *not* check for it; fixing `.fill` is out
of scope here.) Constants defined *above* the
loop, which is the common case, are fine.

### 5.2 An iteration limit

A loop whose bounds are wrong (`.for 0, $FFFF` where `$FF` was meant) would
otherwise emit until it overflows a bank, producing a wall of errors. A single
`.for` is capped at **4096 iterations** (more than a full nametable's 1024
bytes with room to spare), and a nested loop's total iterations are capped at
**65536**; exceeding either is one error on the `.for` line, naming the count
([§11, Q6](#q6-the-iteration-limit)).

### 5.3 What is unchanged

- **ROM output for every existing source.** `.for`, `.endfor` and `\i`/`\j`/`\k`
  are not valid in any source today (`\i` is an "expected expression" parse
  error, `.for` an unknown custom pseudo-op), so no existing program changes
  meaning. This is
  an extension over the reference tool, which has no equivalent; parity tests
  are unaffected.
- **Symbols.** Body lines are the file's own text, so constants defined in a
  body are *not* flagged `from_macro`. Pasted names are the exception (§2.2).
- **Coverage (plan 007).** Each executed body line maps back to its real file
  and line, so CDL-based coverage reports a body line as covered when any
  iteration's bytes ran — no special handling.

## 6. Diagnostics

New i18n messages (`crates/nessemble-i18n/locales/en-US.ftl`), each reported at
the line named:

| Condition | Line |
| --- | --- |
| `.for` with no matching `.endfor` | the `.for` |
| `.endfor` with no open `.for` | the `.endfor` |
| `.for` missing a bound, or with more than two | the `.for` |
| A bound that is not known on pass 1 (§5.1) | the `.for` |
| More than 4096 iterations, or 65536 nested (§5.2) | the `.for` |
| `\i` outside any loop; `\j` / `\k` with too few enclosing loops | the use |
| A plain named label inside a body (§3) | the label |
| A name pasting more than one counter (`cell\j\i`) (§2.2) | the use |
| Pasting a negative counter value (§2.2) | the use, naming the iteration |
| A pasted name that is already defined (§2.2) | the definition |
| `.macrodef` inside a body (§3) | the `.macrodef` |
| `.if` opened in a body and closed outside it (or vice versa) | the `.endfor` |

An error raised by a line *inside* a body (an out-of-range branch, an
overflowing operand) repeats once per iteration that hits it. Those are
de-duplicated to one diagnostic per (file, line, message), with the message
suffixed by the counter value of the first iteration that produced it —
"Branch out of range (in `.for` iteration `\i` = 5)" — so the author can tell
which iteration broke ([§11, Q7](#q7-per-iteration-diagnostics)).

## 7. Tooling

- **Directive catalog** (`crates/nessemble-isa/src/lib.rs`, `DIRECTIVES`):
  a `(".for / .endfor", "repeat a block, unrolled")` entry. This feeds the
  `reference` command and LSP completion/hover.
- **Formatter** (`format_source` in `crates/nessemble-core/src/tooling.rs`):
  no change. The formatter has no block-depth indentation (an `.if` body is not
  indented either); `.for`/`.endfor` follow `indent_directives` like every
  other directive, and body lines format as they would outside a loop. Phase 2
  adds a test pinning that `\i` operands survive formatting unchanged.
- **Language server** (`crates/nessemble-lsp`): `.for`…`.endfor` folding
  ranges (beside `.macrodef`…`.endm` and `.if`…`.endif`); `\i`/`\j`/`\k` get the
  same semantic token class as `\1`; hover on `\i` says which loop it binds to
  and its range.
- **Routine-signature verifier** (plan 010): a routine body containing a
  `.for` is reported as "not fully understood" in v1, the same as any
  directive the verifier does not model — `.for` is deliberately *not* added
  to `is_transparent_directive`, since it emits code.
- **Linting** (plan 008): no new rule in v1. Existing rules see the body once
  (they work on source text), which is the right granularity. Rules that read
  label definitions from text (e.g. `require-block-comment`'s `block_label`)
  must read `row\i:` as one pasted definition, not as a label `row` followed
  by junk. Phase 2 tests this.
- **Pasted names in the LSP**: go-to-definition and hover on `row1` (outside
  the loop) resolve to the `row\i:` line. A text search for `row1:` finds
  nothing, and the assembler's `Symbol` records no definition site. So the
  language server falls back by text: a reference `<base><digits>` with no
  plain definition resolves to a pasted definition `<base>\i:` (or `\j`/`\k`)
  with the same base. That needs no change to the assembler. Hover names the
  loop and, when its bounds are literals, whether the index is in range.
  Completion offers `<base>\i` once, not each pasted name.
- **Editors** (`editors/vscode`): no grammar change — highlighting comes from
  the LSP's semantic tokens.
- **WASM / web playground**: no change beyond rebuilding; the assembler is
  shared.

## 8. Docs

`docs/src/syntax.md` gains a `## Loops` section after `## Macros`: the example
from §1, its hex output, and a `<nessemble-assembler>` "Try it" block, then
short subsections on counting down, nesting with `\j`, anonymous labels in a
body, and pasted names (`row\i:`, with the jump-table example from §2.2 and
why a plain label is rejected).

## 9. Phased plan

**Phase 1 — the directive.** Lexer, AST, parser, assembler changes from §4;
the diagnostics in §6; the `DIRECTIVES` entry; the docs section. Tests in
`crates/nessemble-core`:

- The §1 example assembles to the same bytes as its hand-unrolled form.
- Counting down; a single-iteration loop (`.for 3, 3`); nesting with `\j`;
  bounds from constants; `\i` in `.db`, immediate operands and `.if`.
- Anonymous labels and branches inside a body.
- Pasted labels and constants: the §2.2 jump table assembles to the same bytes
  as a hand-written one, a pasted name is usable outside the loop, `\i` pastes
  inside a macro body invoked in a loop, and pasted symbols are flagged
  `from_macro`.
- A `.macro` and an `.include` inside a body; `\i` as a macro argument.
- A custom pseudo-op inside a body sees a fresh argument each iteration.
- Every diagnostic in §6, including de-duplication of a per-iteration error.
- `.for` inside a false `.if` emits nothing and pairs its `.endfor`.

Changeset: `minor`.

**Phase 2 — tooling.** The formatter test, LSP folding / semantic tokens / hover,
go-to-definition and completion for pasted names, lint rules reading pasted
definitions, and the routine-verifier behavior from §7, with their tests. Changeset:
`minor` (new editor features).

## 10. Non-goals

- **A step argument** (`.for 0, 14, 2`). `\i*2` with halved bounds covers it;
  revisit if real code finds that awkward.
- **Iterating over a list** (`.for x in 1, 4, 9`). That is a table lookup,
  which `.db` plus `LDA table+\i` already expresses.
- **More general pasting.** Pasting in the middle of a name (`row\i_end`),
  pasting several counters (`cell\j\i`), and pasting a computed value
  (`row\(\i*2)`) are all out of v1 (§2.2). Each needs a rule for where the
  pasted part ends; revisit if real code needs it.
- **Runtime loops.** `.for` only ever unrolls; it never emits a counter and a
  branch.

## 11. Decisions

Each question was settled with the maintainer. The chosen option is first and
bold.

### Q1. How nested loops name their counters

- **(a) `\i` innermost, `\j` / `\k` outward.** Short, and matches the
  original `\i`. The trade-offs accepted: the meaning of `\i` shifts when a
  loop is wrapped in another one, and three levels is a hard ceiling.
- (b) Named counters (`.for row, 0, 3` with `\row`). Rejected: a
  three-argument form, for depth nobody needs.
- (c) No access to outer counters. Rejected: 2D nametable copies want `\j`.

### Q2. Inclusive end bound

- **(a) Inclusive**, matching the original example: `.for 0, 2` runs 3 times.
- (b) Exclusive, like a Rust range. Rejected.
- (c) A count form (`.for 3`) as well. Rejected: two spellings of one thing.

### Q3. `START > END`

- **(a) Counts down.**
- (b) Runs zero times. Rejected.
- (c) Is an error. Rejected.

### Q4. Named labels in a loop body

- **(b) Per-iteration pasting**: `row\i:` defines `row0`, `row1`, …
  ([§2.2](#22-per-iteration-names)). Chosen over the draft's
  recommendation because jump tables and per-entry handlers built from a loop
  are real NES use. A plain named label in a body is still an error (§3).
- (a) Reject all named labels in a body. Rejected.

### Q5. Assembler or preprocessor

- **(a) Assembler**, so bounds may use constants defined above the loop.
- (b) Preprocessor. Rejected: bounds would have to be integer literals.

### Q6. The iteration limit

- **(a) 4096 per loop, 65536 total for a nest.**
- (b) Configurable in `nessemble.toml`. Rejected for v1.
- (c) No limit. Rejected.

### Q7. Per-iteration diagnostics

- **(a) One diagnostic per line, naming the first failing iteration's `\i`.**
- (b) One diagnostic per failing iteration. Rejected.

### Q8. The directive's name — **settled: `.for` / `.endfor`**

The name says what the directive does: "for `\i` from `START` to `END`, emit
the body". The other candidate, `.loop` / `.endloop`, was rejected because on the 6502
"loop" usually means a runtime loop (a counter and a `BNE`), which this
directive never emits ([§10](#10-non-goals)). No aliases from other assemblers
(`.rept` / `.endr`, `.repeat` / `.endrepeat`) are accepted. One spelling
keeps the directive catalog, completion and docs unambiguous.
