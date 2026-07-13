# nessemble-rs: A Plan for Workspace-Aware Analysis

> Status: **Proposed.** Awaiting a decision on entry-point discovery
> ([§4](#4-decision-needed)) before implementation. This realizes Phase 6 of
> [001-language-server.md](001-language-server.md) — the "open-buffer include
> overlay" / project awareness left as *scope TBD*.

---

## 1. Problem

nessemble symbols are **global** and resolved across every `.include`d file at
assembly time; there is no per-file import or declaration. The language server,
however, analyzes a single buffer in isolation (`diagnose_source_as` on just the
open file). So a symbol defined in a sibling or parent file is reported as
`Symbol \`xxx\` was not defined` — a false positive whenever the open file is a
**fragment** meant to be included into a larger program.

This is undecidable from one file: the presence of an undefined symbol is itself
the signal that the file is a fragment. The only real fix is to analyze the file
in the context of the **whole project** — i.e. assemble from the top-level entry
file that includes it, and map the resulting diagnostics back onto the open
buffers.

## 2. Approach

Three pieces:

1. **Entry-point discovery** — find the top-level file(s) whose transitive
   `.include` closure contains the open file, so we know what to assemble.
2. **Open-buffer overlay** — when assembling from the entry file, read the
   *editor's* current (possibly unsaved) text for any file that is open, instead
   of the on-disk copy, so diagnostics reflect what the user sees.
3. **Multi-file diagnostics** — one project assembly yields diagnostics spread
   across several files; publish them per-URI to each open file, and clear stale
   ones.

### 2.1 Entry-point discovery (include graph)

On analysis:

- Enumerate candidate source files under the workspace root (`rootUri` /
  `workspaceFolders` from `initialize`): files matching `*.asm` / `*.s`, skipping
  obvious noise (`target/`, `.git/`, hidden dirs), bounded to a sane cap.
- For each, extract its `.include` targets (a cheap lexical scan reusing the
  preprocessor's include-target parsing — resolved **file-relative**, matching
  the assembler) to build a directed graph `file → included files`.
- **Roots** = files not included by any other. For the open file, pick the root
  whose closure contains it. Assemble that root.
- **Fallbacks:** if the open file is itself a root, or an orphan included by
  nothing (or the workspace can't be determined), assemble the open file
  directly — today's behavior. If multiple roots include it, pick one
  deterministically (documented) for now.

The graph is cached and invalidated when a file's include set changes, so the
common keystroke path doesn't re-scan the tree.

### 2.2 Open-buffer overlay

`preprocess::do_include` currently does `std::fs::read_to_string(&path)`
directly. Add a **file-content provider** seam: an optional overlay
`HashMap<PathBuf, String>` (canonicalized keys) consulted before disk; on a miss
it falls back to `read_to_string` exactly as now.

- Threaded through `preprocess` → `assemble_impl` → the diagnostics entry point
  as a new **opt-in** parameter. The CLI passes no overlay, so its path is
  byte-for-byte unchanged — **parity 122/122 must stay green.**
- The LSP builds the overlay from its open-document store (URI→text), so the
  entry file and every open include reflect unsaved edits.

### 2.3 Multi-file diagnostics

- A project assembly returns `Diag`s tagged with a `file`. Group them by file,
  resolve each back to a `Url`, and publish a `PublishDiagnostics` per file.
- Track which URIs we last published non-empty diagnostics for in this project,
  so when an error is fixed we publish an empty set to clear it (LSP has no
  "clear all" — each file must be cleared explicitly).
- Only publish for files we can address as URIs; at minimum every currently-open
  document in the closure. (Publishing for closed files is allowed and lets the
  editor surface project errors in unopened files; start with open files to keep
  it simple.)

## 3. Work breakdown

1. **core: file-content overlay** — provider seam in `preprocess`, threaded
   through `assemble_impl` / `diagnose_source_as` as an opt-in overlay; default
   path untouched. Unit tests; **parity re-run**.
2. **core/lsp: include-target extraction** — a reusable helper to list a
   source's include targets (resolved file-relative), for graph building.
3. **lsp: workspace model** — capture `rootUri`/`workspaceFolders` at
   `initialize`; scan + cache the include graph; map open file → entry root.
4. **lsp: project analysis** — assemble the entry root with the open-buffer
   overlay; group diagnostics by file; publish/clear per-URI. Fall back to
   single-file analysis when no root is found.
5. **docs** — note the model in `editor.md` (fragments are analyzed in the
   context of their entry file; no per-file config needed).
6. **version** — patch bump on top of `2.5.0` when it ships.

Each step keeps the existing single-file tests and ROM parity green.

## 4. Decision needed

**How should the entry point be discovered?**

- **A — Auto (include-graph scan), zero config (recommended).** As in §2.1. No
  project file to maintain; matches how nessemble projects are structured.
  Cost: a workspace scan (bounded/cached) and a documented tie-break when a
  fragment has multiple roots.
- **B — Explicit config.** A project file (e.g. `nessemble.toml`) or LSP
  `initializationOptions` names the entry file(s). Predictable and cheap, but
  every project must declare it or fall back to today's false positives.
- **C — Both: auto by default, config override.** Auto-scan normally; honor an
  explicit entry list when present (multi-root projects, or to force a choice).
  Most robust, slightly more surface.

Recommendation: **start with A**, structured so **C** is a later addition (the
config just overrides discovery). Open question within A: the multi-root
tie-break (first by path order, or analyze against each and intersect the
"undefined" sets so a symbol defined under *any* root is not flagged).
