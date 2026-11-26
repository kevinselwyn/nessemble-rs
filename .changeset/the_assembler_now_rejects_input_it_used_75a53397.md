---
nessemble: minor
---

The assembler now rejects input it used to accept silently: a statement after a label on the same line, tokens left over after a statement (such as `JMP start.loc` or `.db 1 2`), and a `.macro` call with the wrong number of arguments. It also warns when a symbol is redefined (`duplicate-symbol`) and when a value does not fit its byte, word or zero-page field (`value-out-of-range`). **Behaviour change:** source with these mistakes no longer assembles; output for source without them is unchanged.
