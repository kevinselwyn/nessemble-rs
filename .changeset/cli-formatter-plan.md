---
nessemble: minor
---
Add a configurable formatting API to `nessemble-core::tooling`: `format_with(source, &FormatOptions)` with `FormatOptions` (`indent_style`, `indent_width`, `comma_spacing`) and `IndentStyle`. The existing `format` now delegates to it with default options, so output is unchanged (parity 122/122, language server formatting identical). This is Phase 0 of the built-in `nessemble format` command specified in `plans/005-formatter.md`.
