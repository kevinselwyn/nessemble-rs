---
nessemble: minor
---

Custom pseudo-op scripts can return `emit_source(text)` to have the assembler expand assembly source inline at the directive's call site, rather than only bytes — labels and constants the emitted source defines become real symbols usable right after it.
