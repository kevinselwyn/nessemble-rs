---
nessemble: minor
---

**Behaviour change:** `NAME = expr` inside an `.enum` now resets the counter, so the next member is `expr + STEP` (as in C). It used to leave the counter where it was.
