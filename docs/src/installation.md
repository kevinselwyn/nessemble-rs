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

A `linux/amd64` [container image](#container-image) is also published to GHCR for
CI pipelines and coding agents that only need the executable.

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

## Container image

A binary-only image is published to the GitHub Container Registry on every
release:

```text
ghcr.io/kevinselwyn/nessemble-rs:<version>   # e.g. :2.11.0
ghcr.io/kevinselwyn/nessemble-rs:latest
```

It exists for CI pipelines and coding agents that need the `nessemble`
executable but not this source tree — pulling the image is faster and simpler
than building the workspace. The image is a single statically-linked
`linux/amd64` binary on `scratch`: there is no shell, package manager, or libc
inside it, only `/nessemble`. That means you consume it by lifting the binary
out, not by opening a shell in it.

**Copy it into your own image** with a multi-stage `COPY --from` — the most
common pattern for build tools:

```dockerfile
COPY --from=ghcr.io/kevinselwyn/nessemble-rs:2.11.0 /nessemble /usr/local/bin/nessemble
```

**Extract it to the host** without writing a Dockerfile — create a container
from the image (it need not run) and copy the file out:

```sh
docker create --name nessemble ghcr.io/kevinselwyn/nessemble-rs:2.11.0
docker cp nessemble:/nessemble ./nessemble
docker rm nessemble
```

**Run it directly** — the entrypoint is the binary, so pass `nessemble`
arguments straight through and mount your project as the working directory:

```sh
docker run --rm ghcr.io/kevinselwyn/nessemble-rs:2.11.0 --version

docker run --rm -v "$PWD:/work" -w /work \
  ghcr.io/kevinselwyn/nessemble-rs:2.11.0 project.asm --output project.nes --format nes
```

Pin a version tag (`:2.11.0`) rather than `:latest` for reproducible builds. In
a Claude Code on the web (or similar) environment, pulling the image in a setup
script caches it into the environment snapshot, so the binary is on disk at the
start of every later session.

## From source

`nessemble` is a Cargo workspace and builds with a stock Rust toolchain.

```text
git clone https://github.com/kevinselwyn/nessemble-rs
cd nessemble-rs
cargo build --release
```

The binary is written to `target/release/nessemble`. See
[Building](building.md) for cross-compilation and packaging details.
