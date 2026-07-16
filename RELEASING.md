# Releasing

Releases are **driven by changesets** and cut on demand by a **GitHub Action** —
there is no hand-edited version string. Every PR that changes shipped behavior
adds a changeset declaring its version impact; the Release action reads all the
changesets accumulated since the last release, computes the next semantic
version, upversions the whole workspace, and hands off to the build pipeline that
produces the platform artifacts.

The design is documented in
[`plans/004-release-orchestration.md`](plans/004-release-orchestration.md); the
one-time GitHub App setup the Release workflow relies on is in
[`ref/repo/release-app-setup.md`](ref/repo/release-app-setup.md).

## Every PR carries a changeset

Add a file under `.changeset/` describing how your change affects the next
release — see [`.changeset/README.md`](.changeset/README.md) for the format. In
short:

```markdown
---
nessemble: minor
---
A one-line, user-facing description of the change.
```

The change type is `major`, `minor`, or `patch` (or `none` for a change with no
release impact — docs, CI, chores). Because this is a single-version workspace,
use the single umbrella key `nessemble`, not individual crate names.

A PR with no release impact can instead carry the `no-changeset` label.

## Cutting a release

Run the **Release** action (Actions → *Release* → *Run workflow*). It:

1. Reads every changeset in `.changeset/` and computes the next version — the
   **highest** change type wins (any `major` → major bump; else any `minor` →
   minor; else `patch`). A set of only `none` changesets releases nothing.
2. Upversions the **whole workspace** — the root `[workspace.package] version`
   (which every crate inherits via `version.workspace = true`), the internal
   `[workspace.dependencies]` version pins, and `Cargo.lock` — using
   `cargo set-version`.
3. Aggregates the changeset summaries into a new `CHANGELOG.md` section and
   **deletes the consumed changesets**.
4. Commits the result as the `nessemble-release[bot]` GitHub App and pushes it to
   `main`.

## Build & publish

That push to `main` triggers the **Publish** pipeline
(`.github/workflows/release.yml`), which — seeing a new workspace version with no
matching tag — builds every platform artifact, creates the `v<version>` tag and
its GitHub Release, and uploads the assets:

| Platform       | Artifact(s)                                   | Tool        |
|----------------|-----------------------------------------------|-------------|
| macOS          | `nessemble_<v>.pkg`, `nessemble_<v>_macos.tar.gz` | `pkgbuild`  |
| Linux amd64    | `nessemble_<v>_amd64.deb`                     | `cargo-deb` |
| Linux i386     | `nessemble_<v>_i386.deb`                      | `cargo-deb` |
| Windows 32-bit | `nessemble_<v>_win32.exe`, `…_win32.msi`      | `cargo-wix` |
| Windows 64-bit | `nessemble_<v>_win64.exe`, `…_win64.msi`      | `cargo-wix` |
| WebAssembly    | `nessemble_<v>_wasm.tar.gz`                    | `xtask wasm`|

Ordinary pushes to `main` never re-release: between releases the workspace
version stays at the last released version, whose tag already exists, so the
pipeline resolves the version, sees the tag, and does nothing.
