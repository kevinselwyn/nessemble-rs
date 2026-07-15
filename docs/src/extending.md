# Extending

`nessemble` can be extended with custom pseudo-instructions written in
[Rhai](https://rhai.rs), a small, pure-Rust scripting language. Scripts can also
read and write files (see [Filesystem access](#filesystem-access)), so run only
scripts you trust.

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
into a map of the image's dimensions and its pixels:

```rust,ignore
let img = decode_png(open_file("sprite.png", "r").read_blob());
```

`decode_png_file(path)` is a one-call shorthand for the common case, equivalent
to `decode_png(read_blob(path))`:

```rust,ignore
let img = decode_png_file("sprite.png");
```

The returned map has:

- `width` — the image width in pixels (integer).
- `height` — the image height in pixels (integer).
- `pixels` — a flat array of `width * height * 4` integers, four per pixel in
  **`R, G, B, A`** order, row-major. Pixel `(x, y)` starts at index
  `(y * width + x) * 4`.

`decode_png` (and `decode_png_file`) throws if the blob is not a valid PNG.

#### Pixel accessors

Rather than compute `(y * width + x) * 4` offsets by hand, the image map exposes
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

## Bundled scripts

Running `nessemble scripts` installs the bundled scripts. The `ease` script
emits an easing curve as bytes:

```nessemble
.ease "easeInQuad"
```

Supported easing types include `easeInQuad`, `easeOutQuad`, `easeInOutQuad`,
and the cubic, quint, and bounce variants.
