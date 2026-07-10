# AGENTS.md

## Cursor Cloud specific instructions

`nessemble-rs` is a single Rust Cargo workspace (edition 2021, `rust-version = 1.83`)
implementing a 6502 NES assembler CLI. There are **no services, servers, databases,
or background processes** — the product is a self-contained CLI binary that reads
`.asm` source files and writes NES ROM binaries, then exits. "Running the app" means
invoking the CLI; "end-to-end testing" means assembling fixtures and diffing bytes.

Standard build/lint/test commands are in `README.md`; run them from the repo root.
The update script runs `cargo fetch`, so dependencies are already vendored on startup.

Non-obvious notes:

- **Parity is intentionally partial.** `cargo run -p xtask -- parity` currently reports
  `93/119` goldens passing (see `tests/parity-report.txt`). The ~26 failures are
  unimplemented Phase 4/5 features (macros, conditionals, includes, media importers)
  and are expected — the harness process still exits `0`. Do not treat these FAIL
  lines as environment breakage.
- **Oracle fetch is optional and needs network.** `cargo run -p xtask -- fetch-oracle`
  and `verify-goldens` download the upstream v1.1.1 `.deb` from GitHub. The core
  build/test/lint and `xtask parity` (against committed goldens) are fully offline;
  skip the oracle unless you specifically need to re-verify goldens.
- **Quick end-to-end smoke test** (assemble a fixture and confirm byte parity):
  `cargo run -p nessemble-cli -- tests/corpus/examples/ascii/ascii.asm --output /tmp/out.rom`
  then `cmp /tmp/out.rom tests/corpus/examples/ascii/ascii.rom`.
- `nessemble-isa/build.rs` generates the 256-entry opcode table from
  `crates/nessemble-isa/data/opcodes.csv` at build time; a clean build regenerates it.
