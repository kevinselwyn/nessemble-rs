---
nessemble: patch
---
`nessemble format` now aligns the continuation lines of a multi-line statement
(an operand list wrapped onto the next line by a trailing comma) under the
opening line's first argument, instead of re-indenting them to the block indent.
`.metasprite` is the motivating case, but the rule applies to any statement whose
operands span multiple lines:

```asm
    .metasprite $FA, $02, $00, $FA,
                $FA, $03, $00, $02,
                $02, $0D, $00, $FA
```

The behavior is gated behind a new `.nessemblerc` boolean `alignContinuations`
(default `true`); set it to `false` to keep the previous block-indent behavior.
Alignment is computed from the opening line's actual emitted indent, so it stays
correct alongside `indentDirectives`; under `indentStyle: "tab"` the continuation
reuses the opening tab and pads to the first-argument column with spaces. Only
leading whitespace changes, so the assembled bytes are unaffected (covered by a
round-trip byte-preservation test with the option both on and off).
