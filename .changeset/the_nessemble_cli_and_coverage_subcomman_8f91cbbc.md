---
nessemble: minor
---

The `nessemble` CLI and `coverage` subcommand gain a `--root <dir>` flag to override the project root that `@/` paths resolve against, and the language server now resolves, links, hovers, and completes `@/` paths — preferring an open workspace folder as the root.
