---
nessemble: patch
---
Add a `coverage` module to `nessemble-core` that classifies a byte-exact source
map against an emulator CDL capture: a `CdlSource` trait, a `FlatMaskCdl` reader
(FCEUX masks), `classify_span`, and a per-file/per-line `CoverageReport` model.
This is Phase 1 of the CDL-based coverage plan
(`plans/007-cdl-based-coverage.md`); no CLI surface yet.
