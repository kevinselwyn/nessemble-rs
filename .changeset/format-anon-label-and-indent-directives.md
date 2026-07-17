---
nessemble: minor
---
Fix `nessemble format` corrupting assembled output on anonymous-label branches:
a branch whose operand references an anonymous label (`BEQ :+`, `BNE :-`) was
misclassified as an anonymous-label *definition* and de-indented to column 0,
where the assembler then parsed it differently and silently changed the ROM. A
line is now treated as a label definition only when the `:` ends the line (a
trailing comment is allowed), matching the assembler's own rule. Add an
`assemble(x) == assemble(format(x))` regression covering anonymous-label
branches.

Also add an opt-in `indentDirectives` `.nessemblerc` option (default `false`):
when enabled, directive lines (`.db`, `.dw`, `.include`, …) are indented to
block depth like instructions instead of being pinned to column 0, for codebases
that indent data under labels.
