# nessemble

`nessemble` is a **6502 assembler for the Nintendo Entertainment System**,
written in Rust. It assembles NES programs — instructions, macros, conditionals,
includes, media importers, iNES banking — into ROMs, and can be extended with
custom pseudo-instructions written in [Rhai](https://rhai.rs).

> **Upgrading from 1.x?** 2.0 is a ground-up Rust rewrite. Your assembly source
> and ROM output are unchanged, but the tooling around them moved. See
> [`docs/src/upgrading.md`](docs/src/upgrading.md) for what changed.
>
> **Looking for 1.x?** The original C implementation of `nessemble` lives at
> [kevinselwyn/nessemble](https://github.com/kevinselwyn/nessemble).

## Features

- **The assembler** — assemble, check, and coverage, with a hand-written lexer,
  recursive-descent parser, and two-pass code generator (symbols, expressions,
  addressing-mode selection, macros, conditionals, includes).
- **iNES output & banking** — full iNES header and PRG/CHR bank layout,
  `.segment`/`.prg`/`.chr`, and directives like `.checksum`, `.enum`, `.rs`.
- **Media importers** — PNG/CHR graphics, palettes, RLE, and WAV/DPCM audio.
- **Custom pseudo-instructions** — a sandboxed [Rhai](https://rhai.rs) scripting
  host for user-defined directives.
- **Editor support** — a built-in [Language Server](docs/src/editor.md)
  (`nessemble lsp`) speaking LSP over stdio, for VS Code, Neovim, Helix, Emacs,
  and other LSP-capable editors.
- **Runs in the browser** — a WebAssembly build assembles entirely client-side,
  powering the framework-free `<nessemble-assembler>` web component on the
  [project site](https://kevinselwyn.github.io/nessemble-rs/).
- **Internationalization** — messages via [Project Fluent](https://projectfluent.org).
- **Release packaging** — `.deb`, `.msi`, and `.pkg` artifacts for all supported
  platforms, plus a marketing site, the mdBook manual, and the in-browser
  assembler deployed to GitHub Pages.

## Installation

Download the latest release for your platform:

<https://github.com/kevinselwyn/nessemble-rs/releases>

Or build from source (see below).

### Container image (for CI and coding agents)

If you're an automated agent or a CI/Docker build that only needs the
`nessemble` executable — not this source tree — pull it from the published
container image instead of building the workspace. The image is a single
statically-linked `linux/amd64` binary on `scratch` (no shell, no libc), so
`docker cp` and `COPY --from` are the ways to consume it:

```dockerfile
# In your own Dockerfile — lift the binary into any image:
COPY --from=ghcr.io/kevinselwyn/nessemble-rs:latest /nessemble /usr/local/bin/nessemble
```

```sh
# Or extract it to the current directory without a Dockerfile:
docker create --name nessemble ghcr.io/kevinselwyn/nessemble-rs:latest
docker cp nessemble:/nessemble ./nessemble
docker rm nessemble

# Or run it directly, assembling files from the working directory:
docker run --rm -v "$PWD:/work" -w /work \
  ghcr.io/kevinselwyn/nessemble-rs:latest project.asm --output project.nes --format nes
```

Prefer a version tag (e.g. `:2.11.0`) over `:latest` for reproducible builds.
See [Installation → Container image](docs/src/installation.md#container-image)
for details.

## Getting started

```bash
nessemble init                                              # scaffold a project
nessemble project.asm --output project.nes --format nes    # assemble
```

Run `project.nes` in any NES emulator to see the result.

### In the browser

`nessemble` also compiles to WebAssembly and assembles entirely client-side — try
it live in the [in-browser assembler](https://kevinselwyn.github.io/nessemble-rs/),
or embed the framework-free `<nessemble-assembler>` web component (see
[`web/`](web/)).

### Editor support

Run `nessemble lsp` to start the built-in Language Server (LSP over stdio) for
diagnostics in any LSP-capable editor. See
[`docs/src/editor.md`](docs/src/editor.md).

## Workspace layout

```text
crates/
  nessemble-isa/     # 6502 opcode tables + addressing modes (build-time generated)
  nessemble-core/    # lexer, parser, assembler
  nessemble-media/   # asset importers: PNG/CHR, palette, RLE, WAV/DPCM
  nessemble-script/  # Rhai custom pseudo-op host (feature-gated)
  nessemble-i18n/    # Project Fluent i18n
  nessemble-lsp/     # Language Server Protocol implementation
  nessemble-wasm/    # WebAssembly build — in-browser assembler
  nessemble-cli/     # the `nessemble` binary
web/                 # <nessemble-assembler> web component (wraps the wasm build)
website/             # marketing site (deployed with the manual to GitHub Pages)
xtask/               # developer tasks (parity harness, oracle fetch, `dist` site build)
tests/corpus/        # assemble fixtures (.asm + golden .rom)
```

## Building & testing

Requires a Rust toolchain (`rustc`/`cargo` ≥ 1.83); no C toolchain is needed.

```bash
cargo build              # build all crates
cargo test               # run unit tests
cargo fmt --all --check  # formatting
cargo clippy --all-targets --all-features
```

## Parity harness

`nessemble` is validated against a corpus of committed golden `.rom` files —
**all 122 reproduce byte-for-byte**. The harness can optionally cross-check the
goldens against a reference binary.

```bash
# Run nessemble over the corpus and report parity (writes tests/parity-report.txt)
cargo run -p xtask -- parity

# Optional: download a reference binary and confirm it reproduces every golden
cargo run -p xtask -- fetch-oracle
cargo run -p xtask -- verify-goldens
```

## Documentation

The manual lives under [`docs/`](docs/) and builds with
[mdBook](https://rust-lang.github.io/mdBook/):

```bash
mdbook build docs
```

To build the full site published to GitHub Pages — the marketing site, the
manual, and the WebAssembly in-browser assembler — into `site/`:

```bash
rustup target add wasm32-unknown-unknown                 # once
cargo install wasm-bindgen-cli --version <Cargo.lock>    # once, matching Cargo.lock
cargo run -p xtask -- dist
```

## License

GPL-3.0-or-later. See [`COPYING`](COPYING).
