---
nessemble: patch
---
Internal: collapse the ~19 numeric `.inesXxx` directive AST variants into a
single `Pseudo::Ines(InesField, Expr)` node, driven by a name→field table in the
parser and a field→member assignment in the assembler. The three non-numeric
directives (`.ines2`, `.inestiming`, `.inestrn`) keep their own variants. No
change to the emitted iNES / NES 2.0 header bytes or any diagnostic — Phase 2 of
`plans/006-idiomatic-rust-refactor.md`.
