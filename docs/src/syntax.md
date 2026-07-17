# Syntax

The examples on this page are interactive: edit the source and click
**Assemble** to build it in your browser (powered by the WebAssembly build of
nessemble). Assembled bytes are shown as a hex dump and can be downloaded.

<nessemble-assembler>
    LDX #$08
loop:
    DEX
    BNE loop
    BRK
</nessemble-assembler>

## Numbers

Binary, decimal, octal, hexadecimal, and ASCII character are all valid numbers.

| Base        | Example A | Example B |
|-------------|-----------|-----------|
| Binary      | %01000001 | 01000001b |
| Decimal     | 65        | 65d       |
| Octal       | 0101      | 101o      |
| Hexadecimal | $41       | 41h       |
| ASCII char  | 'A'       |           |

## Symbols

### Mathematical Operators

| Symbol   | Description |
|----------|-------------|
| +        | Add         |
| -        | Subtract    |
| \*       | Multiply    |
| /        | Divide      |
| \*\*     | Exponent    |
| &        | Bitwise AND |
| \|       | Bitwise OR  |
| ^        | Bitwise XOR |
| &gt;&gt; | Shift right |
| &lt;&lt; | Shift left  |
| %        | Modulo      |

### Comparison Operators

| Symbol | Description            |
|--------|------------------------|
| ==     | Equals                 |
| !=     | Not equals             |
| &lt;   | Less than              |
| &gt;   | Greater than           |
| &lt;=  | Less than or equals    |
| &gt;=  | Greater than or equals |

### Special

| Symbol | Description                   |
|--------|-------------------------------|
| -&gt;  | Accessor (functions like `+`) |

## Labels

### Named

Named label declarations must be in the follow format:

```nessemble
NAME:
```

* `NAME` - Label name, required.

Example:

```nessemble
    LDX #$08
loop:
    DEX
    BNE loop
    BRK
```

Output:

```text
00000000  a2 08 ca d0 fd 00                                 |......|
00000006
```

```nessemble
    LDX #$08
loop:
    DEX
    BNE loop
    BRK
```

### Temporary

Temporary/un-named labels may also be declared by placing only a colon.

```nessemble
:
```

To jump to a temporary label, the direction and count of the jumps must be
given.

```nessemble
JMP :[+-]
```

`[+-]` - Direction, required.

N-number of `+`s means to jump to the temporary label that is N temporary labels
further down in the code.

N-number of `-`s means to jump to the temporary label that is N temporary labels
further up in the code.

Example:

```nessemble
    LDX #$08
:
    DEX
    BNE :-
    BRK
```

Output:

```text
00000000  a2 08 ca d0 fd 00                                 |......|
00000006
```

```nessemble
    LDX #$08
:
    DEX
    BNE :-
    BRK
```

## Mnemonics

All 56 mnemonics are supported:

| Mnemonic | Description                          |
|----------|--------------------------------------|
| ADC      | Add with Carry                       |
| AND      | Bitwise AND with Accumulator         |
| ASL      | Arithmetic shift left                |
| BIT      | Test bits                            |
| BCC      | Branch on Carry clear                |
| BCS      | Branch on Carry set                  |
| BEQ      | Branch on equal                      |
| BMI      | Branch on minus                      |
| BNE      | Branch on not equal                  |
| BPL      | Branch on plus                       |
| BRK      | Break                                |
| BVC      | Branch on Overflow clear             |
| BVS      | Branch on Overflow set               |
| CLC      | Clear Carry                          |
| CLD      | Clear Decimal                        |
| CLI      | Clear Interrupt                      |
| CLV      | Clear Overflow                       |
| CMP      | Compare Accumulator                  |
| CPX      | Compare X register                   |
| CPY      | Compare Y register                   |
| DEC      | Decrement memory                     |
| DEX      | Decrement X register                 |
| DEY      | Decrement Y register                 |
| EOR      | Bitwise exclusive OR                 |
| INC      | Increment memory                     |
| INX      | Increment X register                 |
| INY      | Increment Y register                 |
| JMP      | Jump                                 |
| JSR      | Jump to subroutine                   |
| LDA      | Load Accumulator                     |
| LDX      | Load X register                      |
| LDY      | Load Y register                      |
| LSR      | Logical shift right                  |
| NOP      | No operation                         |
| ORA      | Bitwise OR with Accumulator          |
| PHA      | Push Accumulator                     |
| PHP      | Push processor status                |
| PLA      | Pull Accumulator                     |
| PLP      | Pull processor status                |
| ROL      | Rotate left                          |
| ROR      | Rotate right                         |
| RTI      | Return from Interrupt                |
| RTS      | Return from subroutine               |
| SBC      | Subtract with Carry                  |
| SEC      | Set Carry                            |
| SED      | Set Decimal                          |
| SEI      | Set Interrupt                        |
| STA      | Store Accumulator                    |
| STX      | Store X register                     |
| STY      | Store Y register                     |
| TAX      | Transfer Accumulator to X register   |
| TAY      | Transfer Accumulator to Y register   |
| TSX      | Transfer Stack Pointer to X register |
| TXA      | Transfer X register to Accumulator   |
| TXS      | Transfer X register to Stack Pointer |
| TYA      | Transfer Y register to Accumulator   |

