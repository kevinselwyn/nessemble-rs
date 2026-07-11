# Extending

`nessemble` can be extended with custom pseudo-instructions written in
[Rhai](https://rhai.rs), a small, sandboxed, pure-Rust scripting language.

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

## Bundled scripts

Running `nessemble scripts` installs the bundled scripts. The `ease` script
emits an easing curve as bytes:

```text
.ease "easeInQuad"
```

Supported easing types include `easeInQuad`, `easeOutQuad`, `easeInOutQuad`,
and the cubic, quint, and bounce variants.
