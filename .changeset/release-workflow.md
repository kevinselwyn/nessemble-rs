---
nessemble: none
---
Add the changeset-driven Release workflow (`version.yml`) that bumps the
workspace via `xtask changeset version` and pushes to `main` as
`nessemble-release[bot]`, rename the build pipeline to Publish, remove the
`-dev` pre-release guard, and use the curated `CHANGELOG.md` section as the
GitHub Release body. CI/infra + docs only (plan 004, Phases 3–4).
