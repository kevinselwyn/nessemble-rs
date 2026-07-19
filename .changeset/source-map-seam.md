---
nessemble: patch
---
Add an opt-in byte-exact source map to the assembler (`Options::source_map`,
exposed as `Assembly::source_map`), recording which source line emitted each ROM
byte. Off by default and side-effect free — assembled bytes are unchanged. This
is the internal seam Phase 0 of the CDL-based coverage plan
(`plans/007-cdl-based-coverage.md`) needs; no CLI surface yet.
