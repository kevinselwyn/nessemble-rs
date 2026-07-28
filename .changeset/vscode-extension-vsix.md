---
nessemble: none
---
Build a VS Code extension (`nessemble_<version>.vsix`) in the release pipeline and attach it to every release, so VS Code and Cursor users can install a language-server client for `nessemble lsp` instead of hand-rolling one. Packaging only — the assembler and CLI are unchanged.
