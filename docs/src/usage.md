# Usage

`nessemble` is driven from the command line. This page documents its options and
subcommands.

```text
Usage: nessemble [options] <infile.asm>
                 <command> [args]

Options:
  -o, --output <outfile.rom>   output file
  -f, --format {NES,RAW}       output format
  -e, --empty <hex>            empty byte value
  -u, --undocumented           use undocumented opcodes
  -l, --list <listfile.txt>    generate list of labels and constants
  -p, --pseudo <pseudo.txt>    use custom pseudo-instruction functions
  -c, --check                  check syntax only
  -C, --coverage               log data coverage
  -v, --version                display program version
  -L, --license                display program license
  -h, --help                   print this message

Commands:
  init [<arg> ...]                 initialize new project
  scripts                          install scripts
  reference [<category>] [<term>]  get reference info about assembly terms
  lsp                              run the language server (stdio)
  format [<opt> ...] <path> ...    format assembly source
```

The `lsp` command starts the built-in [Language Server](editor.md) for use with
LSP-capable editors. The `format` command reformats assembly source (§
[format](#format-opt--path-)).

## Options

### -o, --output &lt;outfile.rom&gt;

Sets the filename where output is written. An outfile of `-` (or omitting the
flag) writes to `stdout`.

```text
nessemble infile.asm --output outfile.rom
```

### -f, --format {NES,RAW}

Specifies the output format:

- `NES` — an iNES ROM, complete with a 16-byte header.
- `RAW` — raw assembled 6502 code.

The format is `RAW` by default, but if iNES header directives (`.inesprg`,
`.ineschr`, …) are present, it becomes `NES` unless overridden.

### -e, --empty &lt;hex&gt;

Sets the fill value for empty/unwritten ROM bytes. Defaults to `FF`.

```text
nessemble infile.asm --empty 00
```

### -u, --undocumented

Allows the use of undocumented ("illegal") opcodes.

### -l, --list &lt;listfile.txt&gt;

Writes a list of labels and constants to the given file.

### -p, --pseudo &lt;pseudo.txt&gt;

Points to a mapping file that enables custom pseudo-instructions. See
[Extending](extending.md).

### -c, --check

Checks the input for syntax errors only; produces no output.

### -C, --coverage

Reports per-bank ROM coverage. Only meaningful when the format is `NES`.

### -v, --version / -L, --license / -h, --help

Print the version, license, or usage message respectively.

## Commands

### init [&lt;arg&gt; ...]

Scaffolds a new project, prompting for any values not supplied as arguments:

```text
nessemble init [filename] [prg] [chr] [mapper] [mirroring]
```

- `filename` — file to create.
- `prg` / `chr` — number of PRG / CHR banks.
- `mapper` / `mirroring` — iNES mapper and mirroring.

### scripts

Installs the bundled custom-pseudo-instruction scripts into
`~/.nessemble/scripts`. See [Extending](extending.md).

### reference [&lt;category&gt;] [&lt;term&gt;]

Prints reference information from locally bundled data. With no arguments it
lists the categories (`instructions`, `directives`); with a category it lists
its entries; with a term it prints the details (e.g. `reference instructions
LDA`).

### format [&lt;opt&gt; ...] &lt;path&gt; ...

Reformats nessemble assembly source in an opinionated, [Prettier][prettier]-style
way: consistent indentation and comma spacing, `.db`/`.dw`/`.color` data
consolidated a fixed number of values per line, a blank line after each
`RTS`/`RTI`, collapsed runs of blank lines, and a normalized final newline.
Formatting is **cosmetic only** — the assembled ROM is never changed.

```text
nessemble format path/to/file.asm       # print formatted source to stdout
nessemble format --write path/to/dir    # rewrite files in place
nessemble format --check path/to/dir    # CI gate: exit non-zero if unformatted
```

- A single file with no flags prints the formatted result to `stdout`, leaving
  the file untouched.
- `-w`, `--write` rewrites each changed file in place and prints its path.
- `-c`, `--check` writes nothing; it lists files that are not already formatted
  and exits non-zero — the gate for CI.
- A directory is walked recursively (for the configured extensions, `.asm` by
  default) and requires `--write` or `--check`.
- `--config <file>` uses `<file>` as the [`.nessemblerc`](#nessemblerc); 
  `--no-config` ignores any `.nessemblerc` and uses built-in defaults.

The editor [Language Server](editor.md)'s "format document" action runs this same
formatter, so editors and the CLI produce identical output.

## .nessemblerc

Formatting is configurable, Prettier-style, via an optional `.nessemblerc` (or
`.nessemblerc.json`) file discovered by walking up from the file or directory
being formatted. It is JSON; every key is optional and takes the default shown
below, so a project with no `.nessemblerc` still gets fully-formatted output.
**Unknown keys are rejected** (to catch typos early).

```json
{
  "extensions": [".asm"],
  "indentStyle": "space",
  "indentWidth": 4,
  "commaSpacing": true,
  "finalNewline": true,
  "indentDirectives": false,
  "alignContinuations": true,
  "dataPerLine": 8,
  "respectStrideHints": true,
  "blankLineAfterReturn": true,
  "maxConsecutiveBlankLines": 2,
  "mnemonicCase": "preserve",
  "hexDigitCase": "preserve",
  "overrides": []
}
```

| Key | Default | Meaning |
| --- | --- | --- |
| `extensions` | `[".asm"]` | File extensions formatted during a directory walk. |
| `indentStyle` | `"space"` | Instruction indent: `"space"` or `"tab"`. |
| `indentWidth` | `4` | Spaces per indent level (space style only). |
| `commaSpacing` | `true` | `", "` between values; `false` for tight commas. |
| `finalNewline` | `true` | Ensure the file ends in exactly one newline. |
| `indentDirectives` | `false` | Indent directive lines (`.db`, `.dw`, `.include`, …) to block depth like instructions. `false` pins them to column 0 (house style); `true` suits codebases that indent data under labels. Labels and constants stay at column 0 either way. |
| `alignContinuations` | `true` | Align the continuation lines of a multi-line statement (operands wrapped onto the next line by a trailing comma) under the opening line's first argument. `false` indents them to the block indent (`indentWidth`). See below. |
| `dataPerLine` | `8` | Values per consolidated `.db`/`.dw`/`.color` line; `0` disables consolidation. |
| `respectStrideHints` | `true` | Honor `; @fmt stride=N[,N,...]` comments (see below). |
| `blankLineAfterReturn` | `true` | Insert one blank line after every `RTS`/`RTI`. |
| `maxConsecutiveBlankLines` | `2` | Collapse longer runs of blank lines down to this. |
| `mnemonicCase` | `"preserve"` | Case the instruction mnemonic: `"preserve"`, `"lower"`, or `"upper"`. |
| `hexDigitCase` | `"preserve"` | Case hex-digit letters (`$ab` vs `$AB`): `"preserve"`, `"lower"`, or `"upper"`. |
| `overrides` | `[]` | Per-glob option overrides (see below). |

Directive names (`.db`, `.DB`) are never re-cased — nessemble is case-sensitive
about them.

### Stride hints

To override `dataPerLine` for one data block, place a `; @fmt stride=N` comment
immediately before it. Multiple strides cycle in order and the last one repeats:

```asm
; @fmt stride=2
    .db $01, $02
    .db $03, $04
```

### Continuation alignment

When a statement's operand list wraps onto further lines (a trailing comma
continues it onto the next physical line), `alignContinuations` (on by default)
lines up each continuation under the opening line's first argument:

```asm
    .metasprite $FA, $02, $00, $FA,
                $FA, $03, $00, $02,
                $02, $0D, $00, $FA
```

With `alignContinuations: false`, continuation lines fall to the block indent
instead:

```asm
    .metasprite $FA, $02, $00, $FA,
    $FA, $03, $00, $02,
    $02, $0D, $00, $FA
```

The alignment is computed from the opening line's actual indent, so it stays
correct together with `indentDirectives`. Under `indentStyle: "tab"` the
continuation reuses the opening line's leading tab and then pads to the
first-argument column with spaces. Only leading whitespace changes, so the
assembled output is unaffected.

### Overrides

`overrides` is an ordered list of `{ "files": <glob>, "options": { … } }`
entries; for each formatted file, later matching entries layer their options on
top of the base config. Globs support `*`, `**`, and `?`.

```json
{
  "dataPerLine": 8,
  "overrides": [
    { "files": "src/data/**/*.asm", "options": { "dataPerLine": 16 } }
  ]
}
```

### .nessembleignore

A `.nessembleignore` file (gitignore-style globs, one per line) excludes matching
paths from directory walks. It is discovered the same way as `.nessemblerc`.

[prettier]: https://prettier.io
