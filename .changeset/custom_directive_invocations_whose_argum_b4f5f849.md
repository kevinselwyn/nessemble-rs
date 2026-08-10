---
nessemble: minor
---

Custom-directive invocations whose arguments are already known (literal numbers, resolvable file paths) are now resolved concurrently ahead of assembly, warming the pseudo-op cache before the sequential passes read from it — a real speedup on script-heavy builds with no script changes required.
