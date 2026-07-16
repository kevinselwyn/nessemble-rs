---
nessemble: none
---
Add the changeset-driven Release workflow (`version.yml`) that bumps the
workspace via `xtask changeset version` and pushes to `main` as
`nessemble-release[bot]`, rename the build pipeline to Publish, and remove the
`-dev` pre-release guard. CI/infra + docs only (plan 004, Phase 3).
