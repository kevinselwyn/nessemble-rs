---
nessemble: minor
---

Add `.for START, END` … `.endfor` unrolled loops: the body is assembled once per counter value (`\i`, with `\j`/`\k` for outer loops), and a name followed by a counter (`row\i:`) defines one label or constant per iteration.
