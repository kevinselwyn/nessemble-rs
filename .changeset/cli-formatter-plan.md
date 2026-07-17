---
nessemble: none
---
Add `plans/005-formatter.md`, a planning document for a prettier-style built-in `nessemble format` subcommand and an optional `.nessemblerc` JSON config, and implement its Phase 0: a `FormatOptions` seam in `nessemble-core::tooling` (`format_with`, with `format` delegating to the defaults). No user-facing behavior change — the CLI has no `format` command yet and the language server's formatting output is unchanged (parity 122/122).
