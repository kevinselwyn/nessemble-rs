---
nessemble: minor
---
Preview `.color` in the language server: hovering the directive shows its whole
argument list as the row of NES colors it maps to — each argument's RGB, the
palette index the assembler emits, and the color the PPU shows — and hovering a
single argument shows just that color. Arguments are evaluated as expressions,
so constants and arithmetic resolve, and the preview uses the assembler's own
palette matcher, so it always agrees with the assembled bytes.
