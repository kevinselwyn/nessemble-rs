# nessemble-rs: A Plan for Scoped `@local` Labels

> Status: **Implemented** (both phases: assembler, formatter and linter; then
> the language server). The *proposed* points and refinements shipped as
> written; "As built" at the end lists where the implementation differs in
> detail. Builds on the macro expansion tracking shipped by
> [plan 017](017-assembler-diagnostics.md).

---

## 1. Summary

An `@name` label becomes local to the nearest global label above it in the same file. You reference it as `@name` from inside that scope, forwards or backwards, and as `parent@name` from anywhere. Four kinds of code get private locals: an `.include`d file starts with no scope, a macro expansion's own `@` definitions belong to that expansion alone, a custom pseudo-op's emitted source gets the same treatment, and anonymous labels are untouched. Resolution happens in the assembler, at run time, against a per-file "current scope". That way pasted (`row\i:`) and `.if`-suppressed labels behave correctly. The lexer gains one rule, `ident@ident`, so qualified names are single tokens, and every tool that reads source (format, lint, LSP, listing) treats locals as labels that never open a block.

## 2. Semantics

### 2.1 Names

- **Local name:** `@` followed by `[A-Za-z0-9_]+`. This is exactly what the lexer accepts today (`@1` is valid).
- **Qualified name:** `<global>@<local>`, e.g. `draw_sprite@clip`. The global part is any ordinary identifier, so a pasted global works too (`row3@x`). Only one `@` segment is allowed. A qualified name can **only be referenced**. Defining one (`p1@a:`, `p1@a = 3`, `p1@a .rs 1`) is an error (§5).
- **Local names are labels only** *(refinement)*. `@x = expr`, a bare `@x` at column 0 (which today is a constant equal to 1) and `@x .rs n` are errors. Constants have no scope rule in the brief, and supporting them doubles the surface (redefinition, `.for` reassignment, plan 017's `duplicate-symbol` warning). Local constants are listed in §8.

### 2.2 What opens a scope (decided, with three clarifications)

- Any executed label definition that isn't `@`-prefixed or anonymous opens a scope, including `name\i:` (pasted per iteration).
- **Labels generated with `\@` (`name\@:` in a macro body) do not open a scope** *(settled with the maintainer; reverses the brief's original rule)*. They are expansion-private names, and letting them open scopes would make the caller's later `@done` resolve under the macro's label. Detection: a label whose name came from pasting `\@`, i.e. any `Name` with a `\@` segment, marked by the preprocessor on the token (`Token.expansion != 0` together with a pasted `\@` part).
- **A label inside a false `.if` opens nothing**, exactly as it defines nothing.
- **`.rs` reservations do not open a scope** *(refinement)*. They are variables, not code, even though the listing files them under `[labels]`.
- Constants, `.org`, `.phase`/`.dephase`, `.segment`, `.prg`/`.chr`, `.enum` and anonymous labels neither open nor close a scope.

### 2.3 Resolution

The assembler keeps `scopes[file_id]`, the name of the current global label for each file, reset at the start of each pass. Scope tracking runs on **both** passes; symbol definition stays pass-1-only, as today.

- **Definition `@x`:** key `scope@x`. With no scope, error `local-no-scope`.
- **Reference `@x`:** key `scope@x`, resolved forwards or backwards. With no scope, `local-no-scope`. Undefined on pass 2 gives `local-undefined`, plus a hint if another scope defines `@x`.
- **Reference `q@x`:** key `q@x`. If it is undefined on pass 2, the message is `local-not-in-parent` when `q` is a defined symbol of any kind, and the existing `symbol-not-defined` for `q` otherwise.
- **Everything that names a symbol uses the same resolver:** operands, data, `.for` bounds, custom pseudo-op integer arguments, `->` chains, `BANK(@x)`, `.ifdef @x`/`.ifndef @x`, `.assert` expressions (plan 022's `eval_final` resolves through it too), and `.table` tag keys (plan 020). `.ifdef` keeps its existing quirk: an entry registered by an earlier reference counts as defined.
- **Pasting runs before qualifying.** `@x\i:` pastes to `@x3`, then qualifies to `p1@x3`. `p1@x\i` pastes to `p1@x3`.
- **Locals inside `.phase` get the phased address,** like any label. The scope is unaffected by the phase.
- **Labels inside a `.table` (plan 020)** follow the same rule; a table's own label opens a scope and locals inside it are fine.

### 2.4 Include boundaries (proposed, kept)

Each include gets its own file id from the preprocessor, and a file included twice gets two ids. A new file id starts with no scope, and the includer's `scopes[id]` is untouched, so its scope resumes after the include. Locals never cross files. `.inestrn` behaves the same way.

### 2.5 Macros (proposed, refined)

- **The preprocessor makes a body's own definitions private.** Before substituting arguments, `substitute` scans the macro body for local *definitions* at statement start: `@x:` and `@x\i:`. Each such name, and every body token spelled the same, is rewritten to a private key `\@<id>@x` (`id` is the existing per-invocation `unique`). The prefix form is chosen so that pasting appends cleanly: `\@7@x` + `3` = `\@7@x3`.
- **A body reference that isn't one of the body's own definitions stays `@x`.** It resolves in the caller's scope (the expansion's tokens carry the caller's file id).
- **Argument tokens are never rewritten.** `.macro jmp_to, @done` refers to the caller's `@done`. Passing a private local as an argument to a nested macro works, because the argument token is already rewritten.
- **A nested macro's body doesn't see its caller-expansion's privates.** It sees its own, then the label scope. Pass the local as an argument instead.
- **Privates need no enclosing label.** They are unique per expansion.
- **Privates can't be qualified from outside the expansion.**
- **Privates are flagged `from_macro`,** so `-l` hides them unless `--mlist` is given.
- **`.for` interaction:** a macro whose body defines `@x` and is invoked inside a `.for` body is one expansion, so it collides across iterations. This matches plan 016 §3 for `label\@`. It is reported with the existing `label-in-for`, using the display name `@x`.
- **A body's `name\@:` does not open a scope** (§2.2), so a caller's `@done` after the macro call still resolves in the caller's scope.

### 2.6 Custom pseudo-op emitted source *(refinement; not in the brief's list)*

- Emitted source is expanded inline at the call site, so it shares the caller's file scope. Its global labels open scopes, per the decided rule.
- An `@x` defined in emitted source is private to that invocation, mirroring macros. A script called twice in one routine must not collide.
- The private id is keyed deterministically by (enclosing emitted id, file, line, index of the directive in the enclosing emitted source). That makes it identical on pass 1 and pass 2 even if a `.if` takes a different branch.
- The same private token rewrite (§3.3) runs on the lexed tokens before `parse::parse`.

This choice is flagged in §8 (R4).

### 2.7 `.for` bodies (proposed, refined)

- Plain `@x:` in a body is rejected at parse time by the existing `label-in-for` check. Its text already suggests `@x\i:`.
- **Refinement:** if the same loop frame has already defined a pasted global label (`row\i:`), each iteration has its own scope and `@x:` is allowed. Inner frames don't inherit the flag.
- At run time (emitted source inside a loop), `@`-names skip the per-loop `label-in-for` check in `exec_stmt`, and duplicate detection (`local-duplicate`) is the guard. Private keys keep `label-in-for`.

### 2.8 Unchanged

- Anonymous labels (`:`, `:+`, `:-`), including counting across a local label.
- `name\@:`, `name\i:`, every global label, labels in `.phase`.
- `@/` paths: include targets are read from raw text, and other path arguments are `QuotString`.
- `'@'` char literals, `"@"` in strings, and `@nessemble-*` comments, which the lexer strips.

### 2.9 Duplicate detection (contract with plan 017's `duplicate-symbol` rules)

- Symbol keys are fully qualified, so `p1@x` and `p2@x` never compare equal.
- A local defined twice under one key is the **hard error** `local-duplicate`, not the warning.
- Plan 017's `duplicate-symbol` and `duplicate-symbol-same-value` warnings apply only to global keys (names without `@`) and never fire for locals or privates; plan 017's same-site exemption (one line of one file seen again, e.g. a file included twice) applies before `local-duplicate` too.
- A global label defined twice reopens the same scope, so locals under both copies share keys and collide. That is reported by `local-duplicate`, naming the other file if needed.

## 3. Code changes

(**[shared]** = shared code.)

### 3.1 Lexer, `crates/nessemble-core/src/lexer.rs` [shared]

Exactly one rule changes: **rule 14** (identifier/pseudo/keyword) in `lex_number_or_word`.

- When the matched word starts with `[A-Za-z_]` (not `.` or `@`), the next byte is `@`, and the byte after that is `[A-Za-z0-9_]`, the match extends over `@[A-Za-z0-9_]+`, once.
- The result is `Tok::Text("p1@x")`. `classify_word` is unchanged.
- `@x@y` still lexes as two words, `p1 @x` as two tokens, `p1@` as `Text`+`CharReg('@')`, and `@/` as before.
- Side effect: a hex-suffix word glued to `@` (`A5h@x`) now lexes as one identifier. Today it is `Number` + ignored garbage.
- **No operator rule changes.** Plan 018 adds `Tok::Tilde`/`Tok::Bang` and two arms in the one-character table of `lex_operator`; it does not touch rule 14, `is_ident_char` or `classify_word`. The two features touch disjoint code and merge in either order.
- No new token field: plan 017's `Token.expansion` already identifies which expansion a token came from, and is what the private id is built from.

### 3.2 AST and parser, `ast.rs`, `parse.rs` [shared statement parser; **no expression-parser change**]

- `Line.expansion` (plan 017) is used as is.
- `parse_stmt` and `parse_pasted_def`:
  - Reject a qualified name in a label, constant or `.rs` definition (`local-def-qualified`).
  - Reject an `@`/private name as a constant, bare constant or `.rs` (`local-not-label`).
- `validate_loops` gets the per-frame "pasted global label seen" flag (§2.7). It displays private names as `@x`.
- `parse_primary` is untouched: a qualified name is already a `Tok::Text`.

### 3.3 New module, `crates/nessemble-core/src/locals.rs` [shared]

- `enum LocalName<'a> { Global(&str), Local(&str), Qualified { parent, local }, Private { id, local } }` and `classify(&str)`.
- `display(&str)` turns `\@7@x` into `@x`.
- `privatize(tokens, id, defs)`, the token rewrite used by `preprocess::substitute` and `Assembler::exec_emitted_source`.
- `local_defs(tokens) -> HashSet<String>`, which finds statement-initial `Text(@x) Colon` / `Text(@x) LoopIndex Colon`.
- `pub` helpers re-exported through `tooling` for the LSP: `is_local_name`, `split_qualified`.

### 3.4 Preprocessor, `preprocess.rs` [shared]

- `substitute` runs `privatize` on body tokens (not argument tokens), using the expansion's id; `mk` already stamps `expansion` (plan 017).
- `Preprocessed.expansions` (plan 017: each expansion's macro name, definition site, call site and parent) is used as is.
- Nothing changes for `.include`.

### 3.5 Assembler and symbol table, `assemble.rs` [shared symbol table]

- **`Symbol`:** gains `parent: Option<String>` (scope at definition; used for privates' display and for hints). The definition site for "(line N)" is plan 017's existing per-symbol site.
- **`Assembler`:**
  - gains `scopes: Vec<Option<String>>`, reset in `reset_state`;
  - gains `emitted_ids: HashMap<(u32, u32, u32, usize), u32>`, which persists across passes.
- **`resolve_def` / `resolve_ref`:** new functions that map a raw name to a key, or report `local-no-scope`. Plan 022's `eval_final` calls `resolve_ref` (pure; it never registers).
- **Symbol paths that change:**
  - `exec_stmt`: labels and pasted labels define, then update `scopes[cur_file]` if global and not suppressed. Skip `label-in-for` for `@` names.
  - `define_symbol`: duplicate check for local and private keys on pass 1.
  - `add_pasted_symbol`: check the qualified key.
  - `eval_symbol`: new messages for local, qualified and private keys, and the hint search.
  - `eval_bank`, and `Pseudo::Ifdef`/`Ifndef` in `exec_pseudo`.
- **`exec_emitted_source`:** privatize before parsing.
- **Diagnostics** go through plan 017's `diagnostic::locate` with `cur_expansion`, like every other assembler diagnostic: a macro-expanded line is reported at its body line, in the body's file, with `in expansion of macro …` notes naming the calls.
- **Hint choice:** the defined non-private key ending in `@x` that is in the same file at the nearest line above; otherwise the first one defined.

### 3.6 Listing [shared listing writer], `assemble.rs::list_symbols`

- Ordinary locals list as their key, `p1@loc`.
- Privates list as `{parent}@x\@{id}` (or `@x\@{id}` with no parent), and only with `--mlist`, since they are `from_macro`.
- `lib.rs::render_list_file` is unchanged.

### 3.7 i18n, `crates/nessemble-i18n/locales/en-US.ftl`

New ids per §5.

### 3.8 Tooling lexer, format and lint, `tooling.rs` [shared: lossless lexer, lint engine]

- **`lex`:** the `Ident` branch gets the same `ident@ident` extension (when the identifier starts with a letter or `_`). Directives are unaffected.
- **`routine_effects_inner`:**
  - A local label line (`@x:`, `@x\i:`) is transparent. Today it falls into the mnemonic check and marks the body "unknown".
  - In `apply_call`, a `JMP`/`JSR` target that is a local defined within the routine's `body_lines` is transparent. Any other local or qualified target is "unknown".
  - Plan 023 replaces this text walk with an emitted-stream walk. Until then these two fixes are needed; after it, plan 023's walk follows branches to locals like any target and attributes them to their parent.
- **`binding_target` / `SignatureProblemKind`:** add `LocalTarget` so an annotation directly above `@x:` gets a precise message (§5), not "not followed by a label".
- **Already correct; pin with tests:** `block_label` already returns `None` for `@` names, so locals never open a block and `body_lines` never ends at one. This is the contract plan 023 relies on. `call_targets` becomes correct automatically: `JSR p1@x` no longer marks `p1` as called.

### 3.9 LSP, `crates/nessemble-lsp/src/lib.rs` [shared]

The LSP is text-based and has its own scanner (`located_lexemes`/`definitions` over `tooling::lex`).

- New `local_scope_at(text, line)`: the nearest preceding non-`@`, non-anonymous label definition (including pasted ones), and its line range up to the next one. If the line is inside a `.macrodef`…`.endm` body that defines the name, the body is the scope.
- **`goto_definition` / `definition_location`:**
  - `@x` goes to its definition in the scope range.
  - `q@x` uses the existing project-wide lookup for `q`, then finds `@x:` in `q`'s range in that file.
- **`references`:** the `@x` tokens in the scope range, plus `scope@x` tokens in every open asm document.
- **`rename`:**
  - A local can only be renamed to a valid local (`@` + `[A-Za-z0-9_]+`, via a new `is_local_identifier`). It edits in-scope `@x` tokens and the suffix range of `scope@x` tokens.
  - Renaming a global `q` also edits the prefix range of `q@x` tokens.
- **`complete`:**
  - When the word prefix starts with `@`, offer the current scope's locals (and the body's own, inside a macrodef).
  - Drop qualified `p1@x` names from the general symbol list.
- **`hover` / `ident_hover`:** qualify via the scope and look up `scope@x` in `doc.symbols`.
- **`document_symbols`:** locals nest as `children` of their parent, with detail "local label".
- **`subroutine_ranges`:** skips locals.
- **Already reject `@`; pin with tests:** `documentable_routine_indent` and `document_routine_actions`.

### 3.10 No change

- `.nessemblerc` schema (`nessemble-rc`): no new keys, and lint `ignore` never sees locals.
- `nessemble-cli` (flags unchanged), `nessemble-wasm` (rebuild only), the VS Code extension (semantic tokens), and coverage (§4).

## 4. Tooling parity

| Tool | Own parser? | What it needs |
|---|---|---|
| `format` | Yes (`tooling::lex`) | Lexer extension (§3.8). `is_named_label` already pins `@x:` to column 0 and whitespace is preserved, so `JMP p1@x` survives. The test must show it is idempotent. Known cosmetic: `blank_line_after_return` inserts a blank line before an `@x:` that follows `RTS` (§8, R8). |
| `lint` | Yes | §3.8: locals never open a block, annotations can't bind to them, and `require-block-comment` and `require-routine-doc` never see them (already true via `block_label`; pinned). Fix the effects walk so `@x:` lines and intra-routine `JMP @x` are transparent. |
| `coverage` | No (source map + comment directives) | Coverage is line-based (`SourceMap`, LCOV `DA:` and JSON lines). No report names a label, so there is nothing to attribute. Contract for future per-routine reports: attribute a local's lines to its `parent`. Pinned by a test (§6). |
| LSP | Yes (text scanner) | §3.9. |
| Listing `-l` / `--mlist` | No | §3.6. |
| Emitted source | Uses the core parser | §2.6. |
| `nessemble reference` | No | No directive is added, so no change. |

## 5. Diagnostics

The text is refined to house style: an initial capital where a sentence starts, and backticks around names so the LSP's `quoted_subject` narrows the range to the `@x` token. The proposed table's lowercase wording is replaced with these. All errors use ``Error in `f.asm` on line N: …``.

| id | Text | Line reported |
|---|---|---|
| `local-no-scope` | ``Local label `@x` has no enclosing label`` | the `@x` definition or reference |
| `local-duplicate` | ``` `@x` is already defined in `p1` (line N) ``` (or ``(`other.asm` line N)`` when in another file) | the second definition |
| `local-duplicate-macro` | ``` `@x` is already defined in macro `wait` (line N) ``` | the second definition's body line, with expansion notes |
| `local-undefined` | ``` `@x` is not defined in `p2` ``` | the reference |
| `local-undefined-hint` | ``` `@x` is not defined in `p2`; did you mean `p1@x`? ``` | the reference |
| `local-not-in-parent` | ``` `p1` has no local label `@x` ``` | the reference |
| `local-def-qualified` | ``` `p1@x` is a qualified name and can only be referenced; define it as `@x:` under `p1` ``` | the definition (parse error) |
| `local-not-label` | ``` `@x` is a local name, so it can only be a label; give the constant a global name ``` | the definition (parse error) |
| `label-in-for` (existing) | unchanged text, name shown as `@x` | the label |
| lint `invalid-routine-signature` (`LocalTarget`) | ``routine annotation `@nessemble-clobbers` is followed by local label `@x`; annotations bind only to global labels`` | the annotation |

- An undefined qualified ref whose parent doesn't exist reuses ``Symbol `nope` was not defined``.
- No new warnings.
- Per-iteration dedupe and the `.for` suffix apply as usual.
- `local-no-scope` on a definition fires on pass 1. The reference errors fire on pass 2, like `symbol-not-defined`.

## 6. Tests (written first)

### 6.1 New integration file, `crates/nessemble-core/tests/local_labels.rs`

Same harness as `for_loops.rs`: `bytes`/`error`/`sym`, plus temporary directories for multi-file cases.

**Bytes:**
1. The brief's example (`p1: @loc: BNE @loc p2: @loc: BNE @loc`) gives `d0 fe d0 fe`. Symbols `p1@loc` = 0 and `p2@loc` = 2.
2. Backward reference: `p1:`/`@top:`/`NOP`/`BNE @top` gives `ea d0 fd`.
3. Forward reference: `p1:`/`BEQ @done`/`NOP`/`@done:`/`RTS` gives `f0 01 ea 60`.
4. Qualified, same file: `p1:`/`@a:`/`RTS`/`p2:`/`JMP p1@a` gives `60 4c 00 00`.
5. Qualified from another file, via `assemble_file`: `lib.asm` defines `draw_sprite:`/`@clip:`/`RTS`, and `main.asm` does `JMP draw_sprite@clip` both before and after `.include "lib.asm"`.
6. Include resumes the scope: main is `p1:`/`BNE @loc`/`.include "inc.asm"`/`@loc:`/`RTS`, and inc.asm is `tbl:`/`@loc:`/`.db 1`. Expect `d0 01 01 60`, no duplicate error, and symbols `tbl@loc` and `p1@loc`.
7. Macro expanded twice in one scope (`.macrodef wait` / `@spin:` / `BIT $2002` / `BPL @spin`): `2c 02 20 10 fb 2c 02 20 10 fb`.
8. A macro body references the caller's local (`BEQ @out` in the body, `@out:` after the call): `f0 01 ea 60`.
9. A local passed as a macro argument (`.macro jmp_to, @x`): `4c 00 00`. Plus a nested macro receiving a private as its argument.
10. `@x\i` in `.for`: `p1:`/`.for 0, 1`/`@x\i:`/`NOP`/`.endfor`/`JMP @x1` gives `ea ea 4c 01 00`, with symbols `p1@x0` and `p1@x1`.
11. Pasted scope in `.for`: `row\i:`/`@x:`/`BNE @x` gives `d0 fe d0 fe`, with symbols `row0@x` and `row1@x`.
12. Locals inside `.phase $0300`: `ram_fn:`/`@loop:`/`DEX`/`BNE @loop`/`JMP @loop` gives `ca d0 fd 4c 00 03`, with `ram_fn@loop` = `$0300`.
13. An anonymous `:+` skipping over a local: `p1:`/`BEQ :+`/`@x:`/`NOP`/`:`/`RTS` gives `f0 01 ea 60`.
14. `.ifdef @x` (true and false branches); `BANK(p1@x)`.
15. Emitted source, using the custom resolver as in `emit_source.rs`: a source with `@l:`/`DEX`/`BNE @l`, invoked twice in `p1`, gives `ca d0 fd ca d0 fd`.
16. A label in a false `.if` opens no scope.
17. A body's `name\@:` does **not** open a scope: `p1:`/`.macro wait`/`BNE @done`/`@done:` resolves `@done` as `p1@done`. A plain `name:` in a macro body still opens one.

**Errors (line and exact text):**
- Every row of §5, plus:
  - an include that starts with `@loc:` gives ``Error in `inc.asm` on line 1``;
  - a reference before any label;
  - a hint and a no-hint variant;
  - `nope@x`;
  - `p1@a = 3` and `p1@a:`;
  - `@a = 3`;
  - plain `@x:` in a `.for` body.
- **Macro cases** (plan 017's reporting rule):
  - A body with two `@x:` reports on the second body line, with an `in expansion of macro` note naming the call.
  - A body's `BNE @out` that isn't in scope reports on its body line, with the hint and the expansion note.
  - A private-local macro inside a `.for` gives `label-in-for` naming `@x`.

### 6.2 Unit tests

- **`lexer.rs`:** `p1@x` is one `Text`. `@x`, `@x@y`, `p1 @x`, `p1@`, `@/x` and `'@'` lex as described in §3.1. `A5h@x` is a `Text` (pins the documented side effect). Plan 018's `!`/`~`/`-` tests still pass unchanged.
- **`parse.rs`:** the definition rejections; the `validate_loops` flag.
- **`preprocess.rs`:** `privatize` rewrites only body-defined locals; arguments are untouched; `expansions` and `parent` are filled for nested calls.
- **`lib.rs`:** `render_list_file` contains `00/0000 = p1@loc` and `00/0002 = p2@loc`. A private appears only with `include_macro_labels`, as `p1@spin\@1`.

### 6.3 Tooling and LSP

- **`tooling.rs`:**
  - `lex("  JMP p1@x")` has a single `Ident` `p1@x`.
  - `format` is idempotent on a source with locals; an indented `@x:` moves to column 0; `JMP p1@x` is untouched.
  - `@x:` after a blank line is not a block and doesn't end `body_lines`.
  - `undeclared-clobber` sees writes after an `@x:`.
  - `JMP @x` inside the routine is transparent.
  - `JSR p1@x` doesn't make `p1` "called".
  - An annotation above `@x:` gives `LocalTarget`.
  - Existing `block_label("@local:")` test is kept.
- **LSP (`lib.rs` tests):**
  - go-to-definition is scope-aware with two `@loc` scopes;
  - a qualified definition across files;
  - scope-aware references;
  - local rename, including qualified suffixes; global rename updates `q@x` prefixes; renaming `@x` to `y` is rejected;
  - `@` completion offers only the current scope (and the macro body's own);
  - document symbols nest locals;
  - no fold for locals;
  - hover value;
  - no signature scaffold above `@x:`.
- **CLI (`crates/nessemble-cli/tests/cli.rs`):**
  - exact stderr ``Error in `f.asm` on line 5: `@x` is not defined in `p2`; did you mean `p1@x`?``;
  - `-l` output and `--mlist` output.
- **Coverage:** a source map for a program using locals equals, span for span, the map for the same program with the locals renamed to globals.

### 6.4 Byte-identical regression

- The existing `tests/corpus` suite (`corpus.rs`) must pass unchanged.
- New `unaffected_*` tests in `local_labels.rs`: anonymous labels at depth 1–7, `name\@:` macros expanded twice, the plan 016 jump table, labels in `.phase`, `.include "@/x.asm"`, `; @nessemble-clobbers A` above a label, `.db '@'`, `.ascii "@x"`, and `@` in comments. Each is compared with bytes captured from v2.27.0.
- Before merging, the maintainer rebuilds the reference corpus and checks its ROM hash. It has no `@` labels, no `\@` labels and no `p@x` text in code (checked), so its bytes must not change.

## 7. Docs and CHANGELOG

- **`docs/src/syntax.md`:** a new `### Local` section between `### Named` and `### Temporary` covering:
  - the scope rule, with an example, hex output and a `<nessemble-assembler>` "Try it" block;
  - qualified references;
  - include boundaries;
  - macro privacy and argument passing;
  - `.for` (`@x\i`, and the pasted-scope exception);
  - "why not anonymous labels" (inserting a label retargets `:++`);
  - a "Migrating from global `@` labels" note: the "did you mean" hint, and constants needing global names.
- **`docs/src/usage.md`** (Documenting routines): annotations bind only to global labels, and locals never start a block for lint.
- **`docs/src/extending.md`** (`emit_source`): emitted `@` labels are private to the invocation.
- **`nessemble reference`:** no change.
- **Changesets** (never edit `CHANGELOG.md`):
  - Phase 1 (assembler, tooling lexer, lint):
    ```
    ---
    nessemble: minor
    ---
    **Behavior change:** `@name` labels are now local to the nearest label above them, so routines can reuse names like `@loop` and `@done`; reference another routine's with `routine@name`. Code that used an `@` label as a global now gets an error suggesting the qualified name.
    ```
  - Phase 2 (LSP), `minor`: "The editor understands `@local` labels: scope-aware go-to-definition, references, rename, outline and `@` completion."
  - This is a **behavior change**, released as `minor` (R1, settled).

## 8. Risks and open questions

- **R1. Version bump — settled: `minor`.** This breaks programs that use `@` as globals, and `@x = …` constants now error; the changeset is framed as a fix of broken semantics and marked as a behaviour change.
- **R2. `\@` labels and scopes — settled.** `\@`-generated labels do not open scopes (§2.2). A plain global label defined in a macro body still does; the docs say to use `name\@:` for macro-internal labels.
- **R3. Local constants** (`@len = 4`) are deferred and are an error in v1.
- **R4. Emitted-source privacy** (§2.6) goes beyond the brief. The alternative (caller's scope) lets a caller reference emitted `@x` but makes repeated invocations collide.
- **R5. Private display** `p1@spin\@7` in `--mlist`: a format choice that external list consumers may need to accept.
- **R6. `.rs` doesn't open a scope:** a choice to confirm.
- **R7. Existing lint gap:** an anonymous `:` line also makes routine effects "unknown". Not fixed here, since it is outside the brief; plan 023's emitted-stream walk removes it.
- **R8. Formatter:** the blank line inserted after `RTS` before `@x:` visually splits a routine. A possible follow-up is to skip it for locals; that would change format output, so it is not done here.
- **R9. Macro reporting** follows plan 017's rule (body line plus expansion notes) rather than the brief's "report at the call site"; the notes name every call, so the call site is still named.
- **R10. LSP scope is textual.** It ignores `.if` suppression and global labels defined inside macro expansions, so it can disagree with the assembler in those cases. Diagnostics stay authoritative.
- **R11. Performance:** `find_symbol` is a linear scan, and locals add symbols. The hint scan runs only on error. A `HashMap` index would be a separate change.
- **R12. Out of scope, listed for follow-up:**
  - a "did you mean `p1@loc`" hint for `JMP p1.loc` (plan 017 already makes it an error);
  - LSP completion after `q@`;
  - a quick fix applying the hint;
  - a lint rule against deep anonymous references.
- **R13. Lexer side effect:** `A5h@x`-style text changes tokenization. It was only garbage, and plan 017 already rejects it.

### Critical files for implementation

- `crates/nessemble-core/src/assemble.rs`
- `crates/nessemble-core/src/preprocess.rs`
- `crates/nessemble-core/src/lexer.rs`
- `crates/nessemble-core/src/tooling.rs`
- `crates/nessemble-lsp/src/lib.rs`

---

## Decisions settled with the maintainer

1. The changeset is `minor`, marked as a behaviour change (R1).
2. Labels generated with `\@` do not open a scope; this reverses that part of the brief's decided rule (§2.2, R2).

---

## As built

Shipped as §3 describes: a new `locals` module in `nessemble-core` (name
classification, `local_defs`, `privatize`), the `ident@ident` rule in both
lexers, the definition checks in `parse_stmt`/`parse_pasted_def` and the
per-frame flag in `validate_loops`, privatization in `preprocess::substitute`
and `exec_emitted_source`, and `resolve_def`/`resolve_ref` plus per-file
`scopes` in the assembler. The LSP's scope logic is its own `locals` module.

Deviations and findings:

- **`\@` label detection.** Rather than a marker on the token, each
  `Expansion` records the names it built with `\@` (`unique_names`); a label
  opens no scope when its line's expansion built its name. This keeps
  `Token` unchanged, as §3.1 wanted, and is exact (a literal `foo7:` in a body
  is not mistaken for `foo\@:`).
- **Emitted-source ids.** The key is (enclosing emitted id, file, line,
  expansion, index of the directive in the enclosing emitted source) — the
  expansion is added so a custom directive inside a macro expanded twice gets
  two ids. Ids are numbered after the `.macro` expansion ids, so the two kinds
  of private key never collide.
- **One extra message**, `local-duplicate-emitted`, for an emitted source that
  defines the same `@x` twice (the macro wording would name a macro that does
  not exist).
- **Same-site exemption.** A local seen again at the same site (a file included
  twice) is exempt from `local-duplicate` whatever its value; the global label
  that opened the scope already gets plan 017's warning.
- **Pass-2 reference errors only.** A reference to `@x` with no scope reports
  `local-no-scope` on pass 2 (definitions on pass 1), as §5 says; `.ifdef @x`
  with no scope reports it too and does not hold.
- **Lint.** `SignatureProblem` gained a `target` field (the local's name) for
  the `LocalTarget` message. A `JMP`/`JSR` to the routine's own local — `@x`,
  or `@x3` for a pasted `@x\i:` — is transparent; any other `@`-name target is
  an unknown. `block_label` also refuses a name containing `@`, so an (invalid)
  `p1@x:` never opens a block.
- **LSP.** Locals in the outline use `SymbolKind::KEY` with detail "local
  label", and a parent's range is widened to cover its children. A global
  label written inside a `.macrodef` body opens no textual scope (it exists
  only when expanded). Hover on a macro-private local says so instead of
  showing a value. Rename of a qualified `q@x` renames the local in `q`'s
  scope; the new name must be a local (`@y`).
- **Docs link.** The new `### Local` section links `.for` as `#loops`, its real
  anchor.
- **Tests.** `crates/nessemble-core/tests/local_labels.rs` holds §6.1 and the
  §6.4 `unaffected_*` cases, whose bytes were checked against a `main` build
  (the anonymous-label case is `d0 ff f0 00`). Plan 017's
  `at_prefixed_labels_that_cross_link_warn` became
  `at_prefixed_labels_are_local_and_do_not_warn`. Unit tests sit in
  `lexer.rs`, `parse.rs`, `preprocess.rs`, `locals.rs`, `lib.rs` and
  `tooling.rs`; LSP tests in `nessemble-lsp`; CLI tests in `cli.rs`. The corpus
  goldens pass unchanged. The reference-corpus ROM-hash check (§6.4) is the
  maintainer's to run before merge.
