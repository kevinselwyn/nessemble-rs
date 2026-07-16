---
nessemble: patch
---
Add the `xtask changeset` command group (add/check/status/version) that parses
`.changeset/` files, computes the next semantic version from the accumulated
changesets, and — via `cargo set-version` — bumps the whole workspace. Internal
release tooling (plan 004, Phase 1); no shipped-behavior change.
