---
nessemble: minor
---
The language server now surfaces `nessemble lint` findings inline as you type. Lint diagnostics use a gentle severity (Information/Hint) and a `nessemble-lint` source so they read as suggestions distinct from assembler errors, honor the project's `.nessemblerc` `lint` config, and clear as soon as the flagged block is documented. Also documents the `lint` command and its `.nessemblerc` config in the manual.
