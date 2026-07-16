# NES 2.0

> Reference mirror of the NESdev Wiki article on the NES 2.0 ROM format:
> <https://www.nesdev.org/wiki/NES_2.0>. Reproduced here for offline reference
> because this assembler's `.ines2` and companion pseudo-instructions emit a
> NES 2.0 header. Companion to [`ines.md`](ines.md).

**NES 2.0** is a backwards-compatible extension of the [iNES](ines.md) ROM
format. It keeps the same `.nes` file extension and 16-byte header layout, but
repurposes the previously-unused header bytes 8-15 to remove iNES's ambiguities
and size limits: a 12-bit mapper number, submappers, exact PRG/CHR ROM and
RAM/NVRAM sizes, precise CPU/PPU timing, and console/expansion-device typing.

## Identification

A header is NES 2.0 when the identifier bits in **byte 7 (bits 2-3) equal `2`**
(binary `10`) *and* the total ROM size implied by the header does not exceed the
file. Formally:

- `(byte 7 & 0x0C) == 0x08`, and the size taking byte 9 into account does not
  exceed the actual image size → **NES 2.0**.

## File layout

A NES 2.0 file consists of, in order:

1. Header (16 bytes)
2. Trainer, if present (0 or 512 bytes)
3. PRG-ROM data
4. CHR-ROM data, if present
5. Miscellaneous ROM data, if present

## Header

| Byte | Description |
| ---- | ----------- |
| 0-3  | Constant `$4E $45 $53 $1A` (`"NES"` + MS-DOS EOF) |
| 4    | PRG-ROM size LSB |
| 5    | CHR-ROM size LSB |
| 6    | Flags 6 — mapper D0-3, mirroring, battery, trainer, four-screen |
| 7    | Flags 7 — mapper D4-7, NES 2.0 identifier, console type |
| 8    | Mapper D8-11, submapper |
| 9    | PRG-ROM size MSB, CHR-ROM size MSB |
| 10   | PRG-RAM / PRG-NVRAM (EEPROM) size |
| 11   | CHR-RAM / CHR-NVRAM size |
| 12   | CPU/PPU timing |
| 13   | Console-type-dependent (VS System, or extended console type) |
| 14   | Miscellaneous ROMs |
| 15   | Default expansion device |

### Byte 6 (Flags 6)

Identical to iNES:

```
76543210
||||||||
|||||||+- Nametable arrangement (mirroring)
||||||+-- Battery / other non-volatile memory
|||||+--- 512-byte trainer
||||+---- Alternative nametable layout (four-screen)
++++----- Mapper number D0-3
```

### Byte 7 (Flags 7)

```
76543210
||||||||
||||||++- Console type
||||++--- NES 2.0 identifier (= 2)
++++----- Mapper number D4-7
```

**Console type** (bits 0-1):

| Value | Console |
| ----- | ------- |
| 0 | Nintendo Entertainment System / Family Computer |
| 1 | Nintendo Vs. System |
| 2 | Nintendo PlayChoice-10 |
| 3 | Extended console type (see byte 13) |

### Byte 8 — Mapper MSB / submapper

```
76543210
||||||||
||||++++- Mapper number D8-11
++++----- Submapper number
```

Combined with bytes 6 and 7, the mapper number is 12 bits (0-4095). The
submapper (0-15) distinguishes hardware variants of a mapper.

### Byte 9 — PRG-ROM / CHR-ROM size MSB

```
76543210
||||||||
||||++++- PRG-ROM size D8-11
++++----- CHR-ROM size D8-11
```

The MSB nibble is prepended to the corresponding LSB (byte 4 / byte 5) to form a
12-bit size in 16 KiB (PRG) or 8 KiB (CHR) units.

**Exponent-multiplier notation.** When an MSB nibble is `$F`, the matching LSB
byte is instead read as `EEEEEEMM`, and the size in bytes is
`2^E * (MM * 2 + 1)`. This encodes sizes that are not a multiple of the bank
unit, or that are very large.

### Byte 10 — PRG-RAM / PRG-NVRAM size

```
76543210
||||||||
||||++++- PRG-RAM (volatile) shift count
++++----- PRG-NVRAM / EEPROM (non-volatile) shift count
```

Each nibble is a shift count: a count of `0` means no RAM of that kind;
otherwise the size is `64 << shift` bytes (so `7` → 8192 bytes). Non-volatile
memory requires the battery bit (Flags 6 bit 1) to be set.

### Byte 11 — CHR-RAM / CHR-NVRAM size

```
76543210
||||||||
||||++++- CHR-RAM (volatile) shift count
++++----- CHR-NVRAM (non-volatile) shift count
```

Same `64 << shift` encoding as byte 10.

### Byte 12 — CPU/PPU timing

```
76543210
||||||||
||||||++- Timing mode
++++++---- Reserved
```

| Value | Timing |
| ----- | ------ |
| 0 | RP2C02 ("NTSC NES") |
| 1 | RP2C07 ("Licensed PAL NES") |
| 2 | Multiple-region |
| 3 | UMC 6527P ("Dendy") |

### Byte 13 — VS System / extended console type

When the console type (byte 7 bits 0-1) is **1 (Vs. System)**:

```
76543210
||||||||
||||++++- Vs. PPU type
++++----- Vs. hardware type
```

When the console type is **3 (extended)**, byte 13 bits 0-3 hold the extended
console type number.

### Byte 14 — Miscellaneous ROMs

```
76543210
||||||||
||||||++- Number of miscellaneous ROMs present
++++++---- Reserved
```

### Byte 15 — Default expansion device

```
76543210
||||||||
||++++++- Default expansion device
++-------- Reserved
```

The expansion device (0-63) indicates the input/peripheral the game expects by
default.

## Relation to this assembler

The `.ines2` pseudo-instruction switches header output to NES 2.0; see
[`docs/src/syntax.md`](../docs/src/syntax.md) for the directives that populate
each field (`.inessubmap`, `.inesprgnvram`, `.ineschrram`, `.ineschrnvram`,
`.inestiming`, `.inesconsole`, `.inesvsppu`, `.inesvshw`, `.inesmiscrom`,
`.inesexpansion`), plus the iNES directives that widen or re-scope in NES 2.0
mode. Exponent-multiplier size notation is not currently emitted.

## References

- Official NES 2.0 specification (NESdev Wiki)
- [iNES format](ines.md)
