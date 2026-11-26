# nessemble-rs: Diagnostics for Source the Assembler Accepted Silently

> Status: **Shipped.** Deviations found while building are recorded in
> [§8](#8-as-built).

---

## 1. Goal

Five shapes of source assembled with exit code 0 and no diagnostic, each
producing a ROM that differs from what its author wrote:

| # | Source | What v2.27.0 did |
|---|---|---|
| 1 | `foo: .db 1` (any label with content on its line) | dropped the rest of the line; `foo` became a constant `= 1` |
| 2 | `.db 1 2 3`, `LDA $10 garbage`, `QQ = 1 2` | ignored the leftover tokens |
| 3 | `.macro MAC, $05` for a body using `\1`–`\3` | missing arguments expanded to `0`; extras were dropped |
| 4 | a label, constant or macro defined twice | last definition won, silently |
| 5 | `.db 300`, `LDA #$1FF`, `STA <$1234`, `JMP $12345` | narrowed to fit, silently |

In a byte-exact disassembly the first sign was a changed hash with no clue
where to look; in a project with no reference hash there was no sign at all.

The fix is diagnostics only. **No valid program's bytes or list file change.**

## 2. Severity

1–3 are **always errors**: the source cannot mean what it says. 4 and 5 are
**warnings by default**, each under a stable rule id a project can set to
`off`, `warn` or `error`:

| Rule id | Reports |
|---|---|
| `duplicate-symbol` | a redefinition with a different value or kind, or any macro redefinition |
| `duplicate-symbol-same-value` | a redefinition with the same value and kind (an includer and the included file placing one label at one address) |
| `value-out-of-range` | a value narrowed to fit a field it does not fit |

Two duplicate rules, because one case is harmless and real projects contain it:
a project can switch the harmless one off, or raise only the dangerous one to an
error, without losing either.

Changing a severity never changes what a *successful* assembly emits:
last-definition-wins stays, and an out-of-range value still emits its narrowed
bytes. Promoting a rule to `error` is the only way it changes whether something
assembles.

## 3. Messages

Errors and warnings share the existing framing on stderr, and the rule id ends
a rule diagnostic:

```text
Error in `t.asm` on line 1: a label must be on a line of its own; move `.db 1` to the next line
Error in `t.asm` on line 1: unexpected `2` after `.db 1`; separate values with commas
Error in `t.asm` on line 4: macro `MAC` takes 3 arguments (it uses \1-\3), but this call passes 1
  note: `MAC` is defined at t.asm line 1
Warning in `t.asm` on line 3: `foo` is already defined at t.asm line 1 (as a label at $0000); this definition replaces it  [duplicate-symbol]
Warning in `t.asm` on line 1: value 300 ($012C) does not fit in a byte; emitting $2C  [value-out-of-range]
```

A diagnostic is reported **where the mistake was written**:

- A leftover-token or label error quotes the user's own text (from token
  spans), not a re-rendering of the tokens.
- A duplicate is reported at the second definition and names the first.
- An arity error is reported at the call, with a note naming the definition.
- A diagnostic on a macro-expanded line is reported at the **macro body line**,
  in the file holding the body, with one `note: in expansion of macro …` per
  enclosing call. A nested call's arity error names its own site and the chain.

The comma hint is given for `.db`/`.dw`/`.byte`/`.word`/`.lobytes`/`.hibytes`
when the unexpected token could start a value. An unindented instruction
(`LDA #1` at column 0, which the language reads as a constant definition) gets
an indentation hint instead.

## 4. Where each check lives

- **1, 2 — the parser's one end-of-statement check.** After every statement,
  anything but end-of-line is an error (`Parser::expect_end`), so no directive
  needs its own check. A `name:` (or `name\i:`, or `:`) followed by anything is
  parsed as a label and then rejected there with the label message; an indented
  `BEQ :+` / `BNE :-` is still an instruction. Multi-line continuations are
  consumed inside the list parsers before the check runs, and comments are not
  tokens, so neither trips it.
- **3 — the preprocessor**, which expands macros. A macro's arity is the highest
  `\N` (N ≥ 1) in its body, counting one inside an `.if`. A body that reads `\#`
  is variadic and is not checked. `\@` and `\i`/`\j`/`\k` are not parameters.
