---
nessemble: patch
---
Internal: model conditional-assembly nesting (`.if`/`.ifdef`/`.ifndef`/`.else`/
`.endif`) as a `Vec<bool>` stack instead of a fixed `[bool; N]` array plus a
manual depth counter — push/pop/flip-top replace the hand-tracked index, and the
`MAX_NESTED_IFS` cap becomes a suppression guard rather than an array length. The
suppression predicate (current level, plus the immediate parent when nested) and
the past-the-limit behavior are preserved exactly. No change to assembled output
or diagnostics — Phase 3 of `plans/006-idiomatic-rust-refactor.md`.
