# nessemble-rs: A Plan for Project-Root-Relative Paths

> Status: **Designed, not yet built.** This document adds a `@/` prefix to
> filename arguments meaning "from the root of this project", so a path stops
> depending on where the file that spells it happens to live
> ([§3](#3-the-syntax-)). The root is discovered by walking up for the config
> marker the project already has ([§4](#4-what-the-root-is)), the prefix is
> honoured by the built-in filename directives
> ([§5](#5-where--resolves)), and a `@/` that cannot be resolved is an error
> naming the sigil rather than a silent fallback ([§6](#6-when--cannot-resolve)).
> The four design forks were settled with the maintainer and are recorded in
> [§11](#11-decisions); one open fork is flagged in [§12](#12-open-decision-the-file-custom-arg-asymmetry).
>
> The through-line: **moving a file should not break the paths inside it.**

---

## 1. Goal

Let any built-in filename argument name a path from the project root:

```nessemble
.include "@/lib/macros.asm"
.incbin  "@/assets/logo.chr"
.incpng  "@/art/tiles.png"
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
   by the CLI's new `--root <dir>` flag (§8.4) and by the language server's
   workspace folder (§8.5).
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

**In scope — the built-in filename directives**, and nothing else:

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

**Explicitly out of scope**, each for a reason:

- **Custom pseudo-op string arguments**, declared or not. An undeclared string
  argument may be an easing name, a label, or arbitrary text — `.ease "linear"` —
  and the assembler cannot tell which. Rewriting one that merely happens to start
  with `@/` would corrupt data. The *declared* (`file://`) case is a genuine
  candidate and is flagged as an open decision in §12.
- **The script-side file API** (`nessemble-script`'s `resolve`). A script
  receives `base` and resolves against it; teaching it about the root means
  passing the root through the `CustomResolver` signature, which is a public API
  break for a case no one has asked for yet.
- **`.nessemblerc` paths, `--pseudo` mapping paths, and CLI output paths**
  (`-o`, `-l`). These are configuration and command-line arguments, not source;
  a shell already expands paths there, and the root is not yet known when they
  are read.

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
- `file://` semantics are untouched: still stripped before use, still the thing
  that makes a path clickable and a missing file a directive-level error.
- The corpus tests in `tests/corpus/` need no updates.

## 8. Phased plan

Each phase compiles, tests, and ships on its own.

### Phase 0 — Root resolution in core, wired to nothing

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

### Phase 1 — `.include` and `.inestrn`

- Bundle `files`/`dirs`/`paths`/`root` per §4.2; compute the root at the three
  entry points in `lib.rs`.
- `Pre::do_include` calls `resolve_path_arg` instead of `dir.join(&name)`.
- The two i18n messages from §6.
- The *included file's* recorded directory (`dirs`) stays its real on-disk parent,
  so a root-included file's own relative paths still work file-relatively.

### Phase 2 — the media importers

- `Assembler` carries the root; `read_media_file` and `exec_incwav` call
  `resolve_path_arg`.
- The `file://` existence check in `exec_custom` (`assemble.rs:1312`) keeps using
  `cur_dir()` — see §12.

### Phase 3 — integration tests

`crates/nessemble-core/tests/project_root.rs`, modeled on the existing
`file_url.rs` (same `TempTree` helper, same shape):

- `.include "@/lib/defs.asm"` from a file two directories deep.
- `.incbin "@/assets/logo.chr"` from a nested include.
- A `.nessemblerc` above the entry file sets the root; a deeper entry file still
  resolves to it.
- No marker anywhere → the entry file's directory is the root.
- `Options::project_root` overrides a marker that would otherwise win.
- `"file://@/lib/defs.asm"` works and is still existence-checked.
- `"@weird/x.chr"` and `"./@x/y.chr"` are untouched.
- `"@/../outside.bin"` errors, naming the root.
- A byte-identical ROM for a project that uses no `@/`.

### Phase 4 — CLI `--root`

- `--root <dir>` on `assemble` (and `coverage`, which assembles too), plumbed to
  `Options::project_root`.
- Rejected with a clear message if the directory does not exist.
- A CLI test in `crates/nessemble-cli/tests/cli.rs`.
- `docs/src/usage.md` entry.

### Phase 5 — the editor surface

`nessemble-lsp` mirrors the assembler's resolution in four features, all
currently routed through its own `resolve_path_arg` (`lib.rs`, ~line 1535):

- **Document links** — cmd-click a `@/` path and open the right file.
- **Hover** — `path_arg_hover` shows where a `@/` path landed, which answers
  "what root did it pick?" without a build.
- **Completion** — offer `@/` as a completion at the start of an empty filename
  argument, and complete *within* a `@/` path against the root.
- **Root source** — prefer the workspace folder (`workspace_roots`) as the
  override, falling back to the marker walk-up so a single-file editor session
  behaves like the CLI.

### Phase 6 — docs and changeset

- `docs/src/syntax.md`: a "Project-root-relative paths" section next to
  "Declaring a filename argument" (~line 993), and an update to the existing
  block quote about relative resolution so it names the new escape from it.
- `docs/src/editor.md`: mention `@/` alongside the `file://` clickable-path note.
- `editors/` syntax highlighting: check whether the `@/` prefix should be
  distinguished inside a string; low value, do it only if it is a one-line grammar
  change.
- `cargo run -p xtask -- changeset add minor "…"` — a new syntax, so `minor`.

## 9. Risks

- **Silent root drift.** Adding a `.nessemblerc` to a parent directory moves the
  root for every `@/` path below it. That is the intended mechanism, but it means
  an unrelated formatting-config file can change what a source file reads. The
  LSP hover (§8.5) is the mitigation: the root is always inspectable.
- **A `@`-prefixed filename.** A real file named `@something` is unaffected
  (§3), but a directory literally named `@` is only addressable as `./@`. This is
  acceptable and documented.
- **Scope creep toward custom pseudo-ops.** §5 draws the line deliberately; §12
  is where it gets revisited, not the implementation.

## 10. Estimated shape

| Phase | Crates touched | Rough size |
| ----- | -------------- | ---------- |
| 0     | `nessemble-core` | small, self-contained |
| 1     | `nessemble-core`, `nessemble-i18n` | medium (the plumbing refactor) |
| 2     | `nessemble-core` | small |
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
3. **Built-in filename directives only** — the seven in §5. Custom pseudo-op
   arguments, the script file API, and configuration paths are out (§5).
4. **A hard error naming the sigil** when `@/` cannot resolve — no silent
   fallback to the entry directory or the containing directory (§6).

## 12. Open decision: the `file://` custom-arg asymmetry

Decision 3 leaves one rough edge that should be closed one way or the other
before Phase 2 ships.

After this plan, `.include "file://@/lib/defs.asm"` works — `.include` is a
built-in directive — but `.tilemap "file://@/art/map.png"` does **not**. The
declaration says "this string is an input file", the assembler existence-checks
it against `cur_dir()` (`assemble.rs:1312`), and the check fails on the literal
`@/art/map.png`. The author gets `Could not open \`@/art/map.png\`` from a
spelling that works one line above, which is worse than either consistent
outcome.

Three ways to close it:

- **(a) Resolve `@/` in declared arguments.** A `file://`-declared string is
  already, by definition, a path — that is the whole content of the declaration —
  so resolving `@/` in it is consistent rather than a scope expansion. The
  resolver receives the resolved absolute path, which it can already handle
  (absolute paths pass through `nessemble-script`'s `resolve` untouched). This is
  the recommendation.
- **(b) Reject it explicitly.** A dedicated diagnostic: "`@/` is not supported in
  custom pseudo-op arguments". Honest, cheap, and no behavior change — but it
  documents an inconsistency instead of removing one.
- **(c) Leave it.** Status quo: the confusing `Could not open` above.

Undeclared custom arguments stay out under all three options, for the
`.ease "linear"` reason in §5.
