---
nessemble: minor
---

Add `.assert EXPR[, "message"]` and `.error "message"` to fail the build when an invariant breaks, and make the documented `.out "message"` print to stderr while assembling. A `pseudo.txt` mapping named `.assert`, `.error` or `.out` is now shadowed by the built-in.
