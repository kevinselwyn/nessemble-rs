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
  nothing.

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
as `.include` and the `.inc*` importers — while absolute paths are used as-is.

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
> instead).

The random functions are available on native builds. They are absent from the
WebAssembly build (which has no system entropy source), where calling one raises
a "function not found" error — the same way [filesystem
access](#filesystem-access) is unavailable there.

## Bundled scripts

Running `nessemble scripts` installs the bundled scripts. The `ease` script
emits an easing curve as bytes:

```nessemble
.ease "easeInQuad"
```

Supported easing types include `easeInQuad`, `easeOutQuad`, `easeInOutQuad`,
and the cubic, quint, and bounce variants.
