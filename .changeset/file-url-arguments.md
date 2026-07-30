---
nessemble: minor
---

Filename arguments can be declared with a `file://` prefix — `.tilemap "file://map.png"` — which reports a missing file against the directive before a custom pseudo-instruction's script runs, and is accepted (harmlessly) on `.include`, `.incbin`, `.incpng`, `.incpal`, `.incrle`, `.incwav`, and `.inestrn` too.
