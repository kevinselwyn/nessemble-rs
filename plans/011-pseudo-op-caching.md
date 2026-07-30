# nessemble-rs: A Plan for Caching Custom Pseudo-Instructions

> Status: **Phases 0–2 shipped ([§12](#12-phased-plan)); Phases 3–5 proposed and
> awaiting go-ahead. Decisions settled with the maintainer in
> [§16](#16-decisions); deviations found by building each phase are recorded in
> [§17](#17-as-built).** This document designs caching
> for custom pseudo-op scripts, in **three layers**: per-assembly
> **memoization** so a directive's `custom()` runs once instead of once per
> assembler pass ([§6.1](#61-layer-1--per-assembly-memoization-core)),
> **input recording** so the script host reports every file a script actually
> read ([§6.2](#62-layer-2--recorded-inputs-nessemble-script)), and a
> **persistent on-disk cache** under `~/.nessemble/cache` keyed by the script,
> its arguments, and the freshness of those recorded inputs
> ([§6.3](#63-layer-3--the-on-disk-cache-cli)) — including the script's own
> freshness, so editing a script invalidates every entry that ran it
> ([§7.3](#73-freshness-size--mtime)).
>
> A `file://` prefix on a filename argument ([§4](#4-the-syntax-file)) declares
> an input at the *source* level. It buys three things: a directive-level
> diagnostic when the file is missing, a dependency a tool can see without
> executing anything, and — the reason an author will actually type it — a
> **cmd-clickable path that opens the file in the editor**
> ([§9](#9-the-editor-surface)).
>
> The through-line: a script that crunches a PNG into CHR data should cost that
> crunch **once per change to the PNG**, not twice per build.

---

## 1. Goal

Make a build that uses expensive custom pseudo-ops cheap to repeat. Today this
source:

```nessemble
.tilemap "map.png", "tiles.png"    ; decode two PNGs, match every 8x8 cell
```

pays for the decode and the cell matching **twice on every single assembly** —
once while the assembler sizes the ROM, once while it emits — and pays for it
again, in full, on the next build even when neither PNG has been touched.

After this plan:

- Two passes cost **one** execution (§6.1). Unconditional, no configuration.
- A rebuild with unchanged inputs costs **zero** executions — a couple of
  `stat` calls and a file read (§6.3).
- A rebuild after editing `map.png` — or after editing `tilemap.rhai` itself —
  costs one execution, because the cache recorded both (§6.2, §7.3).
- With the paths declared:

  ```nessemble
  .tilemap "file://map.png", "file://tiles.png"
  ```

  a missing PNG is an assembler error on the directive's own line before the
  script runs (§4), the same error appears as a squiggle in the editor with no
  new editor code (§9.1), and cmd-clicking either path opens the PNG (§9.2).

## 2. Why this is worth doing now

Scripts got expensive. The host grew `decode_png`, the pixel accessors, and the
three cell-matching methods (`find_cell`, `cell_equals`, `nearest_cell`) —
`bank.find_cell` alone is a linear scan of a tile sheet, per cell, and a script
that converts a full screen is running tens of thousands of shade comparisons
through a scripting engine. That is now the slowest thing in an assembly that
uses it, and the assembler runs it **twice**, and every build runs it again.

Three more reasons the timing is right:

- **The double execution is already a latent bug, not just a cost.** `exec_custom`
  (`crates/nessemble-core/src/assemble.rs:1252`) has no `self.pass` guard, so
  `custom()` is invoked on both passes. A script using `rand()` therefore returns
  *different bytes — potentially a different byte count —* on pass 1 than pass 2,
  which mis-sizes the ROM. The docs warn that random output is not reproducible;
  they do not warn that it can corrupt the layout. Memoization (§6.1) closes that
  without a special case for `rand`.
- **The interception point already exists.** `engine(base_dir)`
  (`crates/nessemble-script/src/lib.rs:98`) already shadows rhai-fs's `path`
  function to root relative paths at the source directory, and already owns
  `read_blob` and `decode_png_file`. Every path a script turns into a file
  flows through those three registrations. Recording the set costs a `RefCell`,
  not an architecture.
- **No new dependency is needed.** The workspace has no hashing crate, and the
  freshness rule chosen in §16.4 (mtime + size) means it never needs one; the
  cache *filename* reuses the existing `crc_32`
  (`crates/nessemble-core/src/assemble.rs:1665`) with an exact key comparison
  behind it, so a collision is a miss and never a wrong answer (§7.2).

## 3. Current state

What exists today, and what each piece implies for the design:

| Piece | Where | Implication |
| --- | --- | --- |
| `exec_custom` evaluates args, calls the resolver, writes bytes | `nessemble-core/src/assemble.rs:1252` | No pass guard → two executions per directive per build. Also the one place that sees the arg *list*, so it is where `file://` is handled (§4). |
| `CustomResolver = Box<dyn Fn(&str, &[i64], &[String], &Path) -> Result<Vec<u8>, String>>` | `nessemble-core/src/assemble.rs:292` | A `Fn`, not `FnMut` — memoizing *inside* a resolver would need interior mutability, which is one reason layer 1 belongs in the assembler instead (§6.1). The signature does **not** change (§6.4). |
| `Resolver::{locate, resolve}` — `--pseudo` mapping first, then `~/.nessemble/scripts` | `nessemble-cli/src/custom.rs:31` | Already reads the script source and knows the script's path; already knows `home::config_dir()`. The natural owner of the on-disk cache. |
| `engine(base_dir)` shadows rhai-fs's `path`; owns `read_blob` / `decode_png_file` | `nessemble-script/src/lib.rs:98` | The choke point for input recording (§6.2). |
| `run_with_coverage` on a debugger-instrumented engine | `nessemble-script/src/coverage.rs` | A cache hit executes nothing and records no lines → the on-disk cache must be bypassed under `nessemble coverage` (§8). |
| `lenient_custom_resolver` returns `Ok(Vec::new())` for known directives | `nessemble-core/src/lib.rs:479` | The LSP stubs the *resolver*, not the assembler — which is why §4's check lands in the editor for free (§9.1), and also why the cache does nothing for editor latency (§13). |
| `goto_definition` already jumps `.foo` → its script file | `nessemble-lsp/src/lib.rs:463` | Cmd-click-to-a-file has precedent in this server; §9.2's links are the sibling feature, and `LexKind::String` (`tooling.rs:27`) already exists to key off. |
| `CustomArg::{Int(Expr), Str(String)}` | `nessemble-core/src/ast.rs:201`, parsed at `parse.rs:401` | Core-only — no LSP or formatter fallout. `file://` needs **no AST change** (§4). |
| File-taking directives: `.include`, `.inestrn` (`preprocess.rs:173`); `.incbin`, `.incpng`, `.incpal`, `.incrle`, `.incwav` (`ast.rs:131`–`139`) | core | The seven places besides a custom directive where a string is a path — all of which accept `file://` (§4) and all of which get links (§9.2). |
| `crc_32` | `nessemble-core/src/assemble.rs:1665` | Reused as the cache filename, not as a correctness boundary (§7.2). |

Two argument-evaluation facts that constrain the key (§7.1):

1. **`ints` can differ between passes.** `exec_custom` evaluates each `Int` arg
   with `self.eval(e)`, and a forward-referenced symbol is undefined (value `1`)
   on pass 1. So `.foo my_later_label` legitimately gets different `ints` on the
   two passes. The key must include `ints`, and a memoization *miss* between
   passes for such a directive is correct behavior, not a bug to optimize away.
2. **Relative reads resolve against `base_dir`.** The same directive with the
   same arguments in two different source directories reads two different files,
   so `base_dir` is key material.

## 4. The syntax: `file://` — **as built (Phase 1)**

A filename argument may carry a `file://` prefix to declare that it names an
input file:

```nessemble
.tilemap "file://map.png", "file://tiles.png"   ; custom directive
.incpng  "file://sprites.png"                   ; built-in importer
.include "file://defs.asm"
```

The rules:

- **The script (and the importer) sees the path with the prefix stripped.**
  `texts[0]` is `"map.png"`, exactly as today. Every existing script — including
  the bundled ones — works unchanged, and adopting the declaration is a one-word
  edit in the `.asm` file rather than a script rewrite (§16.2).
- **It is accepted wherever a directive takes a filename** (§16.7): custom
  directives, `.include`, `.inestrn`, `.incbin`, `.incpng`, `.incpal`, `.incrle`,
  `.incwav`. On an importer the prefix is redundant — the argument is already
  known to be a path — but it is harmless, and once every one of those paths is
  clickable (§9.2) an author will write the prefix on all of them. Failing on
  exactly the directives whose whole argument *is* a file would read as a bug.
- **A declared file must exist.** Resolved against the directive's source
  directory (`cur_dir()`, the same base as `.include`, the `.inc*` importers, and
  the script's own relative reads), or used as-is when absolute. If it is not
  there, the assembler reports `could-not-open` (`en-US.ftl:37`, the same
  diagnostic `.incbin` produces) on the directive's line and **does not run the
  script** (§16.3). On an importer this changes nothing — a missing file was
  already that error.
- **It is an argument-level marker, not a URL scheme.** No `http://`, no
  `file://host/`, no percent-decoding. `file://` followed by whatever path the
  filesystem takes; `file:///abs/path` is the absolute spelling that falls out of
  the rule naturally (the third slash starts the path).
- **The AST does not change.** `parse.rs` keeps producing `CustomArg::Str` and
  the importers keep their `String` fields; the prefix is stripped where the path
  is consumed. The formatter and LSP keep seeing the source's string literal
  verbatim, which is what §9 needs.
- **It does not make the cache correct.** Correctness comes from recorded reads
  (§6.2). A declaration buys diagnostics, links, and visibility; a script that
  reads a palette file nobody declared is still cached correctly.

## 5. Semantics: what a cache hit promises

A hit replays a previous run's **emitted bytes**. It replays nothing else. That
is the whole contract, and it has consequences worth stating before the
architecture:

- **Side effects do not replay.** A script that writes a file writes it once,
  not on every build. Such scripts are therefore never cached (§8) — the write
  *is* the point of running them.
- **Errors are memoized within a build, never persisted.** A thrown message is
  reported identically on both passes (collect mode already dedupes, per the
  comment at `assemble.rs:1699`), but a failure is never written to disk: a
  script that failed because an asset was half-written should be retried on the
  next build, not remembered as broken.
- **Bytes are bytes.** The cache stores the resolver's `Vec<u8>` and the
  assembler writes it through the normal `write_all` path, so banking, offsets,
  and the source map behave exactly as they do on a live run.

## 6. Architecture

Three layers, each in the crate that owns the relevant knowledge. They stack:
layer 1 answers from memory, layer 3 answers from disk, layer 2 is what makes
layer 3 trustworthy.

### 6.1 Layer 1 — per-assembly memoization (core) — **as built (Phase 0)**

A map on `Assembler`, consulted by `exec_custom`:

```rust
/// Identifies one custom-directive invocation: name, evaluated arguments, and the
/// site it was written at (file index into `files`, and line).
type CustomKey = (String, Vec<i64>, Vec<String>, u32, u32);

/// What each custom-directive invocation in this run emitted, so a directive's
/// script executes once rather than once per pass.
custom_memo: HashMap<CustomKey, Result<Vec<u8>, String>>,
```

`exec_custom` builds the key from the name, the evaluated `ints`, the stripped
`texts`, and `(cur_file, cur_line)`, and calls the resolver only on a miss. The
map lives for the whole assembly — it is deliberately **not** cleared between
passes, since sharing across passes is the entire point.

**The invocation site is part of the key** (§17.1): two *separate* directives with
identical arguments each resolve once, rather than the second reusing the first's
bytes. That confines the change to the pass duplication — a script that varies its
output on purpose (the `.noise` example in the extending docs) keeps varying
between call sites, exactly as it does today — and it leaves cross-site
deduplication to layer 3, which is keyed without the site and is the layer
designed to decide whether a script is safe to reuse (§8). The base directory does
not appear in the key because the file index implies it.

Why core and not the CLI resolver: it is where the duplication is, it is the only
place with `&mut self`, and it benefits every embedder — including
`nessemble-wasm`, which calls `nessemble_script::run` directly
(`nessemble-wasm/src/lib.rs:229`) and has no filesystem to cache to. This layer
is unconditional and unconfigurable; `--no-cache` (§10) does not disable it,
because it is not a cache of anything outside the current process and switching
it off would restore the ROM-sizing bug in §2.

### 6.2 Layer 2 — recorded inputs (`nessemble-script`)

`engine(base_dir)` gains a recorder — an `Rc<RefCell<BTreeSet<PathBuf>>>` — that
the three path-taking registrations push into: the shadowed `path` (which every
rhai-fs `open_file`/`open_dir` call routes through), `read_blob`, and
`decode_png_file`. Absolute, post-resolution paths, so the record means the same
thing regardless of the directive's directory.

The crate grows one function and keeps the old one:

```rust
/// What one `custom()` invocation produced, and what it touched.
pub struct RunOutcome {
    pub bytes: Vec<u8>,
    /// Absolute paths the script resolved through the host's file API.
    pub inputs: Vec<PathBuf>,
    /// Whether this run's bytes may be cached across builds (§8).
    pub cacheable: bool,
}

pub fn run_with_inputs(source: &str, ints: &[i64], texts: &[String], base_dir: &Path)
    -> Result<RunOutcome, String>;
```

`run` stays exactly as it is, a thin wrapper that discards the extra data, so
`nessemble-wasm` and the existing tests are untouched.

Recording is **observational**: it captures what the script did on *this* run,
with *these* arguments. A script that reads `palette_a.png` when `ints[0]` is 0
and `palette_b.png` otherwise records the right file each time, because the
arguments are part of the key.

The one route it cannot see is `import`, which rhai resolves through its own
`FileModuleResolver` rather than the host's `path` hook. Scripts that `import`
are therefore not cached at all (§8, §16.8) — refused rather than recorded
incompletely.

### 6.3 Layer 3 — the on-disk cache (CLI)

`Resolver::resolve` (`nessemble-cli/src/custom.rs:49`) becomes:

1. `locate` the script and `stat` it — its path, size, and mtime are key material
   and freshness material both (§7.3).
2. Build the key material, derive the entry filename (§7.2), and try to load.
3. **On a hit** — the stored key material matches exactly *and* every recorded
   input still matches on size and mtime — return the stored bytes. No engine, no
   compile.
4. **On a miss** — read the source, `run_with_inputs`, and, if the outcome is
   cacheable, write the entry (bytes + key material + input records).
5. Bump the entry's mtime on a hit, so eviction (§16.10) is least-recently-used
   rather than least-recently-written.

Entries live under `~/.nessemble/cache/pseudo/<xx>/<key>.{json,bin}` —
`home::config_dir()` (`nessemble-cli/src/home.rs:14`), which already owns
`scripts/` and `locales/`; `<xx>` is the first two hex digits of the filename, to
keep directories small. Metadata is JSON (`serde`/`serde_json` are already
workspace dependencies) so a stale entry can be diagnosed with `cat`; bytes are a
sibling `.bin` so nothing needs base64. Both are written to a temp file and
`rename`d into place, so two concurrent `nessemble` processes cannot interleave a
half-written entry.

### 6.4 What does not change

- **`CustomResolver`'s signature.** `file://` stripping and the existence check
  happen in `exec_custom` before the call; input recording and the disk cache
  live inside the CLI closure. Core never learns what a dependency is.
- **The `.name = path` mapping grammar.** `parse_pseudo_mapping` is shared with
  the LSP; caching adds no annotation to it (§16.3's rejected alternative).
- **The script-facing API.** No new function a script must call, no marker a
  script author must add for the common case. A pure script is cached because it
  is pure, not because it said so.
- **The AST**, and therefore the formatter's output (§4).
- **`nessemble-wasm`.** No filesystem, no cache; it gets layer 1 for free.

## 7. The cache key and invalidation

### 7.1 Key material

Everything that can change the bytes, stored verbatim in the entry:

| Field | Why |
| --- | --- |
| Cache format version | An entry written by an older layout is a miss, not a misread. |
| `nessemble` version (`CARGO_PKG_VERSION`) | Host helpers define the output. If `nes_shade`'s thresholds or `find_cell`'s tie-breaking ever change, every entry must miss. Cheap insurance; a release invalidates the cache. |
| Directive name | `.foo` and `.bar` may map to the same script with different meaning. |
| `ints`, `texts` (post-strip) | The arguments. `ints` differing across passes is expected (§3). |
| `base_dir` (absolute) | Relative reads resolve against it. |
| **Script path (absolute), size, mtime** | The script's identity — see §7.3. |

Absolute paths make entries machine-specific and checkout-specific. That is the
right trade for a local cache and is what makes one shared
`~/.nessemble/cache` safe across projects: two projects' identical-looking
directives cannot collide, because their `base_dir`s differ.

### 7.2 The filename, and why a weak hash is safe here

The entry filename is `crc_32` of the serialized key material, in hex. CRC-32 is
a checksum, not a digest — but it is not being trusted as one. The entry stores
the **full key material**, and a load compares it **byte-for-byte**. A collision
therefore produces a *mismatch*, which is handled as a miss (and the entry is
overwritten). The cache cannot return another invocation's bytes; the worst a
collision costs is one script execution.

This is what lets the whole feature ship with **no new dependency**.

### 7.3 Freshness: size + mtime

Every dependency record — each recorded input **and the script itself** — is
`(absolute path, byte size, mtime as (secs, nanos))`. An entry is valid when
every record still matches. No content hashing (§16.4).

Editing a script therefore invalidates every entry that ran it, which is the
second half of what this plan is for. Four ways a script can change, and how each
is caught:

| Change | Caught by |
| --- | --- |
| The script file is edited | Its size/mtime record (§7.1). |
| `pseudo.txt` re-points `.foo` at a different script | The script **path** is key material, so the key itself changes. |
| A bundled script is replaced by `nessemble scripts` | Same as an edit — the installed file's mtime moves. |
| A helper module the script `import`s is edited | **Not** caught by freshness — such scripts are never cached at all (§8), so there is nothing to invalidate. |

The general behavior of the rule:

- A changed asset or script changes its mtime → miss → re-run. The normal case.
- A `git checkout` or `touch` rewrites mtimes without changing content → miss →
  a needless re-run. Costs time, never correctness.
- A deleted or unreadable dependency → miss.

### 7.4 The blind spot, named

Size + mtime cannot see an edit that **preserves the byte size and lands inside
the same mtime tick** as the recorded one. Then the cache serves stale bytes and
the ROM is silently wrong. Three things keep this narrow:

- Nanosecond mtimes on every filesystem nessemble realistically runs on (ext4,
  APFS, NTFS, btrfs) make same-tick collisions require sub-microsecond timing.
- Editors and asset pipelines write whole files, which bumps mtime.
- `--no-cache` (§10) and `nessemble cache clear` are the documented escape hatch,
  and the docs will say plainly what the rule is rather than implying the cache
  is content-addressed.

It is recorded here as a **known, accepted limitation** (§16.4), so that a future
"why did my ROM not change" report is diagnosed in minutes instead of days. Note
that it applies to scripts as well as assets (§16.9): the one dependency a
developer edits ten times an hour is under the same rule as a PNG.

## 8. Uncacheable runs

Some runs must not be cached at all. They are detected by a **static scan of the
compiled AST** for a small deny-list of impure host functions, done once per
compile (the AST is already in hand, and `internals` is already an enabled rhai
feature on the coverage path):

| Trigger | Why it cannot be cached |
| --- | --- |
| `rand`, `rand_float`, `rand_bool`, array `shuffle` / `sample` | Output is supposed to differ per build. Caching would silently freeze a script the docs describe as non-reproducible. |
| A write-mode `open_file` (one-arg form, or a mode containing `w`, `a`, or `+`), and `File#write` | The file write is the observable effect; a hit would skip it. |
| `open_dir` and directory iteration | Output depends on a directory's *listing*, which the record-a-file freshness model does not describe. |
| `import` | A module's source is invisible to both the script's own identity and the recorder (§6.2). Refused in v1 (§16.8). |

The scan is deliberately **conservative**: a `rand()` in a branch that never runs
still marks the script uncacheable. Being wrong in this direction costs a script
execution; being wrong in the other direction costs a wrong ROM.

Two more bypasses, both structural rather than detected:

- **`nessemble coverage`** never uses the disk cache. Coverage needs the lines to
  actually execute (§3); a hit records nothing. Layer 1 memoization stays on —
  coverage records line *sets*, not hit counts, so one execution per distinct
  invocation is exactly as informative as two.
- **`--no-cache`** (§10) skips read and write.

## 9. The editor surface — **as built (Phase 2)**

Declaring a path should pay off while you are typing, not only at build time.
Three surfaces, all keyed off `LexKind::String` tokens in a file-taking
directive's argument list, all sharing one resolver helper (`base dir + path →
absolute path`) with the assembler's rule from §4.

### 9.1 The missing-file diagnostic comes free

Because the existence check lives in `exec_custom` **before** the resolver is
called (§4), and the LSP's `diagnose_*` path runs the real assembler in collect
mode with only the *resolver* stubbed (`lenient_custom_resolver`), the squiggle
appears in the editor with **no new LSP code, no filesystem access in the LSP,
and no second implementation of the check**. It is automatically consistent with
`.incbin "missing.chr"`, which the LSP already reports the same way.

Severity is **error**, matching the assembler exactly (§16.6) — one behavior, one
code path.

### 9.2 Document links

A `textDocument/documentLink` provider (a new capability; the server has none
today) returns a link for every resolvable path argument:

- **What is linked:** the path text *inside* the quotes and *after* any `file://`
  prefix, so the underline covers the path and not the marker or the quotes.
- **Where:** `file://` arguments of custom directives, and the filename arguments
  of `.include`, `.inestrn`, `.incbin`, `.incpng`, `.incpal`, `.incrle`,
  `.incwav` — the seven directives whose argument is unambiguously a path (§16.7).
  Cmd-clicking `.include "defs.asm"` is arguably the bigger day-to-day win than
  the feature that motivated it.
- **When not:** an unresolvable path gets no link (§16.6) — it gets the §9.1
  error instead. Non-`file:` document URIs (untitled buffers) get no links, since
  there is no directory to resolve against.

Links rather than `goto_definition`: the path renders **underlined**, so it is
discoverably clickable instead of something the user has to guess at, and it is
the request LSP defines for exactly this. The existing `.foo` → script jump at
`lib.rs:463` stays as it is.

### 9.3 Hover and completion

- **Hover** over a path argument shows the **resolved absolute path** — the
  answer to "is it finding the file I think it is?" — plus the file's size, and
  for a PNG its pixel dimensions, which `nessemble-media` can already decode.
- **Completion** inside a string argument offers filenames from the resolved
  directory, **filtered by what the directive can use** (§16.11): `.incpng` →
  `*.png`, `.incwav` → `*.wav`, `.include`/`.inestrn` → `*.asm` / `*.inc`, a
  `file://` argument on a custom directive → everything, since a script may read
  any format. A per-directive extension table, plus `/` added to the completion
  trigger characters (today only `.`, `lib.rs:2695`).

## 10. CLI surface

- **`--no-cache`** on the assemble path: no reads, no writes, no entry mtime
  bumps. The escape hatch §7.4 promises, and the flag a bug report gets asked to
  try first.
- **`nessemble cache info`** — the cache directory, entry count, total size, and
  the oldest and newest entry. Enough to answer "is it even being used".
- **`nessemble cache clear`** — delete every entry, report how many and how much.

Both subcommands sit alongside `Scripts`, `Reference`, `Lsp`, `Format`, `Lint`,
`Coverage` in `Command` (`nessemble-cli/src/main.rs:103`). New i18n keys for the
cache messages go in `en-US.ftl`; the missing-declared-file diagnostic reuses the
existing `could-not-open` (§4).

## 11. Docs

- **`docs/src/extending.md`** — a `## Caching` section after "Random numbers":
  what is cached, the `file://` declaration, the size+mtime freshness rule stated
  outright for assets *and* scripts (including §7.4's limitation), what makes a
  script uncacheable and why, and `--no-cache`. The existing "Random output is
  not reproducible" note gains a sentence saying such scripts are never cached,
  which is why they keep working.
- **`docs/src/syntax.md`** — `file://` on a filename argument, in the directive
  reference rather than only in the scripting page, since it is accepted by the
  built-in importers too (§4).
- **`docs/src/editor.md`** — the three new surfaces in the `## Features` list
  (clickable paths, path hover, path completion).
- **`docs/src/usage.md`** — `--no-cache`, and `cache info` / `cache clear`
  alongside the other subcommands.
- **A changeset per shipped phase**, per `CLAUDE.md` — the changeset body is the
  changelog line, and `CHANGELOG.md` is never touched by hand.

## 12. Phased plan

Each phase is independently shippable, independently revertible, and useful on
its own. The editor work comes **before** the cache (§16.12): the clickable path
is the visible reward for writing `file://`, so authors have a reason to adopt
the prefix before the payoff it was invented for exists.

- **Phase 0 — memoization (core). — shipped.** `custom_memo` in `Assembler`,
  consulted by `exec_custom`. No new syntax, no disk, no configuration. Halves
  script work in every build and fixes the `rand` pass-skew in §2. One deviation
  from the design as written, recorded in §17.1. Tests in
  `crates/nessemble-core/tests/custom_memo.rs`.
- **Phase 1 — `file://` (core). — shipped.** Prefix stripping for custom
  directives and the seven file-taking directives, the existence check, the
  `could-not-open` diagnostic. Useful with no cache and no editor at all — it
  turns a script's confusing throw into a diagnostic on the right line, and
  (§9.1) lights up in the editor for free. Notes in §17.2; tests in
  `crates/nessemble-core/tests/file_url.rs`.
- **Phase 2 — the editor surface (LSP). — shipped.** Document links, path hover,
  filtered path completion (§9.2–9.3). Notes in §17.3; tests in the
  `nessemble-lsp` test module.
- **Phase 3 — input recording (`nessemble-script`).** The recorder in `engine`,
  `RunOutcome`, `run_with_inputs`, the §8 static impurity scan. No behavior
  change yet — this phase only *reports*. *Review this hardest: everything in
  Phase 4 trusts that the recorded set is complete.*
- **Phase 4 — the on-disk cache (CLI).** Key material, `crc_32` filenames with
  exact comparison, entry read/write with atomic rename, freshness checks for
  inputs and the script (§7.3), the coverage bypass, `--no-cache`.
- **Phase 5 — cache management and docs.** `nessemble cache info` / `clear`,
  eviction (§16.10), and the docs in §11.

Phases 0–1 are small and land immediately. Phase 3 is the careful one. Phase 4 is
the payoff.

## 13. Explicitly not in v1

Boundaries as decisions, not oversights:

- **Content hashing.** Settled against in §16.4. If §7.4's blind spot ever bites
  in practice, adding a digest is a change to one freshness function plus a cache
  format version bump — the design is deliberately shaped so that swap is cheap.
- **Caching the built-in importers' work.** `.incbin`, `.incpng`, `.incwav` are a
  `read` and a decode, not a scripting engine; they are not the bottleneck. (They
  do get `file://` and links, which is a separate matter.)
- **Making `import`-using scripts cacheable** by installing a recording module
  resolver (§6.2, §16.8).
- **An LSP that runs scripts.** The editor uses `lenient_custom_resolver` and
  sees zero bytes from every custom directive, so a custom directive's *emitted
  bytes* are invisible in the editor no matter how fast they get. Making it run
  them for real is a genuinely interesting change — the cache is what would make
  it affordable — but it is its own plan, with its own answers about trust and
  timeouts.
- **Links in `pseudo.txt`.** Making `.foo = foo.rhai` clickable means teaching
  the LSP to lex a second document type, which is a new surface rather than a
  wider scan. (`goto_definition` on `.foo` already reaches the script from the
  `.asm` side.)
- **A "create the missing file" quick fix** on the §9.1 diagnostic.
- **A project-level dependency graph / watch mode.** `file://` declarations are
  the raw material for "rebuild when this asset changes", and deliberately so;
  the scheduler that would consume them is a separate feature.
- **Caching across machines** (a shared or CI-uploaded cache). Absolute paths in
  the key (§7.1) rule it out by construction. `--cache-dir` pointed at a mounted
  volume would be the first step, and is easy to add later.

## 14. Testing strategy

- **Memoization (Phase 0).** A resolver that counts invocations: a directive
  called once in the source runs its script exactly once across both passes; two
  directives with different args run twice; a directive with a forward-referenced
  symbol arg runs twice (different `ints`, §3) and both runs are correct. A
  script returning `rand`-derived bytes now emits the *same* bytes on both
  passes — a regression test for the sizing bug.
- **`file://` (Phase 1).** Prefix stripped in `texts`; stripped for each of the
  seven file-taking directives; absolute `file:///…` form; missing declared file
  yields `could-not-open` on the right line *and* the script never runs (counting
  resolver again); a text arg that merely *contains* `file://` mid-string is
  untouched; the AST is byte-identical to the unprefixed parse; a `diagnose_*`
  call reports the missing file with no resolver of its own (§9.1).
- **Editor (Phase 2).** A link per resolvable path, with the range covering the
  path and not the `file://` prefix or the quotes; no link for a missing file; no
  link in an untitled buffer; links for each of the seven importers; hover shows
  the resolved absolute path (and a PNG's dimensions); completion inside
  `.incpng "` offers only `*.png` while a `file://` custom arg offers everything.
- **Recording (Phase 3).** A script reading via `open_file`, `read_blob`, and
  `decode_png_file` records all three absolute paths; a script reading nothing
  records nothing; each §8 deny-list trigger flips `cacheable` to false.
- **The cache (Phase 4).** In a `TempDir` with `HOME` pointed at it: cold run
  executes and writes an entry; second run hits and does not execute; touching an
  input misses; **rewriting the script misses**; **re-pointing the mapping at a
  different script misses**; a different `base_dir` with identical args misses; a
  corrupted/truncated entry misses rather than panics; a forged entry whose
  stored key material disagrees misses (§7.2); `--no-cache` neither reads nor
  writes; `nessemble coverage` still reports script lines with a warm cache.
- **CLI (Phase 5).** `cache info` on an empty and a populated cache; `cache
  clear` empties it; both are covered in `crates/nessemble-cli/tests/cli.rs`
  beside the existing `custom_pseudo_*` tests.

Every phase must leave `cargo fmt --all --check`, `cargo clippy --all-targets
--all-features -- -D warnings`, and `cargo test --all-features` green — the CI
gate in `.claude/README.md` runs exactly those.

## 15. Risks & mitigations

| Risk | Mitigation |
| --- | --- |
| **Stale bytes from the mtime blind spot (§7.4)**, for a script as much as an asset. | Nanosecond mtimes; `--no-cache` and `cache clear` documented as the first thing to try; the freshness rule stated plainly in the docs rather than implied to be stronger. |
| **An unrecorded input** — a script reaching the filesystem by a route that does not pass through the host's three registrations. | The §8 deny-list covers the known routes (`open_dir`, `import`) by refusing to cache. A new host function that takes a path must record it; the recorder lives beside the registrations so the omission is visible in review, and Phase 3's tests assert coverage of each existing route. |
| **A cached side effect.** | Scripts that write are never cached (§8), detected statically and conservatively. |
| **Cache growth.** | Size cap plus LRU eviction (§16.10) and `cache clear`. Entries are ROM-fragment sized — kilobytes, not megabytes. |
| **Concurrent builds.** | Temp file + atomic `rename`; a partially-written entry is never visible, and a losing writer simply replaces an equivalent entry. |
| **A red editor for generated assets** — a `file://` path produced by a build step that runs before assembly but after editing is a hard error (§16.6) and squiggles all day. | The declaration is opt-in per argument: a path that may legitimately not exist yet is simply written without the prefix, and the script keeps its own fallback. Called out in the docs beside the syntax. |
| **Link/hover cost on large files.** | Both are computed from the existing lexer token stream on request, not on every keystroke, and touch the filesystem only to `stat` a candidate path. |
| **Debuggability** — "is this a cache bug?" | JSON metadata is human-readable; `cache info` reports state; `--no-cache` isolates the cache from the question in one flag. |
| **Behavior change from memoization** (a script with side effects runs once per build instead of twice). | Such scripts were already broken across passes; running once is the correct semantics, and it is called out in the Phase 0 changeset. |

## 16. Decisions

### Settled with the maintainer

1. **Dependencies are both recorded and declarable.** The cache's correctness
   rests on *recording* the paths a script actually opened, through the host
   registrations that already exist (§6.2); `file://` (§4) is a complementary
   *declaration* that buys a directive-level diagnostic, a dependency visible
   without executing anything, and the editor surfaces in §9. *Rejected:*
   declarations alone, which is silently wrong for any script that opens a file
   it was not handed — a script given a tilemap that also reads a palette by
   convention would cache against a partial input set; and recording alone, which
   leaves the assembler unable to say anything about a directive's inputs without
   running it.
2. **Caching is layered: memoization within a build, plus a persistent cache
   across builds.** Layer 1 is unconditional and fixes the pass-skew bug; layer 3
   is what makes an unchanged rebuild free. *Rejected:* memoization only, which
   leaves the motivating "crunch this PNG on every build" cost untouched; and the
   disk cache only, which would technically absorb the double execution as a
   cache hit but would leave the ROM-sizing bug in place whenever the cache is
   disabled or unwritable.
3. **On by default, with automatic opt-out.** A run proves itself uncacheable —
   randomness, file writes, directory listings, `import` (§8) — and the coverage
   path bypasses the cache structurally. `--no-cache` is the manual override.
   *Rejected:* an annotation in the `--pseudo` mapping file, which changes a
   grammar shared with the LSP and asks whoever wires up a script to know whether
   it is pure; an in-script marker (`; @nessemble-cache`, `fn cacheable()`), which
   is a clean fit for plan 009's registry but means **no existing script benefits
   until it is edited**; and opt-in behind `--cache`, which helps only the people
   who already know the feature exists.
4. **Freshness is size + mtime; no content hashing, no new dependency.** The
   cache reuses `crc_32` for entry filenames with an exact key comparison behind
   it (§7.2), so nothing about correctness rests on a weak checksum. *Rejected:*
   adding `sha2` for content hashing, which would close §7.4's blind spot at the
   cost of the workspace's first hashing dependency; a hand-rolled 128-bit
   non-crypto hash; and keying anything on `crc_32` *without* the exact
   comparison. **The accepted cost is §7.4** — a size-preserving edit inside one
   mtime tick serves stale bytes — and the design keeps the swap to a digest
   cheap (one function plus a format-version bump) should that ever matter.
5. **The cache lives in `~/.nessemble/cache/`.** It matches the existing
   `~/.nessemble` convention (`scripts/`, `locales/`), needs no `.gitignore`
   entry, and is safe to share across projects because absolute paths are key
   material (§7.1). *Rejected:* a per-project `.nessemble-cache/`, which needs a
   base directory the assembler has no concept of and a gitignore entry in every
   project; and shipping `--cache-dir` in v1 before anyone has asked for a
   mounted CI cache.
6. **A `file://` argument reaches the consumer with the prefix stripped, and a
   missing one is a hard error in the assembler and the editor alike.** One
   behavior, one code path, matching `.incbin "missing.chr"`; the editor's copy
   is free (§9.1). An unresolvable path also gets no document link — the error is
   the feedback. *Rejected:* passing the prefix through verbatim, which breaks
   every script the moment a caller adds a declaration; warning-then-running,
   which needs a new warning category and an ambiguous answer about whether such
   a run is cacheable; and a configurable lint rule in the editor only, which
   means a second implementation of the check and a squiggle whose severity
   disagrees with the build's.
7. **`file://` is accepted wherever a directive takes a filename**, not only on
   custom directives: `.include`, `.inestrn`, `.incbin`, `.incpng`, `.incpal`,
   `.incrle`, `.incwav`. Redundant there, harmless, and the alternative is that
   the prefix fails on exactly the arguments that most obviously *are* files —
   which, once all of them are clickable, is what an author will try first.
   *Rejected:* custom directives only, which keeps the marker's meaning tighter
   ("this string is a path" is news only for a custom directive) at the cost of a
   confusing error; and accept-but-warn.
8. **A script that `import`s a module is not cached, in v1.** Rhai resolves
   modules through its own `FileModuleResolver`, not the host's `path` hook, so
   the recorder cannot see them and freshness cannot cover them. Refusing is
   correct by construction and costs nothing. *Rejected:* installing a recording
   module resolver, which makes the shared-helper pattern cacheable but is a real
   component with its own base-path decision (rhai's differs from our source-dir
   rooting) — the natural v2 (§13); and recording one level deep, which is a
   partially-recorded dependency set, the exact shape of bug that makes people
   distrust a cache.
9. **The script itself is checked by size + mtime, like every other
   dependency.** One freshness rule to document and reason about. *Rejected:*
   storing the script's source in the entry and comparing it byte-for-byte —
   exact, zero-dependency, and cheap at script sizes, and it would close §7.4
   precisely where edits are most frequent — and hashing the script alone. The
   accepted cost is that a same-length script edit inside one mtime tick is
   missed; the mitigation is `--no-cache`.

### Still the author's call

Reversible, recorded so they are decisions rather than defaults. Say the word on
any of them.

10. **Eviction is a total-size cap with LRU by entry mtime, enforced
    opportunistically on write; default 256 MB.** *Alternative:* an entry-count
    cap, or no eviction at all with `cache clear` as the only reclaim —
    defensible given how small entries are, but a cache with no bound is a
    support burden.
11. **Completion filters by directive** (`.incpng` → `*.png`, and so on, §9.3).
    *Alternative:* offering every file, which is never wrong about a `.PNG` or a
    `.s` include but is noisy in an assets directory; or filtering with all files
    as a second tier, which is the same table plus sort-order care.
12. **The editor surface ships as Phase 2, before the cache.** *Alternative:*
    last, which gets the performance win — the actual motivation — reviewed and
    merged soonest and leaves the LSP crate untouched meanwhile.
13. **A declared `file://` file that the script never reads does not invalidate
    the entry.** Recorded reads are the truth; a declaration the run did not use
    is not a dependency. *Alternative:* treat declarations as dependencies too,
    which is more intuitive ("I said it was an input") but requires widening
    `CustomResolver` to carry declarations into the resolver — the one signature
    this plan otherwise leaves alone (§6.4).
14. **Impurity is detected statically from the compiled AST, conservatively.**
    *Alternative:* runtime detection through rhai's debugger hook, which is exact
    (a `rand()` in a dead branch would not disqualify the script) but means
    enabling the `debugging` feature for ordinary runs, paying its cost on every
    execution to slightly widen what is cacheable.
15. **Errors are never persisted** (§5) — only memoized within a build.
    *Alternative:* caching failures too, which would make a broken build's second
    run marginally faster and its recovery confusing.
16. **`--no-cache` does not disable layer 1 memoization.** *Alternative:* a flag
    that forces every invocation to execute, useful for diagnosing a script whose
    output legitimately varies — but that is the behavior §2 identifies as a bug.

## 17. As built

Deviations found by building a phase, recorded here rather than by quietly
rewriting the design above.

### 17.1 The memo key includes the invocation site (Phase 0)

**Designed:** `(name, ints, texts, base_dir)` (§6.1, as originally written).
**Built:** `(name, ints, texts, cur_file, cur_line)`.

The designed key deduplicates across *call sites*, not only across passes: two
`.noise 16` directives in one file would have shared one resolution and emitted
**identical** bytes, where today they emit different ones. That is a change to
what a program assembles to, in a direction the extending docs specifically
advertise ("a `.noise` directive that emits `\1` random bytes"), and it is a
separate question from the pass skew this phase exists to fix.

Adding the site makes Phase 0 a strict improvement with **no other observable
change**: same site, same arguments, two passes → one execution (the bug fixed);
different sites → one execution each (today's behavior preserved). Cross-site
reuse is left to layer 3, which is keyed without the site and already has the
machinery to decide whether a script is safe to reuse at all (§8) — a rand-using
script is uncacheable there, so it never collapses two call sites.

The base directory dropped out of the key: `cur_file` indexes `dirs`, so the file
index already implies it.

Two smaller notes from the same phase:

- **A `Result` is memoized, not just the bytes.** A resolver error is still
  reported on every pass that reaches the directive (collect mode dedupes,
  `assemble.rs:1699`), but the resolver is asked once. Covered by
  `a_resolver_error_is_reported_once_and_asked_once`.
- **The pass-1 placeholder for an undefined symbol is `1`**, so a forward
  reference whose resolved value *is* 1 memoizes legitimately between passes —
  the two invocations really are identical as far as the resolver can tell. The
  test for pass-dependent arguments pads the label to 3 to avoid pinning that
  coincidence.

### 17.2 `file://` landed at four choke points, and added no importer check (Phase 1)

Stripping did not need seven call sites. Every media importer reads through
`Assembler::read_media_file`, so stripping there covers `.incbin`, `.incpng`,
`.incpal`, `.incrle` and `.incwav` at once; `.include` and `.inestrn` share
`Pre::do_include`. The only extra sites are the three importer error messages,
which name the **bare** path rather than echoing the declaration back.

Two notes on what the phase deliberately does *not* do:

- **No new existence check for the importers.** A missing `.incbin` file was
  already `could-not-read`/`could-not-open`; the declaration changes nothing
  there. The check §4 describes exists only for custom pseudo-ops, which are the
  only directives where the assembler could not otherwise tell a string is a path.
- **The check runs on both passes.** It is one `Path::exists` per declared
  argument per pass, ahead of the memo lookup, which is far cheaper than
  arranging to remember that it already ran.

`strip_file_url` and `FILE_URL_PREFIX` are **public** in `nessemble-core`, not
private helpers: the language server needs exactly the same rule to compute a
link's range (§9.2), and two implementations of "what counts as a declaration"
would drift.

### 17.3 Path arguments include the empty one, and one filter is looser than §16.11 (Phase 2)

**`path_args` reports an argument whose path is empty.** A half-typed `"` is an
empty path, and that is precisely when completion has to fire; the first version
skipped empties and completion silently fell through to offering mnemonics inside
a string. Links and hover reject them instead — an empty path resolves to the
containing directory, which is not a file, so the link filter already excludes it
and hover checks explicitly.

**Three directives are not extension-filtered.** §16.11 settled on filtering
completion by directive, and `.incpng`/`.incpal` (PNG), `.incwav` (WAV) and
`.include`/`.inestrn` (`asm`, `inc`, `s`) do. But `.incbin` and `.incrle` take an
*arbitrary* blob, and a `file://` argument on a custom pseudo-op can be any format
its script understands — a guessed extension list there would hide the author's
own naming for no benefit, so those offer every file. Directories are always
offered regardless, since they are on the way to a file.

Two smaller notes:

- **PNG dimensions come from the IHDR, not a decode.** Hover fires on every mouse
  pause, and `nessemble-media`'s decoder would expand a full-resolution image to
  learn two numbers. Reading the 24-byte header keeps the LSP crate's dependency
  list unchanged as well.
- **A trailing comma continues the argument list.** A directive's arguments end at
  the line break *except* when the line ends in `,`, matching the parser's
  continuation rule — so a declared path on a continuation line still links.
