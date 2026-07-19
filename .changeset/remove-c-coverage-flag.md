---
nessemble: major
---
**Breaking:** remove the `-C`/`--coverage` assemble-time write-coverage flag. It
reported per-bank byte counts (a disassembly-progress metric) and is superseded
by the new `coverage` subcommand, which reports true runtime execution coverage
from an emulator CDL. Scripts invoking `nessemble -C …` should switch to
`nessemble coverage …`. Phase 3 of `plans/007-cdl-based-coverage.md`.
