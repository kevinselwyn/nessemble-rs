# Installation

Download and install the latest release for your system:

[https://github.com/kevinselwyn/nessemble-rs/releases](https://github.com/kevinselwyn/nessemble-rs/releases)

Release artifacts are provided for all five supported platforms:

| Platform       | Artifact(s)                               |
|----------------|-------------------------------------------|
| macOS          | `nessemble_<v>.pkg`                       |
| Linux amd64    | `nessemble_<v>_amd64.deb`                 |
| Linux i386     | `nessemble_<v>_i386.deb`                  |
| Windows 32-bit | `nessemble_<v>_win32.exe`, `…_win32.msi`  |
| Windows 64-bit | `nessemble_<v>_win64.exe`, `…_win64.msi`  |

## From source

`nessemble` is a Cargo workspace and builds with a stock Rust toolchain.

```text
git clone https://github.com/kevinselwyn/nessemble-rs
cd nessemble-rs
cargo build --release
```

The binary is written to `target/release/nessemble`. See
[Building](building.md) for cross-compilation and packaging details.
