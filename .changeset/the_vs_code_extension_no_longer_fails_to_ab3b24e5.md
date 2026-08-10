---
nessemble: patch
---

The VS Code extension no longer fails to start with `unexpected argument '--stdio' found`: it left the language client's transport at an explicit `stdio`, which appends a `--stdio` flag that `nessemble lsp` does not accept.
