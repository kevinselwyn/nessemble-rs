# Upgrading from 1.x to 2.x

`nessemble` 2.0 is a ground-up rewrite in Rust. Assembly source and ROM output
are compatible — the same `.asm` files assemble to the same bytes — but the tool
around them changed. This page covers what a 1.x user needs to know.

## Assembly & ROM output

- **No changes needed to your source.** The assembly language (instructions,
  addressing modes, expressions, labels, macros, conditionals, includes, data
  and iNES directives, media importers) is unchanged, and assembled ROMs are
  byte-for-byte identical to 1.x output. The one behavioral change is how
  relative filenames are resolved — see [Include & asset paths](#include--asset-paths).

## Include & asset paths

- In 1.x, every filename-based directive resolved its path against a single
  global working directory (the top-level file's directory). A relative path in
  an included file was therefore resolved from the project root, not from the
  included file.
- In 2.x, relative filenames in `.include`, `.inestrn`, and the `.inc*` media
  importers (`.incbin`, `.incpng`, `.incpal`, `.incrle`, `.incwav`) resolve
  relative to **the directory of the file that contains the directive**. This
  makes subdirectory modules self-contained: a file in `sub/` that does
  `.include "helper.asm"` or `.incbin "data.bin"` now finds them in `sub/`.
- **What to check:** if a file you `.include` from a subdirectory referenced a
  sibling file or asset by a path written relative to the *project root*, update
  that path to be relative to the including file instead. Projects that keep each
  file's includes and assets alongside it need no changes.

## Custom pseudo-instructions

- The three embedded scripting engines (JavaScript, Lua, and Scheme) and native
  shared-object (`.so`/`.dll`) plugins are replaced by a single embedded
  language, **[Rhai](https://rhai.rs)**.
- Rewrite custom scripts as `.rhai` files and update your `--pseudo` mapping to
  point at them. A script now defines `fn custom(ints, texts)` and returns the
  emitted bytes. See [Extending](extending.md).
- The bundled `ease` script is provided as `.rhai`; run `nessemble scripts` to
  install it.
- Rhai scripts can still read and write files (as the old Lua/Scheme hosts
  could), via the `rhai-fs` `open_file` API; relative paths resolve against the
  source file's directory. See [Filesystem access](extending.md#filesystem-access).

## Removed commands and options

The following 1.x features are **not** part of 2.x — they are not parsed and do
not appear in `--help`:

- The **disassembler / reassembler** (`-d`/`--disassemble`, `-R`/`--reassemble`).
- The **simulator / debugger** (`-s`/`--simulate`, `-r`/`--recipe`).
- The **package registry**: `registry`, `install`, `uninstall`, `publish`,
  `info`, `ls`, `search`, and the user/auth commands (`adduser`, `login`,
  `logout`, `forgotpassword`, `resetpassword`).

`config` remains, but is now a general key/value store (the registry key it used
to manage is gone).

## Internationalization

- Translations moved from gettext (`.po`/`.mo`) to
  [Project Fluent](https://projectfluent.org). Drop a
  `~/.nessemble/locales/<lang>.ftl` file and select it with `NESSEMBLE_LANG`.
  See [Translating](translating.md).

## Building & installing

- Building no longer needs a C toolchain, flex/bison, or gettext — just a Rust
  toolchain. See [Building](building.md).
- Release artifacts (`.deb`, `.msi`, `.pkg`, and standalone `.exe`) are provided
  for the same platforms as before. See [Installation](installation.md).
