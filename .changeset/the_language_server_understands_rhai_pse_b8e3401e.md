---
nessemble: minor
---

The language server understands `.rhai` pseudo-op scripts: host-API completion, hover, and signature help work even without a scripting host built in, and syntax diagnostics, four script-specific lints, an outline, folding, and go-to-definition/references for script-local functions are added when the `scripting` feature is on. Hovering a `.foo` directive now also shows the doc comment above the script's `custom` function.
