# Changelog

## 2.9.0 - 2026-07-16

### Minor changes

- Rebuild the in-browser assembler component's editor on CodeMirror 6. Text
  selection now works and shows what's selected, and Cmd-F opens a working
  in-editor search instead of breaking the highlighting. Syntax colors and the
  overall look are unchanged.

## 2.8.2 - 2026-07-16

### Patch changes

- Add the `xtask changeset` command group (add/check/status/version) that parses
  `.changeset/` files, computes the next semantic version from the accumulated
  changesets, and — via `cargo set-version` — bumps the whole workspace. Internal
  release tooling (plan 004, Phase 1); no shipped-behavior change.
