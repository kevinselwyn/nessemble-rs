# Extending

`nessemble` can be extended with custom pseudo-instructions written in
[Rhai](https://rhai.rs), a small, pure-Rust scripting language. Scripts can also
read and write files (see [Filesystem access](#filesystem-access)), so run only
scripts you trust.

## Usage

Pass the `--pseudo` flag to point at a mapping file that associates each custom
directive with a script.

Example `pseudo.txt`:

```text
.foo = foo.rhai
```

Example `example.asm`:

```text
.foo 1, 2, 3
```

To assemble:

```text
nessemble example.asm --pseudo pseudo.txt
```

Directive script paths are resolved relative to the source file's directory.
Bundled scripts installed with `nessemble scripts` (into `~/.nessemble/scripts`)
are resolved via `~/.nessemble/scripts/scripts.txt` and need no `--pseudo` flag.

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

```text
.product 1, 2, 3   ; emits a single byte: 6
```

### String arguments

String arguments arrive (with quotes removed) in `texts`:

```text
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

Relative paths resolve against the **source file's directory** — the same base
as `.include` and the `.inc*` importers — while absolute paths are used as-is.

A `.embed "file"` directive that emits a file's bytes verbatim:

```rust,ignore
fn custom(ints, texts) {
    open_file(texts[0], "r").read_blob()
}
```

```text
.embed "logo.chr"   ; emits the raw bytes of logo.chr
```

> **Filesystem access is not sandboxed.** A script can read or write any path the
> `nessemble` process can. Only run pseudo-op scripts you trust, as with any
> build tooling.

## Bundled scripts

Running `nessemble scripts` installs the bundled scripts. The `ease` script
emits an easing curve as bytes:

```text
.ease "easeInQuad"
```

Supported easing types include `easeInQuad`, `easeOutQuad`, `easeInOutQuad`,
and the cubic, quint, and bounce variants.
