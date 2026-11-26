---
nessemble: minor
---

Add assembler warnings, each with a rule id that can be set to `off`, `warn` or `error`: `duplicate-symbol` (a label, constant or macro redefined with a different value or kind), `duplicate-symbol-same-value` (redefined to the same value, e.g. a label placed by an includer and again by the included file), and `value-out-of-range` (a value narrowed to fit a byte, word, or zero-page or absolute operand it does not fit, such as `.db 300` or `STA <$1234`). Configure them per project under `assemble.rules` in `.nessemblerc`, or per run with `-W <rule>=<level>` and `--warnings-as-errors`. Assembly now reads `.nessemblerc`, so a malformed one fails the build. Warnings change nothing about what assembles; the language server shows them with their rule id, and `nessemble reference diagnostics` lists them.
