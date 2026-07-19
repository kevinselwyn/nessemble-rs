---
nessemble: minor
---
Add a `nessemble coverage <infile.asm> --cdl <file.cdl>` subcommand that reports
runtime execution coverage of an assembled ROM against an emulator CDL capture.
It classifies each PRG source line as code / data / mixed / unaccessed and writes
JSON and/or LCOV reports (`--format`, default both) plus a one-line summary.
FCEUX and Mesen flat-mask CDLs are supported via `--emulator` (default `fceux`);
multiple `--cdl` files are merged by bitwise OR. Phase 2 of
`plans/007-cdl-based-coverage.md`.
