# Releasing

Releases are cut with a tag-driven pipeline. Cutting a release is a two-step,
button-click process — no local tooling required.

## 1. Bump the version

On `main`, set the workspace version in the root `Cargo.toml`
(`[workspace.package] version = "…"`), commit it, and merge. The tag process
verifies the tag matches this version.

## 2. Tag the release

Run the **Tag Release** workflow (Actions → *Tag Release* → *Run workflow*) and
enter the version (without the leading `v`, e.g. `0.1.0`). It:

- validates the version format,
- checks the workspace `Cargo.toml` version matches,
- ensures the tag does not already exist, and
- creates and pushes the annotated tag `v<version>`.

Pushing the tag triggers the **Release** workflow.

## What the Release workflow does

`.github/workflows/release.yml` runs on the `v*` tag and:

1. Builds every platform artifact — the seven files matching the upstream
   v1.1.1 release:

   | Platform       | Artifact(s)                              | Tool        |
   |----------------|------------------------------------------|-------------|
   | macOS          | `nessemble_<v>.pkg`                      | `pkgbuild`  |
   | Linux amd64    | `nessemble_<v>_amd64.deb`               | `cargo-deb` |
   | Linux i386     | `nessemble_<v>_i386.deb`                | `cargo-deb` |
   | Windows 32-bit | `nessemble_<v>_win32.exe`, `…_win32.msi`| `cargo-wix` |
   | Windows 64-bit | `nessemble_<v>_win64.exe`, `…_win64.msi`| `cargo-wix` |

2. Creates the GitHub Release for the tag, uploads all seven artifacts, and
   **auto-generates release notes** listing every pull request merged since the
   previous release (GitHub's "What's Changed" changelog; grouping is configured
   in `.github/release.yml`).

## Testing the build without releasing

Run the **Release** workflow via *Run workflow* (workflow_dispatch) with a
version input. This builds and uploads the artifacts as workflow artifacts but
does **not** create a GitHub Release (the release step only runs for tag
pushes).
