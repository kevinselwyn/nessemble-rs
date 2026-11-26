# nessemble-rs: A Plan for Unary Operators (`-`, `~`, `!`)

> Status: **Proposed.** Points marked **decided** were settled with the
> maintainer. Points marked **proposed** are this plan's own choices, and §8
> lists them for sign-off. Suggested merge position: first of the six plans
> 017–022.

---

## 1. Summary

nessemble 2.27.0 can't write `.db -4`, `.db ~$0F`, `.if !FLAG` or `LDA #-1`. Each fails with `expected expression`, so authors write `0 - 4`, `LOW($10000 - 40), HIGH($10000 - 40)` or two's-complement hex by hand. This plan adds three prefix operators: `-` (negate), `~` (bitwise complement) and `!` (logical not). They bind tighter than every binary operator, stack freely, and are allowed anywhere an expression is. The change is one new AST node, one new parser function and two new lexer tokens. The grammar only grows into positions that are a parse error today, so every program that assembles on 2.27.0 still emits identical bytes.

---

## 2. Semantics

### 2.1 The operators (decided)

| Operator | Meaning | Result |
|---|---|---|
| `-x` | arithmetic negation | `0 - x`, using a wrapping negate on `i64` so `i64::MIN` can't panic |
| `~x` | bitwise complement | `!x` in Rust (all 64 bits), equal to `-x - 1` |
| `!x` | logical not | `1` if `x == 0`, else `0` |

Results are ordinary signed `i64` values, the same as any other expression value. Nothing narrows at the operator. Narrowing happens where the value is emitted, exactly as for `0 - 4` today.

### 2.2 Binding (decided)

- **A unary operator binds tighter than every binary operator.** Binary grouping is not touched: it stays right to left with no precedence.
  - `-2 * 3` is `(-2) * 3` = -6
  - `2 * -3` is -6
  - `4 - -2` is 6
  - `-2 - 1` is `(-2) - 1` = -3
  - `20 - -2 - 1` is `20 - ((-2) - 1)` = 23, because binary grouping still runs right to left
  - `~0 & $0F` is 15
  - `!0 + 1` is 2; `!(0 + 1)` is 0
  - `!FLAG == 0` is `(!FLAG) == 0`. To negate a comparison, write `!(FLAG == 0)`.
- **They stack:**
  - `- -4` = 4, and `--4` is the same thing (parser tokens carry no column)
  - `!!x` normalises to 0 or 1
  - `~-1` = 0, `-~0` = 1, `~~7` = 7, `-!0` = -1, `!-1` = 0
