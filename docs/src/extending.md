# Extending

`nessemble` can be extended with custom pseudo-instructions written in
[Rhai](https://rhai.rs), a small, pure-Rust scripting language. Scripts can also
read and write files (see [Filesystem access](#filesystem-access)), so run only
scripts you trust.

## Macros or scripts?

`nessemble` offers two ways to generate code and data from your own logic:
[macros](syntax.md#macros) (built in) and custom-pseudo-op scripts (this page).
They overlap, but each is better at different things.

**Reach for a macro when** the task is *assembly-shaped* — repeating a sequence
of instructions, filling in a few parameters, or defining local labels that the
rest of your program can branch to:

- It emits real assembly: instructions, labels (including `\@`-uniquified ones),
  and directives, expanded inline where you call it.
- Its parameters are numbers or label variables (`\1`, `\2`, …), and `\#` / `\@`
  cover argument counts and per-call unique ids.
- It needs no external file, no `--pseudo` mapping, and nothing outside the
  assembler — it is part of the source.

**Reach for a script when** the task is *computational* — anything that would be
painful or impossible to express as assembly text:

- Non-trivial math (easing curves, checksums, trig tables), string handling, or
  data transforms that macros' single-precedence integer expressions can't do.
- Reading assets from disk and converting them (PNG → CHR, WAV → DPCM, arbitrary
  binary blobs) via the [filesystem](#filesystem-access) and
  [PNG](#decoding-pngs) helpers.
- Randomized or procedurally generated data (see [Random
  numbers](#random-numbers)).
- Logic you'd rather write, test, and reuse as a real program.

A rule of thumb: if you're mostly *stamping out assembly*, use a macro; if you're
mostly *computing bytes*, use a script. The two compose freely — a macro can wrap
a `.custom`-style directive, and a script emits raw bytes that assembly around it
refers to by label. Note the trade-offs: macro-created labels are hidden from the
list file unless you pass [`--mlist`](usage.md#mlist), and scripts run arbitrary
code with filesystem access, so only run ones you trust.

## Usage

Pass the `--pseudo` flag to point at a mapping file that associates each custom
directive with a script.

Example `pseudo.txt`:

```nessemble
.foo = foo.rhai
```

Example `example.asm`:

```nessemble
.foo 1, 2, 3
```

To assemble:

```text
nessemble example.asm --pseudo pseudo.txt
```

A script path in the mapping file is resolved relative to the **mapping file's
own directory**, so a `pseudo.txt` and the scripts it names can live together and
be pointed at from anywhere. Bundled scripts installed with `nessemble scripts`
(into `~/.nessemble/scripts`) are resolved via `~/.nessemble/scripts/scripts.txt`
and need no `--pseudo` flag.

## Writing a script

A script defines a function named `custom` that receives the directive's
arguments and returns the bytes to emit:

```rust,ignore
fn custom(ints, texts) {
    // ...
}
```

- `ints` is an array of the integer arguments.
- `texts` is an array of the string arguments (quotes already removed).
- Return the emitted bytes as an **array of integers** (each taken `& 0xFF`), a
  **blob**, or a **string** (its bytes are emitted). Returning `()` emits
  nothing. Wrap a string in [`emit_source(...)`](#emitting-assembly-source)
  instead to return assembly *source* for the assembler to expand, rather than
  bytes.

### Example

A `.product` directive that multiplies its integer arguments:

```rust,ignore
fn custom(ints, texts) {
    let product = 0;
    let first = true;
    for i in ints {
        if first { product = i; first = false; } else { product *= i; }
    }
    [product % 256]
}
```

```nessemble
.product 1, 2, 3   ; emits a single byte: 6
```

### String arguments

String arguments arrive (with quotes removed) in `texts`:

```nessemble
.foo "easeInQuad", 0, 16
```

```rust,ignore
fn custom(ints, texts) {
    let name = texts[0];   // "easeInQuad"
    // ...
}
```

### Declaring file arguments

A script's string argument is opaque to the assembler: `.tilemap "map.png"` could
be a filename, an easing name, or a label. Prefix it with `file://` to say that it
names an input file:

```nessemble
.tilemap "file://map.png", "file://tiles.png"
```

The script sees the path **with the prefix stripped** — `texts[0]` is `"map.png"`
— so nothing about the script changes, and adding the declaration to a call site
is a one-word edit. What it buys:

- **The file is checked before the script runs.** A missing declared file is
  reported against the directive, on its own line, the same way a missing
  `.incbin` file is — instead of whatever error the script happens to throw when
  its `open_file` fails. The script is not run at all in that case.
- **The path is visible to tooling.** Editors resolve a declared path, so
  cmd/ctrl-clicking it opens the file, hovering it shows where it resolved to, and
  typing inside the quotes completes filenames — see
  [editor support](editor.md#features). None of that requires running the script.

Relative paths resolve against the **source file's directory** — the same base as
the script's own [file reads](#filesystem-access) — and an absolute path
(`file:///home/me/assets/map.png`) is used as-is. A declared argument prefixed
`@/` instead resolves from the
[project root](syntax.md#project-root-relative-paths):

```nessemble
.tilemap "file://@/art/map.png"
```

The script still sees a plain path in `texts[0]`, just an absolute one this
time — `@/` resolution happens once, before the file-existence check, so
`texts[0]` is already `"/path/to/project/art/map.png"` by the time `custom`
runs. Nothing about the script changes; `read_blob(texts[0])` works exactly as
it does for a source-relative path.

A script's own file reads honour `@/` too — see
[Filesystem access](#filesystem-access) — so declaring an argument is about the
existence check and editor support above, not about unlocking `@/` for a path
the script builds itself.

Declaring is optional and per-argument. A script that treats a missing file as
*optional* — falling back to a default when it isn't there — should leave the
prefix off that argument and keep its own fallback, since a declared file that is
absent is an error. The same prefix is accepted on the
[built-in filename directives](syntax.md#declaring-a-filename-argument), where it
is redundant but harmless, so a project can spell every path the same way.

### Errors

Signal an error with `throw`. The thrown message becomes the assembler
diagnostic:

```rust,ignore
fn custom(ints, texts) {
    if texts.is_empty() {
        throw "No arguments provided";
    }
    []
}
```

### Emitting assembly source

A script can return `emit_source(text)` instead of bytes: `text` is assembly
source, expanded **inline at the directive's own call site** — lexed, parsed,
and executed exactly as if it had been written there — rather than emitted as
raw bytes. This is the escape hatch for a directive whose job is to *generate*
assembly, not compute a fixed byte sequence: a data table expressed as real
`.db`/`.dw` lines with labels a caller can reference, or a repeated pattern a
script would rather spell as instructions than as opcode bytes.

```rust,ignore
fn custom(ints, texts) {
    let out = "";
    for i in 0..ints[0] {
        out += "frame" + i + ": .db " + (i * 8) + "\n";
    }
    emit_source(out)
}
```

```nessemble
.frames 3
    LDA frame1   ; a label the emitted source defined, used right after it
```

A plain returned string already means "emit these bytes" (matching the
reference Lua host's convention) — `emit_source` is what distinguishes
"this is source to expand" from that, so wrap the string rather than
returning it directly.

A few things follow from the source being expanded **inline**, not in a
separate file:

- **Labels and constants the emitted source defines are real symbols**,
  usable by code before or after the directive, exactly like a label defined
  in a `.macro` body. Like a macro-defined label, one from emitted source is
  hidden from the `-l` list file unless [`--mlist`](usage.md#--mlist) is
  given.
- **Diagnostics, spans, and coverage are attributed to the directive's own
  line**, not to a position inside the emitted text — there is no file for an
  editor to open at "line 3 of whatever `.frames` returned", so a parse error
  in the emitted source is reported as `.frames`'s own error, on `.frames`'s
  own line.
- **`.include`, `.inestrn`, `.macro`, and `.macrodef` cannot appear in emitted
  source.** Those are preprocessor constructs — `.include`/`.macro` splice
  text *before* parsing, at a stage that has already finished by the time a
  script runs — and using one in `emit_source`'s text is a directive-specific
  error rather than the more confusing "unknown custom pseudo-op" a bare parse
  of `.include` would otherwise give.
- **A script that emits source is never [cached](#caching).** The assembler
  has to re-expand the source on every build regardless — it has assembly-time
  side effects (symbols, byte emission) an on-disk cache cannot replay — so
  caching would only ever have saved the (comparatively cheap) expansion step,
  not the script's own execution.
- **Emitted source can itself invoke a custom directive**, including one that
  emits source again — it dispatches exactly like any other directive. Nested
  `emit_source` more than ten levels deep is a hard error rather than a stack
  overflow.

### Filesystem access

Scripts can read and write files through the
[`rhai-fs`](https://docs.rs/rhai-fs) package, so a directive can pull bytes from
disk instead of only computing them. The main entry point is `open_file`:

- `open_file(path, "r")` opens a file for reading; `open_file(path)` opens it for
  reading and writing, **creating or truncating** it.
- On the returned file handle: `read_blob()` / `read_string()` return the whole
  file, `read_blob(n)` / `read_string(n)` read `n` bytes, `write(blob_or_string)`
  writes bytes and returns the count, and `seek(pos)` moves the cursor.
- `read_blob(path)` is a one-call shorthand for reading a whole file — it returns
  the file's bytes as a blob, equivalent to `open_file(path, "r").read_blob()`.

Relative paths resolve against the **source file's directory** — the same base
as `.include` and the `.inc*` importers — absolute paths are used as-is, and a
path prefixed `@/` resolves from the
[project root](syntax.md#project-root-relative-paths), the same as everywhere
else `@/` is honoured:

```rust,ignore
fn custom(ints, texts) {
    read_blob("@/assets/shared.bin")
}
```

`open_file`, `read_blob`, `decode_png_file`, `parse_xml_file`, and
`parse_json_file` all resolve `@/` identically — no one of them is left
behaving differently from the others. A `@/` path is an error when no project
root could be determined, naming the sigil rather than falling back to the
source file's directory.

A `.embed "file"` directive that emits a file's bytes verbatim:

```rust,ignore
fn custom(ints, texts) {
    open_file(texts[0], "r").read_blob()
}
```

```nessemble
.embed "logo.chr"   ; emits the raw bytes of logo.chr
```

> **Filesystem access is not sandboxed.** A script can read or write any path the
> `nessemble` process can. Only run pseudo-op scripts you trust, as with any
> build tooling.

### Decoding PNGs

`decode_png(blob)` decodes PNG bytes (typically from `open_file(...).read_blob()`)
into an **image**:

```rust,ignore
let img = decode_png(open_file("sprite.png", "r").read_blob());
```

`decode_png_file(path)` is a one-call shorthand for the common case, equivalent
to `decode_png(read_blob(path))`:

```rust,ignore
let img = decode_png_file("sprite.png");
```

An image exposes:

- `img.width` — the image width in pixels (integer).
- `img.height` — the image height in pixels (integer).
- `img.pixels` — a flat array of `width * height * 4` integers, four per pixel in
  **`R, G, B, A`** order, row-major. Pixel `(x, y)` starts at index
  `(y * width + x) * 4`.

`decode_png` (and `decode_png_file`) throws if the blob is not a valid PNG.

An image is a **handle**: assigning it, or passing it to a function, shares the
decoded pixels rather than copying them, so image work can be factored into
helper functions freely. `img.pixels`, by contrast, builds a fresh array of every
channel each time you ask for it — on a full-resolution image that is tens of
millions of values, so prefer the accessors below, which read the decoded pixels
directly.

#### Pixel accessors

Rather than compute `(y * width + x) * 4` offsets by hand, an image exposes
accessor methods:

- `img.r(x, y)` — the **red** channel of pixel `(x, y)`. The images these scripts
  work with are grayscale (`R == G == B`), so this is the pixel's shade value.
- `img.pixel(x, y)` — the whole pixel as a `[r, g, b, a]` array.
- `img.tile(col, row, tw, th)` — the `tw`×`th` block at tile coordinate
  `(col, row)` (i.e. pixels `[col*tw, (col+1)*tw)` × `[row*th, (row+1)*th)`) as a
  flat, row-major array of red-channel (shade) values.

All three throw if the coordinates fall outside the image. Using them, the
red-channel-of-a-tile example above becomes a single call:

```rust,ignore
fn custom(ints, texts) {
    decode_png_file(texts[0]).tile(0, 0, 8, 8)   // top-left 8x8 tile's shades
}
```

#### Cell matching

Converting a picture into tile indices means asking, over and over, *which cell
of this sheet does this cell of my image draw?* Three methods answer that
natively, against a **bank** image gridded into `w`×`h` cells left to right, top
to bottom (`floor(width / w)` by `floor(height / h)` of them — a ragged right or
bottom edge is not a cell, exactly as `img.tile` grids an image):

- `bank.find_cell(src, col, row, w, h)` — the index of the bank cell that draws
  the same thing as the `w`×`h` cell at grid position `(col, row)` of `src`, or
  `-1` if none does. When several bank cells are identical, the **lowest** index
  wins.
- `bank.cell_equals(index, src, col, row, w, h)` — whether bank cell `index`
  draws that same cell. An `index` outside the bank is simply `false`; use this
  to validate an index you already have without re-scanning.
- `bank.nearest_cell(src, col, row, w, h)` — the closest bank cell by summed
  per-pixel shade difference, for a cell with no exact match. Ties go to the
  lowest index, and it always returns an index.

All three compare **NES shade indices** — each pixel's red channel put through
the same snapping [`nes_shade`](#palette-quantization) uses — and ignore green,
blue and alpha. So `bank.find_cell(src, col, row, w, h)` agrees exactly with
scanning the bank for `nes_shade(bank.tile(…)) == nes_shade(src.tile(…))`, and
pixels differing only below the snapping thresholds (say, in the low nibble of a
byte, where a script can stash its own per-cell data) compare equal.

A `.tilemap "map.png", "tiles.png"` directive emitting one index per 8×8 cell,
falling back to the closest tile when a cell isn't in the sheet:

```rust,ignore
fn custom(ints, texts) {
    let map = decode_png_file(texts[0]);
    let tiles = decode_png_file(texts[1]);
    let out = [];
    for row in 0..(map.height / 8) {
        for col in 0..(map.width / 8) {
            let i = tiles.find_cell(map, col, row, 8, 8);
            if i < 0 { i = tiles.nearest_cell(map, col, row, 8, 8); }
            out.push(i);
        }
    }
    out
}
```

A cell position outside `src`, a zero or negative cell size, or a bank too small
to hold one whole cell throws an error naming the call.

### Palette quantization

`quantize(value, thresholds)` snaps a value to a palette index by counting how
many of the ascending `thresholds` it reaches — useful for turning a grayscale
shade into a fixed-palette index. It also accepts an **array** of values and
returns an array of indices, so it pairs directly with `img.tile`:

```rust,ignore
// [43, 128, 213] are the midpoints between the four NES shades (0, 85, 170, 255).
let shades = quantize(img.tile(0, 0, 8, 8), [43, 128, 213]);
```

`nes_shade(value)` is that NES four-shade case with the thresholds built in
(equivalent to `quantize(value, [43, 128, 213])`), returning `0`–`3`. It also
accepts an array:

```rust,ignore
let shades = nes_shade(img.tile(0, 0, 8, 8));
```

### Parsing structured data

Assets aren't always images. A map editor, a tracker, or a spreadsheet usually
saves XML or JSON, and `parse_xml`/`parse_xml_file` and `parse_json`/
`parse_json_file` read them the same way `decode_png`/`decode_png_file` reads a
PNG: the host does the parsing, and the script only ever walks an
already-parsed document. A Rhai script tokenizing a document by hand is roughly
10× slower than an entire out-of-process conversion in a compiled language —
these functions exist so no script has to.

#### XML

`parse_xml_file(path)` (and `parse_xml(source)`, for a string already in hand)
returns the root element as a node with:

- `.name` — the element name.
- `.attrs` — a map of attribute name → string value (sorted by name, not
  document order — see the note below).
- `.attr(name)` — an attribute's value, or `()` if it isn't set. The common
  case, and unaffected by the `.attrs` ordering note.
- `.children` — an array of child **elements** (not text).
- `.text` — the element's own text content, entities decoded, or `()` if it has
  none. Whitespace used purely for indentation between child elements is not
  filtered out — call `.trimmed()` on it if you only want meaningful text.
- `.find(name)` — the first child element with that name, or `()`.
- `.find_all(name)` — every child element with that name, as an array.

```rust,ignore
fn custom(ints, texts) {
    let doc = parse_xml_file(texts[0]);
    let out = [];
    for row in doc.find_all("row") {
        out += parse_int_list(row.attr("data"), ",");
    }
    out
}
```

Scope is deliberately narrow: elements, attributes, text, and entities (the
five predefined ones, plus numeric character references like `&#10;`/`&#x41;`).
No namespaces, no XPath, no schema validation — and **no DTD processing**: a
`<!DOCTYPE` is a parse error, not something silently skipped or expanded, since
resolving external entities on a script's behalf is exactly the shape of an XXE
vulnerability. Errors name the file and the line/column where parsing failed.

> `.attrs` is a plain Rhai map, which is always key-sorted — Rhai has no
> insertion-ordered map type. `.attr(name)` (a direct lookup) is unaffected;
> only a script that iterates `.attrs` as a whole to reproduce document order
> would notice.

#### JSON

`parse_json_file(path)` / `parse_json(source)` convert a document straight into
native Rhai values: an object becomes a map, an array becomes an array, and
scalars become the matching `int`/`float`/string/bool/`()`.

```rust,ignore
fn custom(ints, texts) {
    let doc = parse_json_file(texts[0]);
    let out = [];
    for tile in doc.tiles {
        out.push(tile.id);
    }
    out
}
```

A syntax error's message already names its line and column.

#### Bulk numeric decoding

Structured formats store grids and arrays as delimited text, often thousands of
values at a time. `parse_int_list(text, delim)` (and the three-argument form,
`parse_int_list(text, delim, radix)`) decode a whole column in one native call
instead of one interpreter iteration per value: split on the literal
delimiter, trim whitespace, skip empty fields, and parse the rest.

```rust,ignore
let values = parse_int_list("1, 2,,3 ,", ",");   // [1, 2, 3]
let bytes = parse_int_list("ff,1a", ",", 16);    // [255, 26]
```

#### String and hex helpers

- `to_char(value)` — a one-character string for the Unicode scalar `value`, for
  building a string out of bytes read from a blob (`s += to_char(b);`).
- `"  text  ".trimmed()` — a trimmed **copy** of a string. The stock `trim()`
  mutates in place and returns `()`, so `let t = s.trim();` binds unit; reach
  for `trimmed()` when you want the result as a value.
- `format_hex(value, width)` — assembly's own hex spelling: `$`-prefixed,
  zero-padded. `format_hex(255, 2)` is `"$FF"`, `format_hex(0x1A, 4)` is
  `"$001A"`.
- `parse_int(str, radix)` (the two-argument form) and `blob.as_string()`
  already exist in Rhai's own standard library and need nothing from this
  crate — reach for them directly.

### Random numbers

Scripts can draw random values through the
[`rhai-rand`](https://docs.rs/rhai-rand) package — handy for procedural noise,
scrambled data tables, or randomized test fixtures:

- `rand()` — a random integer.
- `rand(min, max)` — a random integer in the **inclusive** range `min..=max`.
- `rand_float()` — a random float in `0.0..1.0`.
- `rand_bool()` — a random `true`/`false`.
- `rand_bool(p)` — `true` with probability `p` (a float in `0.0..1.0`).
- On arrays: `array.shuffle()` shuffles in place, and `array.sample()` /
  `array.sample(n)` draw one or `n` random elements.

A `.noise` directive that emits `\1` random bytes:

```rust,ignore
fn custom(ints, texts) {
    let out = [];
    for i in 0..ints[0] {
        out.push(rand(0, 255));
    }
    out
}
```

```nessemble
.noise 16   ; emits 16 random bytes
```

> **Random output is not reproducible.** Each assembly draws fresh values, so a
> script using these functions produces a different ROM every run. Keep them out
> of builds that must be deterministic (or seed your own generator in the script
> instead). A script that draws random values is never
> [cached](#caching) — which is what keeps it working.

The random functions are available on native builds. They are absent from the
WebAssembly build (which has no system entropy source), where calling one raises
a "function not found" error — the same way [filesystem
access](#filesystem-access) is unavailable there.

## Caching

A script that crunches a PNG into CHR data should cost that crunch once per change
to the PNG, not once per build. `nessemble` therefore remembers what each custom
directive emitted, in two layers:

- **Within one build**, a directive's script runs **once**, not once per assembler
  pass. This is unconditional.
- **Across builds**, the emitted bytes are stored in `~/.nessemble/cache` and
  reused while nothing the script depended on has changed.

Nothing needs configuring, and scripts need no changes: the host **records every
file a script opens** — through `open_file`, `read_blob`, `decode_png_file`,
`parse_xml_file`, or `parse_json_file` — and remembers those as the run's
inputs. A script that computes a filename, reads a palette nobody passed it, or
follows a reference from inside one parsed document to another, is covered.

An entry is reused only when all of the following still hold:

- the **script** itself is unchanged (a `--pseudo` mapping pointing at a different
  script is a different entry too),
- every **file the script read** is unchanged,
- the directive's **arguments**, the directory it was called from, and the
  [project root](syntax.md#project-root-relative-paths) (if any) are the same —
  two builds that agree on everything else but disagree on `--root` do not
  share an entry, since a `@/`-prefixed path the script resolves itself could
  read a different file under each,
- and the `nessemble` version is the same — the host's helpers define the output,
  so a new release starts from an empty cache.

"Unchanged" means **the same size and modification time**. That is what a build
tool can check in microseconds, and it catches every ordinary edit; a `git
checkout` that rewrites timestamps costs a needless re-run rather than a wrong
result. The one gap: an edit that keeps a file's exact byte size *and* lands inside
the same timestamp tick as the previous one can go unnoticed. If a build ever looks
stale, [`--no-cache`](usage.md#no-cache) bypasses the cache entirely and
[`nessemble cache clear`](usage.md#cache-info--cache-clear) empties it.

### Prewarming runs independent scripts concurrently

Before assembling, `nessemble` scans the program for custom-directive
invocations whose arguments it can already work out — a directive whose
integer arguments are plain numbers (no forward-referenced label) and whose
string arguments are either undeclared or name a file that exists. Every
invocation is independent by construction, so the ones it finds are resolved
**concurrently**, across as many CPU cores as are available, filling the cache
before the sequential assembly passes read from it. On a script-heavy build —
several `.tilemap`/`.incpng`-style directives, each decoding its own PNG — this
overlaps work that used to run one script at a time.

This is transparent to almost every script: nothing about *what* a directive
computes changes, only when it computes it, and the same "once per build,
memoized across passes" guarantee still holds for the bytes that actually
reach the ROM. Two things follow from prewarming happening ahead of, and
independently of, that per-build memoization:

- **A script that writes a file, draws randomness, or otherwise does
  something the [never-cached](#what-is-never-cached) list covers is never
  prewarmed** — running it an extra, uncounted time would be a real side
  effect, not merely wasted work, so `nessemble` checks for exactly that
  (without running the script) before ever including it.
- **A directive whose arguments are not yet knowable — a forward-referenced
  label, most commonly — is left for the sequential passes**, exactly as
  before prewarming existed; nothing about it changes.

Directive scripts remain independent of each other in every way that matters
(each runs on its own interpreter instance, its own cache reads and writes),
so no script needs to change to benefit from this. The one thing worth
knowing: if a script reaches *outside* what the host tracks — writing to a
fixed path some other tool also touches, say — assume it can now run
concurrently with other invocations, not only with itself across builds.

### What is never cached

Some scripts must really run every time, and are detected and excluded
automatically:

- **Random output** — `rand`, `rand_float`, `rand_bool`, `shuffle`, `sample`. The
  point of these is to differ per build.
- **Writing a file** — the write *is* the effect, and replaying stored bytes would
  skip it.
- **Listing a directory** (`open_dir`) — a listing's contents are not described by
  any per-file check.
- **`import`ing a module** — a module's source is invisible to the recorder, so
  such a script is refused rather than tracked incompletely.
- **[`emit_source`](#emitting-assembly-source) output** — the assembler must
  re-expand it fresh on every build regardless (it has assembly-time side
  effects a cache cannot replay), so nothing would be saved by storing it.

The check is deliberately cautious: it looks at what a script *could* do, so a
`rand()` in a branch that never runs is enough to keep the script out of the
cache. Being wrong that way costs one script execution; being wrong the other way
would emit a stale ROM.

`nessemble coverage --scripts` also bypasses the cache, since a cached result
executes no lines and would report a covered script as uncovered.

## Bundled scripts

Running `nessemble scripts` installs the bundled scripts. The `ease` script
emits an easing curve as bytes:

```nessemble
.ease "easeInQuad"
```

Supported easing types include `easeInQuad`, `easeOutQuad`, `easeInOutQuad`,
and the cubic, quint, and bounce variants.
