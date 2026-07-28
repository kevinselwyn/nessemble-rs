# Building

`nessemble` is a Cargo workspace of pure-Rust crates. Building requires only a
stock Rust toolchain (1.83+).

## Build

```text
cargo build --release
```

The CLI binary is written to `target/release/nessemble`.

## Test

```text
cargo test
```

The test suite includes hermetic golden-ROM tests that assemble the committed
corpus (`tests/corpus/`) and compare the output against the golden `.rom` files
byte-for-byte — no external binary or network access is required.

## Cross-compilation

The dependencies are pure Rust, so the five release targets cross-compile
cleanly. Add a target and build:

```text
rustup target add i686-unknown-linux-gnu
cargo build --release --target i686-unknown-linux-gnu
```

| Platform       | Target triple                |
|----------------|------------------------------|
| macOS          | `x86_64-apple-darwin`        |
| Linux amd64    | `x86_64-unknown-linux-gnu`   |
| Linux i386     | `i686-unknown-linux-gnu`     |
| Windows 32-bit | `i686-pc-windows-msvc`       |
| Windows 64-bit | `x86_64-pc-windows-msvc`     |

## Packaging

Release artifacts are produced by the CI release workflow
(`.github/workflows/release.yml`):

- **`.deb`** (Linux) via [`cargo-deb`](https://crates.io/crates/cargo-deb).
- **`.msi`** (Windows) via [`cargo-wix`](https://crates.io/crates/cargo-wix).
- **`.pkg`** (macOS) via `pkgbuild`.
- **`.tar.gz`** (macOS) — the raw binary, as a signing-free alternative to the
  unsigned `.pkg` (which Gatekeeper blocks after download).
- **`.vsix`** (VS Code) — the editor extension in `editors/vscode/`, packaged
  with [`vsce`](https://github.com/microsoft/vscode-vsce).

### The VS Code extension

```text
cargo run -p xtask -- vsix
```

Needs `npm` and `npx` on `PATH`; nothing else — the extension is plain
JavaScript with no compile step. The task builds from a staged copy under
`target/vsix-build/`, stamping the workspace version into the extension
manifest on the way (`editors/vscode/package.json` holds a `0.0.0-dev`
placeholder, so the shipped extension version always tracks the release and no
version string is hand-edited). The result is `nessemble_<version>.vsix` in the
repository root.

CI packages the extension on every pull request, so a broken manifest or a
stale `package-lock.json` fails there rather than during a release.

## Scripting

Custom pseudo-instruction scripting (Rhai) is enabled by default. To build the
CLI without it:

```text
cargo build --release -p nessemble-cli --no-default-features
```
