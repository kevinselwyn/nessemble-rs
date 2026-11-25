# nessemble-rs: A Plan for Unrolled Loops (`.loop` / `.endloop`)

> Status: **Draft — under review.** No code has been written. The design forks
> that need the maintainer's call are collected in
> [§11](#11-open-questions); each has a recommended answer that the rest of this
> document assumes, so the plan reads as one coherent proposal. Once those are
> settled, §11 becomes "Decisions" and its answers are final.

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
.loop 0, 2
  LDA src_label+\i
  STA PPUDATA
.endloop
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
.loop START, END
  body
.endloop
```

- **`START` and `END` are both inclusive** — `.loop 0, 2` runs three times, with
  `\i` = 0, 1, 2, matching the example above. This reads the way the bounds of
  a table are usually written (`0, LEN-1`), and agrees with the request's
  example ([§11, Q2](#q2-inclusive-end-bound)).
- **The bounds are expressions**, not just literals: `.loop 0, PALETTE_LEN-1`,
  `.loop FIRST_SPRITE, FIRST_SPRITE+7`. They must resolve on the first pass —
  see [§4](#4-where-the-loop-runs-the-assembler-not-the-preprocessor) and
  [§5.1](#51-bounds-must-be-known-on-pass-1).
- **`\i` is the counter**, usable anywhere an expression is: `LDA table+\i`,
  `LDA #\i*4`, `.db \i`, `.if \i % 2`. It evaluates to a plain integer, so
  `\i*2` gives a step-of-two sequence without any extra syntax.
- **`START > END` counts down**: `.loop 7, 0` runs with `\i` = 7, 6, … 0
  ([§11, Q3](#q3-start--end)). Copying a buffer backwards to the PPU, or
  pushing a table onto the stack in reverse, is common enough that making the
  author write `7-\i` is a needless indirection.
- **`.loop` and `.endloop` are case-insensitive** and may be indented, like
  every other directive.

### 2.1 Nesting

Loops nest. `\i` always names the **innermost** enclosing loop's counter; the
counters of enclosing loops are `\j` (one level out) and `\k` (two levels out):

```nessemble
.loop 0, 3          ; row
  .loop 0, 7        ; column
    LDA tiles+\j*8+\i
    STA PPUDATA
  .endloop
.endloop
```

This is [§11, Q1](#q1-how-nested-loops-name-their-counters) — the biggest open
question, since the alternatives (named counters) change the syntax of `.loop`
itself. A `\j`/`\k` with no loop that many levels out is an error
([§6](#6-diagnostics)).

## 3. What a loop body may contain

Everything that may appear at top level, with the semantics it would have if
the body were written out N times in place:

- **Instructions and data directives** — the point of the feature.
- **`.if` / `.ifdef` / `.else` / `.endif`**, including conditions on `\i`
  (`.if \i == 0` to special-case the first iteration). A conditional opened in
  a body must close in the same body ([§6](#6-diagnostics)).
- **Constant assignment** (`X = …`) — reassigned every iteration, as a constant
  redefined on consecutive lines already is.
- **Anonymous labels** (`:` with `:-` / `:+` references) — these already
  resolve to the nearest definition, so a body with an internal branch works
  in every iteration unchanged:

  ```nessemble
  .loop 0, 3
    LDA status+\i
    BEQ :+
    JSR handle
  : NOP
  .endloop
  ```

- **`.macro` invocations and `.include`** — expanded once by the preprocessor
  (which has already run before the loop executes) and then executed every
  iteration with the rest of the body. `\i` may appear in a macro *argument*
  (`.macro SET_PPU, \i`), since the argument's tokens are passed through
  verbatim and evaluated per iteration. `\i` in a macro *body* refers to
  whichever loop the expansion lands in.
- **Nested `.loop` blocks** (§2.1).

What it may **not** contain:

- **Named labels.** A named label inside a body would be defined once per
  iteration. Redefining a label is *not* an error today — `add_symbol`
  silently overwrites it — so every reference would quietly resolve to the
  last iteration's address. That is a wrong-code bug with no diagnostic, so a
  named label in a body is an error that says *why* ("label `x` is defined
  inside a `.loop` body, so it would be defined once per iteration"). The same
  applies to a `label\@` from a macro expanded inside a
  loop: the preprocessor gave it one unique suffix, not one per iteration
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
the bounds could only be integer literals — `.loop 0, PALETTE_LEN-1` would be
an error, and that is the form real code wants.

**The assembler** (`crates/nessemble-core/src/assemble.rs`) evaluates
expressions, so bounds may reference constants. Its passes already walk a flat
`&[Line]` and handle `.if` blocks by keeping a stack (`if_stack`); a loop is
the same shape with one addition — on `.endloop`, jump back to the line after
`.loop` instead of falling through. `\i` becomes an expression node evaluated
against a loop stack rather than a token substituted into a copy of the body,
so no tokens are duplicated and every executed line is still the real line in
the real file.

**This plan takes the assembler route** ([§11, Q5](#q5-assembler-or-preprocessor)).
Concretely:

- **Lexer** (`lexer.rs`): `\i`, `\j`, `\k` lex as a new `Tok::LoopIndex(depth)`
  (0, 1, 2) alongside the existing `\N` / `\#` / `\@` rules. `.loop` and
  `.endloop` lex as `Tok::Pseudo` with no lexer change.
- **AST** (`ast.rs`): `Expr::LoopIndex(u8)`; `Pseudo::Loop(Expr, Expr)` and
  `Pseudo::EndLoop`.
- **Parser** (`parse.rs`): parse the two pseudo-ops; parse `Tok::LoopIndex` as a
  primary expression. Matching `.loop`/`.endloop` and checking what a body may
  contain (§3) is done in one pass over the parsed lines, before assembly, so
  an unterminated loop is reported once at parse time and not once per pass.
- **Assembler**: `run_pass` becomes an index-based walk. A `loop_stack` holds
  one frame per open loop — `{ body_start, current, end, step }`. `.loop`
  evaluates its bounds and pushes a frame; `.endloop` either advances
  `current` and jumps to `body_start`, or pops. `Expr::LoopIndex(d)` reads
  `loop_stack[len - 1 - d].current`. Inside a suppressed `.if`, `.loop` pushes
  a zero-iteration frame so its `.endloop` still pairs up.
- **`exec_emitted_source`** (a custom pseudo-op's `emit_source`): `.loop` in
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

A loop whose bounds are wrong (`.loop 0, $FFFF` where `$FF` was meant) would
otherwise emit until it overflows a bank, producing a wall of errors. A single
`.loop` is capped at **4096 iterations** (more than a full nametable's 1024
bytes with room to spare), and a nested loop's total iterations are capped at
**65536**; exceeding either is one error on the `.loop` line, naming the count
([§11, Q6](#q6-the-iteration-limit)).

### 5.3 What is unchanged

- **ROM output for every existing source.** `.loop`, `.endloop` and `\i`/`\j`/`\k`
  are not valid in any source today (`\i` is an "expected expression" parse
  error, `.loop` an unknown custom pseudo-op), so no existing program changes
  meaning. This is
  an extension over the reference tool, which has no equivalent; parity tests
  are unaffected.
- **Symbols.** Body lines are the file's own text, so constants defined in a
  body are *not* flagged `from_macro`.
- **Coverage (plan 007).** Each executed body line maps back to its real file
  and line, so CDL-based coverage reports a body line as covered when any
  iteration's bytes ran — no special handling.

## 6. Diagnostics

New i18n messages (`crates/nessemble-i18n/locales/en-US.ftl`), each reported at
the line named:

| Condition | Line |
| --- | --- |
| `.loop` with no matching `.endloop` | the `.loop` |
| `.endloop` with no open `.loop` | the `.endloop` |
| `.loop` missing a bound, or with more than two | the `.loop` |
| A bound that is not known on pass 1 (§5.1) | the `.loop` |
| More than 4096 iterations, or 65536 nested (§5.2) | the `.loop` |
| `\i` outside any loop; `\j` / `\k` with too few enclosing loops | the use |
| A named label inside a body (§3) | the label |
| `.macrodef` inside a body (§3) | the `.macrodef` |
| `.if` opened in a body and closed outside it (or vice versa) | the `.endloop` |

An error raised by a line *inside* a body (an out-of-range branch, an
overflowing operand) repeats once per iteration that hits it. Those are
de-duplicated to one diagnostic per (file, line, message), with the message
suffixed by the counter value of the first iteration that produced it —
"Branch out of range (in `.loop` iteration `\i` = 5)" — so the author can tell
which iteration broke ([§11, Q7](#q7-per-iteration-diagnostics)).

## 7. Tooling

- **Directive catalog** (`crates/nessemble-isa/src/lib.rs`, `DIRECTIVES`):
  a `(".loop / .endloop", "repeat a block, unrolled")` entry. This feeds the
  `reference` command and LSP completion/hover.
- **Formatter** (`format_source` in `crates/nessemble-core/src/tooling.rs`):
  no change. The formatter has no block-depth indentation (an `.if` body is not
  indented either); `.loop`/`.endloop` follow `indent_directives` like every
  other directive, and body lines format as they would outside a loop. Phase 2
  adds a test pinning that `\i` operands survive formatting unchanged.
- **Language server** (`crates/nessemble-lsp`): `.loop`…`.endloop` folding
  ranges (beside `.macrodef`…`.endm` and `.if`…`.endif`); `\i`/`\j`/`\k` get the
  same semantic token class as `\1`; hover on `\i` says which loop it binds to
  and its range.
- **Routine-signature verifier** (plan 010): a routine body containing a
  `.loop` is reported as "not fully understood" in v1, the same as any
  directive the verifier does not model — `.loop` is deliberately *not* added
  to `is_transparent_directive`, since it emits code.
- **Linting** (plan 008): no new rule in v1. Existing rules see the body once
  (they work on source text), which is the right granularity.
- **Editors** (`editors/vscode`): no grammar change — highlighting comes from
  the LSP's semantic tokens.
- **WASM / web playground**: no change beyond rebuilding; the assembler is
  shared.

## 8. Docs

`docs/src/syntax.md` gains a `## Loops` section after `## Macros`: the example
from §1, its hex output, and a `<nessemble-assembler>` "Try it" block, then
short subsections on counting down, nesting with `\j`, anonymous labels in a
body, and why named labels are rejected.

## 9. Phased plan

**Phase 1 — the directive.** Lexer, AST, parser, assembler changes from §4;
the diagnostics in §6; the `DIRECTIVES` entry; the docs section. Tests in
`crates/nessemble-core`:

- The §1 example assembles to the same bytes as its hand-unrolled form.
- Counting down; a single-iteration loop (`.loop 3, 3`); nesting with `\j`;
  bounds from constants; `\i` in `.db`, immediate operands and `.if`.
- Anonymous labels and branches inside a body.
- A `.macro` and an `.include` inside a body; `\i` as a macro argument.
- A custom pseudo-op inside a body sees a fresh argument each iteration.
- Every diagnostic in §6, including de-duplication of a per-iteration error.
- `.loop` inside a false `.if` emits nothing and pairs its `.endloop`.

Changeset: `minor`.

**Phase 2 — tooling.** The formatter test, LSP folding / semantic tokens / hover,
and the routine-verifier behavior from §7, with their tests. Changeset:
`minor` (new editor features).

## 10. Non-goals

- **A step argument** (`.loop 0, 14, 2`). `\i*2` with halved bounds covers it;
  revisit if real code finds that awkward.
- **Iterating over a list** (`.loop x in 1, 4, 9`). That is a table lookup,
  which `.db` plus `LDA table+\i` already expresses.
- **Per-iteration named labels** (something like `row\i:` producing `row0`,
  `row1`, …). It needs a token-pasting rule the expression-level `\i` does not
  have; out of scope unless Q4 decides otherwise.
- **Runtime loops.** `.loop` only ever unrolls; it never emits a counter and a
  branch.

## 11. Open questions

Each question lists the option this draft assumes first.

### Q1. How nested loops name their counters

- **(a) `\i` innermost, `\j` / `\k` outward** (assumed). Short, matches the
  request's `\i`, but the meaning of `\i` shifts when a loop is wrapped in
  another one, and three levels is a hard ceiling.
- (b) Named counters: `.loop row, 0, 3` with the counter spelled `\row`. Never
  ambiguous, any depth — but a three-argument form, and the request's
  two-argument example would need a default name (`\i`).
- (c) No access to outer counters: `\i` is always innermost, and an outer
  counter must be copied into a constant (`ROW = \i`) before the inner loop.

### Q2. Inclusive end bound

- **(a) Inclusive** (assumed), matching the example: `.loop 0, 2` runs 3 times.
- (b) Exclusive (`.loop 0, 3` runs 3 times), like a Rust range.
- (c) A count instead of an end: `.loop 3` runs `\i` = 0…2, with the two-bound
  form also accepted.

### Q3. `START > END`

- **(a) Counts down** (assumed).
- (b) Runs zero times.
- (c) Is an error.

### Q4. Named labels in a loop body

- **(a) An error** explaining why (assumed).
- (b) Add per-iteration label pasting so `row\i:` defines `row0`, `row1`, ….
  Useful for jump tables built from a loop, but a new token-pasting rule.

### Q5. Assembler or preprocessor

- **(a) Assembler** (assumed) — bounds may use constants defined above the loop.
- (b) Preprocessor — a smaller change, reusing the macro-expansion machinery,
  but bounds must be integer literals.

### Q6. The iteration limit

- **(a) 4096 per loop, 65536 total for a nest** (assumed).
- (b) A different limit, or a configurable one in `nessemble.toml`.
- (c) No limit — rely on bank-overflow errors.

### Q7. Per-iteration diagnostics

- **(a) One diagnostic per line, naming the first failing iteration's `\i`**
  (assumed).
- (b) One diagnostic per failing iteration.

### Q8. The directive's name

- **(a) `.loop` / `.endloop`** (assumed), as requested.
- (b) An alias for familiarity from other 6502 assemblers — `.rept` / `.endr`
  (asm6, NESASM-style) or `.repeat` / `.endrepeat` (ca65) — accepted alongside
  `.loop`.
