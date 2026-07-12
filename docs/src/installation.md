# Installation

Download and install the latest release for your system:

[https://github.com/kevinselwyn/nessemble-rs/releases](https://github.com/kevinselwyn/nessemble-rs/releases)

Release artifacts are provided for all five supported platforms:

| Platform       | Artifact(s)                                     |
|----------------|-------------------------------------------------|
| macOS          | `nessemble_<v>.pkg`, `nessemble_<v>_macos.tar.gz` |
| Linux amd64    | `nessemble_<v>_amd64.deb`                       |
| Linux i386     | `nessemble_<v>_i386.deb`                        |
| Windows 32-bit | `nessemble_<v>_win32.exe`, `…_win32.msi`        |
| Windows 64-bit | `nessemble_<v>_win64.exe`, `…_win64.msi`        |

## macOS: "Apple could not verify…"

The macOS `.pkg` is not signed with an Apple Developer ID or notarized, so after
you download it, Gatekeeper blocks it with:

> "nessemble_&lt;v&gt;.pkg" Not Opened — Apple could not verify "nessemble_&lt;v&gt;.pkg"
> is free of malware…

This is expected for an unsigned package; it does not mean the file is harmful.
You have two options.

**Install the `.pkg` anyway** — clear the download-quarantine flag, then install:

```sh
xattr -d com.apple.quarantine nessemble_<v>.pkg
sudo installer -pkg nessemble_<v>.pkg -target /
```

**Or use the plain binary tarball** (`nessemble_<v>_macos.tar.gz`) and skip the
installer:

```sh
tar -xzf nessemble_<v>_macos.tar.gz
xattr -d com.apple.quarantine nessemble     # clear quarantine
sudo mv nessemble /usr/local/bin/            # put it on your PATH
```

Both install the same binary to `/usr/local/bin/nessemble`. (The tarball binary
is a 64-bit Intel build, matching the `.pkg`; it runs on Apple Silicon under
Rosetta.)

## From source

`nessemble` is a Cargo workspace and builds with a stock Rust toolchain.

```text
git clone https://github.com/kevinselwyn/nessemble-rs
cd nessemble-rs
cargo build --release
```

The binary is written to `target/release/nessemble`. See
[Building](building.md) for cross-compilation and packaging details.
