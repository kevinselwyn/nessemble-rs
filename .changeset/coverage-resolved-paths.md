---
nessemble: patch
---
Fix `nessemble coverage` report paths so `genhtml` (and other LCOV tools) can
find the sources. The source map now identifies each file by its resolved
absolute path instead of the assembler's per-file display name (which lost the
top-level directory and left includes relative to a different base), and the
`coverage` command emits each `SF:`/JSON path relative to the current directory
(clean, no `../..`) when the file is under it, else absolute. Running
`genhtml coverage.lcov` from the project root now resolves every source.
