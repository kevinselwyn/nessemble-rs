---
nessemble: minor
---
Add `.inesbat`, `.ines4scr`, `.inesprgram`, `.inestv`, `.inesvs`, and
`.inespc10` pseudo-instructions so the battery, four-screen, PRG-RAM size, TV
system, VS Unisystem, and PlayChoice-10 fields of the iNES header can be
configured from source. `.inestv 1` (PAL) is also mirrored into the unofficial
Flags 10 TV-system field.
