# Usage

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
  config [<key>] [<val>]           list/get/set config info
  lsp                              run the language server (stdio)
```

The `lsp` command starts the built-in [Language Server](editor.md) for use with
LSP-capable editors.

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

### config [&lt;key&gt;] [&lt;val&gt;]

Gets or sets configuration stored in `~/.nessemble/config`. With no arguments it
lists all keys; with a `<key>` it prints that value; with a `<key>` and `<val>`
it sets the key.