- **Operands.** A unary operator applies to:
  - any literal in any base: `-$28`, `-28h`, `-%0101`, `-0101b`, `-0101` (octal), `-101o`, `-65`, `-65d`, `-'A'`
  - symbols and pasted symbols (`-row\i`)
  - `.for` counters (`-\i`)
  - macro arguments
  - parenthesised expressions
  - `HIGH()`, `LOW()` and `BANK()`: `-LOW(x)` is `-(x & $FF)`
  - anonymous references (`-:-` parses; it's silly but harmless)
- **The `->` accessor (proposed).** `->` is treated as part of the operand, like C's `->`: `-sprite->coord_x` is `-(sprite + coord_x)`. Today the `->` chain is parsed inside `parse_primary` as a postfix chain, and unary wraps the result of `parse_primary`. Making it `(-sprite) + coord_x` would mean moving the accessor loop out of the primary; rejected because `->` reads as field access. The reference corpus uses no `->`.

### 2.3 Where it is valid (decided)

Anywhere `parse_expr` is called, so nothing needs to be wired per directive:

- **Data and fill:** `.db`, `.dw`, `.hibytes`, `.lobytes`, `.fill`, `.color`, `.defchr`, `.font`, `.random`
- **Operands:**
  - `#imm`
  - absolute and indexed operands
  - `<` zero-page: `LDA <-off`
  - `[` indirect operands
- **Definitions:**
  - `NAME = -1` and `name\i = -\i`
  - `NAME .rs -1`, which behaves as `.rs 0 - 1` does today
- **Conditionals and loops:**
  - `.if`
  - `.for` bounds: `.for -2, 2` and `.for 2, -2`
  - the `.enum` start and step
  - `.rsset`
- **Other directives:**
  - `.org`, `.phase`, `.checksum`, `.ines*`, `.prg`, `.chr`
  - the `.incbin`/`.incpng` offset and limit
- **Custom Rhai pseudo-instruction arguments** (`CustomArg::Int`)
- **Macro arguments.** `split_args` already passes `-4` through as `[Minus, Number(4)]`, and `substitute` wraps every `\N` in parentheses. So a body `.db 10 - \1` invoked with `-2` becomes `10 - (-2)` = 12. No preprocessor change is needed.

### 2.4 Narrowing and range (decided; follows the companion range rule)

| Source | Bytes | Warning |
|---|---|---|
| `.db -4` | `fc` | none |
| `.dw -40` | `d8 ff` | none |
| `LDA #-1` | `a9 ff` | none |
| `.db ~$0F` (−16) | `f0` | none |
| `.db LOW(-40), HIGH(-40)` | `d8 ff` | none; same bytes as the idiom `LOW($10000 - 40), HIGH($10000 - 40)` |
| `.db ~$FF` (−256) | `00` | companion `value-out-of-range` (byte rule is `-128…255`) |
| `AND #~$80` (−129) | `29 7f` | companion `value-out-of-range` |
| `LDA <-off`, where `off = -$10` | `a5 10` | none |
| `LDA -1` | `ad ff ff` | Absolute (addressing mode is syntactic), as `LDA 0-1` is today; whether the companion word rule warns is its call |

If the range rule hasn't landed, today's `& $FF` truncation already produces the same bytes. The `AND #~$80` case is a real usability cost; see §8.

### 2.5 Lexer hazards (each must keep working)

- **Anonymous references.** `BNE :-` and `BNE :---` are `Colon` followed by a run of `Minus`. `parse_primary`'s `Colon` arm still eats the run greedily before any unary logic sees it. Because tokens carry no column, `:- - 1` is still `:--` with a leftover `1`, today and after this change. Docs recommend `(:-) - 1` or `:- + -1`.
- **Binary minus.** `a-1`, `a - 1`, `(x)-(y)` and `val - -4` all work. After an operand, `Minus` is still taken as binary by `binop()`. A unary operator is recognised only where a primary is expected.
- **`!=`.** The two-character arm in `lex_operator` matches before the new one-character `!` arm, so `!=` stays `NotEqu`. `a!=-1` lexes as `NotEqu, Minus`. `a ! = 1` lexes as `Bang, Equ` and leaves trailing tokens, like any other non-operator.
- **`->`.** `->` is still lexed first. So `-` directly before `>` is never unary, but no real expression puts `>` after a unary operator.
- **`#` and `<` prefixes.** `parse_operand` eats `Hash` or `Lt` and then calls `parse_expr`, which reaches the unary parser.
- **`.ascii "s" -1`.** The `.ascii` arm in `parse_pseudo` still consumes the `Minus` itself as its negate-offset form. The meaning is unchanged, and it agrees with `.ascii "s" + -1`.
- **Single-letter names** are registers (`CharReg`), not symbols. That's unchanged, so tests and docs use names of two or more characters (`LDA <-off`, not `LDA <-x`).
- **`@` in identifiers (plan 020).** This plan touches only:
  - the `Tok` enum (two new variants);
  - two new arms in the one-character table of `lex_operator`.

  It does not touch `is_ident_char`, rule 14 (identifiers), rule 10 (`\@`, `\N`, `\i`), `classify_word`, or the tooling lexer's `is_ident_start`/`is_ident`. The two features merge cleanly in either order, and neither depends on the other.

### 2.6 Known gap (decided: document, don't fix)

`.db 1 -2` is one expression, `1 - 2`, and emits one byte, `ff`. Neither this feature nor the companion trailing-token check can catch it, because tokens carry no column. The docs warn about it. A later lint rule could flag a data-list element with a space before `-` and none after (see §8).

### 2.7 What changes for invalid programs

- **`!` and `~`** used to fall through to the lexer's unknown-character fallback as `CharReg('!')` and `CharReg('~')`.
  - `LDA !` used to report ``Unknown register `!` ``. It now reports `expected expression`.
  - `.db 1 ! 2` assembles today, because trailing tokens are ignored. After this change it still does, since `!` is not a binary operator. The companion trailing-token check will turn it into an error.
- **No valid program changes output.** The new operators appear only:
  - in operand positions that hit `_ => self.err("expected expression")` today;
  - as the two characters `!` and `~`, which can't appear in valid code today outside comments and strings. The reference corpus has none, checked by grep with comments stripped.

---

## 3. Code changes

**SHARED** marks a change to shared code.

### 3.1 `crates/nessemble-core/src/lexer.rs` (lexer, SHARED)

- Add `Tok::Tilde` and `Tok::Bang` with doc comments.
- In `lex_operator`'s one-character table, add `b'~' => Tok::Tilde` and `b'!' => Tok::Bang`. They go after the two-character match, so `!=` keeps priority.
- Nothing else changes. `-` is already `Tok::Minus`, and the parser decides unary vs binary.
- Update the module doc's list of context-sensitive pieces with one line: "`-` is unary or binary by position, which the parser decides".

### 3.2 `crates/nessemble-core/src/ast.rs` (expression AST, SHARED)

- Add `pub enum UnOp { Neg, Not, Compl }`.
- Add `Expr::Unary(UnOp, Box<Expr>)`.
- Fix the `BinOp` doc comment: binary operators share one level and are right-associative, and unary operators bind tighter than all of them.

### 3.3 `crates/nessemble-core/src/parse.rs` (expression parser, SHARED)

- **New `fn parse_unary(&mut self) -> Result<Expr, ParseError>`:**
  - on `Tok::Minus`, `Tok::Tilde` or `Tok::Bang`: consume it and return `Expr::Unary(op, Box::new(self.parse_unary()?))`, which is recursive, so operators stack;
  - otherwise return `self.parse_primary()`.
- **`parse_expr`:** change `let left = self.parse_primary()?` to `self.parse_unary()?`. The right operand already recurses through `parse_expr`, so it picks up unary too. The binary loop is untouched, so grouping is unchanged.
- **Leave alone:**
  - the `Colon` arm of `parse_primary` (anonymous references);
  - the `->` loop;
  - the `.ascii` `Minus` arm in `parse_pseudo`;
  - `binop()`, which still excludes `Tilde` and `Bang`.
- Keep the error text `expected expression` unchanged (§5).

### 3.4 `crates/nessemble-core/src/assemble.rs` (evaluator)

- Add `fn apply_unary(op: UnOp, v: i64) -> i64`:
  - `Neg` gives `v.wrapping_neg()`
  - `Compl` gives `!v`
  - `Not` gives `(v == 0) as i64`
- Add `Expr::Unary(op, e) => Self::apply_unary(*op, self.eval(e))` to `Assembler::eval`. Unknown-symbol tracking (`note_unknown`/`eval_unknown`) flows through the inner `eval`, so the pass-1 check on `.for` bounds still names an unresolved symbol inside `-SYM`.
- Add `Expr::Unary(op, e) => Some(Assembler::apply_unary(*op, literal_eval(e)?))` to `literal_eval`, so custom-pseudo prewarm (plan 011) treats `.mypseudo -1` as a literal candidate and evaluates it exactly as the real pass does.
- `exec_instruction`, `emit_relative` and the data emitters don't change, because they already mask `i64` values.

### 3.5 Not changed (each needs a statement and a test, not code)

| Area | Why it is unchanged |
|---|---|
| `preprocess.rs` | `split_args` and `substitute` pass `Minus`/`Tilde`/`Bang` through, and the parenthesised substitution keeps meaning |
| Symbol table (SHARED) | values are already `i64` |
| Listing writer, `render_list_file` in `lib.rs` (SHARED) | negative constants print as 32-bit two's complement (`FFFFFFD8 = NEG`), exactly as `NEG = 0 - 40` does today; changing that is out of scope (§8) |
| Lint engine, `tooling.rs` `lint` and its rules (SHARED) | rules work on operand text (`instruction_writes`, `ends_with_index`, `apply_call`), and `#-1`, `#~$0F` don't change which register is written; no new rule |
| `.nessemblerc` schema (`nessemble-rc`, SHARED) | no change |
| `coverage.rs` | works on assembled line maps |

---

## 4. Tooling parity

- **Formatter** (`tooling::format`, which has **its own lossless lexer**, `tooling::lex`):
  - `-`, `~` and `!` lex as `LexKind::Punct`.
  - `build_body` keeps internal whitespace as written, except around commas. So `.db -4, ~$0F`, `LDA #-1` and `4 - -2` are all reproduced exactly.
  - `consolidate_data` splits on `,` and trims, so merging `.db -4` and `.db ~$0F` gives `.db -4, ~$0F`.
  - No code change. Add tests that format leaves unary forms unchanged and is idempotent.
- **Highlighting.** This covers LSP semantic tokens (`tooling::highlight`), the wasm highlighter (`crates/nessemble-wasm`, the same function) and the web component (`web/nessemble-assembler.js`, which uses the wasm build).
  - `-`, `~` and `!` all classify as `TokenClass::Operator`. `!=` is two `Punct` lexemes, both classed Operator, as today.
  - `editors/vscode` has no TextMate grammar; it gets highlighting from the LSP.
  - No change. Add a pin test.
- **LSP** (`crates/nessemble-lsp`):
  - Diagnostics come from the core parser and assembler (`parse_recovering`), so they pick the feature up automatically.
  - Hover on a negative constant shows the decimal `-40` correctly. `format_hex` falls into its `${value:X}` arm, which prints the 64-bit `$FFFFFFFFFFFFFFD8`. That behaviour already exists for `0 - 40`. Recommended follow-up: show `$FFD8` for `-32768…-1` (§8). Not in this plan.
- **`lint`, `coverage`, listing `-l`/`--mlist`:** no change; see §3.5.
- **`nessemble reference`:** it has no operators category (only `instructions`, `directives`, `script`), and the `DIRECTIVES` text in `nessemble-isa` doesn't describe expressions. No change.

---

## 5. Diagnostics

There is no new message and no new i18n key. `expected expression` is a literal string in `parse.rs` and stays so.

| Source (line N) | Message |
|---|---|
| `.db -` | ``Error in `f.asm` on line N: expected expression`` (unchanged) |
| `LDA #~` | ``Error in `f.asm` on line N: expected expression`` |
| `.if !` | ``Error in `f.asm` on line N: expected expression`` |
| `LDA !` | ``Error in `f.asm` on line N: expected expression`` (was ``Unknown register `!` ``) |
| `.db -"s"` | ``Error in `f.asm` on line N: expected expression`` |
| `.db ~$FF` | companion ``Warning in `f.asm` on line N: …`` (`value-out-of-range`; text owned by the companion) |
| `.for -BAD, 2` with `BAD` defined later | the existing plan-016 pass-1 bounds error, naming `BAD` |

- The line number is the line of the token that fails. On a continued `.db 1,` list, that is the continuation line.
- Inside a macro expansion, the existing and companion call-site rule applies. Nothing new is added.

---

## 6. Tests (write first; all fail on 2.27.0 except the regressions)

### 6.1 Lexer unit tests (`lexer.rs` `mod tests`)

| Input | Tokens |
|---|---|
| `-1` | `[Minus, Number(1)]` |
| `~1` | `[Tilde, Number(1)]` |
| `!0` | `[Bang, Number(0)]` |
| `!=` | `[NotEqu]` |
| `a!=-1` | `Text, NotEqu, Minus, Number` |
| `:---` | `[Colon, Minus, Minus, Minus]` |
| `->` | `[Arrow]` |
| `!~-` | `[Bang, Tilde, Minus]` |

### 6.2 Parser unit tests (`parse.rs` `mod tests`, AST shapes)

| Input | AST |
|---|---|
| `-2*3` | `Binary(Unary(Neg, 2), Mul, 3)` |
| `4 - -2` | `Binary(4, Sub, Unary(Neg, 2))` |
| `- -4` | `Unary(Neg, Unary(Neg, 4))` |
| `!!five` | `Unary(Not, Unary(Not, Symbol))` |
| `~-1` | `Unary(Compl, Unary(Neg, 1))` |
| `-LOW(five)` | `Unary(Neg, Low(Symbol))` |
| `-sp->cx` | `Unary(Neg, Binary(sp, Add, cx))` |
| `BNE :-` | `Absolute(LocalBackward(1))` |
| `BNE :---` | `LocalBackward(3)` |
| `.ascii "A" -1` | `negate: true`, offset `Num(1)` (unchanged) |

### 6.3 New integration file `crates/nessemble-core/tests/unary_operators.rs`

Use the same `raw()`/`bytes()`/`error()` helpers as `for_loops.rs`.

**Brief examples:**

| Source | Bytes |
|---|---|
| `.db -1` | `ff` |
| `.db ~1` | `fe` |
| `.if !0` / `.db 1` / `.endif` | `01` |
| `.if !1 …` | nothing |
| `LDA #-1` | `a9 ff` |
| `.db -4` | `fc` |
| `.dw -40` | `d8 ff` |
| `.db ~$0F` | `f0` |
| `.db 0 - 4` | `fc` (unchanged) |
| `.dw 0 - 40` | `d8 ff` (unchanged) |
| `LDA #(0 - 1)` | `a9 ff` (unchanged) |

**Each operator on each base:**

| Operand | `-` | `~` | `!` |
|---|---|---|---|
| `$28` | `d8` | `d7` | `00` |
| `28h` | `d8` | `d7` | `00` |
| `%0101` | `fb` | `fa` | `00` |
| `0101b` | `fb` | `fa` | `00` |
| `0101` (octal 65) | `bf` | `be` | `00` |
| `101o` | `bf` | `be` | `00` |
| `65` | `bf` | `be` | `00` |
| `65d` | `bf` | `be` | `00` |
| `'A'` | `bf` | `be` | `00` |

Also `!0` = `01` and `!%0` = `01`.

**Stacked operators:** each row is one test (`.db` unless noted).

| Source | Bytes |
|---|---|
| `- -4` | `04` |
| `--4` | `04` |
| `!!5` | `01` |
| `!!0` | `00` |
| `~-1` | `00` |
| `-~0` | `01` |
| `~~7` | `07` |
| `-!0` | `ff` |
| `!-1` | `00` |
| `- - -4` | `fc` |

**Precedence:**

| Source | Bytes |
|---|---|
| `-2 * 3` | `fa` |
| `2 * -3` | `fa` |
| `-(2 * 3)` | `fa` |
| `4 - -2` | `06` |
| `-2 - 1` | `fd` |
| `20 - -2 - 1` | `17` (proves right grouping is untouched) |
| `20 - 2 - 1` | `13` |
| `2*3 - 1` | `04` |
| `~0 & $0F` | `0f` |
| `!0 + 1` | `02` |
| `!(0 + 1)` | `00` |
| `-LOW($1234)` | `cc` |

**Symbols and binary-minus spellings.** With `val = 5`:

| Source | Bytes |
|---|---|
| `.db val-1, val - 1, (val)-(1), val - -4, -val` | `04 04 04 09 fb` |
| `NEG = -40`, then `.dw NEG` | `d8 ff`, and the symbol value is `-40` |

**Idiom equivalence.** `.db LOW(-40), HIGH(-40)` is byte-equal to `.db LOW($10000 - 40), HIGH($10000 - 40)`.

**Operand prefixes:**

| Source | Bytes |
|---|---|
| `off = -$10` / `LDA <-off` | `a5 10` |
| `LDA <-off, X` | `b5 10` |
| `LDA -1` | `ad ff ff` |
| `JMP [-1]` | `6c ff ff` |

**`.if`:**
- `.if !UNSET_FLAG` with `UNSET_FLAG = 0` emits the body.
- `.if !(a == b)`.
- `.if -1` is true.

**`.for` bounds and counters:**

| Source | Bytes |
|---|---|
| `.for -2, 0` / `.db \i` | `fe ff 00` |
| `.for 0, -2` | `00 ff fe` |
| `.for 0, 2` / `.db -\i` | `00 ff fe` |
| `.for 0, 2` / `.db ~\i` | `ff fe fd` |
| `.for -LAST, 0` | uses the plan-016 pass-1 unknown error, naming `LAST` |

**`.fill`, `.enum`, `.rs`:**
- `.fill 3, -1` emits `ff ff ff`.
- `.enum -2` with two `.rs 1` entries gives symbols `-2` and `-1`.

**Macros:**
- `.macrodef EMIT` / `.db \1` / `.endm` / `.macro EMIT, -4` emits `fc`.
- `.macrodef SUBR` / `.db 10 - \1` / `.endm` / `.macro SUBR, -2` emits `0c`, because the argument is parenthesised.
- `.macro EMIT, ~$0F` emits `f0`.

**Custom pseudo:**
- With the test `CustomResolver` pattern from `custom_memo.rs`, `.probe -1, ~0, !0, "s"` passes `ints == [-1, -1, 1]`.
- A prewarm test in `prewarm.rs` shows `.probe -1` is a literal candidate with `ints == [-1]`.

**Known gap, pinned:** `.db 1 -2` emits one byte, `ff`. Comment the test as documented behaviour, so a future lint or fix changes it on purpose.

**Diagnostics:** every row of §5, asserting `(line, message)`, including that `LDA !` now says `expected expression`.

### 6.4 Regressions (byte-identical)

- **Existing goldens under `tests/corpus/`** stay green unchanged:
  - `examples/labels-local` (`:-`, `:+`, `:--`, `:++` branches)
  - `examples/math` (binary `-`)
  - `examples/ascii` (`"…"-32`)
  - `examples/dot-notation` (`->`)
  - `errors/branch-minus`
- **New anonymous-label test** in `unary_operators.rs`: a program using `BNE :-`, `BNE :---`, `BCC :+` and `.dw :- + -1`, asserted against hard-coded bytes taken from 2.27.0 before the change.
- **Reference corpus:** before merging, the maintainer assembles the reference corpus and confirms its ROM hash is unchanged; record the result in the PR.

### 6.5 Tooling tests (`tooling.rs` `mod tests`, `crates/nessemble-cli/tests/cli.rs`)

- `format` leaves these unchanged and is idempotent on them:
  - `    LDA #-1`
  - `    .db -4, ~$0F, !0`
  - `    .db 4 - -2`
  - `    BNE :-`
- `format` merges `.db -4` and `.db ~1` into `.db -4, ~1` under `data_per_line`.
- `highlight("    LDA #-1")` classes `-` as Operator and `1` as Number. `~` and `!` are classed Operator.
- `lint` on a routine using `LDA #-1` gives the same findings as with `LDA #$FF`.
- An LSP test, following the existing semantic-token or diagnostic tests in `nessemble-lsp`: a buffer with `.db -1` produces no diagnostics.

---

## 7. Docs and CHANGELOG

### 7.1 `docs/src/syntax.md`

**New `### Unary Operators` table** under `## Symbols`, after Comparison Operators:

| Symbol | Description |
|---|---|
| `-` | Negate |
| `~` | Bitwise NOT (complement) |
| `!` | Logical NOT (1 if zero, else 0) |

Follow the table with:
- one paragraph: unary operators bind tighter than any binary operator and may be stacked;
- examples with output hex dumps: `.db -4`, `.dw -40`, `LDA #-1`, `.db ~$0F`;
- a `<nessemble-assembler>` "Try it" block.

**New `### Evaluation order` subsection.** It states the right-to-left, no-precedence rule for binary operators (`20 - 2 - 1` is 19, `2*3 - 1` is 4). Today that rule is only mentioned in passing in the `.for` section. Show where unary sits: `4 - -2` is 6 and `-2 * 3` is -6.

**Signed values in data.** A note that values narrow to the byte or word range. Mention `~` on masks with bit 7 set (`AND #~$80` is −129, outside the byte range) and recommend `AND #LOW(~$80)`.

**Caveats, as callouts:**
- `.db 1 -2` is one value (`1 - 2`), not two. Separate list items with commas.
- `:- - 1` is read as `:--` followed by a stray `1`. Write `(:-) - 1`. Put this under `### Temporary` as well.

**Cross-references:**
- In `### .if`, link to Unary Operators for `!`.
- In `### .db`, `### .dw` and `### .fill`, show one negative example each.

### 7.2 `nessemble reference`

No change; see §4.

### 7.3 Changeset

Additive, not a behaviour change:

```bash
cargo run -p xtask -- changeset add minor "Add unary operators: \`-x\` (negate), \`~x\` (bitwise complement) and \`!x\` (logical not), usable in any expression, so \`.db -4\`, \`.dw -40\`, \`LDA #-1\` and \`.if !FLAG\` now assemble."
```

Do **not** edit `CHANGELOG.md`.

### 7.4 Plan file

When this ships, update this plan's status line and add an "As built" section.

---

## 8. Risks and open questions

### Proposed points needing sign-off

1. `-a->b` is `-(a->b)`: `->` is an accessor, part of the operand (§2.2).
2. No new diagnostic text. A dangling unary reports the existing `expected expression`. A more specific message, ``expected expression after `-` ``, would need an i18n key.
3. `LDA !` changes its error message (§2.7). This only affects an invalid program.

### Risks

- **`AND #~MASK` with bit 7 set warns** under the companion range rule (`~$80` is −129). This is the likeliest complaint. Options:
  - (a) document `LOW(~MASK)`, which is this plan's choice;
  - (b) a follow-up that treats `~x` of a byte-range `x` as an 8-bit complement in byte contexts. That contradicts the decided "ordinary signed integers" rule, so it is out of scope.
- **Merge with the `@`-in-identifiers feature (plan 020).** Both touch `lexer.rs`, but in disjoint places (§2.5). A textual conflict is possible in `lexer.rs` `mod tests` only.
- **Merge with the companion trailing-token check.** `.db 1 ! 2` and `a ! = 1` become errors there, not here. The tests in §6.3 must not depend on whether the companion has landed.
- **Overflow.** `apply` uses plain `+`, `-` and `*`, which can panic in debug builds on overflow. That predates this plan. `apply_unary` uses `wrapping_neg`. Making binary arithmetic wrap is a separate fix.

### Out of scope, listed for later

- Changing binary precedence or grouping (explicitly excluded).
- A lint rule for "space before `-`, none after" inside data lists (the `.db 1 -2` gap).
- Listing (`-l`) printing negative constants as `FFFFFFD8`, and LSP hover printing `$FFFFFFFFFFFFFFD8`. Both already happen today for `0 - 40`. A follow-up could print 16-bit two's complement for `-32768…-1`.
- An `operators` category for `nessemble reference`.
- Unary `+` (not requested; `+x` stays an error).
- `<`/`>` as unary low- and high-byte operators (ca65 style). `<` is already the zero-page prefix in operand position.

### Critical files for implementation

- `crates/nessemble-core/src/parse.rs`
- `crates/nessemble-core/src/lexer.rs`
- `crates/nessemble-core/src/ast.rs`
- `crates/nessemble-core/src/assemble.rs`
- `docs/src/syntax.md`
