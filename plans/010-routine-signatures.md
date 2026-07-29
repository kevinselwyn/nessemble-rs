# nessemble-rs: A Plan for Routine Signature Annotations

> Status: **Proposed — for review.** This document designs the JSDoc-shaped
> comment annotations that document a subroutine's **register calling
> convention**: which registers it takes, which it returns, and which it
> destroys. It lands as three rows in the
> [plan 009](009-comment-directives.md) directive registry
> (`@nessemble-param`, `@nessemble-returns`, `@nessemble-clobbers`), a resolver
> that binds an annotation run to the routine below it, editor surfaces that
> show the signature **at the call site**, and — the part that keeps the
> annotations honest — a **static check that a declared clobber list matches
> what the routine actually writes** ([§8](#8-verification--the-part-that-keeps-it-honest)).
>
> Filed as `010-routine-signatures.md` rather than `010-subroutine-comment-docs.md`:
> the comments are the *syntax*, but what is being written down is a
> **signature** — the one piece of a 6502 routine's contract the assembler
> otherwise has no way to state, and the only reason any of the tooling in §8
> is possible. If you'd rather the filename match the original slug, it is a
> `git mv`.

---

## 1. Goal

Let an author write a subroutine's calling convention where the subroutine is,
in a shape the tools can read:

```asm
; Draw one metasprite from `metasprite_table` into the OAM shadow buffer.
;
; @nessemble-param   A  metasprite index into `metasprite_table`
; @nessemble-param   X  screen x, in pixels
; @nessemble-param   Y  screen y, in pixels
; @nessemble-returns C  set when the sprite was clipped off-screen
; @nessemble-clobbers A, X, Y, [oam_cursor]
draw_metasprite:
    ...
    RTS
```

and get back, from the same three lines:

1. **The signature at the call site.** Hovering `JSR draw_metasprite` — anywhere
   in the project — renders the table above. On the 6502 the question "does this
   call eat my `Y`?" is asked once per `JSR` written, and today it is answered by
   scrolling to the callee and reading its body.
2. **A check that the declaration is true.** `draw_metasprite` claims it
   clobbers `A, X, Y`. If it also does `TXS`, or grows an `LDA` in a routine
   that claimed only `X`, the linter says so. An annotation nobody verifies
   decays into a comment that lies (§8).
3. **The usual editor affordances** — completion for the tags, an outline that
   shows each routine's clobber list, a code action that scaffolds a signature
   block over an undocumented routine, and a quick fix that adds a register the
   verifier caught missing.

The through-line: 6502 has no calling convention. Every project invents one per
routine, records it in prose if at all, and rediscovers it by reading the body.
This makes that convention **written down in one shape** and, uniquely among
comment conventions, **checkable**.

## 2. Why this is worth doing now

- **The registry exists and was built for this.** Plan 009 closed with "directive
  number four costs a table row." This is directives four, five, and six, and
  the cost really is three rows plus their argument parse: the scanner, the
  own-line rule, the malformed-name lint, the LSP completion/hover plumbing, and
  the semantic-token modifier all already work and need no change.
- **The hover already wants it.** `nessemble-lsp::preceding_doc` already shows
  the comment run above a symbol's definition when you hover a use of it — so
  the *plumbing* for "documentation travels to the call site" shipped in plan
  001 Phase 5. What arrives there today is undifferentiated prose. Structure it
  and the same hover becomes a signature card.
- **Clobbering is the bug.** The characteristic NES bug is a routine that
  started preserving `Y` and stopped, and eight call sites that assumed it
  still did. Nothing in the toolchain can see that today. With a declared
  clobber set, a mechanical check of "what does this body actually write"
  catches the moment the routine changed, which is the moment it is cheap to
  fix.
- **`require-block-comment` set the precedent.** Plan 008 already ships the
  opinion that a code block should be documented and the machinery to say so per
  project. This is the same opinion, one level more specific, using the same
  severity/config/report/diagnostic path.

## 3. Current state

**What exists**

- `nessemble-core::tooling` — the plan 009 directive registry: `DIRECTIVES`
  (token → `DirectiveName` → is-deprecated), `DirectiveArgs`, `Directive`
  (line/column/byte range/`own_line`), `MalformedDirective`,
  `scan_directives_with_errors`, and the `@nessemble-` closed-namespace rule.
- `tooling::lint` — the plan 008 rule registry (`RuleId`, `RULES`, `SeverityMap`,
  `Finding`), surfaced as the `nessemble lint` report **and** as LSP diagnostics.
  `rule_require_block_comment` already carries the helpers this plan needs:
  `block_label` (a line that is exactly `name:`) and `is_block_entry` (a label
  preceded by a blank line or the top of file, i.e. not an internal branch
  target).
- `nessemble-rc` — `.nessemblerc` `lint` section, per-rule severities resolved
  through `RuleId::from_id`, plus `overrides`. **New rules need no `rc` change.**
- `nessemble-lsp` — `preceding_doc` (comment run above a definition, shown on
  hover of any use), `definitions`/`document_symbol` (outline with a `detail`
  string), project-wide symbol resolution across `.include`s, comment-context
  completion (`comment_directive_items`), `comment_directive_hover`, and the
  `@fmt` → `@nessemble-format` quick fix — the template for every editor surface
  below.
- `nessemble-isa` — `Opcode { mnemonic, mode, opcode, length, timing, meta }`,
  generated at build time from `data/opcodes.csv`.

**What's missing**

- No annotation vocabulary — nothing distinguishes "this comment describes the
  routine's inputs" from prose.
- No notion of a **routine** anywhere in the tree. The formatter knows `RTS`
  ends a run (Pass 2), the linter knows a label opens a block; neither knows a
  block is a callable with a body extent.
- **`nessemble-isa` carries no register effects.** The table says how long `LDX`
  takes, not that it writes `X`. §8 adds that column; it is the one genuinely
  new data set in this plan.
- The LSP has no call-site view of a callee's contract, no inlay hints, and no
  scaffold action.

## 4. The annotations

Three registry entries, in plan 009's grammar unchanged — own-line comment,
`@…` as the comment's first token, exact lower-case name, arguments parsed once
in the scanner:

| Directive | Args | Means |
| --- | --- | --- |
| `@nessemble-param <slot> [description]` | one slot + prose | the routine **reads** this on entry |
| `@nessemble-returns <slot> [description]` | one slot + prose | the routine **defines** this on exit |
| `@nessemble-clobbers <slot>[, <slot>…]` \| `none` | a slot list | the routine **destroys** these; everything else survives |

**One slot per `param`/`returns` tag, a list for `clobbers`.** The first two
carry prose about a single thing, exactly as `@param` does in JSDoc; a clobber
list carries no prose per entry and reads better as a set. Everything after the
slot token on a `param`/`returns` line is the description, verbatim — including
a `;`, which is prose here, not the trailing-prose separator (per-directive
argument parsing is what plan 009 §4.5 already licenses). A leading `-`, `--`,
or `:` in a description is stripped for rendering, so `@nessemble-param A -- index`
and `@nessemble-param A index` hover identically.

### Slots

A **slot** is a place a value can live across a call. The vocabulary is closed,
so a typo is caught rather than silently becoming a memory symbol:

| Slot | Meaning |
| --- | --- |
| `A`, `X`, `Y` | the registers |
| `S` | the stack pointer (`TXS`/`TSX`; a routine that leaves the stack unbalanced) |
| `P` | the whole status register (`PHP`/`PLP`) |
| `C`, `Z`, `N`, `V`, `D`, `I` | one flag — `@nessemble-returns C` is how a 6502 routine returns a boolean |
| `[symbol]` | a named memory location — `[oam_cursor]`, `[tmp1]` |
| `$NN`, `$NNNN`, `$NN-$NN` | a raw address or inclusive address range — `$10-$1F` |
| `none` | *(clobbers only)* preserves everything; a claim, not an absence |

**Slot names are case-insensitive** (`a` and `A` both work, rendered upper-case)
— a deliberate departure from the case-sensitive rule for directive *names*.
Names are registry identifiers addressed to a tool; slots are assembly operands,
and nessemble accepts `lda` and `LDA` alike. Making the author remember which
half of the line is case-sensitive is a trap for no benefit.

Brackets on memory symbols are not decoration: they are what keeps the
vocabulary closed. Without them, `@nessemble-clobbers AX` would have to be read
as "the memory symbol `AX`" and the typo would go unreported — the exact silent
failure the `@nessemble-` namespace was introduced to end. The 6502 uses no
brackets in any addressing mode, so the notation is free.

### Binding: what an annotation attaches to

An annotation run binds to **the first block-entry label at or below it**, using
plan 009's existing transparent-skip rule (blank lines, comment lines, and
label/constant lines are skipped between a directive and its subject). In
practice: a signature block sits directly above its label, optionally with prose
comments and blank lines interleaved.

```asm
; Reset the PPU and clear OAM.        ← prose: the routine's summary
;
; @nessemble-clobbers A, X            ← the signature block
                                      ← blank lines are transparent
; Called once, from the reset vector. ← so is more prose
ppu_reset:                            ← the subject
```

Rules, each chosen so the resolver never guesses:

1. **A run binds to one label.** Tags need not be contiguous — a prose comment
   between two tags is fine — but any **code** line between a tag and the label
   ends the run and makes the tag ineffective (reported, §7).
2. **A label may carry at most one of each tag per slot.** Two
   `@nessemble-param A` lines on one routine is a mistake, not an override
   (reported).
3. **Order is free; rendering is canonical.** Tags render params → returns →
   clobbers regardless of the order written, and slots render in vocabulary
   order (`A, X, Y, S, P`, flags, memory), so two routines documented by two
   authors hover identically.
4. **A signature is per label, and labels are per file.** Nothing follows an
   `.include` — the annotation is next to its routine by construction. (The
   *consumers* are project-wide: the LSP resolves `JSR foo` to `foo`'s
   definition across includes today, and the signature travels with it.)
5. **Any one tag makes the label a documented routine.** There is no
   `@nessemble-routine` marker to remember; a routine with no inputs and no
   clobbers is spelled `@nessemble-clobbers none`, which says something
   (§13 Q4).

## 5. What "clobbers" means

The declaration is a **may-clobber set**, the same contract every real calling
convention publishes: *if the routine returns, a slot in the list may hold
anything; a slot not in the list holds what it held on entry.* Specifically:

- **Any path counts.** A register written on one branch and not another is
  clobbered. Callers cannot know which branch ran.
- **A `returns` slot is clobbered by definition** and need not be repeated in
  `clobbers`. `@nessemble-returns A` and `@nessemble-clobbers A` together are
  redundant, not contradictory — the resolver unions them and the verifier
  treats `returns` slots as declared-written.
- **A `param` slot is not automatically clobbered.** Taking `X` as an input and
  preserving it is a real, common contract (`; @nessemble-param X` with `X`
  absent from `clobbers` reads exactly right).
- **Callees count.** A routine that `JSR`s something that eats `Y` eats `Y`.
- **Flags are documentation, not verification** (§8.3). Nearly every instruction
  disturbs `N`/`Z`; a routine that fails to declare that is not lying in any way
  a caller can act on.
- **`none` is a claim.** `@nessemble-clobbers none` means "provably preserves
  everything" — the strongest statement, and the one the verifier checks hardest.
  Omitting the tag entirely means "undeclared", which is not the same thing and
  is never verified.

## 6. Architecture

### 6.1 Core: three registry rows and a resolver

In the "Comment directives" section of `nessemble-core/src/tooling.rs`, beside
the existing registry:

```rust
pub enum DirectiveName {
    Format, CoverageIgnore, CoverageIgnoreNextLine,
    Param, Returns, Clobbers,                       // new
}

/// A place a value lives across a call (§4).
pub enum Slot {
    A, X, Y, S, P,
    Flag(Flag),                 // C Z N V D I
    Symbol(String),             // [name]
    Address(u16, u16),          // $NN, or $NN-$NN as an inclusive range
}
pub enum Flag { C, Z, N, V, D, I }

pub enum DirectiveArgs {
    Strides(Vec<usize>), Region(RegionBound), None,
    Slot(Slot, String),         // param/returns: one slot + description
    Slots(Vec<Slot>),           // clobbers; empty vec == `none`
}
```

`DirectiveName::ALL` grows to six, `canonical()`/`arg_syntax()` gain three arms,
`parse_args` gains three, and `DIRECTIVES` gains three rows. Nothing else in the
scanner changes: the leading-`;`-run handling, own-line detection, byte ranges,
and `MalformedReason::{UnknownName, BadArgs}` all apply as-is.

Then the one new abstraction — the thing that turns a pile of directives into a
signature:

```rust
/// A routine's declared calling convention, bound to the label below it.
pub struct Signature {
    pub name: String,           // the label it binds to
    pub params:   Vec<(Slot, String)>,
    pub returns:  Vec<(Slot, String)>,
    pub clobbers: Vec<Slot>,    // empty == `none` declared
    pub declares_clobbers: bool,// distinguishes `none` from "no tag"
    pub line: u32,              // 1-based line of the label
    pub first_tag_line: u32,    // 1-based line of the first tag (for diagnostics)
}

/// Every signature in `source`, in source order.
pub fn resolve_signatures(source: &str) -> Vec<Signature>;

/// `resolve_signatures`, plus the annotation problems §7's rules report:
/// a tag bound to nothing, a duplicate slot, a `param`/`returns` on a
/// non-routine.
pub fn resolve_signatures_with_errors(source: &str)
    -> (Vec<Signature>, Vec<SignatureProblem>);
```

Built on `scan_directives_with_errors` + `split_lines` + the existing
`block_label`/`is_block_entry` helpers, so the resolver sees exactly what the
linter and formatter see, lexes once, and adds **no dependency**. Owned data, no
lifetimes, so the LSP can cache a `Vec<Signature>` per document.

`Slot` gets `Display` (canonical rendering: `A`, `C`, `[oam_cursor]`,
`$10-$1F`) and an `Ord` matching §4.3's vocabulary order, so every consumer
prints the same string in the same order without agreeing on anything.

### 6.2 `nessemble-isa`: register effects

The verifier needs to know what each instruction writes. Today the ISA table has
timing and length; this adds effects. Two columns on `data/opcodes.csv`, read by
the existing `build.rs` generator:

```csv
"LDA",MODE_IMMEDIATE,0xA9,2,2,0,A,          # writes A, reads nothing
"STA",MODE_ABSOLUTE_X,0x9D,3,5,0,,AX        # writes nothing, reads A and X
"ASL",MODE_ACCUMULATOR,0x0A,1,2,0,A,A       # accumulator mode writes A…
"ASL",MODE_ABSOLUTE,0x0E,3,6,0,,            # …absolute mode does not
```

```rust
/// The registers an instruction writes / reads. Flags are deliberately absent
/// (§8.3): the set is A, X, Y, S only.
pub struct RegSet(u8);        // bitset over A, X, Y, S
impl Opcode {
    pub const fn writes(&self) -> RegSet;
    pub const fn reads(&self) -> RegSet;
}
```

**Per opcode, not per mnemonic**, because the mode decides: `ASL A` writes `A`
and `ASL $00` does not; `LDA $10,X` reads `X` and `LDA $10` does not. Keying the
table by mnemonic would bake in a wrong answer for the four shift/rotate
instructions and every indexed mode — the sort of thing that produces a lint
rule nobody trusts. The data is mechanical and complete (151 documented opcodes,
plus the undocumented ones, which are marked and can be filled in as effects are
confirmed); it is generated into the same static table, so it costs no runtime.

This is independently useful — a future "this `LDA` is dead" rule, or a
peephole pass, wants exactly this column — but it is introduced here for §8 and
should be judged on that.

### 6.3 Lint rules

Four new `RuleId` variants (§7, §8), each an entry in `RULES` and a function.
`.nessemblerc` severities, `overrides`, the CLI report, and the LSP diagnostics
pipeline all work unchanged — `RuleId::from_id` is the only wiring, and it is
already generic.

### 6.4 LSP

Everything is an addition to an existing handler (§9); no new server
capabilities except the optional inlay-hint provider in Phase 4.

### 6.5 What does **not** change

- **The formatter never touches a signature block.** Comments are the author's
  text (plan 009 §6.2, decision 2). Aligning the tag columns is tempting and is
  arguably spacing rather than content — deliberately not proposed; see §13 Q6.
- **The assembler is untouched.** Annotations are comments; not one assembled
  byte moves. `xtask parity` stays 122/122 through every phase, which is the
  cheap regression signal for this whole plan.
- **`nessemble-wasm` builds core, so core stays fs-free.** The resolver takes
  `&str`; project-wide resolution happens in the LSP, which already has the
  buffers.

## 7. Validating the annotations themselves

Free, from plan 009's existing rules:

- `unknown-comment-directive` already fires on `@nessemble-parm` (unknown name)
  and on `@nessemble-clobbers A, Q` (`BadArgs`, quoting `arg_syntax()`).
- `ineffective-comment-directive` already fires on a tag in a trailing comment.

One new rule for the annotation-shaped mistakes the scanner cannot see, because
they are about *binding* rather than syntax:

| Rule | Default | Flags |
| --- | --- | --- |
| `invalid-routine-signature` | `warn` | A tag that binds to no label (code intervenes, or end of file); a duplicate slot in one signature (`@nessemble-param A` twice, or `A` listed twice in one clobber list); `none` mixed with other slots in a clobber list. |

Deliberately **not** rules: a routine whose `param` slot is never read (needs the
same dataflow as §8 with far less payoff and an obvious false positive — a
routine that forwards its inputs to a callee), and a `returns` slot the caller
ignores (that is the caller's business).

## 8. Verification — the part that keeps it honest

A comment convention nobody checks becomes a comment convention nobody trusts.
This is the phase that makes the annotations worth writing, and it is a real
static analysis, so its limits are stated up front.

### 8.1 The routine body

A documented routine's body runs **from its label to the last line before the
next block-entry label** (`is_block_entry` — a label preceded by a blank line or
the top of file), or end of file. Internal branch targets stay inside the body;
the next documented routine starts the next body.

This under-approximates on purpose. A routine that falls through into the next
one really does clobber what the next one clobbers, and the analysis will not
see it — a **false negative**, which costs a missed report. The alternative
(following fall-through and branches into neighbouring blocks) risks attributing
another routine's writes to this one — a **false positive**, which costs the
rule its credibility. For a warn-level lint, that trade is not close.

### 8.2 The two rules

| Rule | Default | Fires when |
| --- | --- | --- |
| `undeclared-clobber` | `warn` | The body definitely writes a register that the signature does not list in `clobbers` or `returns` — **only on routines that declared a clobber list.** Undeclared routines make no claim and are never flagged. |
| `overdeclared-clobber` | `warn` | The signature lists a register the body cannot write — **and** the body contains no unknowns (§8.4). Self-suppressing by construction. |

`undeclared-clobber` is the one that catches the bug in §2: it needs only a
single definite write, so it is unaffected by unknown callees, and every finding
it produces is a real discrepancy between the comment and the code. It carries
the quick fix (§9) that adds the missing register to the list, which makes the
whole loop — write the routine, get told, accept the fix — take one keystroke.

`overdeclared-clobber` catches the opposite drift: a routine that stopped using
`Y` and still tells every caller to spill it. Over-declaring is *safe* (callers
preserve more than needed), so it is a documentation-accuracy finding, and it
only fires when the body is fully understood.

### 8.3 What is verified

**`A`, `X`, `Y`, and `S`. Not flags, not memory.** Flags are excluded because
nearly every instruction writes `N`/`Z`, so verifying them would flag every
routine in the tree on day one and teach everyone to switch the rule off — the
declaration still renders on hover and is still how `@nessemble-returns C` is
written, it is simply not checked. Memory slots (`[tmp1]`, `$10-$1F`) are
excluded in v1 because resolving a store target through indexed and indirect
addressing is a different, larger analysis; `STA tmp1` in absolute/zero-page
mode is genuinely checkable and is the obvious v2 (§12).

### 8.4 Unknowns

A body region the analysis cannot read makes the routine's write set *at least*
what was seen, but not *at most* — so unknowns suppress `overdeclared-clobber`
and leave `undeclared-clobber` untouched. They are:

- a **macro invocation** (the expansion is not analyzed);
- a **`JSR` to a routine with no signature**, or one outside the analyzed
  buffers;
- an **indirect `JMP`/`JSR`** or a computed jump table;
- a **custom pseudo-instruction** (Rhai scripts emit arbitrary bytes);
- a **`.incbin`/`.db` run inside the body** — data in a code block, which may be
  executed by something else.

A `JSR` to a routine that *does* have a signature contributes that routine's
declared clobbers (transitively, resolved across `.include`s by the LSP and
within the CLI's file set). Recursion is broken by visiting each routine once
and treating a cycle as an unknown.

### 8.5 Why this stays a lint and not an error

Every finding is a statement about comments, not about correctness: the ROM
assembles identically either way. Defaults are `warn`, both rules are
`off`-able and `overrides`-able per glob, and `undeclared-clobber` fires only on
routines that opted in by declaring a clobber list. A project can adopt this one
routine at a time, which is the only adoption story that works for an existing
codebase.

## 9. The editor surface

Each item is an addition inside an existing handler.

- **Hover at the call site** *(the headline)* — `ident_hover` already finds a
  symbol and appends `preceding_doc`. It gains: if a `Signature` binds to this
  label, render it as a table above the prose, and hover anywhere the symbol is
  used — including `JSR draw_metasprite` — shows it:

  ```
  draw_metasprite  (label) = 49572 (0xC1A4)

  Draw one metasprite from `metasprite_table` into the OAM shadow buffer.

  | in | | out | |
  | --- | --- | --- | --- |
  | A | metasprite index | C | set when clipped |
  | X | screen x, pixels | | |
  | Y | screen y, pixels | | |

  clobbers  A, X, Y, [oam_cursor]
  ```

- **Hover on the tag itself** — `comment_directive_hover` gains three entries by
  existing mechanism (name, `arg_syntax`, description).
- **Completion inside a comment** — the three tags join `comment_directive_items`
  automatically. Plus one composite item, **"routine signature block"**, offered
  only when the comment sits above a block-entry label, inserting a scaffold:
  `@nessemble-param`, `@nessemble-returns`, `@nessemble-clobbers` on three
  lines with tab stops. This is how the feature gets discovered.
- **Outline detail** — `document_symbol` sets `detail` to `"label"` today; a
  documented routine shows `"label · clobbers A, X"` instead, so the whole file's
  register discipline is visible in one panel.
- **Code action: "Document this routine"** — on a block-entry label with no
  signature, insert the scaffold above it. Sibling of the existing `@fmt` quick
  fix, same `code_actions` handler.
- **Quick fix: "Add `X` to clobbers"** — on an `undeclared-clobber` diagnostic,
  rewrite the clobber list in place (canonical order, §4.3). The verifier found
  the register; the editor should not make you type it.
- **Semantic highlighting** — free: directive comments already get the
  `documentation` modifier (plan 009 decision 3).
- **Inlay hints** *(Phase 4, optional)* — render a callee's clobber set at the
  end of a `JSR` line:

  ```asm
      JSR draw_metasprite      ‹A X Y›
  ```

  This is the one genuinely new LSP capability in the plan
  (`textDocument/inlayHint`). It is also the surface most likely to be
  divisive — it puts text on every call line — so it ships last, behind the
  standard client-side toggle, and can be dropped without touching anything
  else (§13 Q7).

## 10. Docs

- **`docs/src/usage.md#comment-directives`** — three rows in the directive
  table, and a new `### Documenting routines` subsection: the slot vocabulary,
  the worked example from §1, what `clobbers` promises (§5), and the `none` vs
  omitted distinction.
- **`docs/src/usage.md#lint`** — three rows in the rule table
  (`invalid-routine-signature`, `undeclared-clobber`, `overdeclared-clobber`),
  each with one sentence on what it flags, plus a note that the clobber checks
  cover `A`/`X`/`Y`/`S` and treat flags and memory as documentation.
- **`docs/src/editor.md`** — extend the Hover, Completion, Outline, and Code
  actions bullets; add Inlay hints if Phase 4 ships.
- **`docs/src/syntax.md`** — a pointer from the label/subroutine section, since
  that is where a reader learns what a label *is*.
- **`.changeset/`** — `minor` per shipping phase, per plan 004.

## 11. Phased plan

Each phase is independently shippable and independently revertible.

- **Phase 0 — vocabulary and resolver (core).** Three `DIRECTIVES` rows, `Slot`,
  the two new `DirectiveArgs` variants, `parse_args` arms, `Signature`, and
  `resolve_signatures[_with_errors]`. Unit tests only, no surface. *This is the
  phase to review hardest — everything downstream reads these types.*
- **Phase 1 — validation and docs.** `invalid-routine-signature`; docs §10
  minus the editor bullets. After this phase the annotations are a documented,
  validated convention a project can adopt with no editor at all.
- **Phase 2 — editor surface.** Call-site hover, tag hover, completion +
  scaffold item, outline detail, "Document this routine" action.
- **Phase 3 — verification.** `nessemble-isa` effect columns and `RegSet`; the
  body-extent walk; `undeclared-clobber` and `overdeclared-clobber`; the
  "Add `X` to clobbers" quick fix.
- **Phase 4 — optional extras.** `require-routine-doc` (a `JSR`-targeted
  block-entry label with no signature; default `off`, §13 Q5) and inlay hints.

Phases 0–1 are small. Phase 3 is the bulk of the work and the bulk of the value;
Phase 2 is what makes anyone write the annotations in the first place, which is
why it comes first.

## 12. Explicitly not in v1

Listed so the boundary is a decision rather than an oversight:

- **Caller-side preservation checking** — flagging `JSR foo` (clobbers `Y`)
  followed by a read of `Y` with no reload. This is the highest-value analysis in
  the space and needs real intraprocedural dataflow over branches; it deserves
  its own plan, on top of Phase 3's effect table.
- **Memory clobber verification** — `STA <resolvable symbol>` against declared
  `[symbol]` slots. Tractable for absolute/zero-page stores; the natural v2.
- **Cycle/size annotations** (`@nessemble-cycles`) — the ISA table already has
  the data and it is a tempting fourth tag. Different feature, different plan.
- **`nessemble doc`** — generating a Markdown/JSON API index of a project's
  routines from the signatures. Falls out of `resolve_signatures` almost for
  free and is worth doing; it is a new CLI subcommand and a new output contract,
  so it is its own change.
- **Signature-aware `.macrodef`** — macros have arguments the assembler already
  knows about; documenting them is a different (easier) problem.
- **Enforcing a project-wide calling convention** ("every routine must take its
  first argument in `A`"). A configurable policy engine on top of this data;
  premature until signatures exist in the wild.

## 13. Decisions to settle

Recommendations first, alternatives stated. These are the questions worth
answering before Phase 0, because they are the ones that are expensive to change
after annotations exist in a project.

1. **Tag names — `@nessemble-param` / `-returns` / `-clobbers`.** *Recommended.*
   The ask was JSDoc-like, and these are the JSDoc spellings with the mandatory
   namespace (plan 009 keeps bare `@param` as prose forever, so the prefix is not
   optional). *Alternative:* the 6502-idiomatic `-in` / `-out` / `-clobbers`,
   which is shorter and matches how existing NES sources write it by hand.
2. **Bracketed memory symbols (`[tmp1]`) and `$` addresses in v1.** *Recommended*
   — scratch-RAM clobbering is the other half of the real-world problem, the
   brackets keep the vocabulary closed (§4), and the slots render on hover even
   though v1 does not verify them. *Alternative:* registers and flags only in
   v1, memory described in prose.
3. **Verification ships in v1 (Phase 3).** *Recommended* — it is the difference
   between a convention and a contract, and §8's scoping (A/X/Y/S only, opt-in
   by declaring, unknowns suppress) is what keeps it quiet. *Alternative:* ship
   Phases 0–2 and treat verification as a follow-up plan, at the risk that the
   annotations land, drift, and lose trust before the check arrives.
4. **`@nessemble-clobbers none` as the explicit "preserves everything" claim,
   distinct from omitting the tag.** *Recommended.* *Alternative:* no `none`
   keyword, and an empty list is a syntax error — but then the strongest and most
   useful statement a routine can make is unwriteable.
5. **`require-routine-doc` defaults to `off`.** *Recommended* — unlike
   `require-block-comment` (warn), this rule would fire on every routine in every
   existing project at once, and a rule that is loud on day one gets switched off
   permanently. Off by default, on for projects that adopt the convention.
   *Alternative:* `warn`, consistent with its sibling.
6. **The formatter does not align signature blocks.** *Recommended*, consistent
   with plan 009 decision 2. *Alternative:* treat tag-column alignment as
   spacing (which the formatter already normalizes) rather than content, and
   align the slot column within a run — attractive, and a slippery slope worth
   entering deliberately if at all.
7. **Inlay hints on `JSR` lines.** *Recommended as Phase 4, last and optional* —
   the highest-delight/highest-annoyance surface here. Easy to cut.
8. **Multi-slot `param` (`@nessemble-param A, X  the coordinate pair`).** *Not
   recommended* — one tag per slot keeps the description attached to the thing it
   describes and keeps rendering trivial. Noting it because a pointer passed in
   two registers is a real 6502 idiom and someone will ask.

## 14. Testing strategy

- **Core scanner/resolver (Phase 0)** — each tag with each slot kind; slot case
  insensitivity; description capture including a `;`; leading `-`/`--`/`:`
  stripping; `none`; `[symbol]` and `$NN-$NN` parsing; a bad slot yielding
  `BadArgs`; binding across blank/comment/label lines; a code line breaking the
  run; duplicate slots; canonical ordering independent of written order; a tag
  at end of file binding to nothing.
- **Lint (Phases 1, 3)** — one fixture per rule and per finding shape, plus a
  clean fixture of ordinary prose `@`-comments and hand-rolled `; in: A`-style
  comments proving zero false positives on code written before this plan
  existed; each rule `off`; a per-glob `overrides` entry.
- **Verifier (Phase 3)** — a routine that writes `X` undeclared (fires); one
  that declares it (silent); `ASL A` vs `ASL $00` (the mode distinction); `LDA
  $10,X` reading `X`; a `JSR` to an annotated callee inflating the set; a `JSR`
  to an unannotated callee suppressing `overdeclared-clobber` but not
  `undeclared-clobber`; a macro invocation as an unknown; a recursive pair; a
  routine falling through into the next (documented false negative, asserted so
  the behavior is pinned rather than discovered); `none` on a routine that
  writes nothing and on one that writes something.
- **ISA (Phase 3)** — a generated-table test asserting every documented opcode
  has an effect entry, so the CSV cannot go stale silently, plus spot checks for
  each addressing-mode family.
- **LSP (Phases 2, 4)** — hover on a `JSR` operand returns the callee's
  signature table; hover on the definition returns the same; completion in a
  comment above a label includes the scaffold item and above a non-label does
  not; outline detail carries the clobber list; the "Add `X` to clobbers" edit
  produces a canonically ordered list; inlay hints appear only on `JSR` lines
  with an annotated target.
- **Parity** — `xtask parity` stays 122/122 in every phase. Nothing here can
  change a byte, and that is the assertion that proves it.

## 15. Risks & mitigations

- **Annotation rot** — the whole risk. A signature that stops being true is worse
  than no signature, because callers act on it. *Mitigation:* §8 is in v1, not
  deferred; `undeclared-clobber` fires on exactly the drift that matters and
  carries a one-keystroke fix.
- **Adoption noise** — three new rules on an existing codebase. *Mitigation:*
  `undeclared-clobber` only fires on routines that opted in;
  `overdeclared-clobber` self-suppresses on any unknown; `require-routine-doc`
  defaults off; everything is `off`-able and glob-overridable.
- **Verifier credibility** — one confident false positive and the rule is off
  forever. *Mitigation:* under-approximate the body (§8.1), verify only what is
  unambiguous (§8.3), treat every unreadable construct as an unknown (§8.4), and
  pin the known false negatives with tests so they are documented behavior.
- **Vocabulary creep** — someone will want `@nessemble-param zp:$10 ptr_lo`, or
  types, or nullability. *Mitigation:* the slot vocabulary is closed and
  finite — it is the 6502's actual state, which does not grow — and §12 names the
  things that are separate features.
- **ISA table churn** — two hand-maintained columns across 256 opcodes. *Mitigation:*
  generated at build time from the CSV that already exists, with a completeness
  test; undocumented opcodes are marked and may be filled in incrementally
  without blocking the rules.
- **Divergence from hand-rolled conventions** — many projects already write
  `; in: A = index`. This does not read those and does not try to.
  *Mitigation:* a documented `sed`-able mapping in the docs, and the fact that
  the old comment stays visible on hover as prose alongside the new table, so
  migration can be partial and gradual.
