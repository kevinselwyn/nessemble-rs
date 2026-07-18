---
nessemble: patch
---
Internal: type the single-bit iNES Flags-6 toggles (`mir`, `bat`, `fsc`, `trn`)
as `bool` instead of `i64`, alongside the already-boolean `nes2`. The value-set
directives mask bit 0 exactly as the header emission's former `& 0x01` did, so
the emitted header is byte-identical. The multi-value fields (mapper, bank
counts, RAM sizes, timing, console, …) and the dual-use `vs`/`pc10` flags stay
`i64` to preserve exact emission and range-check diagnostics — Phase 6 of
`plans/006-idiomatic-rust-refactor.md`.