Read more about 6502 opcodes
[here](http://www.6502.org/tutorials/6502opcodes.html).

In addition, 24 illegal/undocumented mnemonics may be used when assembled with
the [`-u, --undocumented`](/usage/#-u-undocumented) flag.

| Mnemonic | Description                                  |
|----------|----------------------------------------------|
| AAC      | AND with Accumulator                         |
| AAX      | AND X register with Accumulator              |
| ARR      | AND with Accumulator                         |
| ASR      | AND with Accumulator                         |
| ATX      | AND with Accumulator                         |
| AXA      | AND X register with Accumulator              |
| AXS      | AND X register with Accumulator              |
| DCP      | Subtract 1 from memory                       |
| DOP      | No operation (x2)                            |
| ICS      | Increase memory by 1                         |
| KIL      | Stop program counter                         |
| LAR      | AND memory with stack pointer                |
| LAX      | Load Accumulator and X register              |
| NOP      | No operation                                 |
| RLA      | Rotate one bit left in memory                |
| RRA      | Rotate one bit right in memory               |
| SBC      | Subtract with Carry                          |
| SLO      | Shift left one bit in memory                 |
| SRE      | Shift right one bit in memory                |
| SXA      | AND Y register with the high byte of address |
| SYA      | AND Y register with the high byte of address |
| TOP      | No operation (x3)                            |
| XAA      | Unknown                                      |
| XAS      | AND X register with Accumulator              |

Read more about undocumented 6502 opcodes
[here](http://nesdev.com/undocumented_opcodes.txt).

## Addressing Modes

| Mode        | Example      |
|-------------|--------------|
| Implied     | RTS          |
| Accumulator | ROL A        |
| Immediate   | LDA #$42     |
| Zeropage    | STA <$42     |
| Zeropage, X | EOR <$42, X  |
| Zeropage, Y | LDX <$42, Y  |
| Absolute    | STA $4200    |
| Absolute, X | EOR $4200, X |
| Absolute, Y | LDX $4200, Y |
| Indirect    | JMP [$4200]  |
| Indirect, X | LDA [$42, X] |
| Indirect, Y | STA [$42], Y |
| Relative    | BEQ label    |

> `nessemble` uses square brackets `[]` instead of parentheses `()` in its
> addressing modes because the latter are used to indicate precedence in
> mathematical operations.

Read more about 6502 addressing modes
[here](http://www.emulator101.com/6502-addressing-modes.html).

## Functions

| Function | Description              |
|----------|--------------------------|
| HIGH()   | Get high byte of address |
| LOW()    | Get low byte of address  |
| BANK()   | Get bank of address      |

## Pseudo-Instructions

| Pseudo-Instruction     | Description                                                                         |
|------------------------|-------------------------------------------------------------------------------------|
| [.ascii](#ascii)       | Convert ASCII string to bytes                                                       |
| [.byte](#db)           | Alias for [`.db`](#db)                                                              |
| [.checksum](#checksum) | Calculate crc32 checksum                                                            |
| [.chr](#chr)           | Set CHR bank index                                                                  |
| [.color](#color)       | Convert hex color to NES color                                                      |
| [.db](#db)             | Define 8-bit byte(s)                                                                |
| [.defchr](#defchr)     | Define CHR tile                                                                     |
| [.dw](#dw)             | Define 16-bit word(s)                                                               |
| [.else](#else)         | Else condition of an [`.if`](#if)/[`.ifdef`](#ifdef)/[`.ifndef`](#ifndef) statement |
| [.endenum](#endenum)   | End [`.enum`](#enum)                                                                |
| [.endif](#endif)       | End [`.if`](#if)/[`.ifdef`](#ifdef)/[`.ifndef`](#ifndef) statement                  |
| [.endm](#endm)         | End [`.macrodef`](#macrodef)                                                        |
| [.enum](#enum)         | Start enumerated variable declarations                                              |
| [.fill](#fill)         | Fill with bytes                                                                     |
| [.font](#font)         | Generate font character tile                                                        |
| [.hibytes](#hibytes)   | Output only the high byte of 16-bit word(s)                                         |
| [.if](#if)             | Test if condition                                                                   |
| [.ifdef](#ifdef)       | Test if variable is defined                                                         |
| [.ifndef](#ifndef)     | Test if variable has not been defined                                               |
| [.incbin](#incbin)     | Include binary file                                                                 |
| [.include](#include)   | Include assembly file                                                               |
| [.incpal](#incpal)     | Include palette from PNG                                                            |
| [.incpng](#incpng)     | Include PNG                                                                         |
| [.incrle](#incrle)     | Include binary data to be RLE-encoded                                               |
| [.incwav](#incwav)     | Include WAV                                                                         |
| [.ines2](#ines2)       | Emit a NES 2.0 header                                                                |
| [.ines4scr](#ines4scr) | iNES four-screen VRAM flag                                                           |
| [.inesbat](#inesbat)   | iNES battery / persistent memory flag                                               |
| [.ineschr](#ineschr)   | iNES CHR count                                                                      |
| [.ineschrnvram](#ineschrnvram) | NES 2.0 battery CHR-RAM size                                                 |
| [.ineschrram](#ineschrram) | NES 2.0 CHR-RAM size                                                             |
| [.inesconsole](#inesconsole) | NES 2.0 console type                                                           |
| [.inesexpansion](#inesexpansion) | NES 2.0 default expansion device                                          |
| [.inesmap](#inesmap)   | iNES / NES 2.0 mapper number                                                        |
| [.inesmir](#inesmir)   | iNES mirroring                                                                      |
| [.inesmiscrom](#inesmiscrom) | NES 2.0 miscellaneous ROM count                                               |
| [.inespc10](#inespc10) | iNES PlayChoice-10 flag                                                              |
| [.inesprg](#inesprg)   | iNES / NES 2.0 PRG count                                                            |
| [.inesprgnvram](#inesprgnvram) | NES 2.0 battery PRG-RAM size                                                 |
| [.inesprgram](#inesprgram) | iNES / NES 2.0 PRG-RAM size                                                      |
| [.inessubmap](#inessubmap) | NES 2.0 submapper number                                                         |
| [.inestiming](#inestiming) | NES 2.0 CPU/PPU timing                                                           |
| [.inestrn](#inestrn)   | iNES trainer include                                                                |
| [.inestv](#inestv)     | iNES / NES 2.0 TV system                                                            |
| [.inesvs](#inesvs)     | iNES VS Unisystem flag                                                              |
| [.inesvshw](#inesvshw) | NES 2.0 VS System hardware type                                                      |
| [.inesvsppu](#inesvsppu) | NES 2.0 VS System PPU type                                                         |
| [.lobytes](#lobytes)   | Output only the low byte of 16-bit word(s)                                          |
| [.macro](#macro)       | Call macro                                                                          |
| [.macrodef](#macrodef) | Start macro definition                                                              |
| [.org](#org)           | Organize code                                                                       |
| [.out](#out)           | Output debugging message                                                            |
| [.prg](#prg)           | Set PRG bank index                                                                  |
| [.random](#random)     | Output random byte(s)                                                               |
| [.rsset](#rsset)       | Set initial value for [`.rs`](#rs) declarations                                     |
| [.rs](#rs)             | Reserve space for variable declaration                                              |
| [.segment](#segment)   | Set code segment                                                                    |
| [.word](#dw)           | Alias for [`.dw`](#dw)                                                              |

### .ascii

Convert ASCII string to bytes.

Usage:

```nessemble
.ascii "STRING"[(+/-)NUMBER]
```

* `"STRING"` - String, required. ASCII string to turn into bytes. Must be within
quotes.
* `(+/-)NUMBER` - Number, optional. Amount to increase/decrease ASCII values.

Example:

```nessemble
.ascii "When, in disgrace with fortune and men's eyes"
```

Output:

```text
00000000  57 68 65 6e 2c 20 69 6e  20 64 69 73 67 72 61 63  |When, in disgrac|
00000010  65 20 77 69 74 68 20 66  6f 72 74 75 6e 65 20 61  |e with fortune a|
00000020  6e 64 20 6d 65 6e 27 73  20 65 79 65 73           |nd men's eyes|
0000002d
```

Try it:

<nessemble-assembler>
.ascii "When, in disgrace with fortune and men's eyes"
</nessemble-assembler>

The `+/-` operators may also be used to increase/decrease the output.

Example:

```nessemble
.ascii "I all alone beweep my outcast state"-32
```

Output:

```text
00000000  29 00 41 4c 4c 00 41 4c  4f 4e 45 00 42 45 57 45  |).ALL.ALONE.BEWE|
00000010  45 50 00 4d 59 00 4f 55  54 43 41 53 54 00 53 54  |EP.MY.OUTCAST.ST|
00000020  41 54 45                                          |ATE|
00000023
```


### .checksum

Calculate crc32 checksum.

Usage:

```nessemble
.checksum LABEL
```

* `LABEL` - Label, required. Label at which to start generating the checksum.

Example:

```nessemble
start:
    LDA #$01
    STA <$02
    .checksum start
```

Output:

```text
00000000  a9 01 85 02 b8 1f ee 86                           |........|
00000008
```

The checksum is `b8 1f ee 86`.

> Checksums may only be performed on preceding data.

```nessemble
start:
    LDA #$01
    STA <$02
    .checksum start
```

### .chr

Set CHR bank index.

Usage:

```nessemble
.chr NUMBER
```

* `NUMBER` - Number, required. CHR bank index.

Example:

```nessemble
.chr 0
```

> CHR banks are 2K bytes (0x2000) in size.

### .color

Convert hex color to NES color.

Finds the closest valid NES color to the given hex color.

Usage:

```nessemble
.color NUMBER[, NUMBER, ...]
```

* `NUMBER` - Number, required. At least one number is required.
* `, NUMBER, ...` - Number(s), optional. Additional comma-separated numbers may
be used.

Example:

```nessemble
.color $FF0000
```

Output:

```text
00000000  16                                                |.|
00000001
```

Read more about the NES color palette
[here](https://en.wikipedia.org/wiki/List_of_video_game_console_palettes#NES).


### .db

Define 8-bit byte(s).

Usage:

```nessemble
.db NUMBER[, NUMBER, ...]
```

* `NUMBER` - Number, required. At least one number is required.
* `, NUMBER, ...` - Number(s), optional. Additional comma-separated numbers may
be used.

Example:

```nessemble
.db $12, $34
```

Output:

```text
00000000  12 34                                             |.4|
00000002
```

A trailing comma continues the list onto the next line, so a long run of bytes
can be wrapped across several indented lines:

```nessemble
.db $00, $01, $02, $03,
    $04, $05, $06, $07
```

The same line-continuation rule applies to every comma-separated data directive
(`.dw`, `.fill`, `.color`, `.hibytes`, `.lobytes`, and `.defchr`).


### .defchr

Define CHR tile.

Only numbers from `0-3` may be used: `0` representing black, `1` dark grey, `2`
light grey, and `3` representing white.

Usage:

```nessemble
.defchr XXXXXXXX,
        XXXXXXXX,
        XXXXXXXX,
        XXXXXXXX,
        XXXXXXXX,
        XXXXXXXX,
        XXXXXXXX,
        XXXXXXXX
```

* `XXXXXXXX,` - Number, required. Must be exactly 8 numbers of 8-characters
each.

Example:

```nessemble
.defchr 333333333,
        300000003,
        300000003,
        300000003,
        300000003,
        300000003,
        300000003,
        333333333
```

Output:

```text
00000000  ff 01 01 01 01 01 01 ff  ff 01 01 01 01 01 01 ff  |................|
00000010
```

Read more about PPU pattern tables
[here](https://wiki.nesdev.com/w/index.php/PPU_pattern_tables).

```nessemble
.defchr 333333333,
        300000003,
        300000003,
        300000003,
        300000003,
        300000003,
        300000003,
        333333333
```

### .dw

Define 16-bit word(s).

Usage:

```nessemble
.dw NUMBER[, NUMBER, ...]
```

* `NUMBER` - Number, required. At least one number is required.
* `, NUMBER, ...` - Number(s), optional. Additional comma-separated numbers may
be used.

Example:

```nessemble
.dw $1234, $45678
```

Output:

```text
00000000  34 12 78 56                                       |4.xV|
00000004
```


### .else

Else condition of an [`.if`](#if)/[`.ifdef`](#ifdef)/[`.ifndef`](#ifndef) statement.

Usage:

```nessemble
.else
```

Example:

```nessemble
.ifdef SOMETHING
    STA $00
.else
    STA $01
.endif
```

### .endenum

End [`.enum`](#enum).

Usage:

```nessemble
.endenum
```

Example:

```nessemble
.enum $0080

TEST_0 .rs 1
TEST_1 .rs 2
TEST_2 .rs 1

.endenum
```

### .endif

End [`.if`](#if)/[`.ifdef`](#ifdef)/[`.ifndef`](#ifndef) statement.

Usage:

```nessemble
.endif
```

Example:

```nessemble
.ifdef SOMETHING
    STA $00
.else
    STA $01
.endif
```

### .endm

End [`.macrodef`](#macrodef).

Usage:

```nessemble
.endm
```

Example:

```nessemble
.macrodef TEST_MACRO
    LDA #\1
    STA <\2
.endm
```

See the section on [Macros](#macros) for more information.

### .enum

Start enumerated variable declarations.

Usage:

```nessemble
.enum START[, INC]
```

* `START` - Number, required. Value at which to start enumerating.
* `, INC` - Number, optional. Amount to increment after each enumeration.

Example:

```nessemble
.enum $0080

TEST_0 .rs 1
TEST_1 .rs 2
TEST_2 .rs 1

.endenum
```

### .fill

Fill with bytes.

Usage:

```nessemble
.fill COUNT[, VALUE]
```

* `COUNT` - Number, required. Number of bytes to fill.
* `, VALUE` - Number, optional. Value of each byte. Defaults to $FF.

Example:

```nessemble
.fill 16
```

Output:

```text
00000000  ff ff ff ff ff ff ff ff  ff ff ff ff ff ff ff ff  |................|
00000010
```


### .font

Generate font character tile.

Usage:

```nessemble
.font START[, END]
```

* `START` - Character/number, required. Starting ASCII character or code.
* `[, END]` - Character/number, optional. Ending ASCII character or code. If
included, all font tiles from `START` to `[, END]` (inclusive) will be
generated.

Example:

```nessemble
.font 'A', 'G'
```

Output:

```text
00000000  38 44 7c 44 44 44 44 00  38 44 7c 44 44 44 44 00  |8D|DDDD.8D|DDDD.|
00000010  78 44 78 44 44 44 78 00  78 44 78 44 44 44 78 00  |xDxDDDx.xDxDDDx.|
00000020  38 44 40 40 40 44 38 00  38 44 40 40 40 44 38 00  |8D@@@D8.8D@@@D8.|
00000030  78 44 44 44 44 44 78 00  78 44 44 44 44 44 78 00  |xDDDDDx.xDDDDDx.|
00000040  7c 40 70 40 40 40 7c 00  7c 40 70 40 40 40 7c 00  ||@p@@@|.|@p@@@|.|
00000050  7c 40 70 40 40 40 40 00  7c 40 70 40 40 40 40 00  ||@p@@@@.|@p@@@@.|
00000060  3c 40 4c 44 44 44 38 00  3c 40 4c 44 44 44 38 00  |<@LDDD8.<@LDDD8.|
00000070
```

Read more about PPU pattern tables
[here](https://wiki.nesdev.com/w/index.php/PPU_pattern_tables).


### .hibytes

Output only the high byte of 16-bit word(s).

Usage:

```nessemble
.hibytes NUMBER[, NUMBER]
```

* `NUMBER` - Number, required. At least one number is required.
* `, NUMBER, ...` - Number(s), optional. Additional comma-separated numbers may
be used.

Example:

```nessemble
.hibytes $1234, $5678
```

Output:

```text
00000000  12 56                                             |.V|
00000002
```


### .if

Test if condition.

Can be accompanied by an [`.else`](#else) and must be accompanied by an
[`.endif`](#endif).

Usage:

```nessemble
.if CONDITION
```

* `CONDITION` - Condition, required. The code that follows will be processed if
the condition is true. See [Comparison Operators](#comparison-operators).

Example:

```nessemble
.if SOMETHING == $01
    LDA #$01
.endif
```

### .ifdef

Test if variable is defined.

Can be accompanied by an [`.else`](#else) and must be accompanied by an
[`.endif`](#endif).

Usage:

```nessemble
.ifdef VARIABLE
```

* `VARIABLE` - Variable/constant/etc., required. The code that follows will be
processed if the variable has been defined.

Example:

```nessemble
.ifdef SOMETHING
    STA $00
.else
    STA $01
.endif
```

### .ifndef

Test if variable has not been defined.

Can be accompanied by an [`.else`](#else) and must be accompanied by an
[`.endif`](#endif).

Usage:

```nessemble
.ifndef VARIABLE
```

* `VARIABLE` - Variable/constant/etc., required. The code that follows will be
processed if the variable has not been defined.

Example:

```nessemble
.ifndef SOMETHING
    STA $01
.else
    STA $00
.endif
```

### .incbin

Include binary file.

Usage:

```nessemble
.incbin "FILENAME"[, OFFSET[, LIMIT]]
```

* `"FILENAME"` - Path to file, required. Must be within quotes.
* `[, OFFSET` - File offset index, optional. Index at which to start including
binary file.
* `[, LIMIT]]` - Limit in bytes, optional. Number of total bytes to include.

Example:

```nessemble
.incbin "file.bin"
```

### .include

Include assembly file.

Usage:

```nessemble
.incbin "FILENAME"
```

* `"FILENAME"` - Path to file, required. Must be within quotes.

Example:

```nessemble
.include "file.asm"
```

> Included files share a global state with other included files and the main
> entry point file. That means if a variable is defined in one file, it is
> available to all other files, provided that they are included after the
> definition.

> Relative filenames in `.include` — and in every filename-based directive
> (`.incbin`, `.incpng`, `.incpal`, `.incrle`, `.incwav`, `.inestrn`) — are
> resolved relative to the directory of the file that contains the directive.
> A file included from a subdirectory therefore resolves its own includes and
> assets from that subdirectory, not from the top-level project directory.

### .incpal

Include palette from PNG.

Usage:

```nessemble
.incpal "FILENAME"
```

* `"FILENAME"` - Path to file, required. Must be within quotes.

Example:

```nessemble
.incpal "palette.png"
```

> The PNG will be scanned, row-by-row/pixel-by-pixel, from the top-left to
> the bottom-right until it encounters 4 different, but not necessarily unique,
> colors.

### .incpng

Include PNG.

Converts the PNG to CHR tiles. The image must include only 4 colors:

| Color                                  | Name       | RGB           | Hex     |
|:--------------------------------------:|------------|---------------|---------|
| <i class="fa fa-stop color-black"></i> | Black      | 0, 0, 0       | #000000 |
| <i class="fa fa-stop color-dgrey"></i> | Dark Grey  | 85, 85, 85    | #555555 |
| <i class="fa fa-stop color-lgrey"></i> | Light Grey | 170, 170, 170 | #AAAAAA |
| <i class="fa fa-stop color-white"></i> | White      | 255, 255, 255 | #FFFFFF |

> Other colors may be used, but accuracy is not guaranteed.

Usage:

```nessemble
.incpng "FILENAME"
```

* `"FILENAME"` - Path to file, required. Must be within quotes.

Example:

```nessemble
.incpng "image.png"
```

Read more about PPU pattern tables
[here](https://wiki.nesdev.com/w/index.php/PPU_pattern_tables).

### .incrle

Include binary data to be RLE-encoded

The RLE-encoding scheme used is one featured in a few Konami NES titles, known
as `Konami RLE`. The breakdown of bytes:

| Value | Description                                          |
|-------|------------------------------------------------------|
| 00-80 | Read another byte and write it to the output N times |
| 81-FE | Copy N-128 bytes from input to output                |
| FF    | End of compressed data                               |

Usage:

```nessemble
.incrle "FILENAME"
```

* `"FILENAME"` - Path to file, required. Must be within quotes.

Read more about NES RLE compression
[here](https://wiki.nesdev.com/w/index.php/Tile_compression).

### .incwav

Include WAV.

Converts WAV to a 1-bit PCM.

Usage:

```nessemble
.incwav "FILENAME"[, AMPLITUDE]
```

* `"FILENAME"` - Path to file, required. Must be within quotes.
* `[, AMPLITUDE]` - Amplitude, optional. Amplitude of WAV.

Example:

```nessemble
.incwav "audio.wav", 24
```

### .ines2

Emit a NES 2.0 header.

Sets bits 2-3 of Flags 7 to the NES 2.0 identifier (`10`) and switches the
output header from iNES 1.0 to [NES 2.0](https://www.nesdev.org/wiki/NES_2.0),
which widens the mapper (0-4095) and PRG/CHR sizes and repurposes bytes 8-15.

When NES 2.0 mode is active, some existing directives change meaning:

- [.inesprg](#inesprg) / [.ineschr](#ineschr) become 12-bit (the byte-9 MSB
  nibbles are written automatically for counts above 255).
- [.inesmap](#inesmap) becomes 12-bit (writes byte 8 as well).
- [.inesprgram](#inesprgram) targets the NES 2.0 PRG-RAM field and takes a
  **byte** size (not 8 KB units).
- [.inestv](#inestv) provides the NTSC/PAL fallback for the
  [.inestiming](#inestiming) byte.
- [.inesvs](#inesvs) / [.inespc10](#inespc10) become sugar for the
  [.inesconsole](#inesconsole) type.

NES 2.0-only directives ([.inessubmap](#inessubmap),
[.inesprgnvram](#inesprgnvram), [.ineschrram](#ineschrram),
[.ineschrnvram](#ineschrnvram), [.inestiming](#inestiming),
[.inesconsole](#inesconsole), [.inesvsppu](#inesvsppu),
[.inesvshw](#inesvshw), [.inesmiscrom](#inesmiscrom),
[.inesexpansion](#inesexpansion)) require this directive.

Usage:

```nessemble
.ines2 FLAG
```

* `FLAG` - Number, required. Non-zero to emit a NES 2.0 header.

Example:

```nessemble
.ines2 1
```

### .ines4scr

iNES four-screen VRAM flag.

Sets bit 3 of Flags 6 (the "alternative nametable layout" bit), used by boards
that provide four-screen VRAM instead of the hard-wired mirroring selected by
[.inesmir](#inesmir).

Usage:

```nessemble
.ines4scr FLAG
```

* `FLAG` - Number, required. Non-zero to set four-screen VRAM.

Example:

```nessemble
.ines4scr 1
```

### .inesbat

iNES battery / persistent memory flag.

Sets bit 1 of Flags 6, indicating the cartridge contains battery-backed PRG-RAM
at `$6000-$7FFF` (or other persistent memory).

Usage:

```nessemble
.inesbat FLAG
```

* `FLAG` - Number, required. Non-zero to indicate persistent memory.

Example:

```nessemble
.inesbat 1
```

### .ineschr

iNES CHR count.

Usage:

```nessemble
.ineschr COUNT
```

* `COUNT` - Number, required. Number of CHR banks. In [NES 2.0](#ines2) mode a
  count above 255 also writes the byte-9 MSB nibble (up to 4095).

Example:

```nessemble
.ineschr 1
```

### .ineschrnvram

NES 2.0 battery CHR-RAM size.

Sets the battery-backed CHR-RAM field (byte 11 bits 4-7) of a
[NES 2.0](#ines2) header. Requires [.ines2](#ines2).

Usage:

```nessemble
.ineschrnvram BYTES
```

* `BYTES` - Number, required. Size in bytes: `0`, or a power-of-two byte count
  from 128 to 2097152 (stored as the shift count `size = 64 << n`).

Example:

```nessemble
.ineschrnvram 8192
```

### .ineschrram

NES 2.0 CHR-RAM size.

Sets the volatile CHR-RAM field (byte 11 bits 0-3) of a [NES 2.0](#ines2)
header. Requires [.ines2](#ines2).

Usage:

```nessemble
.ineschrram BYTES
```

* `BYTES` - Number, required. Size in bytes: `0`, or a power-of-two byte count
  from 128 to 2097152 (stored as the shift count `size = 64 << n`).

Example:

```nessemble
.ineschrram 8192
```

### .inesconsole

NES 2.0 console type.

Sets bits 0-1 of Flags 7. Requires [.ines2](#ines2). This is the canonical form
of [.inesvs](#inesvs) (value 1) and [.inespc10](#inespc10) (value 2); setting a
conflicting combination is an error.

| Value | Console type       |
|:-----:|:-------------------|
| 0     | Nintendo NES / FC  |
| 1     | VS System          |
| 2     | PlayChoice-10      |
| 3     | Extended (unsupported) |

> Value 3 (extended console type) is not yet supported.

Usage:

```nessemble
.inesconsole NUMBER
```

* `NUMBER` - Number, required. Console type (0-3).

Example:

```nessemble
.inesconsole 1
```

### .inesexpansion

NES 2.0 default expansion device.

Sets byte 15 (bits 0-5) of a [NES 2.0](#ines2) header. Requires
[.ines2](#ines2).

Usage:

```nessemble
.inesexpansion NUMBER
```

* `NUMBER` - Number, required. Expansion device (0-63).

Example:

```nessemble
.inesexpansion 1
```

### .inesmap

iNES mapper number.

Usage:

```nessemble
.inesmap NUMBER
```

* `NUMBER` - Number, required. Mapper number. iNES supports 0-255; in
  [NES 2.0](#ines2) mode the range widens to 0-4095 (byte 8 holds the high
  nibble). A value above 255 requires [.ines2](#ines2).

Read more about NES mappers
[here](https://wiki.nesdev.com/w/index.php/List_of_mappers).

### .inesmir

iNES mirroring.

Sets bit 0 of Flags 6, the hard-wired nametable arrangement. The other Flags 6
bits are controlled by their own directives: [.inesbat](#inesbat) (battery),
[.inestrn](#inestrn) (trainer), and [.ines4scr](#ines4scr) (four-screen VRAM).

```text
xxxxxxx0
       |
       +- Mirroring: 0: horizontal (vertical arrangement)
                     1: vertical (horizontal arrangement)
```

| Value | Mirroring  |
|:-----:|:----------:|
| 0     | Horizontal |
| 1     | Vertical   |

Usage:

```nessemble
.inesmir NUMBER
```

* `NUMBER` - Number, required. Mirroring type.

### .inesmiscrom

NES 2.0 miscellaneous ROM count.

Sets byte 14 (bits 0-1) of a [NES 2.0](#ines2) header, the number of
miscellaneous ROMs present. Requires [.ines2](#ines2).

Usage:

```nessemble
.inesmiscrom NUMBER
```

* `NUMBER` - Number, required. Number of miscellaneous ROMs (0-3).

Example:

```nessemble
.inesmiscrom 1
```

### .inespc10

iNES PlayChoice-10 flag.

Sets bit 1 of Flags 7, marking the ROM as a PlayChoice-10 title. This bit is not
part of the official specification and most emulators ignore it. In
[NES 2.0](#ines2) mode this is sugar for [.inesconsole](#inesconsole) type 2.

> Only the header bit is set. The optional 8 KB PlayChoice INST-ROM and PROM data
> sections are not emitted.

Usage:

```nessemble
.inespc10 FLAG
```

* `FLAG` - Number, required. Non-zero to mark a PlayChoice-10 title.

Example:

```nessemble
.inespc10 1
```

### .inesprg

iNES PRG count.

Usage:

```nessemble
.inesprg COUNT
```

* `COUNT` - Number, required. Number of PRG banks. In [NES 2.0](#ines2) mode a
  count above 255 also writes the byte-9 MSB nibble (up to 4095).

Example:

```nessemble
.inesprg 1
```

### .inesprgnvram

NES 2.0 battery PRG-RAM size.

Sets the battery-backed PRG-RAM field (byte 10 bits 4-7) of a [NES 2.0](#ines2)
header. Requires [.ines2](#ines2).

Usage:

```nessemble
.inesprgnvram BYTES
```

* `BYTES` - Number, required. Size in bytes: `0`, or a power-of-two byte count
  from 128 to 2097152 (stored as the shift count `size = 64 << n`).

Example:

```nessemble
.inesprgnvram 8192
```

### .inesprgram

iNES / NES 2.0 PRG-RAM size.

In iNES mode, sets byte 8 of the header, the size of PRG-RAM in 8 KB units (a
value of `0` infers 8 KB for compatibility).

In [NES 2.0](#ines2) mode, sets the volatile PRG-RAM field (byte 10 bits 0-3)
and the argument is a **byte** size instead — pair it with
[.inesprgnvram](#inesprgnvram) for battery-backed PRG-RAM.

Usage:

```nessemble
.inesprgram SIZE
```

* `SIZE` - Number, required. In iNES mode, PRG-RAM size in 8 KB units. In
  NES 2.0 mode, a byte size: `0`, or a power-of-two byte count from 128 to
  2097152 (stored as the shift count `size = 64 << n`).

Example:

```nessemble
.inesprgram 1
```

### .inessubmap

NES 2.0 submapper number.

Sets byte 8 (bits 4-7) of a [NES 2.0](#ines2) header, the submapper that
distinguishes variants of a mapper. Requires [.ines2](#ines2).

Usage:

```nessemble
.inessubmap NUMBER
```

* `NUMBER` - Number, required. Submapper number (0-15).

Example:

```nessemble
.inessubmap 1
```

### .inestiming

NES 2.0 CPU/PPU timing.

Sets byte 12 of a [NES 2.0](#ines2) header, the region timing. Requires
[.ines2](#ines2). When unset, [.inestv](#inestv) provides the NTSC/PAL value.

| Value | Timing        |
|:-----:|:--------------|
| 0     | RP2C02 (NTSC) |
| 1     | RP2C07 (PAL)  |
| 2     | Multi-region  |
| 3     | UMC 6527P (Dendy) |

Usage:

```nessemble
.inestiming NUMBER
```

* `NUMBER` - Number, required. Timing (0-3).

Example:

```nessemble
.inestiming 0
```

### .inestrn

iNES trainer include.

> The assembled trainer must be no larger than 512 (0x200) bytes. The
> appropriate flag is automatically set in the iNES header to indicate a trainer
> is present.

Usage:

```nessemble
.inestrn "FILENAME"
```

* `"FILENAME"` - Path to file, required. Must be within quotes.

Example:

```nessemble
.inestrn "trainer.asm"
```

### .inestv

iNES TV system.

Sets bit 0 of Flags 9, the TV system the ROM targets. PAL is also mirrored into
the unofficial Flags 10 TV-system field (bits 0-1: `0` NTSC, `2` PAL) that some
emulators honor. In [NES 2.0](#ines2) mode this instead provides the NTSC/PAL
fallback for the [.inestiming](#inestiming) byte.

```text
xxxxxxx0
       |
       +- TV system: 0: NTSC
                     1: PAL
```

| Value | TV system | Flags 9 | Flags 10 |
|:-----:|:---------:|:-------:|:--------:|
| 0     | NTSC      | 0       | 0        |
| 1     | PAL       | 1       | 2        |

Usage:

```nessemble
.inestv SYSTEM
```

* `SYSTEM` - Number, required. `0` for NTSC, `1` for PAL.

Example:

```nessemble
.inestv 1
```

### .inesvs

iNES VS Unisystem flag.

Sets bit 0 of Flags 7, marking the ROM as a VS Unisystem arcade title. In
[NES 2.0](#ines2) mode this is sugar for [.inesconsole](#inesconsole) type 1.

Usage:

```nessemble
.inesvs FLAG
```

* `FLAG` - Number, required. Non-zero to mark a VS Unisystem title.

Example:

```nessemble
.inesvs 1
```

### .inesvshw

NES 2.0 VS System hardware type.

Sets byte 13 (bits 4-7) of a [NES 2.0](#ines2) header. Only meaningful when the
[.inesconsole](#inesconsole) type is VS (1); ignored otherwise. Requires
[.ines2](#ines2).

Usage:

```nessemble
.inesvshw NUMBER
```

* `NUMBER` - Number, required. VS System hardware type (0-15).

Example:

```nessemble
.inesvshw 0
```

### .inesvsppu

NES 2.0 VS System PPU type.

Sets byte 13 (bits 0-3) of a [NES 2.0](#ines2) header. Only meaningful when the
[.inesconsole](#inesconsole) type is VS (1); ignored otherwise. Requires
[.ines2](#ines2).

Usage:

```nessemble
.inesvsppu NUMBER
```

* `NUMBER` - Number, required. VS System PPU type (0-15).

Example:

```nessemble
.inesvsppu 0
```

### .lobytes

Output only the low byte of 16-bit word(s).

Usage:

```nessemble
.lobytes NUMBER[, NUMBER]
```

* `NUMBER` - Number, required. At least one number is required.
* `, NUMBER, ...` - Number(s), optional. Additional comma-separated numbers may
be used.

Example:

```nessemble
.lobytes $1234, $5678
```

Output:

```text
00000000  34 78                                             |4x|
00000002
```


### .macro

Call macro.

Usage:

```nessemble
.macro MACRO[, NUMBER, ...]
```

* `MACRO` - Name, required. Name of previously-defined macro.
* `, NUMBER, ...` - Number(s), optional. Additional comma-separated numbers may
be used.

Example:

```nessemble
.macro TEST_MACRO
```

See the section on [Macros](#macros) for more information.

### .macrodef

Start macro definition.

Usage:

```nessemble
.macrodef MACRO
    CODE...
.endm
```

* `MACRO` - Name, required. Name of macro.
* `CODE...` - Code, required. Assembly code.

Example:

```nessemble
.macrodef TEST_MACRO
    LDA #\1
    STA <\2
.endm
```

See the section on [Macros](#macros) for more information.

### .org

Organize code.

Set the address of the current bank in which to start organizing code.

Usage:

```nessemble
.org ADDRESS
```

Example:

```nessemble
.org $C000
```

### .prg

Set PRG bank index.

Usage:

```nessemble
.prg NUMBER
```

* `NUMBER` - Number, required. PRG bank index.

Example:

```nessemble
.prg 0
```

> PRG banks are 4K bytes (0x4000) in size.

### .random

Output random byte(s).

The algorithm for the PRNG is the suggested POSIX implementation of `rand()`.

Usage:

```nessemble
.random [SEED[, COUNT]]
```

* `[SEED` - Number or string, optional. Seeds the random number generator.
Defaults to the current system time.
* `[, COUNT]]` - Number of bytes to output, optional. Defaults to 1.

Example:

```nessemble
.random "Secret Key", 16
```

### .rsset

Set initial value for [`.rs`](#rs) declarations.

Usage:

```nessemble
.rsset ADDRESS
```

* `ADDRESS` - Number, required. Address to start [`.rs`](#rs) declarations.

Example:

```nessemble
.rsset $0000
```

### .rs

Reserve space for variable declaration.

Usage:

```nessemble
VARIABLE .rs NUMBER
```

* `VARIABLE` - Variable name, required. Name of variable to declare.
* `NUMBER` - Number (in bytes) to reserve, required.

Example:

```nessemble
.rsset $0000

label_01 .rs 1
label_02 .rs 2
label_03 .rs 1

.db label_01, label_02, label_03
```

Output:

```text
00000000  00 01 03                                          |...|
00000003
```

```nessemble
.rsset $0000

label_01 .rs 1
label_02 .rs 2
label_03 .rs 1

.db label_01, label_02, label_03
```

### .segment

Set code segment.

Usage:

```nessemble
.segment "SEGMENT[0-9]+"
```

* `SEGMENT` - Type of segment, required. `PRG` or `CHR`.
* `[0-9]+` - Number, required. Segment index.

> The whole segment must be within quotes.

Example:

```nessemble
.segment "PRG1"
```

> This is an alias for `.prg x`.

## Optional Scripts

Some scripts are included with `nessemble`, but totally optional. They must be
installed with the [`scripts`](/usage/#scripts) command which provides additional
pseudo-instructions to use.

| Pseudo-Instruction | Description                        |
|--------------------|------------------------------------|
| [.ease](#ease)     | Generates bytes to simulate easing |

### .ease

Generates bytes to simulate easing

Usage:

```nessemble
.ease FUNCTION[, START[, END[, STEPS]]]
```

* `FUNCTION` - String, required. Easing function to perform. Must be within
quotes.
* `[, START` - Number, optional. Starting value. Defaults to 0.
* `[, END` - Number, optional. Ending value. Defaults to 16.
* `[, STEPS]]]` - Number, optional. Steps to perform. Defaults to 16.

Valid `FUNCTION`s include:

* "easeInQuad"
* "easeOutQuad"
* "easeInOutQuad"
* "easeInCubic"
* "easeOutCubic"
* "easeInOutCubic"
* "easeInQuint"
* "easeOutQuint"
* "easeInOutQuint"
* "easeInBounce"
* "easeOutBounce"
* "easeInOutBounce"

Example:

```nessemble
.ease "easeOutBounce", 0, $20, $40
```

Output:

```text
00000000  00 00 00 00 00 01 02 02  03 04 06 07 08 0a 0b 0d  |................|
00000010  0f 11 13 16 18 1a 1d 1f  1e 1d 1c 1b 1a 19 19 18  |................|
00000020  18 18 18 18 18 18 18 19  19 1a 1b 1c 1d 1e 1f 1f  |................|
00000030  1e 1e 1e 1e 1e 1e 1e 1e  1f 1f 1f 1f 1f 1f 1f 20  |............... |
00000040
```


Try it — the `.ease` script runs in your browser (custom pseudo-op scripting,
compiled to WebAssembly):

<nessemble-assembler data-opts='{"pseudo":{"ease":true}}'>
.ease "easeOutBounce", 0, $20, $40
</nessemble-assembler>

## Macros

Macros may be utilized to maximize code-reuse and may also be treated as custom
functions.

Example:

```nessemble
.macrodef TEST_MACRO
    LDA #$00
    STA $2005
    STA $2005
.endm

.macro TEST_MACRO
```

Output:

```text
00000000  a9 00 8d 05 20 8d 05 20                           |.... .. |
00000008
```

```nessemble
.macrodef TEST_MACRO
    LDA #$00
    STA $2005
    STA $2005
.endm

.macro TEST_MACRO

```

### Parameters

Macros may also have parameters.

Example:

```nessemble
.macrodef TEST_MACRO
    LDA #\1
    STA \2
    STA \2
.endm

.macro TEST_MACRO, $00, $2005
```

Output:

```nessemble
.macrodef TEST_MACRO
    LDA #\1
    STA \2
    STA \2
.endm

.macro TEST_MACRO, $00, $2005
```

One macro may have up to 256 parameters which are denoted with a `\` prefix. The
first parameter being `\1`, the next `\2`, and so on up to `\256`. All
parameters must be numbers (or label variables).

There is also a pseudo-parameter, `\#`, that returns the number of input
parameters.

Example:

```nessemble
.macrodef COUNT_PARAMS
    .db \#
.endm

.macro COUNT_PARAMS, $01, $01, $01
```

Output:

```text
00000000  03                                                |.|
00000001
```

There is another pseudo-parameter, `\@`, that returns a unique number every time
the macro is called.

Example:

```nessemble
.macrodef TEST_MACRO
    LDX #$08
label_\@:
    DEX
    BNE label_\@:
.endm

.macro TEST_MACRO
.macro TEST_MACRO
.macro TEST_MACRO
```

Output:

```text
00000000  a2 08 ca d0 fd a2 08 ca  d0 fd a2 08 ca d0 fd     |...............|
0000000f
```
