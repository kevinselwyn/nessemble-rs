# nessemble-rs: A Plan for Build-Time Assertions (`.assert`, `.error`, `.out`)

> Status: **Proposed.** Points marked **decided** were settled with the
> maintainer. Every **proposed** point is refined below, and where this plan
> departs from the brief it says so ([§11](#11-decisions-proposed)). Extends the
> final-pass hook introduced by [plan 019](019-index-keyed-tables.md) §5.2.

---

## 1. Summary

`.assert EXPR [, "message"]` fails the build when `EXPR` evaluates to 0. `.error "message"` fails it unconditionally whenever it sits in a branch that is assembled. `.out "message"` prints a line during the build. None of the three emits bytes, moves the PC or defines a symbol.

During pass 2 (the assembler's last pass) `.assert` and `.error` are queued on the **deferred-check queue** shared with `.table` (plan 019 §5.2), together with a snapshot of their context: loop counters, PC and bank. Pass 1 skips them. After pass 2 the queue is drained exactly once, against the final symbol table. So forward references work, `.phase` addresses are already settled, and nothing runs once per pass.

A failure is reported at the line the author wrote. For a macro expansion that is the **call site**, using the `call_line` field on `Token` and `Line` shared with plan 019. `.out` text goes to **stderr**, not stdout ([§2.6](#26-out)).

## 2. Semantics

### 2.1 Syntax

```nessemble
KIND_LAST = $07
  .assert (kind_table - ptr_table) == (2 * (KIND_LAST + 1)), "one pointer per KIND_*"
  .assert load_stub == $8000, "other banks call this stub by address"
  .assert BANK(level_table) == 3
  .if LEVEL_TYPE > 4
    .error "unsupported LEVEL_TYPE"
  .endif
  .out "building bank 3"
```

- `.assert` takes one expression, then optionally a comma and one quoted string. `.error` and `.out` take exactly one quoted string.
- The directives are lowercase and may be indented, like every other directive. `.ASSERT` stays an unknown custom pseudo-instruction, as `.ORG` is.
- The message must be on the same line. A trailing comma does not continue it onto the next line. Any token left after the message is an error ([§5](#5-diagnostics)), whether or not the companion "leftover tokens" fix has landed.
- Strings have no escape syntax today, and that does not change. The only sequences treated specially inside a message are the substitutions in §2.4.

### 2.2 When things are evaluated: pass 2, then a single drain

The assembler runs two passes. `define_symbol` writes only on pass 1, so on pass 2 the symbol table is already final, `.phase` included: labels got their phased values on pass 1.

- **Pass 1:** the directives do nothing. **The expression is not evaluated.** This matters for byte-identity. On pass 1, `eval_symbol` registers an unknown name as `SymType::Undefined`, and `.ifdef` / `.ifndef` test `find_symbol(..).is_some()`, which counts those entries. Evaluating `.assert FUTURE == 1` on pass 1 would therefore flip a later `.ifdef FUTURE` and change the output. Not evaluating on pass 1 rules that out.
- **Pass 2:** each directive that is assembled (see §2.3) pushes a `Deferred` record, holding:
  - the expression (cloned), or the message;
  - a snapshot of the loop counters;
  - `address_offset()` and `prg_index`, for `:+` / `:-`;
  - its `Origin` (file, reporting line, loop-counter suffix).

  `.out` text is appended to the output list at this point.
- **Drain:** `run_deferred()` runs once after pass 2 and `validate_ines`. It evaluates each assertion record with a new **side-effect-free** evaluator against the final table, in queue order (pass-2 execution order). `.table` records (plan 019) are drained by the same loop.
  - In `run()` (CLI, non-collect): the drain runs only if pass 2 recorded no errors. A program that didn't assemble has meaningless addresses, and `run()` returns `errors.last()`, which would otherwise hide the real error behind an assertion. The drain stops at the first failure, matching the stop-at-first-hard-error rule.
  - In `diagnostics()` (LSP, collect mode): the drain always runs and reports every failure.
- **Exactly once per build:** pass 1 never queues and the drain runs once, so a failing assertion is reported once by the CLI and once by `diagnose_*`.
- **Final values:** a constant reassigned later in the file is read at its final value, exactly as pass 2 already emits it for `.db X`. The assertion checks what is emitted. A test pins this.
- **No current-PC operand:** the expression grammar has none (no `*` or `$`), so "the stub is at `$8000`" is written against a label. Adding a PC operand is out of scope.

### 2.3 Which lines count as assembled

A directive is queued only when **no** level of the `.if` stack is false.

This is stricter than `if_suppressed()`, which (to match the reference) looks at only the current level and its parent. Today `.if 0 / .if 1 / .if 1 / .db 7` emits `07`; checked on v2.27.0. Under that rule an `.error` written three levels inside a false `.if` would fire. The strict check emits no bytes, so it cannot change output, and it matches what the author wrote. Flagged in [§8](#8-risks-and-open-questions). (Plan 019's `.table` deliberately keeps `if_suppressed()`, because its entries must agree with the bytes `write_byte` emits; the shared `defer` leaves the suppression test to the caller.)

- An `.assert` or `.error` inside a false branch is neither evaluated nor queued.
- A `.for` inside a false branch is skipped entirely (existing behaviour), and so is everything in its body.
- Branch selection is pass 2's. Pass 1 treats a forward reference as `1`, so `.if LATER == 3` can be false on pass 1 and true on pass 2. Deferring means `.error` follows the pass-2 branch, the one that produced the bytes. A test pins this.

### 2.4 Macros and `.for`

- **The expression.** `\1…`, `\#` and `\@` are already substituted by `preprocess::substitute`. `\i`, `\j`, `\k` and pasted names (`row\i`) are already parsed. They resolve against the loop-counter snapshot at drain time.
- **The message.** Substitution happens only in the quoted-string argument of `.assert`, `.error` and `.out`. Strings elsewhere are never rewritten. Real projects have text-script strings such as `"…\2"` and `"\4"` that rely on this, and so does any `.ascii` or custom-op string.
  - In a macro body, `substitute` rewrites `\N`, `\#` and `\@` inside that string:
    - `\N` becomes the canonical text of argument N, with no added parentheses, or `0` if the argument is missing (as in expressions; the companion arg-count fix makes a missing argument an error).
    - `\#` becomes the argument count.
    - `\@` becomes the expansion's unique number.
  - At pass-2 queue time the assembler replaces `\i`, `\j` and `\k` with the counter's decimal value when that many loops are open. Otherwise they are left as literal text, so a path such as `C:\items` in an `.out` survives.
  - Outside a macro body, `\1` in a message is literal text.
- **Where a failure is reported.** For a macro expansion: the outermost call site in the file the user wrote (`Line.file`, which already holds the invoking file, plus `Line.call_line`). Otherwise: the directive's own line. A custom op's `emit_source` keeps today's rule and reports at the directive's line.
- **In a `.for` body:** each iteration queues its own record. Failures are de-duplicated by `(file, line, expression text, unsubstituted message)` and reported once, naming the first failing iteration with the existing `for-iteration-suffix`, as plan 016 Q7 does. Two different assertions on one line, such as in emitted source, stay distinct.

### 2.5 The failure message

Format:

```text
assertion failed: MESSAGE — `EXPR` is 0 (DETAIL)
```

Without a message, the `MESSAGE — ` part is left out.

- **`EXPR` is rendered from the parsed expression** (`Display for Expr`, shared with plan 019), not copied from the source. Tokens don't carry their text or columns, and `Tok::Number` drops its radix. This refines the brief's "source text"; [§11 Q2](#q2-expression-text) covers the alternative. The rendering rules:
  - A nested binary is parenthesised.
  - Numbers 0–9 are printed in decimal. Anything else is `$`-hex, uppercase, padded to 2 or 4 digits, or as wide as needed above `$FFFF`. Negative numbers are printed in decimal.
  - Operators are printed with spaces around them; unary operators (plan 017) are printed directly before their operand. `BANK(x)`, `HIGH(…)`, `LOW(…)`, `:+`, `\i` and `row\i` keep their source spelling.
  - Because of the parenthesisation, the rendering shows how the assembler grouped the expression. `a - b == 2` renders as `a - (b == 2)`, which exposes the right-associative trap.
- **DETAIL**, separated by `; `, omitted when empty:
  1. When the top-level operator is a comparison and at least one side is not a plain number: both sides' values with the operator, e.g. `$0E == $10`.
  2. Each distinct `Symbol`, `PastedSymbol` (shown under its pasted name) and `BANK(name)` in first-appearance order, as `name = VALUE`, capped at 8 entries followed by `, …`.

  Values use the same number format. Anonymous labels are not listed.
- **Example:**

  ``Error in `tables.asm` on line 440: assertion failed: one pointer per KIND_* — `(kind_table - ptr_table) == (2 * (KIND_LAST + 1))` is 0 ($0E == $10; kind_table = $8A1E, ptr_table = $8A10, KIND_LAST = $07)``

### 2.6 `.out`

**Implemented, not removed from the docs.**

- **Output goes to stderr, not stdout.** This refines the brief. With no `-o` the ROM itself is written to stdout (`write_output("-")`), so `.out` on stdout would corrupt a piped ROM. The core never prints; it collects the lines. Stderr is also where warnings and script timings already go. [§11 Q4](#q4-out-destination).
- **Exactly once per build:** lines are recorded on pass 2 only, and only for assembled lines (§2.3).
- **The CLI prints after assembly**, in source and execution order: `.out` lines first, then warnings, then any error. Each line is the message text alone, with no prefix. A `.out` in a `.for` body prints once per iteration with `\i` substituted.
- **When nothing prints:** if pass 1 fails, pass 2 never runs, so no `.out` lines appear. If pass 2 fails partway, the lines collected before the failure are still printed.
- **Never printed by** `diagnostics()` (LSP, whose stdout is JSON-RPC) or by `lint`.

### 2.7 Unresolved and invalid expressions

The drain's evaluator returns `Err(Unresolved)` instead of guessing `1`:
- **A name not in the table, or present only as `Undefined`:** error ``Symbol `x` in `.assert` was not defined``. No failure message is added.
- **`BANK(x)` with unknown `x`:** the same error.
- **`\i`, `\j` or `\k` with too few enclosing loops:** the existing `loop-index-outside` / `loop-index-too-deep` message.
- **`:+` or `:-` with no matching anonymous label:** ``No anonymous label for `:+` in `.assert` ``.
- **Division or modulo by zero:** stays `0`, as in `apply` today.

### 2.8 What is unchanged

- **Bytes, PC, symbols, `.ifdef` results and the list file (`-l`, `--mlist`)** are the same as if these lines were deleted.
- **Every valid program assembles to the same output.** Before this change all three directives were "Unknown custom pseudo-instruction" errors (observed on v2.27.0).
- **Shadowing (the one behaviour change):** a project that mapped a Rhai pseudo-op to `.assert`, `.error` or `.out` in `pseudo.txt` is now shadowed by the built-in, as `.db` would be. The reference corpus has no such mapping.

## 3. Code changes

**SHARED** marks a change to code that other features use. Items marked *(from 019)* are introduced by plan 019, which merges first; if this plan lands first, it adds them with the same shape.

### `crates/nessemble-core/src/lexer.rs` (SHARED)
- *(from 019)* `Token` gains `call_line: Option<u32>`: the outermost macro call line. The lexer emits `None`. A `Token::new(tok, line, file)` helper keeps the churn down (9 constructions in lexer, 5 in preprocess, 8 in tooling tests, 1 in the LSP).
- `pub(crate) fn render_tokens(&[Token]) -> String` produces the canonical text of a macro argument for message substitution, using the §2.5 number rule.

### `crates/nessemble-core/src/preprocess.rs` (SHARED)
- *(from 019)* `expand_macro`: `call = toks[i].call_line.unwrap_or(toks[i].line)`, passed to `substitute`, which stamps it on every emitted token through `mk`. Nested expansions therefore keep the outermost call line.
- `substitute`: track whether the current body line starts (after an `Indent`) with `Tok::Pseudo("assert" | "error" | "out")`. If it does, rewrite `\N`, `\#` and `\@` inside that line's `QuotString`, using `render_tokens(&args[n])`. No other string is touched.

### `crates/nessemble-core/src/ast.rs` (SHARED)
- `Pseudo::Assert(Expr, Option<String>)`, `Pseudo::Error(String)`, `Pseudo::Out(String)`. Message strings are stored unquoted.
- *(from 019)* `Line` gains `call_line: Option<u32>`.
- *(from 019)* `impl fmt::Display for Expr` (§2.5). The match is exhaustive, so it must cover plan 017's `Expr::Unary`; plan 017 merges first, so this is written with those arms from the start.

### `crates/nessemble-core/src/parse.rs` (SHARED statement parser)
- `parse_pseudo` gets arms for `"assert"`, `"error"` and `"out"`, ahead of the `Custom` fallback. They use `parse_expr` and `parse_quoted`, and check what comes after (§5).
- `parse_program` and `parse_program_recovering` copy `call_line` from the statement's first token, as they already do for `from_macro` *(from 019)*.
- `ParseError`s from these three arms use the call line when the token has one.

### `crates/nessemble-core/src/assemble.rs` (SHARED: symbol table, error machinery)
- **The shared hook** *(from 019 §5.2)*: `Origin { file, line, suffix }`, `Deferred { origin, check }`, `fn origin(&self) -> Origin`, `fn defer(&mut self, origin, check)` (no-op unless pass 2; the caller applies suppression), `fn run_deferred(&mut self)`, `fn report_at(&mut self, &Origin, String)` (with de-dup).
- **This plan adds:**
  - `EvalCtx { loops: Vec<i64> /* innermost first */, address: i64, bank: usize }`;
  - `DeferredCheck::Assert { expr, message: Option<String>, template: Option<String>, ctx }` and `DeferredCheck::Error { message }`;
  - `enum Unresolved { Symbol(String), Bank(String), Loop(u8), Local(i32) }`;
  - a widened de-dup key for assertions, `(file, line, expression text, unsubstituted message)` (§2.4), applied inside `report_at` by the `Assert` drain arm;
  - `fn if_any_false(&self) -> bool`, the strict §2.3 test, checked by the `.assert`/`.error`/`.out` arms before calling `defer`;
  - `fn eval_ctx(&self) -> EvalCtx`;
  - `fn eval_final(&self, ctx: &EvalCtx, e: &Expr) -> Result<i64, Unresolved>`: pure, never registers symbols, reuses `Self::apply` and plan 017's `apply_unary`.
- **New `Assembler` fields:** `out_lines: Vec<Diag>`; `cur_call_line: Option<u32>`, set in `run_lines` when `attribute` is set *(from 019 if present)*.
- **`get_symbol_local`** is split into `get_symbol_local_at(direction, offset, bank)`, which takes the captured values so the drain can resolve `:+`. The existing function calls it, so behaviour is unchanged.
- **`exec_pseudo`:**
  - `Assert` → `defer(self.origin(), Assert{…})`, with the message's `\i\j\k` substituted at queue time.
  - `Error` → `defer(self.origin(), Error{…})`.
  - `Out` → push to `out_lines` (pass 2, strict check).
- **`run()`** calls `run_deferred()` after `validate_ines` only if `errors` is still empty. **`diagnostics()`** always calls it *(from 019)*.
- **LSP layout approximation** (§4):
  - a new `CustomOutput::Unmodeled` variant, which writes zero bytes;
  - `approx_bank: Vec<bool>` (PRG and CHR), set when an `Unmodeled` result is written and cleared by `.org` in that bank;
  - `Symbol.approx: bool`, set at definition time for labels in an approximate bank, and for constants whose expression read an approximate symbol (an `eval_taint` flag alongside `eval_unknown`);
  - `eval_final` reports taint. In collect mode, a *failing* assertion that is tainted is dropped.

### `crates/nessemble-core/src/lib.rs`
- `lenient_custom_resolver` returns `CustomOutput::Unmodeled` instead of `Bytes(vec![])`.
- `assemble_impl` also returns the `.out` lines. New public `assemble_with_out` / `assemble_file_with_out` → `Outcome { result: Result<Assembly, AssembleError>, out: Vec<Diag> }`.
- The existing `assemble*` APIs and `AssembleError(pub Diag)` keep their signatures, so the 8 `AssembleError(d)` match sites are untouched.

### `crates/nessemble-core/src/tooling.rs` (SHARED lint engine)
- `is_transparent_directive` gains `.assert`, `.error` and `.out`, so the clobber verifier treats them as not code.
- `is_block_entry` skips lines whose first significant lexeme is one of those directives, exactly as it skips comment-only lines. Without this, an assertion placed between a blank line and a routine label has two effects:
  - the label stops being a block entry, so `require-block-comment`, `require-routine-doc` and the routine verifier lose it;
  - `body_lines` runs on into the next routine, which produces false clobber findings.

  Plan 022 replaces the text-based routine body with an emitted-stream walk, where these lines are invisible anyway because they emit nothing; it must keep the `is_block_entry` rule for block detection.

### Other crates
- `crates/nessemble-isa/src/lib.rs` (SHARED, `DIRECTIVES`): add `(".assert", "fail the build unless an expression holds")`, `(".error", "fail the build with a message")`, `(".out", "print a message while assembling")`.
- `crates/nessemble-cli/src/main.rs`: use the `_out` variants and print `.out` lines to stderr before warnings or the error. Exit code on failure is `RETURN_EPERM`, as for other errors. `--check` runs the assertions too.
- `crates/nessemble-cli/src/coverage.rs`: same stderr printing. An assertion failure fails the run through the existing `AssembleError` arm.
- `crates/nessemble-script/src/lib.rs`: update the test match on `CustomOutput` for the new variant.
- `crates/nessemble-i18n/locales/en-US.ftl`: add a `## Assertions` section (§5).
- `crates/nessemble-wasm/src/lib.rs` (optional parity): an `out()` getter, so a docs "Try it" block can show `.out`.

## 4. Tooling parity

- **`format`:** has its own lexer (`tooling::lex`). The directives format like any other: they follow `indent_directives`, strings stay verbatim (including `\1` and `\i`), and comma spacing is normalised. No code change is expected. Tests pin it and check idempotence.
- **`lint`:** text-based, and it never assembles, so assertion failures don't appear there. `require-block-comment` only looks at labels and never flags these lines. The `is_block_entry` and `is_transparent_directive` changes in §3 keep both block detection and the clobber verifier from noticing the lines at all.
- **`coverage`:** the lines emit no bytes, so they have no source-map spans and count as neither covered nor uncovered.
- **The listing file (`-l`, `--mlist`):** no symbols are added. An undefined name in an assertion is never registered, so the list is the same with or without the assertions.
- **LSP:**
  - Assertion failures and `.error` arrive through `diagnose_*` as ordinary errors. `diagnostic_range` narrows to the first backticked subject if it appears on the line, otherwise the whole line.
  - Completion and hover come from `DIRECTIVES`.
  - `.out` is never shown.
  - **The LSP runs custom ops with the lenient resolver, which emits nothing.** Without the taint rule in §3, every assertion on a label placed after a custom op in the same bank would show a false error in the editor. Assertions on constants (strides, counts) and on labels before the first custom op in a bank are still checked. The same taint applies to plan 019's table checks in the editor; see §8.
- **`.nessemblerc` schema (`nessemble-rc`):** no change.
- **`nessemble reference`:** the new `DIRECTIVES` rows.

## 5. Diagnostics

New `en-US.ftl` keys. All use the existing framing, ``Error in `f.asm` on line N: …``. "Report line" means the call site for a macro expansion and the directive's own line otherwise.

| Key / text | Reported on |
| --- | --- |
| `assert-failed` = ``assertion failed: { $message } — `{ $expr }` is 0{ $detail }`` | report line |
| `assert-failed-bare` = ``assertion failed: `{ $expr }` is 0{ $detail }`` | report line |
| `assert-undefined` = ``Symbol `{ $name }` in `.assert` was not defined`` | report line |
| `assert-no-local` = ``No anonymous label for `{ $dir }` in `.assert` `` | report line |
| existing `loop-index-outside` / `loop-index-too-deep` | report line |
| `error-directive` = `{ $message }` (the user's text) | report line |
| `error-directive-empty` = `` `.error` reached `` | report line |
| `assert-needs-expression` = `` `.assert` needs an expression to check `` (bare or string-only `.assert`) | directive line (parse) |
| `assert-message-not-string` = ``The `.assert` message must be a quoted string`` | directive line (parse) |
| `directive-needs-message` = `` `.{ $directive }` needs a quoted message `` (`.error` / `.out`) | directive line (parse) |
| `directive-extra-args` = `` `.{ $directive }` takes { $form }; remove what follows it `` (`form` is "an expression and an optional message" or "one quoted message") | directive line (parse) |

A failure inside a `.for` body gets the existing `for-iteration-suffix`. `$detail` is `" (…)"` or empty.

`assert-failed` keeps the brief's lowercase "assertion failed:" opening, because it reads as a fixed prefix in front of the user's own message; the other keys follow the capitalized style of `en-US.ftl`, as plans 018 and 019 do.

## 6. Tests (written first)

### `crates/nessemble-core/tests/assertions.rs` (new, in the style of `for_loops.rs`)

**Basics**
1. A passing assertion assembles, and its bytes equal the same source with the line deleted.
2. A failing assertion gives the exact §2.5 text and the right line, with and without a message.
3. A forward reference to a label defined later passes.
4. An unresolved symbol gives `assert-undefined` naming it, with no "assertion failed" message.

**Macros, loops and conditionals**
5. Inside a macro with `\1` in the message: the substituted message, reported at the **call site** line. Also when the macro is defined in an included file, and when it is nested in another macro (outermost call line).
6. Inside `.for` with `\i` in the message: the substituted value, one diagnostic naming the first failing iteration plus the suffix. A passing loop is silent.
7. Inside a false `.if`: an assertion that would fail and one with an undefined symbol are both silent. Also three levels deep under an outer false `.if`, the strict §2.3 check.
8. `.error` in a true `.if` reports its message at its line. In a false `.if` it is silent.
9. `.error` whose condition depends on a forward reference follows the pass-2 branch.

**`.out`**
10. `Outcome.out` has exactly one entry for a single `.out`, and one per iteration in a `.for` with `\i` substituted. None in a false `.if`.

**Once per build**
11. One failing assertion produces exactly one error from `diagnose_source_as` and one from `assemble`.
12. In collect mode several failures are all reported. A failure does not hide a later line's own error.
13. In `run()` mode, when pass 2 has its own error, that error is the one returned.
14. A program with both a failing `.assert` and a failing `.table` check (plan 019) reports both in collect mode, in source order, each once.

**Values and context**
15. `.phase`: `.assert stub == $8000` checks the phased address.
16. `:+` / `:-` in an assertion resolve against the captured PC.
17. A reassigned constant is read at its final value.
18. Pasted names and `BANK()` resolve and appear in the value list.
19. Unary operators (plan 017) render and evaluate: `.assert -1 == ~0`.

**Byte-identity regressions**
20. `.assert FUTURE == 1` before `.ifdef FUTURE` gives the same bytes as without the assertion.
21. `-l` output is the same with and without the assertions, as is `--mlist`.
22. A NES-mode multi-bank program with assertions, `.out` and a false-branch `.error` gives the same bytes as with those lines removed.

**Parse errors**
23. Each row of §5.

**Message strings**
24. `"\2"` in `.ascii` or a custom-op string inside a macro body is **not** substituted.

**LSP layout approximation**
25. With `lenient_custom_resolver`, a label assertion after an unmodeled custom op is not reported. After an `.org` in the same bank it is checked again. A constant assertion is always checked.

### Other test locations
- **`parse.rs` unit tests:** the AST for each form; `call_line` propagation.
- **`ast.rs` unit tests:** `Display for Expr`: parenthesisation, number format, unary, `BANK` / `HIGH` / `LOW` / `:+` / `\i` / `row\i`.
- **`tooling.rs` unit tests:**
  - `format` keeps the lines, under both `indent_directives` settings, idempotently;
  - `is_transparent_directive(".assert")`;
  - a routine with an assertion inside is still "fully understood";
  - with an assertion between a blank line and a label, the label is still a block entry, `body_lines` ends correctly, and `require-block-comment` behaves as if the assertion were absent.
- **`crates/nessemble-cli/tests/cli.rs`:**
  - `.out` goes to stderr and stdout holds only the ROM, byte-identical;
  - a failing assertion exits 1 with one `Error in` line;
  - `--check` reports it.
- **LSP unit test:** an assertion failure is published as an error diagnostic.
- **Existing goldens:** `tests/corpus` must stay unchanged.
- **Manual check before merge:** the maintainer confirms the reference corpus still assembles byte-for-byte, then adds a few of its invariants as assertions locally and confirms the hashes don't change.

## 7. Docs and changeset

- **`docs/src/syntax.md`:**
  - add `.assert` and `.error` rows to the pseudo-instruction table;
  - add `### .assert`, `### .error` and `### .out` sections in alphabetical order (the existing `.out` row links to an `#out` anchor that doesn't exist);
  - each section gets usage, an example, the failure text, and the rules on macros, loops and `.if`;
  - note that the rendered expression shows the assembler's grouping, so write the parentheses.
- **`docs/src/usage.md`:** `.out` writes to stderr; an assertion failure is an ordinary error with exit 1; `--check` runs assertions.
- **`nessemble reference`:** via `DIRECTIVES`.
- **Changeset** (no CHANGELOG edit), via `cargo run -p xtask -- changeset add minor "…"`. Additive; the shadowing note is the only behaviour change:

  > Add `.assert EXPR[, "message"]` and `.error "message"` to fail the build when an invariant breaks, and make the documented `.out "message"` print to stderr while assembling. A `pseudo.txt` mapping named `.assert`, `.error` or `.out` is now shadowed by the built-in.

## 8. Risks and open questions

- **Macro diagnostics today report the wrong place.** The body's line number is paired with the invoking file. Checked on v2.27.0: an undefined argument reports body line 3, not call line 7. This plan moves only `.assert`, `.error` and `.out` to the call site (plan 019 does the same for table checks). Moving every macro diagnostic to the call site is out of scope, but now cheap with `call_line`.
- **The CLI reports one error** (`errors.last()`). "Report every failing assertion" therefore holds only in the LSP and `diagnose_*`. Printing all errors from the CLI is a separate change.
- **`.out` goes to stderr, not stdout as the brief asked** (settled). It also prints nothing when pass 1 fails.
- **Strict `.if` check vs. the reference quirk** (§2.3, settled: strict). A line three levels deep under a false `.if` is assembled, yet an `.assert` there is skipped.
- **LSP taint rule:**
  - It hides some real failures in the editor; the CLI build is still authoritative.
  - It adds `Symbol.approx` to the shared symbol table.
  - It ignores `.if` conditions that depend on approximate labels, so an `.error` may still show falsely in the editor.
  - Plan 019's table checks have the same exposure in the editor (entry sizes after an unmodeled custom op read as 0). The taint flag should cover them too; agree that when both plans are implemented.
  - Simpler options: drop *every* label-dependent failure in lenient mode, or show them as warnings.
- **Constants read at their final value** (§2.2) can surprise someone asserting about a value mid-file. It is consistent with emitted bytes. Document it.
- **Plan 016's `.fill` pass skew** (labels can differ between passes) is not touched. Assertions read pass-1 label values, which are the values the pass-2 bytes use.
- **Interactions:**
  - The companion leftover-token and arg-count errors overlap `directive-extra-args` and the "missing argument → `0`" rule. Whichever lands second should keep one message.
  - `.table` records are queued through the same `defer`, never on pass 1, with its own suppression rule.
- **Out of scope:**
  - `.out` with formatted expression values (e.g. printing a stride);
  - a current-PC operand;
  - `.warning`;
  - a lint rule for unchecked invariants;
  - showing `.out` in the LSP;
  - warning when `pseudo.txt` maps a built-in name.

## 9. Phased plan

- **Phase 1:** everything in §3 except the wasm getter, including the LSP taint rule and the lint transparency. Without those two, the first project to adopt assertions gets false editor errors and false clobber findings. All §6 tests and the §7 docs. Changeset `minor`.
- **Phase 2 (optional):** the wasm `out()` getter and a docs "Try it" block for `.out`.

## 10. Non-goals

- Assertions that run at runtime (emulator).
- Static-assert evaluation on pass 1.
- Escape sequences in strings.
- A configurable `.out` destination.

## 11. Decisions (proposed)

### Q1. Where assertions run
- **(a) Queued on pass 2, with a context snapshot; drained once after pass 2, on the queue shared with `.table`.** Chosen.
- (b) Evaluated in place on pass 2. Rejected: no shared hook, and the ordering with `run()`'s `errors.last()` is worse.
- (c) Evaluated on every pass. Rejected: duplicate reports, and pass-1 evaluation registers undefined names, which flips `.ifdef`.

### Q2. Expression text
- **(a) Rendered from the AST, fully parenthesised.** Chosen. One code path, and it exposes grouping mistakes.
- (b) The exact source text, through a preprocessor side table keyed by (file, line) and sliced with `tooling::lex`. Better for directly written lines, but it needs a fallback inside macros and emitted source.

### Q3. Macro argument text in messages
- **(a) Canonical token rendering.** Chosen.
- (b) Raw call-line text when the call line itself is not from a macro. Revisit if `$`-hex rendering of decimal arguments annoys.

### Q4. `.out` destination
- **(a) stderr, printed by the CLI.** Chosen.
- (b) stdout. Rejected: it corrupts a ROM written to stdout and the LSP's protocol stream.
- (c) Remove `.out` from the docs. Rejected: it is cheap and useful with `.if`.

### Q5. Failures in loops
- **(a) One per (line, assertion), naming the first failing iteration.** Chosen, matching plan 016 Q7.

### Critical files for implementation
- `crates/nessemble-core/src/assemble.rs`
- `crates/nessemble-core/src/preprocess.rs`
- `crates/nessemble-core/src/parse.rs`
- `crates/nessemble-core/src/ast.rs`
- `crates/nessemble-core/src/tooling.rs`
- `crates/nessemble-core/src/lib.rs`
- `crates/nessemble-cli/src/main.rs`
- `crates/nessemble-i18n/locales/en-US.ftl`

---

## Decisions settled with the maintainer

1. `.out` prints to **stderr** (Q4).
2. `.assert` and `.error` use the **strict** `.if` test: they never run in any branch written as false, however deeply nested. `.table` keeps `if_suppressed()` (§2.3).
