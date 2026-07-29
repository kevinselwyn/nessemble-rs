---
nessemble: patch
---
Silence a lint finding at the site with `; @nessemble-lint-ignore-next-line [rule[, rule...]]` or a `; @nessemble-lint-ignore start|end [rule[, rule...]]` region — the shapes the coverage directives already use. Bare, they suppress every rule; named, only the rules listed. This closes the gap 2.21.0 shipped: when the clobber analysis is wrong about a routine — a fall-through entry point, or a `PHA`/`PLA` pair whose restore it does not model — the only options were a false annotation, no annotation, or turning the rule off project-wide. A suppressed finding is gone rather than downgraded: it does not print, does not count toward `--max-warnings`, and does not affect the exit code, even at `error` severity. Parse and assembly errors are never suppressible.