- **4 — the assembler's symbol definition**, on pass 1 only (definitions are
  recorded only on pass 1, so a pass-2 re-visit is never a redefinition). Macro
  redefinitions are found by the preprocessor and handed to the assembler, which
  reports every rule diagnostic at its configured severity.
- **5 — the assembler's encoders**, on the final pass only (every value is known)
  and never in a false conditional, checking the exact value that is encoded
  (after `.phase`).

| Field | Accepted without a warning |
|---|---|
| byte (`.db`, `.byte`, a `.fill` value, `#imm`, an `.ascii` character plus offset) | `-128 … 255` |
| word (`.dw`, `.word`) | `-32768 … 65535` |
| zero-page operand (`<x`, `[x, X]`, `[x], Y`) | `0 … 255` |
| absolute operand (including `JMP [x]`) | `0 … 65535` |

`LOW()`, `HIGH()`, `.lobytes`, `.hibytes` and explicit masks are the way to
narrow on purpose; their *result* is always in range, so they never warn.

## 5. Not a redefinition

- names made unique per expansion (`wait_\@:`) or per iteration (`row\i:`);
- anonymous `:` labels;
- a forward reference registering its placeholder;
- **the same definition seen again** — the same line of the same file on disk,
  from the same expansion, with the same value: a file `.include`d twice (by two
  spellings, say `@/defs.asm` and `defs.asm`) re-runs its definitions but defines
  each once;
- a constant re-seen in another `.prg` bank: a constant's bank is only where it
  was written, unlike a label's (`BANK(name)`), so it is not part of its value.

Each source location reports a rule once — whichever pass, loop iteration or
macro expansion reaches it again.

## 6. Configuration

- **Per project**, in `.nessemblerc`, beside `lint`:

  ```json
  { "assemble": { "rules": { "duplicate-symbol": "error", "duplicate-symbol-same-value": "off" } } }
  ```

  Unknown rules, levels and keys are errors, like the rest of the file. The
  block is project-wide: it is not accepted inside `overrides`.
- **Per run**: `-W <rule>=<off|warn|error>` (repeatable, overriding the file) and
  `--warnings-as-errors`, which raises every assembler warning — rule warnings
  and the built-in ones such as bank overflow — to an error. A rule set to `off`
  stays off.

The CLI's assemble mode, `coverage`, and the language server all read the same
`assemble.rules`. The language server publishes the new diagnostics with the
rule id as the diagnostic `code` and the notes in the message.

## 7. Tooling agreement

`lint` and `format` have their own light lexer but report no syntax errors, so
there is nothing for them to disagree about — with one exception: the formatter
used to indent `foo: .db 1` as if it were an instruction. It now leaves a
`name: <content>` line at column 0, so formatting never moves a label (the line
is the same error before and after). `nessemble reference diagnostics` lists the
rules offline.

## 8. As built

- **`Diag` grew `rule` and `notes`** and moved to its own module with the rule
  types. Every stage builds diagnostics through one locator that maps a
  macro-expanded line to its body file and adds the expansion chain.
- **Tokens carry a span and an expansion id.** A substituted `\N` (and the
  parentheses wrapped around it) takes the span of the `\N`, so an error quotes
  the body as written: ``unexpected `2` after `.db \1` ``.
- **Diagnostics on macro-expanded lines name the body's file.** Previously they
  paired the body's line number with the *calling* file's name, which was wrong
  whenever the macro was defined in another file. Only the reported location
  changed; where a body's filename directives resolve is untouched.
- **Assembling reads `.nessemblerc`.** A malformed config now fails a build with
  its error instead of being ignored.
- **The same-site and constant-bank exemptions in §5** came out of running a
  large real-world disassembly (~51,000 lines, heavy use of macros, `.for`,
  `.phase`, `@/` includes and custom pseudo-instructions): it reports exactly its
  20 harmless same-value pairs and nothing else, and its ROM and list file are
  byte-identical to v2.27.0's.
- **Two documentation examples were wrong** and the new checks found them: a
  `BNE label_\@:` with a stray trailing colon, and a `.dw $45678` that does not
  fit a word.

## 9. Non-goals

Scoped `@` labels, unary minus, operator precedence, `label: instruction`
syntax, changing which duplicate definition wins (or whether a use before a
constant's redefinition sees the earlier value), and `.org` stepping back over
emitted bytes. Each changes the language or is a separate issue.
