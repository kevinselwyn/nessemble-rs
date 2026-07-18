---
nessemble: patch
---
Internal: fold the repeated opcode-resolution logic in the instruction encoder
into a single `resolve_opcode` helper (plus an `indexed_mode` helper for the
`X`/`Y`-indexed forms), and drop the `opcode_byte` sentinel wrapper now that the
`Option` flows through to emission. No change to assembled output, diagnostics,
or the addressing-mode selection — Phase 1 of `plans/006-idiomatic-rust-refactor.md`.
