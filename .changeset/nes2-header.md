---
nessemble: minor
---
Add NES 2.0 header support. The new `.ines2` pseudo-instruction emits a NES 2.0
header, widening `.inesmap` to 12-bit mappers and `.inesprg`/`.ineschr` to
12-bit sizes, and enabling the companion directives `.inessubmap`,
`.inesprgnvram`, `.ineschrram`, `.ineschrnvram`, `.inestiming`, `.inesconsole`,
`.inesvsppu`, `.inesvshw`, `.inesmiscrom`, and `.inesexpansion`. In NES 2.0 mode
`.inesprgram` takes a byte size, `.inestv` provides the timing fallback, and
`.inesvs`/`.inespc10` become console-type sugar.
