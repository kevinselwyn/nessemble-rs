# Releasing

The release version is the **workspace version** in the root `Cargo.toml`
(`[workspace.package] version`). It is the single source of truth: it is the
version the CLI reports (`nessemble --version`) and the version the release
pipeline publishes. There is no manual version input.

## Cutting a release

Bump the workspace version on `main` and merge:

```toml
# Cargo.toml
[workspace.package]
version = "0.2.0"
```

That's it. On the push to `main`, the **Release** workflow
(`.github/workflows/release.yml`) reads the workspace version and, because no
`v0.2.0` tag exists yet:

1. builds every platform artifact — the seven files matching the upstream
   v1.1.1 release:

   | Platform       | Artifact(s)                              | Tool        |
   |----------------|------------------------------------------|-------------|
   | macOS          | `nessemble_<v>.pkg`                      | `pkgbuild`  |
   | Linux amd64    | `nessemble_<v>_amd64.deb`               | `cargo-deb` |
   | Linux i386     | `nessemble_<v>_i386.deb`                | `cargo-deb` |
   | Windows 32-bit | `nessemble_<v>_win32.exe`, `…_win32.msi`| `cargo-wix` |
   | Windows 64-bit | `nessemble_<v>_win64.exe`, `…_win64.msi`| `cargo-wix` |

2. creates the `v<version>` tag at that commit and its GitHub Release, uploads
   all seven artifacts, and **auto-generates release notes** listing every pull
   request merged since the previous release (GitHub's "What's Changed"
   changelog; grouping is configured in `.github/release.yml`).

If the version is unchanged (the tag already exists), the pipeline resolves the
version, sees the tag, and does nothing — so ordinary pushes to `main` never
re-release.

## Re-running on demand

The Release workflow can also be started manually (Actions → *Release* → *Run
workflow*). It applies the same logic: it releases only if the workspace
version has no matching tag yet.
