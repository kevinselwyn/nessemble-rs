---
nessemble: patch
---

Fixed a build break in emit_source on 32-bit targets (wasm32, i686), which hardcoded Rhai's Dynamic tag type as i32 instead of the width Rhai actually uses per target.
