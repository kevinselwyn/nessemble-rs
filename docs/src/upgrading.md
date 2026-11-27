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

## `.enum` blocks (value enums)

The release that adds value enums (bare member names and named enums; see
[`.enum`](syntax.md#enum)) changes what two kinds of line inside an `.enum`
mean. Neither is common, and a block made only of `NAME .rs N` lines is
unaffected.

- **A bare name inside an `.enum` is now a member.** It takes the counter's
  value and advances it, like `NAME .rs 1`. Before, it was silently a constant
  equal to `1`:

  ```nessemble
  .enum $10
  FIRST     ; was $01, now $10
  SECOND    ; was $01, now $11
  .endenum
  ```

  This includes pasted names in a `.for` body (`SLOT\i` gave every slot `1`).

- **`NAME = expr` inside an `.enum` now resets the counter** to `expr + STEP`.
  Before, it defined the constant and left the counter where it was:

  ```nessemble
  .enum $10, 2
  FOO .rs 1   ; $10
  BAR .rs 1   ; $12
  QQ = 5      ; $05
  ZZ .rs 1    ; was $14, now $07
  .endenum
  ```

  To keep the old values, move the `=` line outside the block.

## Lint: whole-program clobber analysis

The release that adds [program-mode lint](usage.md#program-mode-and-file-mode)
changes what `nessemble lint` reports for routines with a clobber list.

- **`unused-lint-ignore` is on by default** (`warn`). Every
  `@nessemble-lint-ignore…` that silences nothing is now reported — in every
  project, with or without an entry. Switch it off under the
  [`lint`](usage.md#lint) key if you want to clean up later.
- **Adding an [`entry`](usage.md#entry) changes the clobber findings.** The
  routine rules then follow calls across files, fall-through, branches and
  save/restore pairs, and see macros and custom pseudo-instructions as the code
  they emit. Findings the text analysis could not see may appear (each names
  where the write comes from), and findings it got wrong disappear. `lint main.asm`
  on a file that assembles on its own already uses program mode, entry or not.
- **To clean up suppressions**, run `nessemble lint <entry>` and delete what
  `unused-lint-ignore` reports, or apply the editor's *Remove unused
  suppression* quick fix to each.

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
- **Script paths in a `--pseudo` mapping now resolve relative to the mapping
  file's own directory**, not the source file's directory. In 1.x the paths were
  resolved against the input file's location (the tool's `cwd_path`). If your
  `pseudo.txt` sits next to the source you assemble, nothing changes; otherwise,
  keep each script beside the mapping file that names it (or update the paths).

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
