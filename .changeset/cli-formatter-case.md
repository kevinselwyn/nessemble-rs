---
nessemble: minor
---
Add opt-in case normalization to `nessemble format` (Phase 4 of `plans/005-formatter.md`): `.nessemblerc` gains `mnemonicCase` and `hexDigitCase` keys (`"preserve"` | `"lower"` | `"upper"`, default `"preserve"`). `mnemonicCase` re-cases only the instruction mnemonic (labels, registers, and identifiers are left alone); `hexDigitCase` re-cases the hex-digit letters of numeric literals (`$ab` ↔ `$AB`). Directive names are never re-cased, since nessemble is case-sensitive about them. Both are byte-safe — nessemble matches mnemonics and hex literals case-insensitively — and covered by a byte-preservation test.
