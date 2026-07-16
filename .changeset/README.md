# Changesets

Every pull request that changes shipped behavior carries a **changeset**: a small
Markdown file in this directory that declares how the change affects the next
release version and gives the changelog line an author writes at the moment they
understand the change. Releases are then cut on demand by the Release action,
which computes the next version from every changeset accumulated here since the
last release. See [`plans/004-release-orchestration.md`](../plans/004-release-orchestration.md)
for the full design.

You can scaffold a changeset with `cargo run -p xtask -- changeset add <major|minor|patch|none> "summary"`,
or just write the file by hand in the format below.

## File format

A changeset is a `.md` file in the format the
[`changesets`](https://crates.io/crates/changesets) crate parses: a front-matter
block delimited by `---` lines containing `package: change_type` pairs, followed
by a Markdown summary.

```markdown
---
nessemble: minor
---
Add syntax highlighting to the in-browser assembler component.
```

- **Package key.** This is a single-version workspace — every crate shares the
  one version in the root `Cargo.toml` — so use the single umbrella key
  **`nessemble`** rather than naming individual crates.
- **Change type.** One of:
  - `major` — incompatible / breaking change.
  - `minor` — backwards-compatible new functionality.
  - `patch` — backwards-compatible fix or internal change.
  - `none` — no release impact (docs, CI, chores). Documents the intent
    explicitly and produces no version bump.
- **Summary.** The Markdown body is the user-facing changelog entry for this
  change. Write it for a reader of the release notes, not a reviewer of the diff.
- **Filename.** Anything ending in `.md`; a short unique slug avoids collisions
  between concurrent PRs (e.g. `brave-otters-sing.md`). This `README.md` is not a
  changeset and is ignored.

## Which change type?

The release version is the **highest** change type across all accumulated
changesets: any `major` makes the release a major bump; otherwise any `minor`
makes it a minor bump; otherwise it is a `patch`. A release consisting only of
`none` changesets cuts no release.

## Opting out

A PR that genuinely has no release impact can either:

- add a `nessemble: none` changeset (preferred — it records the intent in the
  history), or
- carry the `no-changeset` label, which the CI check honors.
