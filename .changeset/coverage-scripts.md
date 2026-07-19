---
nessemble: minor
---
Add `nessemble coverage --scripts`, which also reports line coverage for the
project's `-p` Rhai pseudo-op scripts — revealing script code that never runs
during assembly. Executed lines come from a debugger-instrumented engine; the
coverable set is the compiled AST, so never-run branches show as uncovered. Each
script joins the JSON/LCOV report as its own file. Behind the `coverage` Cargo
feature (on by default). Phase 4 of `plans/007-cdl-based-coverage.md`.
