<!--
  Nessemble pull-request template.

  Fill in the sections that apply and delete the ones that don't — a one-line
  CSS tweak doesn't need a Parity section. The HTML comments are guidance and
  won't render in the final description; leave them or delete them. Keep the
  write-up focused on WHAT changed and WHY — the commit history has the
  play-by-play.
-->

## Summary

<!-- One short paragraph: what this PR does and why. If it implements a plan
     phase, link it, e.g. "Phase 3 of `plans/002-wasm.md`". -->

## Changeset

<!-- Every PR that changes shipped behavior carries a changeset under
     `.changeset/` declaring its version impact; releases are cut on demand by
     the Release action, which computes the next version from the accumulated
     changesets. See `.changeset/README.md`. Tick the line that applies: -->

- [ ] Added a changeset (`nessemble: major | minor | patch`) with a user-facing
      summary.
- [ ] No release impact — a `nessemble: none` changeset, or the `no-changeset`
      label.

## Changes

<!-- Bullet the notable changes, grouped by area when it helps (nessemble-core,
     -isa, -media, -script, -i18n, -lsp, -wasm, -cli, xtask, docs, website).
     Lead each bullet with a bold subject. -->

-

## Parity & safety

<!-- The assembler's ROM output is guarded by a golden-ROM parity harness
     (`cargo run -p xtask -- parity`). If you touched nessemble-core or the
     assemble path, confirm parity is still green and that the CLI `assemble`
     path is byte-for-byte unchanged (new tooling/analysis paths should be
     separate seams, not changes to the parity path). Delete this section for
     changes that can't affect ROM output (docs, website, CI-only). -->

- Parity: **__ / __** golden ROMs still byte-for-byte
- CLI `assemble` path unchanged (no parity-path edits)

## Verification

<!-- How you confirmed it works. Tick the gate below and add any feature-specific
     checks (headless-Chromium for the web component, end-to-end LSP over stdio,
     `nessemble --version`, etc.). -->

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets`
- [ ] `cargo test --workspace`
- [ ] `cargo run -p xtask -- parity` — if core / the assemble path was touched

## Docs & notes

<!-- Docs or plan updates, deliberate trade-offs, deferred work, or follow-ups.
     Delete if there's nothing to add. -->
