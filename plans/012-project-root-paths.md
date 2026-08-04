# nessemble-rs: A Plan for Project-Root-Relative Paths

> Status: **Shipped, all phases (0–6).** The
> deviations found by building each phase are recorded in
> [§13](#13-as-built). This document adds a `@/` prefix to
> filename arguments meaning "from the root of this project", so a path stops
> depending on where the file that spells it happens to live
> ([§3](#3-the-syntax-)). The root is discovered by walking up for the config
> marker the project already has ([§4](#4-what-the-root-is)), the prefix is
> honoured everywhere a string is known to be a path — the built-in filename
> directives and any argument declared with `file://`
> ([§5](#5-where--resolves)) — and a `@/` that cannot be resolved is an error
> naming the sigil rather than a silent fallback ([§6](#6-when--cannot-resolve)).
> Every design fork was settled with the maintainer and is recorded in
> [§11](#11-decisions), with the reasoning for the `file://` one in
> [§12](#12-why--resolves-in-file-declared-arguments).
>
> The through-line: **moving a file should not break the paths inside it.**

---

## 1. Goal

Let a filename argument name a path from the project root:

```nessemble
.include "@/lib/macros.asm"
.incbin  "@/assets/logo.chr"
.incpng  "@/art/tiles.png"
.tilemap "file://@/art/map.png"    ; custom pseudo-op, path declared
```

`@/` resolves against the project root regardless of which file the directive
sits in or how deeply that file is nested.

## 2. Why this is worth doing

Every filename argument today resolves against **the directory of the file that
contains it** — `.include` in `preprocess.rs:248`, the media importers via
`Assembler::cur_dir()` in `assemble.rs:1340`. That rule is correct, it matches
the reference tool, and it is the right default; it is also the source of three
recurring annoyances.

**Depth leaks into the path.** A shared graphics header that wants the same
asset spells it differently depending on where it lives:

```nessemble
; src/main.asm
.incbin "assets/logo.chr"
; src/gfx/sprites.asm
.incbin "../assets/logo.chr"
; src/gfx/meta/big.asm
.incbin "../../assets/logo.chr"
```

**Moving a file silently breaks it.** Dragging `sprites.asm` one directory
deeper invalidates every relative path *inside* it, and nothing about the edit
suggests that. The failure surfaces as `Could not read` on a line the author did
not touch.

**Paths are not copy-pasteable between files.** A path that is right in one file
is wrong in its neighbour one level down, so the obvious editing move — copy the
`.incbin` line that works — produces a broken line.

Vite, webpack, and TypeScript all solved this with a root alias, and assembly
projects have exactly the same shape: a `lib/` of shared macros and an `assets/`
of binaries, both referenced from source files at varying depths.

Note that this **adds** a spelling rather than replacing one. File-relative
resolution stays the default and stays unchanged; a project that never types
`@/` assembles byte-identically after this plan.

## 3. The syntax: `@/`

A filename argument whose **first two characters** are `@/` is
project-root-relative. Everything after the `@/` is a path relative to the root:

| Argument                  | Resolves to                |
| ------------------------- | -------------------------- |
| `"@/assets/logo.chr"`     | `<root>/assets/logo.chr`   |
| `"@/logo.chr"`            | `<root>/logo.chr`          |
| `"assets/logo.chr"`       | `<containing dir>/assets/logo.chr` (unchanged) |
| `"./@weird/logo.chr"`     | `<containing dir>/@weird/logo.chr` (unchanged) |
| `"@weird/logo.chr"`       | `<containing dir>/@weird/logo.chr` (unchanged) |

Three properties are deliberate:

- **Only a leading `@/` counts.** `@` followed by anything other than `/` is an
  ordinary path character, so a file or directory whose name starts with `@` is
  still addressable — and `./@x` is the escape hatch for the pathological
  directory literally named `@`. This mirrors how
  [`strip_file_url`](../crates/nessemble-core/src/lib.rs) treats its prefix: a
  *leading* marker, not a substring match.
- **`@/` is a prefix, not a URL scheme.** There is no host part, no
  percent-decoding, and no `@//`-style variants.
- **It composes with `file://`.** The declaration is stripped first, so
  `"file://@/lib/defs.asm"` is a declared, root-relative path. The two markers
  are orthogonal and stack in that order only (`@/file://…` is not a thing).

There is **no lexer change**. `@` already begins a local label at identifier
position (`lexer.rs:405`), but a filename argument is a quoted string — the
assembler's `.include` target is even extracted from the raw source line
(`preprocess.rs:206`), never from a token — so `@/` inside quotes is
unambiguous and touches nothing the lexer does.

### 3.1 Why `@/` and not `~/`

`~` means "home directory" in every shell on every platform, and a path that
means one thing at a prompt and another in a source file is a trap. `@` has no
prior meaning in a nessemble *path* position, so it can be given one cleanly.
This also leaves `~/` available to mean the actual home directory later, should
that ever be wanted.

## 4. What the root is

Root resolution is a **three-step ladder**, evaluated once per assembly at the
entry point — not per file, not per directive:

1. **`Options::project_root`**, if set. This is the explicit override, populated
   by the CLI's new `--root <dir>` flag (Phase 4) and by the language server's
   workspace folder (Phase 5).
2. **The nearest config marker**, found by walking up from the entry file's
   directory for `.nessemblerc`, `.nessemblerc.json`, or `.nessembleignore`. The
   root is the directory *containing* the first marker found.
3. **The entry file's own directory**, when no marker exists anywhere on the way
   up.

Step 2 reuses the discovery rule `nessemble-rc` already implements
(`find_upwards`, `crates/nessemble-rc/src/lib.rs:492`) — but it cannot reuse the
*code*, because `nessemble-rc` depends on `nessemble-core` and not the reverse.
Core therefore grows its own walk-up over the same three filenames. That is a
handful of lines matching literal names, with no `serde` and no config parsing,
so the duplication is a constant, not a schema. A shared constant for the marker
names, exported from core and consumed by `nessemble-rc`, keeps the two lists
from drifting.

Two consequences worth stating outright:

- **A project with a `.nessemblerc` gets `@/` for free**, rooted where the
  author already thinks the project is rooted. No new file, no new flag.
- **A single loose `.asm` file still works.** Step 3 makes `@/foo` mean `./foo`
  there, which is the only sensible reading when the file *is* the project.

### 4.1 Where the root is computed

All three preprocessing entry points already derive `base` identically
(`lib.rs:167`, `lib.rs:264`, `lib.rs:360`). The root is derived alongside it, in
the same three places, and threaded down. `assemble_with` (stdin / in-memory
source) uses the current working directory as its base today, so the ladder runs
from there; under wasm, where `current_dir()` fails and there is no filesystem at
all, the root is `None` (§6).

### 4.2 A note on plumbing

`Assembler::new` already takes seven arguments (`lib.rs:300`), which is exactly
clippy's `too_many_arguments` threshold — and CI runs clippy with `-D warnings`.
Adding the root as an eighth would trip it. Rather than paper over that with an
`#[allow]`, Phase 1 should bundle the per-file tables and the root into one
struct — `files`, `dirs`, `paths`, `root` are already produced together by
`Preprocessed` and consumed together by the assembler — and pass that. This drops
`Assembler::new` to four arguments and makes the "these travel together"
relationship explicit instead of positional.

## 5. Where `@/` resolves

The governing rule: **`@/` resolves exactly where the assembler already knows a
string is a path.** That is two places.

**The built-in filename directives:**

| Directive   | Resolved in |
| ----------- | ----------- |
| `.include`  | `preprocess.rs` — `Pre::do_include` |
| `.inestrn`  | `preprocess.rs` — same path |
| `.incbin`   | `assemble.rs` — `Assembler::read_media_file` |
| `.incpng`   | `assemble.rs` — via `decode_media_png` → `read_media_file` |
| `.incpal`   | `assemble.rs` — via `decode_media_png` → `read_media_file` |
| `.incrle`   | `assemble.rs` — via `read_media_file` |
| `.incwav`   | `assemble.rs` — `exec_incwav` → `read_media_file` |

Because five of the seven funnel through `read_media_file`, the assembler side is
essentially one function's worth of change.

**Any argument declared with `file://`**, including on a custom pseudo-op:

```nessemble
.tilemap "file://@/art/map.png", "file://@/art/tiles.png"
```

The declaration's entire content is "this string names an input file", so a
declared string is a path by construction and `@/` resolves in it. See §12 for
the reasoning and §5.1 for what the script receives.

**Explicitly out of scope**, each for a reason:

- **Undeclared custom pseudo-op arguments.** An undeclared string may be an
  easing name, a label, or arbitrary text — `.ease "linear"` — and the assembler
  cannot tell which. Rewriting one that merely happens to start with `@/` would
  corrupt data. `file://` is precisely the marker that lifts a string out of this
  category, which is why the declared case is in scope and this one is not.
- **The script-side file API** (`nessemble-script`'s `resolve`). A path a script
  *constructs itself* and passes to `read_file` still resolves against `base`.
  Teaching that layer about the root means threading the root through the
  `CustomResolver` signature — a public API break for a case no one has asked
  for. Declared arguments reach the script already resolved (§5.1), which covers
  the common need without it.
- **`.nessemblerc` paths, `--pseudo` mapping paths, and CLI output paths**
  (`-o`, `-l`). These are configuration and command-line arguments, not source;
  a shell already expands paths there, and the root is not yet known when they
  are read.

### 5.1 What a declared `@/` argument hands to the script

`exec_custom` (`assemble.rs:1293`) strips `file://`, existence-checks the result
against `cur_dir()`, and passes the bare string to the resolver in `texts`. With
`@/` in scope, the resolution happens **once, before both**, and the resolved
path is what flows onward:

```
"file://@/art/map.png"
  → strip file://        →  "@/art/map.png"
  → resolve_path_arg     →  "<root>/art/map.png"   (absolute)
  → existence check, and texts[i] handed to the script
```

Four consequences, all of which the existing machinery already handles:

- **Scripts need no changes.** `nessemble-script`'s `resolve`
  (`lib.rs:287`) passes an absolute path through untouched, joining `base` only
  for relative ones. A script that does `read_file(texts[0])` works unmodified.
- **The existence check tests the right file**, so the directive-level "missing
  declared input" diagnostic keeps working for `@/` paths — the property that
  made `file://` worth having in plan 011 (§4 there).
- **Memoization stays correct.** The `CustomKey` includes `texts`
  (`assemble.rs:1317`), which now holds the resolved path. Two directives naming
  the same file by different spellings — `"file://@/art/map.png"` from one
  directory and `"file://../art/map.png"` from another — no longer collide or
  diverge by accident; they key on what they actually read.
- **Undeclared arguments are untouched**, so a `.tilemap "@/x.png"` without the
  declaration reaches the script literally. That asymmetry is the point of the
  declaration rather than a wart: `file://` is how an author says "this is a
  path", and `@/` resolution is one of the things saying so now buys.

## 6. When `@/` cannot resolve

A `@/` path that cannot be resolved is a **hard error naming the sigil**, never a
silent fallback. Two cases produce one:

- **No root at all.** The only environment that reaches this is wasm, where there
  is no filesystem and file directives already fail. The message should say the
  root could not be determined, not `Could not read`.
- **A path that climbs above the root.** `"@/../secret.bin"` normalizes to
  somewhere outside the project. `@/` means "from the root"; a spelling that
  leaves the root defeats the purpose and is almost always a typo. Resolution
  therefore lexically normalizes the segment after `@/` and rejects one that pops
  past the root. (Lexical, not `canonicalize` — the target may not exist yet, and
  symlink-following is not wanted here.)

Both get a distinct i18n message in `crates/nessemble-i18n/locales/en-US.ftl`,
reported against the directive's own line, e.g.:

```
project-root-unresolved = `@/` used in `{ $file }`, but no project root could be determined
project-root-escape     = `{ $file }` resolves outside the project root
```

A generic `Could not read \`@/assets/logo.chr\`` would send the author looking for
a missing file when the real problem is a missing root — which is exactly the
failure mode this plan exists to remove.

## 7. What does not change

- Every path without a `@/` prefix resolves exactly as it does today, against
  the containing file's directory. `Preprocessed::dirs` and
  `Assembler::cur_dir()` keep their current meaning and their current tests.
- ROM output for any existing project is byte-identical.
- `file://` keeps every property it has: still stripped before use, still what
  makes a path clickable and a missing file a directive-level error. It gains
  one — `@/` resolution (§12) — and gains it only for arguments that already
  start with `@/`, so no existing declared path changes.
- The corpus tests in `tests/corpus/` need no updates.

## 8. Phased plan

Each phase compiles, tests, and ships on its own.

### Phase 0 — Root resolution in core, wired to nothing ✅

- `pub const PROJECT_ROOT_PREFIX: &str = "@/";` beside `FILE_URL_PREFIX` in
  `lib.rs`.
- `pub const PROJECT_MARKERS: &[&str]` — the three filenames from §4.
- `pub fn find_project_root(start: &Path) -> Option<PathBuf>` — the walk-up.
- `pub fn resolve_path_arg(root: Option<&Path>, base: &Path, arg: &str) -> Result<PathBuf, RootError>`
  — the single function every consumer calls. Handles the non-`@/` case by
  joining `base`, so callers have one code path rather than a branch each.
- `Options::project_root: Option<PathBuf>`, defaulting to `None`.
- Unit tests for the prefix rule, the ladder, and the escape check.

No directive uses any of it yet; behavior is unchanged and provably so.

### Phase 1 — `.include` and `.inestrn` ✅

- Bundle `files`/`dirs`/`paths`/`root` per §4.2; compute the root at the three
  entry points in `lib.rs`.
- `Pre::do_include` calls `resolve_path_arg` instead of `dir.join(&name)`.
- The two i18n messages from §6.
- The *included file's* recorded directory (`dirs`) stays its real on-disk parent,
  so a root-included file's own relative paths still work file-relatively.

### Phase 2 — the media importers and declared arguments ✅

- `Assembler` carries the root; `read_media_file` and `exec_incwav` call
  `resolve_path_arg`.
- `exec_custom` (`assemble.rs:1293`) resolves each `file://`-declared argument
  once, per §5.1: resolve after stripping, then existence-check and populate
  `texts` from the resolved path. Undeclared arguments keep flowing through
  verbatim.
- A resolution failure here is the directive's own error, reported on its line
  before the script runs — the same ordering plan 011 established for a missing
  declared file, and for the same reason.

### Phase 3 — integration tests ✅

`crates/nessemble-core/tests/project_root.rs`, modeled on the existing
`file_url.rs` (same `TempTree` helper, same shape):

- `.include "@/lib/defs.asm"` from a file two directories deep.
- `.incbin "@/assets/logo.chr"` from a nested include.
- A `.nessemblerc` above the entry file sets the root; a deeper entry file still
  resolves to it.
- No marker anywhere → the entry file's directory is the root.
- `Options::project_root` overrides a marker that would otherwise win.
- `"file://@/lib/defs.asm"` on `.include` works and is still existence-checked.
- `"@weird/x.chr"` and `"./@x/y.chr"` are untouched.
- `"@/../outside.bin"` errors, naming the root.
- A byte-identical ROM for a project that uses no `@/`.

Declared custom-pseudo-op arguments (§5.1) extend `file_url.rs`'s
`recording_resolver` pattern, which already captures the `texts` a resolver was
handed — so these assert on the exact strings:

- `.tilemap "file://@/art/map.png"` from a nested include hands the script the
  **resolved** path, and the recorded `texts` prove it.
- The same directive with a *missing* `@/` target errors on its own line, names
  the bare path, and the script never runs.
- An **undeclared** `.tilemap "@/art/map.png"` hands the script `@/art/map.png`
  verbatim — the asymmetry in §5.1 is deliberate, so it gets a test rather than
  being left to chance.
- Two directives in different directories naming the same file, one via `@/` and
  one via `../`, key the memo on the same resolved `texts` (§5.1).

### Phase 4 — CLI `--root` ✅

- `--root <dir>` on `assemble` (and `coverage`, which assembles too), plumbed to
  `Options::project_root`.
- Rejected with a clear message if the directory does not exist.
- A CLI test in `crates/nessemble-cli/tests/cli.rs`.
- `docs/src/usage.md` entry.

### Phase 5 — the editor surface ✅

`nessemble-lsp` mirrors the assembler's resolution in four features, all
currently routed through its own `resolve_path_arg` (`lib.rs`, ~line 1535):

- **Document links** — cmd-click a `@/` path and open the right file. `path_args`
  (`lib.rs:1466`) already yields declared arguments alongside built-in filename
  ones and already carries the `declared` flag, so custom pseudo-op paths become
  clickable through the same change rather than a second one.
- **Hover** — `path_arg_hover` shows where a `@/` path landed, which answers
  "what root did it pick?" without a build.
- **Completion** — offer `@/` as a completion at the start of an empty filename
  argument, and complete *within* a `@/` path against the root.
- **Root source** — prefer the workspace folder (`workspace_roots`) as the
  override, falling back to the marker walk-up so a single-file editor session
  behaves like the CLI.

### Phase 6 — docs and changeset ✅

- `docs/src/syntax.md`: a "Project-root-relative paths" section next to
  "Declaring a filename argument" (~line 993), and an update to the existing
  block quote about relative resolution so it names the new escape from it.
- `docs/src/extending.md`: the "Declaring file arguments" section (~line 127)
  gains the `@/` case — including that a declared `@/` argument reaches the
  script already resolved (§5.1), which is the part a script author needs.
- `docs/src/editor.md`: mention `@/` alongside the `file://` clickable-path note.
- `editors/` syntax highlighting: check whether the `@/` prefix should be
  distinguished inside a string; low value, do it only if it is a one-line grammar
  change.
- `cargo run -p xtask -- changeset add minor "…"` — a new syntax, so `minor`. The
  body should name both halves: the `@/` prefix, and that a declared argument now
  reaches a script resolved (§9).

## 9. Risks

- **Silent root drift.** Adding a `.nessemblerc` to a parent directory moves the
  root for every `@/` path below it. That is the intended mechanism, but it means
  an unrelated formatting-config file can change what a source file reads. The
  LSP hover (Phase 5) is the mitigation: the root is always inspectable.
- **A `@`-prefixed filename.** A real file named `@something` is unaffected
  (§3), but a directory literally named `@` is only addressable as `./@`. This is
  acceptable and documented.
- **A declared argument's meaning shifts slightly.** A script that previously
  received a relative path and did something other than open it — parsing it,
  echoing it into a log — now receives an absolute one. Any script doing that
  with a *declared* argument was already relying on something `file://` does not
  promise, but it is a real behavior change and belongs in the changeset text
  (Phase 6).

## 10. Estimated shape

| Phase | Crates touched | Rough size |
| ----- | -------------- | ---------- |
| 0     | `nessemble-core` | small, self-contained |
| 1     | `nessemble-core`, `nessemble-i18n` | medium (the plumbing refactor) |
| 2     | `nessemble-core` | small (two call sites plus `exec_custom`) |
| 3     | `nessemble-core` (tests) | medium |
| 4     | `nessemble-cli`, docs | small |
| 5     | `nessemble-lsp` | medium |
| 6     | docs, `editors/` | small |

## 11. Decisions

Settled with the maintainer before implementation:

1. **`@/` only** — not `~/`, and not both. `~` is the home directory everywhere
   else and must not mean something different here (§3.1).
2. **Marker walk-up with an entry-directory fallback** — reuse the
   `.nessemblerc`/`.nessembleignore` discovery the project already has, so
   existing projects need no new file, with an explicit `--root` override on top
   (§4).
3. **Wherever a string is known to be a path** — the seven built-in filename
   directives, plus any argument declared with `file://` (§5, §12). Undeclared
   custom pseudo-op arguments, the script file API, and configuration paths are
   out (§5).
4. **A hard error naming the sigil** when `@/` cannot resolve — no silent
   fallback to the entry directory or the containing directory (§6).

## 12. Why `@/` resolves in `file://`-declared arguments

Decision 3's second half was settled after the rest, and the reasoning is worth
keeping because it is what makes the scope rule a rule rather than a list.

Scoping to built-in directives alone would have left this:

```nessemble
.include "file://@/lib/defs.asm"    ; works — .include is a built-in
.tilemap "file://@/art/map.png"     ; fails — .tilemap is a custom pseudo-op
```

The second line strips to the literal `@/art/map.png`, fails its existence check
against `cur_dir()` (`assemble.rs:1312`), and reports
`` Could not open `@/art/map.png` `` — sending the author to look for a missing
file when the spelling one line above works fine. Two identical-looking paths
behaving differently because of which *directive* they sit on is exactly the kind
of positional surprise this plan set out to remove.

The fix is not an exception carved out for `.tilemap`; it is noticing that
"built-in filename directive" was the wrong rule. The right one is **"the
assembler knows this string is a path"** — which is true of the built-ins by
their signature, and true of a declared argument by the declaration. `file://`
exists to say precisely that; plan 011 (§4) introduced it so the assembler could
know a custom pseudo-op's string was a file without executing the script. Having
been told, honouring `@/` in it follows.

This also costs almost nothing. The resolver already receives absolute paths
without trouble, so no script changes (§5.1), and the existence check gains
rather than loses precision. What it *adds* is a reason to type `file://` on a
custom pseudo-op beyond diagnostics and clickability: it is now how you get a
root-relative path there at all.

Undeclared arguments stay out regardless, for the `.ease "linear"` reason in §5 —
and that boundary is now the same boundary `file://` already draws, rather than a
second one to remember.

## 13. As-built

Deviations found by building each phase, recorded as they ship.

### 13.1 The helpers live in `paths.rs`, not `lib.rs` (Phase 0)

Phase 0 said "beside `FILE_URL_PREFIX` in `lib.rs`". `lib.rs` is already ~950
lines of crate-level API, and the resolution rules want room for the reasoning
that justifies them — why the escape check is lexical, why it iterates
`Component`s. They went into a private `paths` module re-exported from the crate
root, matching how `assemble`/`preprocess` are already structured. The public API
is exactly what §8 specified: `nessemble_core::{PROJECT_ROOT_PREFIX,
PROJECT_MARKERS, find_project_root, project_root, resolve_path_arg, PathArgError}`.

The module is named `paths` rather than `project` because clippy's pedantic
`module_name_repetitions` — on for this workspace — objects to `project::project_root`.

### 13.2 The ladder became a function, and it canonicalizes (Phase 0)

§4 described the three-step ladder in prose, leaving Phase 1 to assemble it at
the three entry points. Writing the tests made it obvious that "explicit, else
marker, else base" is itself the thing worth testing once rather than three
times, so it shipped as `project_root(explicit, base)`, with `find_project_root`
underneath as the marker walk-up alone.

It **canonicalizes `base` before walking**, which §4 did not mention and which
matters more than it looks: `Path::new("src").parent()` is `""`, and *its* parent
is `None`, so an uncanonicalized walk from a relative base gives up after one
step and finds nothing. `nessemble-rc` canonicalizes for the same reason
(`lib.rs:386`). The consequence is that a discovered root is absolute, so `@/`
paths resolve to absolute paths — which is what the LSP hover in Phase 5 wants to
show anyway.

### 13.3 The escape check iterates components rather than splitting on `/` (Phase 0)

The obvious implementation splits the remainder on `/` and counts `..` segments.
That is wrong on Windows: `"@/..\\..\\secret"` contains no `/`-delimited `..`
segment, so it would pass the check and *then* be traversed by `PathBuf::push`,
which treats a backslash as a separator there. Iterating `Path::components()`
delegates the separator rules to the platform, so the same string is a real
traversal on Windows (caught) and an ordinary filename on Unix (left alone).

Two smaller rules fell out of using components, both tested: a `Prefix` component
(`C:`) after `@/` names a different volume and is an escape, and `RootDir` is
skipped so `@//x` is simply `@/x`.

### 13.4 The i18n messages landed a phase early (Phase 0)

§8 put them in Phase 1. But `PathArgError` without messages is half a type — the
caller would have to know which Fluent key each variant maps to — so
`PathArgError::message(arg)` and the two `en-US.ftl` keys shipped together in
Phase 0. Fluent does not warn about unused keys, so they are inert until Phase 1
calls them. Phase 1 is now pure wiring.

### 13.5 `PROJECT_MARKERS` is not yet consumed by `nessemble-rc` (Phase 0)

§4 promised a shared constant "exported from core and consumed by
`nessemble-rc`, keeps the two lists from drifting". Only the export shipped.
`nessemble-rc` discovers `.nessemblerc`/`.nessemblerc.json` and
`.nessembleignore` as **two independent walks** (`lib.rs:392`, `lib.rs:402`) —
they mean different things there, and the second deliberately runs even when the
first found nothing. `PROJECT_MARKERS` is their union, which is right for "where
is the project rooted" and wrong as a drop-in for either walk.

Closing the drift properly means extracting rc's two lists into named constants
and asserting their union equals `PROJECT_MARKERS` — a genuine test, but rc-side
work that Phase 0 has no reason to carry. The constant documents the relationship
in the meantime. Worth doing alongside Phase 4, which is the next time the CLI's
config handling is open.

### 13.6 The bundle is `SourceTables`, five arguments rather than four (Phase 1)

§4.2 estimated that bundling `files`/`dirs`/`paths`/`root` would drop
`Assembler::new` "to four arguments". It bundles the four into a
`pub(crate) SourceTables { files, dirs, paths, root }` next to `Assembler`
in `assemble.rs`, but the count that falls out is five: `nes`, `undocumented`,
`empty_byte`, `tables`, `custom`. The estimate undercounted by one — `nes`,
`undocumented`, and `empty_byte` were never candidates for bundling, since
they're independent scalars an `Options` reference could carry but the
existing call sites already destructure by hand. Five is well under clippy's
`too_many_arguments` threshold (8) either way, so the eighth-argument problem
§4.2 raised is resolved regardless of the exact count.

`root` also had to reach `Preprocessed` itself, not just `Assembler::new`'s
bundle: `Pre::do_include` needs it mid-preprocessing, before an `Assembler`
exists. `preprocess`/`preprocess_with` now take `root: Option<PathBuf>` as a
parameter (computed once by the caller, per §4.1) and `Preprocessed` carries
it back out alongside `files`/`dirs`/`paths`, so the one value flows through
both bundles instead of being computed twice.

### 13.7 Only `assemble_with` (in-memory source) can produce a `None` root (Phase 1)

§4.1 says the wasm build reaches `PathArgError::NoProjectRoot` because there
is no filesystem at all — but `paths::project_root` never actually returns
`None`; on a canonicalize failure it falls back to the (uncanonicalized) `base`
itself, so a `base` of any kind always yields *some* root. The `None` case
therefore has to be manufactured at the call site, and only one of the four
public entry points has a `base` that can genuinely fail to exist:
`assemble_with`, whose base is `std::env::current_dir()` — the one path with no
on-disk entry file to derive a directory from, and the one `std::env::current_dir`
can fail on under wasm. `assemble_file_with`/`assemble_source_as`/the two
`diagnose_*` entry points all derive `base` from a caller-supplied `Path` via
`Path::parent()`, a text-only operation that never fails, so their root is
always `Some`.

`lib.rs` gained one small helper, `resolve_root(options, base: Option<&Path>)`,
used by all five entry points: an explicit `Options::project_root` wins
regardless of whether `base` is known (it doesn't touch the filesystem), and
otherwise the root is `Some(paths::project_root(None, base))` if `base` is
`Some`, or `None` if it isn't. Only `assemble_with` can pass `None` for `base`
(when `current_dir()` errors); every other caller always has one.

### 13.8 `--root` is validated and canonicalized in the CLI, not left to core (Phase 4)

§8 said `--root <dir>` "plumbed to `Options::project_root`" and rejected "if the
directory does not exist", without saying where the check lives. `nessemble-core`
never validates `Options::project_root` — `paths::project_root` treats an
explicit root as a plain override and joins it as given, whether or not it exists
— so a bad `--root` would otherwise surface later as a confusing `@/` resolution
failure on whatever line first used the prefix, rather than as a flag error up
front. `resolve_root_flag` in `main.rs` (shared by `assemble_mode` and
`coverage::run`, since both take `--root`) checks `Path::canonicalize` and
`is_dir` before assembly starts, and returns the *canonical* path — matching
§13.2's observation that a discovered root is always absolute, so an explicit
one should be too, rather than mixing relative and absolute roots depending on
whether `--root` or the marker walk-up supplied it.

### 13.9 The LSP's root ladder omits the wasm-only "no root" case, and needed its own `resolve_path_arg` rename (Phase 5)

§8's four bullets map onto one new method, `Server::root_dir`, called by
`hover`, `document_links`, and `path_completions` alongside the existing
`base_dir`. It mirrors `lib.rs`'s `resolve_root` (§13.7) with one simplification:
the LSP only ever runs where a real filesystem exists, so there is no
`current_dir()`-failure path to reproduce, and `root_dir` returns `None` only
when `base_dir` itself does (an untitled buffer) — never `PathArgError::NoProjectRoot`
in practice.

The workspace-folder override is *containment*-based rather than "the first
workspace folder": `root_dir` picks the workspace root the document's `base_dir`
is actually `starts_with`, so a multi-root workspace does not root every open
file at whichever folder happens to be first in `workspace_roots`.

The module already had a private `resolve_path_arg(base, path) -> PathBuf`
(plain `base.join`, no `@/` handling) backing all four features listed in §8.
Rather than teach it `@/` under the same name — which would have shadowed
`nessemble_core::resolve_path_arg` at every call site — it was deleted in favor
of calling the core function directly, so the LSP's resolution is *the same
code* as the assembler's rather than a parallel reimplementation of it.

Completion's directory-splitting needed one adjustment §8 didn't anticipate:
splitting the typed text on its last `/` (to separate "directory so far" from
"prefix being completed") loses the `@/` marker when there is exactly one path
segment after it — `"@/ass"` naively splits to a bare `"@"`, which is not
`PROJECT_ROOT_PREFIX` and so falls through to ordinary (wrong) relative
resolution. The split now peels a leading `@/` off first and reattaches it to
whatever directory portion the generic split produces, so `"@/ass"` completes
against the root and `"@/foo/ba"` completes against `<root>/foo/`.

### 13.10 The Phase 1 changeset already covered the user-facing `@/` behavior (Phase 6)

§8 filed the changeset under Phase 6, but Phase 1's own commit already added a
`minor` changeset for the `@/` prefix and declared-argument resolution the
moment those became real (rather than leaving the user-facing entry to wait
for the docs phase). Phase 6 therefore added a second, separate changeset for
what *it* shipped — the CLI's `--root` flag and the language-server support —
rather than editing the first one, since a changeset is consumed and rendered
once and editing an already-accurate entry to describe unrelated work would
misattribute it.

The `editors/` bullet turned out to be moot: the VS Code extension carries no
TextMate grammar at all (highlighting comes entirely from the LSP's semantic
tokens), so there was no one-line grammar change available to make — the
"low value, do it only if" clause resolved to "don't."
