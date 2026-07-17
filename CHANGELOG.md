# Changelog

## 2.10.0 - 2026-07-16

### Minor changes

- Add `.inesbat`, `.ines4scr`, `.inesprgram`, `.inestv`, `.inesvs`, and
  `.inespc10` pseudo-instructions so the battery, four-screen, PRG-RAM size, TV
  system, VS Unisystem, and PlayChoice-10 fields of the iNES header can be
  configured from source. `.inestv 1` (PAL) is also mirrored into the unofficial
  Flags 10 TV-system field.
- Add NES 2.0 header support. The new `.ines2` pseudo-instruction emits a NES 2.0
  header, widening `.inesmap` to 12-bit mappers and `.inesprg`/`.ineschr` to
  12-bit sizes, and enabling the companion directives `.inessubmap`,
  `.inesprgnvram`, `.ineschrram`, `.ineschrnvram`, `.inestiming`, `.inesconsole`,
  `.inesvsppu`, `.inesvshw`, `.inesmiscrom`, and `.inesexpansion`. In NES 2.0 mode
  `.inesprgram` takes a byte size, `.inestv` provides the timing fallback, and
  `.inesvs`/`.inespc10` become console-type sugar.

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
