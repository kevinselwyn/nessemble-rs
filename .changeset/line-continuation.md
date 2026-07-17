---
nessemble: minor
---
Support line continuation in comma-separated directives. A trailing comma at the
end of a line now continues the operand list onto the next (indented) line, so a
long run can be wrapped across several lines:

```nessemble
.db $00, $01, $02, $03,
    $04, $05, $06, $07
```

This already worked for `.defchr`; it now applies uniformly to `.db`/`.byte`,
`.dw`/`.word`, `.fill`, `.color`, `.hibytes`, and `.lobytes`, as well as to
custom (`--pseudo`) directives, whose argument lists — numbers or quoted
strings — can now be wrapped the same way.
