---
nessemble: minor
---

**Behavior change:** `@name` labels are now local to the nearest label above them, so routines can reuse names like `@loop` and `@done`; reference another routine's with `routine@name`. Code that used an `@` label as a global now gets an error suggesting the qualified name.
