---
nessemble: minor
---

**Potentially breaking:** the assembler now rejects three kinds of source it used to accept silently, so a project that assembled before may now fail. A label with anything after it on the same line (`foo: .db 1`, `: NOP`) is an error — v2.27.0 dropped the rest of the line and made `foo` a constant `= 1`. Tokens left over after a complete statement (`.db 1 2 3`, `LDA $10 garbage`, `QQ = 1 2`) are an error, with a hint about the missing comma or indentation. A `.macro` call passing a different number of arguments than the body's highest `\N` is an error at the call, naming the definition (a macro that reads `\#` is exempt). Each of these used to produce a ROM that differed from the source without any message.
