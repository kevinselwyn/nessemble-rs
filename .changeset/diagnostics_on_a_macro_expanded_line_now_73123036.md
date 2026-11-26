---
nessemble: patch
---

Diagnostics on a macro-expanded line now name the file the macro body is in, with a note for each `.macro` call that expanded it. Previously they paired the body's line number with the calling file's name.
