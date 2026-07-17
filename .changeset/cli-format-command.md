---
nessemble: minor
---
Add a `nessemble format <path>...` subcommand that formats assembly source. A single file is printed to stdout; `--write` rewrites files in place (reporting each changed file); `--check` lists unformatted files and exits non-zero for CI. Directories are walked recursively for `.asm` files and require `--write` or `--check`. This is Phase 1 of `plans/005-formatter.md`; it uses the default formatting options (indentation, comma spacing, trailing-whitespace tidy) — the opinionated structural rules and `.nessemblerc` config follow in later phases.
