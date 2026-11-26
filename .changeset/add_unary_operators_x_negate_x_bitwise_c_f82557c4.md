---
nessemble: minor
---

Add unary operators: `-x` (negate), `~x` (bitwise complement) and `!x` (logical not), usable in any expression, so `.db -4`, `.dw -40`, `LDA #-1` and `.if !FLAG` now assemble.
