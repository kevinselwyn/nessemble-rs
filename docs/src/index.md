# nessemble

`nessemble` is a 6502 assembler targeting the Nintendo Entertainment System
(NES). This is **nessemble-rs**, a from-scratch Rust reimplementation of the
original tool, targeting byte-for-byte ROM-output parity with the upstream
v1.1.1 release.

## Getting Started

To initialize a new project:

```text
nessemble init
```

Build the project:

```text
nessemble project.asm --output project.nes --format nes
```

Run `project.nes` in any NES emulator to see the result.

## Documentation

Start here: [Installation](installation.md).

- [Usage](usage.md) — the command-line interface.
- [Syntax](syntax.md) — the assembly language reference.
- [Extending](extending.md) — custom pseudo-instructions with Rhai.
- [Building](building.md) — building from source.
- [Translating](translating.md) — adding a locale.
