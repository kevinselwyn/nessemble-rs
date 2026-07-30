---
nessemble: none
---

Add plan 011 designing caching for custom pseudo-instructions: per-assembly memoization so a script runs once instead of once per assembler pass, recorded script inputs, a persistent `~/.nessemble/cache` invalidated by the size and mtime of both those inputs and the script itself, and a `file://` prefix on any filename argument that declares an input file — reported as an error when missing, and cmd-clickable in the editor (planning only; no shipped behavior changes).
