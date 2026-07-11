# nessemble

`nessemble` is a 6502 assembler targeting the Nintendo Entertainment System
(NES), written in Rust.

> Upgrading from a 1.x release? See [Upgrading](upgrading.md) for what changed
> in 2.0.

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
