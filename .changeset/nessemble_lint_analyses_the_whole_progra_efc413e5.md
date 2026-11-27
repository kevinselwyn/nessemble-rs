---
nessemble: minor
---

`nessemble lint` analyses the whole program from its entry file: routine clobber checks follow calls across files, fall-through, branches and save/restore pairs, see macros, `.for` and custom pseudo-instructions as the code they emit, and report indirect jumps as the new `unverifiable-clobbers` warning. **Behaviour change** when an entry is configured, or when a linted file assembles on its own.
