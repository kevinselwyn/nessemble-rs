# nessemble-rs

A fresh, from-scratch **Rust** reimplementation of
[`kevinselwyn/nessemble`](https://github.com/kevinselwyn/nessemble) — a 6502
assembler for the Nintendo Entertainment System — targeting **byte-for-byte ROM
output parity** with the upstream **v1.1.1** release.

This repository contains **no C source**; the upstream project is used only as a
behavioral reference. See [`PLAN.md`](PLAN.md) for the full multi-phase plan,
scope decisions, and architecture.

## Scope

In scope: the **assembler** (assemble / check / coverage), media importers, the
CLI (config / init / reference), i18n, custom pseudo-op scripting, and
documentation/website generation + release packaging.

Out of scope: the disassembler/reassembler, the simulator/debugger, and the
package-registry functionality. A WASM build is deferred. (Details in `PLAN.md`.)

## Status

**Phase 0 — Foundations & parity harness (in progress).**

- [x] Cargo workspace with crate seams (`isa`, `core`, `media`, `script`,
      `i18n`, `cli`) + `xtask`.
- [x] `nessemble-isa`: the full 256-entry 6502 opcode table, generated at build
      time from `crates/nessemble-isa/data/opcodes.csv`.
- [x] Minimal `nessemble` CLI (argument parsing, `--version`, `--license`).
- [x] Reference corpus imported as test fixtures under `tests/corpus/`
      (122 assemble cases, out-of-scope disassemble/simulate artifacts removed).
- [x] `xtask` parity harness that diffs output against the committed golden
      ROMs and against the official v1.1.1 release binary (the "oracle").
- [ ] The assembler itself — Phases 1–5.

The oracle reproduces **all 119** non-scripting golden ROMs in the corpus
(3 scripting cases are deferred to Phase 8), confirming the goldens are
trustworthy for parity testing.

## Workspace layout

```text
crates/
  nessemble-isa/     # 6502 opcode tables + addressing modes (build-time generated)
  nessemble-core/    # lexer, parser, assembler (Phases 1–5)
  nessemble-media/   # asset importers: PNG/CHR, palette, RLE, WAV/DPCM (Phase 5)
  nessemble-script/  # Rhai custom pseudo-op host (Phase 8, feature-gated)
  nessemble-i18n/    # Project Fluent i18n (Phase 7)
  nessemble-cli/     # the `nessemble` binary
xtask/               # developer tasks (parity harness, oracle fetch)
tests/corpus/        # reference assemble fixtures (.asm + golden .rom)
```

## Building & testing

Requires a Rust toolchain (`rustc`/`cargo` ≥ 1.83).

```bash
cargo build              # build all crates
cargo test               # run unit tests
cargo fmt --all --check  # formatting
cargo clippy --all-targets --all-features
```

## Parity harness

The harness compares assembler output against the committed golden `.rom` files
and, optionally, the official v1.1.1 release binary.

```bash
# Download & extract the v1.1.1 reference binary into ./.oracle (git-ignored)
cargo run -p xtask -- fetch-oracle

# Confirm the oracle reproduces every committed golden (sanity check)
cargo run -p xtask -- verify-goldens

# Run nessemble-rs over the corpus and report parity (writes tests/parity-report.txt)
cargo run -p xtask -- parity
```

## License

GPL-3.0-or-later, matching the upstream project. See [`COPYING`](COPYING).
