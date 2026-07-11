# Building

`nessemble-rs` is a Cargo workspace of pure-Rust crates. Building requires only a
stock Rust toolchain (1.83+) — there is no C toolchain, flex/bison, or gettext
dependency.

## Build

```text
cargo build --release
```

The CLI binary is written to `target/release/nessemble`.

## Test

```text
cargo test
```

The parity harness compares `nessemble-rs` output against the committed golden
ROMs (produced by the upstream v1.1.1 release binary):

```text
cargo run -p xtask -- parity
```

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

## Scripting

Custom pseudo-instruction scripting (Rhai) is enabled by default. To build the
CLI without it:

```text
cargo build --release -p nessemble-cli --no-default-features
```
