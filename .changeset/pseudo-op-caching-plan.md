---
nessemble: none
---

Add plan 011 designing caching for custom pseudo-instructions: per-assembly memoization so a script runs once instead of once per assembler pass, recorded script inputs, a persistent `~/.nessemble/cache` keyed on those inputs' size and mtime, and a `file://` prefix that declares a text argument as an input file (planning only; no shipped behavior changes).
