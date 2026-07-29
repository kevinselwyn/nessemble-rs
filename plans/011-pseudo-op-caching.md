# nessemble-rs: A Plan for Caching Custom Pseudo-Instructions

> Status: **Proposed — awaiting go-ahead. Decisions settled with the maintainer
> in [§15](#15-decisions); nothing built yet.** This document designs caching
> for custom pseudo-op scripts, in **three layers**: per-assembly
> **memoization** so a directive's `custom()` runs once instead of once per
> assembler pass ([§6.1](#61-layer-1--per-assembly-memoization-core)),
> **input recording** so the script host reports every file a script actually
> read ([§6.2](#62-layer-2--recorded-inputs-nessemble-script)), and a
> **persistent on-disk cache** under `~/.nessemble/cache` keyed by the script,
> its arguments, and the freshness of those recorded inputs
> ([§6.3](#63-layer-3--the-on-disk-cache-cli)). A `file://` prefix on a text
> argument ([§4](#4-the-syntax-file)) declares an input at the *source* level:
> it buys a directive-level diagnostic for a missing asset and a dependency a
> tool can see without executing anything.
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
- A rebuild after editing `map.png` costs one execution, because the cache
  recorded that the run read `map.png` (§6.2).
- `.tilemap "file://map.png", "file://tiles.png"` additionally reports a missing
  PNG as an assembler diagnostic on the directive's own line, before the script
  runs (§4).

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
  freshness rule chosen in §15.4 (mtime + size) means it never needs one; the
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
| `lenient_custom_resolver` returns `Ok(Vec::new())` for known directives | `nessemble-core/src/lib.rs:479` | The **LSP never runs scripts**. This plan speeds up CLI builds; it does not speed up the editor, and cannot until that changes (§12). |
| `CustomArg::{Int(Expr), Str(String)}` | `nessemble-core/src/ast.rs:201`, parsed at `parse.rs:401` | Core-only — no LSP or formatter fallout. `file://` needs **no AST change** (§4). |
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

## 4. The syntax: `file://`

A text argument may carry a `file://` prefix to declare that it names an input
file:

```nessemble
.tilemap "file://map.png", "file://tiles.png"
.embed   "file://logo.chr"
```

The rules:

- **The script sees the path with the prefix stripped.** `texts[0]` is
  `"map.png"`, exactly as today. Every existing script — including the bundled
  ones — works unchanged, and adopting the declaration is a one-word edit in the
  `.asm` file rather than a script rewrite (§15.2).
- **A declared file must exist.** Resolved against the directive's source
  directory (`cur_dir()`, the same base as `.include`, the `.inc*` importers, and
  the script's own relative reads), or used as-is when absolute. If it is not
  there, the assembler reports `could-not-open` (`en-US.ftl:37`, the same
  diagnostic `.incbin`/`.incpng` produce) on the directive's line and **does not
  run the script** (§15.3).
- **It is an argument-level marker, not a URL scheme.** No `http://`, no
  `file://host/`, no percent-decoding. `file://` followed by whatever path the
  filesystem takes; `file:///abs/path` is the absolute spelling that falls out of
  the rule naturally (the third slash starts the path).
- **The AST does not change.** `parse.rs` keeps producing `CustomArg::Str`; the
  prefix is recognized in `exec_custom` while it builds `texts`. The formatter
  and LSP keep seeing the source's string literal verbatim, which is what they
  want, and a future "complete a path inside a `file://` string" feature needs no
  parser work.
- **It does not make the cache correct.** Correctness comes from recorded reads
  (§6.2). A declaration buys diagnostics and visibility; a script that reads a
  palette file nobody declared is still cached correctly.

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

### 6.1 Layer 1 — per-assembly memoization (core)

A map on `Assembler`, consulted by `exec_custom`:

```rust
/// Every distinct custom-directive invocation in this assembly, and what it
/// emitted. Keyed so that pass 1 and pass 2 share one execution of a script
/// whose arguments resolved identically — which is all of them except the ones
/// with forward-referenced symbols (§3).
custom_memo: HashMap<(String, Vec<i64>, Vec<String>, PathBuf), Result<Vec<u8>, String>>,
```

`exec_custom` builds the key from the name, the evaluated `ints`, the stripped
`texts`, and `cur_dir()`, and calls the resolver only on a miss. The map lives
for the whole assembly — it is deliberately **not** cleared between passes, since
sharing across passes is the entire point.

Why core and not the CLI resolver: it is where the duplication is, it is the only
place with `&mut self`, and it benefits every embedder — including
`nessemble-wasm`, which calls `nessemble_script::run` directly
(`nessemble-wasm/src/lib.rs:229`) and has no filesystem to cache to. This layer
is unconditional and unconfigurable; `--no-cache` (§9) does not disable it,
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

### 6.3 Layer 3 — the on-disk cache (CLI)

`Resolver::resolve` (`nessemble-cli/src/custom.rs:49`) becomes:

1. `locate` the script, `stat` it (§7.1 uses its size and mtime as its identity).
2. Build the key material, derive the entry filename (§7.2), and try to load.
3. **On a hit** — the stored key material matches exactly *and* every recorded
   input still matches on size and mtime — return the stored bytes. No engine, no
   compile.
4. **On a miss** — read the source, `run_with_inputs`, and, if the outcome is
   cacheable, write the entry (bytes + key material + input records).
5. Bump the entry's mtime on a hit, so eviction (§15.7) is least-recently-used
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
  the LSP; caching adds no annotation to it (§15.3's rejected alternative).
- **The script-facing API.** No new function a script must call, no marker a
  script author must add for the common case. A pure script is cached because it
  is pure, not because it said so.
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
| Script path (absolute), size, mtime | The script's identity, under the same freshness rule as its inputs (§7.3) — editing a script must invalidate. |

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

An input record is `(absolute path, byte size, mtime as (secs, nanos))`. An entry
is valid when every record still matches. No content hashing (§15.4).

- A changed asset changes its mtime → miss → re-run. The normal case.
- A `git checkout` or `touch` rewrites mtimes without changing content → miss →
  a needless re-run. Costs time, never correctness.
- A deleted or unreadable input → miss.

### 7.4 The blind spot, named

Size + mtime cannot see an edit that **preserves the byte size and lands inside
the same mtime tick** as the recorded one. Then the cache serves stale bytes and
the ROM is silently wrong. Three things keep this narrow:

- Nanosecond mtimes on every filesystem nessemble realistically runs on (ext4,
  APFS, NTFS, btrfs) make same-tick collisions require sub-microsecond timing.
- Editors and asset pipelines write whole files, which bumps mtime.
- `--no-cache` (§9) and `nessemble cache clear` are the documented escape hatch,
  and the docs will say plainly what the rule is rather than implying the cache
  is content-addressed.

It is recorded here as a **known, accepted limitation** (§15.4), so that a future
"why did my ROM not change" report is diagnosed in minutes instead of days.

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
| `import` | A module's source is not covered by the script's own identity. Cacheable once modules are recorded as inputs; out of scope for v1. |

The scan is deliberately **conservative**: a `rand()` in a branch that never runs
still marks the script uncacheable. Being wrong in this direction costs a script
execution; being wrong in the other direction costs a wrong ROM.

Two more bypasses, both structural rather than detected:

- **`nessemble coverage`** never uses the disk cache. Coverage needs the lines to
  actually execute (§3); a hit records nothing. Layer 1 memoization stays on —
  coverage records line *sets*, not hit counts, so one execution per distinct
  invocation is exactly as informative as two.
- **`--no-cache`** (§9) skips read and write.

## 9. CLI surface

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

## 10. Docs

- **`docs/src/extending.md`** — a `## Caching` section after "Random numbers":
  what is cached, the `file://` declaration, the size+mtime freshness rule stated
  outright (including §7.4's limitation), what makes a script uncacheable and why,
  and `--no-cache`. The existing "Random output is not reproducible" note gains a
  sentence saying such scripts are never cached, which is why they keep working.
- **`docs/src/usage.md`** — `--no-cache`, and `cache info` / `cache clear`
  alongside the other subcommands.
- **A changeset per shipped phase**, per `CLAUDE.md` — the changeset body is the
  changelog line, and `CHANGELOG.md` is never touched by hand.

## 11. Phased plan

Each phase is independently shippable, independently revertible, and useful on
its own.

- **Phase 0 — memoization (core).** `custom_memo` in `Assembler`, consulted by
  `exec_custom`. No new syntax, no disk, no configuration. Halves script work in
  every build and fixes the `rand` pass-skew in §2. *Ship this first: it is the
  largest win per line of code in the whole plan.*
- **Phase 1 — `file://` (core).** Prefix stripping in `exec_custom`, the
  existence check, the `could-not-open` diagnostic, parser-level tests that the
  AST is unchanged. Useful with no cache at all — it turns a script's confusing
  throw into a diagnostic on the right line.
- **Phase 2 — input recording (`nessemble-script`).** The recorder in `engine`,
  `RunOutcome`, `run_with_inputs`, the §8 static impurity scan. No behavior
  change yet — this phase only *reports*. *Review this hardest: everything in
  Phase 3 trusts that the recorded set is complete.*
- **Phase 3 — the on-disk cache (CLI).** Key material, `crc_32` filenames with
  exact comparison, entry read/write with atomic rename, freshness checks, the
  coverage bypass, `--no-cache`.
- **Phase 4 — cache management and docs.** `nessemble cache info` / `clear`,
  eviction (§15.7), and the docs in §10.

Phases 0–1 are small and land immediately. Phase 2 is the careful one. Phase 3 is
the payoff.

## 12. Explicitly not in v1

Boundaries as decisions, not oversights:

- **Content hashing.** Settled against in §15.4. If §7.4's blind spot ever bites
  in practice, adding a digest is a change to one freshness function plus a cache
  format version bump — the design is deliberately shaped so that swap is cheap.
- **Caching anything but custom pseudo-ops.** `.incbin`, `.incpng`, `.incwav` are
  a `read` and a decode, not a scripting engine; they are not the bottleneck.
- **An LSP that runs scripts.** The editor uses `lenient_custom_resolver` and
  sees zero bytes from every custom directive. Making it run them for real is a
  genuinely interesting change — the cache is what would make it affordable — but
  it is its own plan, with its own answers about trust and timeouts.
- **A project-level dependency graph / watch mode.** `file://` declarations are
  the raw material for "rebuild when this asset changes", and deliberately so;
  the scheduler that would consume them is a separate feature.
- **Caching across machines** (a shared or CI-uploaded cache). Absolute paths in
  the key (§7.1) rule it out by construction. `--cache-dir` pointed at a mounted
  volume would be the first step, and is easy to add later.
- **Making `import`-using scripts cacheable** by recording module sources (§8).

## 13. Testing strategy

- **Memoization (Phase 0).** A resolver that counts invocations: a directive
  called once in the source runs its script exactly once across both passes; two
  directives with different args run twice; a directive with a forward-referenced
  symbol arg runs twice (different `ints`, §3) and both runs are correct. A
  script returning `rand`-derived bytes now emits the *same* bytes on both
  passes — a regression test for the sizing bug.
- **`file://` (Phase 1).** Prefix stripped in `texts`; absolute
  `file:///…` form; missing declared file yields `could-not-open` on the right
  line *and* the script never runs (counting resolver again); a text arg that
  merely *contains* `file://` mid-string is untouched; the AST is byte-identical
  to the unprefixed parse.
- **Recording (Phase 2).** A script reading via `open_file`, `read_blob`, and
  `decode_png_file` records all three absolute paths; a script reading nothing
  records nothing; each §8 deny-list trigger flips `cacheable` to false.
- **The cache (Phase 3).** In a `TempDir` with `HOME` pointed at it: cold run
  executes and writes an entry; second run hits and does not execute; touching an
  input misses; rewriting the *script* misses; a different `base_dir` with
  identical args misses; a corrupted/truncated entry misses rather than panics; a
  forged entry whose stored key material disagrees misses (§7.2); `--no-cache`
  neither reads nor writes; `nessemble coverage` still reports script lines with
  a warm cache.
- **CLI (Phase 4).** `cache info` on an empty and a populated cache; `cache
  clear` empties it; both are covered in `crates/nessemble-cli/tests/cli.rs`
  beside the existing `custom_pseudo_*` tests.

Every phase must leave `cargo fmt --all --check`, `cargo clippy --all-targets
--all-features -- -D warnings`, and `cargo test --all-features` green — the CI
gate in `.claude/README.md` runs exactly those.

## 14. Risks & mitigations

| Risk | Mitigation |
| --- | --- |
| **Stale bytes from the mtime blind spot (§7.4).** | Nanosecond mtimes; `--no-cache` and `cache clear` documented as the first thing to try; the freshness rule stated plainly in the docs rather than implied to be stronger. |
| **An unrecorded input** — a script reaching the filesystem by a route that does not pass through the host's three registrations. | The §8 deny-list covers the known routes (`open_dir`, `import`) by refusing to cache. A new host function that takes a path must record it; the recorder lives beside the registrations so the omission is visible in review, and Phase 2's tests assert coverage of each existing route. |
| **A cached side effect.** | Scripts that write are never cached (§8), detected statically and conservatively. |
| **Cache growth.** | Size cap plus LRU eviction (§15.7) and `cache clear`. Entries are ROM-fragment sized — kilobytes, not megabytes. |
| **Concurrent builds.** | Temp file + atomic `rename`; a partially-written entry is never visible, and a losing writer simply replaces an equivalent entry. |
| **Debuggability** — "is this a cache bug?" | JSON metadata is human-readable; `cache info` reports state; `--no-cache` isolates the cache from the question in one flag. |
| **Behavior change from memoization** (a script with side effects runs once per build instead of twice). | Such scripts were already broken across passes; running once is the correct semantics, and it is called out in the Phase 0 changeset. |

## 15. Decisions

### Settled with the maintainer

1. **Dependencies are both recorded and declarable.** The cache's correctness
   rests on *recording* the paths a script actually opened, through the host
   registrations that already exist (§6.2); `file://` (§4) is a complementary
   *declaration* that buys a directive-level diagnostic for a missing asset and a
   dependency visible without executing anything. *Rejected:* declarations alone,
   which is silently wrong for any script that opens a file it was not handed —
   a script given a tilemap that also reads a palette by convention would cache
   against a partial input set; and recording alone, which leaves the assembler
   unable to say anything about a directive's inputs without running it.
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
6. **A `file://` argument reaches the script with the prefix stripped, and a
   missing one is a directive-level error.** *Rejected:* passing the prefix
   through verbatim, which breaks every script the moment a caller adds a
   declaration; and warning-then-running, which needs a new warning category and
   an ambiguous answer about whether such a run is cacheable. Note the accepted
   consequence: a script that treats a missing file as *optional* cannot use the
   declaration for it — it keeps its own `open_file` and its own fallback, and
   still caches correctly via recording.

### Still the author's call

Reversible, recorded so they are decisions rather than defaults. Say the word on
any of them.

7. **Eviction is a total-size cap with LRU by entry mtime, enforced
   opportunistically on write; default 256 MB.** *Alternative:* an entry-count
   cap, or no eviction at all with `cache clear` as the only reclaim — defensible
   given how small entries are, but a cache with no bound is a support burden.
8. **A declared `file://` file that the script never reads does not invalidate
   the entry.** Recorded reads are the truth; a declaration the run did not use
   is not a dependency. *Alternative:* treat declarations as dependencies too,
   which is more intuitive ("I said it was an input") but requires widening
   `CustomResolver` to carry declarations into the resolver — the one signature
   this plan otherwise leaves alone (§6.4).
9. **Impurity is detected statically from the compiled AST, conservatively.**
   *Alternative:* runtime detection through rhai's debugger hook, which is exact
   (a `rand()` in a dead branch would not disqualify the script) but means
   enabling the `debugging` feature for ordinary runs, paying its cost on every
   execution to slightly widen what is cacheable.
10. **Errors are never persisted** (§5) — only memoized within a build.
    *Alternative:* caching failures too, which would make a broken build's second
    run marginally faster and its recovery confusing.
11. **`--no-cache` does not disable layer 1 memoization.** *Alternative:* a flag
    that forces every invocation to execute, useful for diagnosing a script whose
    output legitimately varies — but that is the behavior §2 identifies as a bug.
