# Working in this repository

Notes for coding agents. See also [`.claude/README.md`](.claude/README.md) for
the CI gate that runs on every turn, and [`RELEASING.md`](RELEASING.md) for how
a release is actually cut.

## Never hand-edit generated files

**`CHANGELOG.md` is generated. Do not edit it — not to add an entry, not to fix
one, not "just this once".**

The Release action (`cargo run -p xtask -- changeset release`) renders a new
changelog section from the changesets accumulated in `.changeset/`, prepends it,
and deletes the changesets it consumed ([`RELEASING.md`](RELEASING.md) step 3).
A hand-written entry does not replace that — it *duplicates* it, because your
changeset still renders at release time, and the two copies then disagree the
moment either is edited.

**What to do instead:** write the changeset. Its Markdown body *is* the changelog
line, so put the care there:

```bash
cargo run -p xtask -- changeset add patch "One sentence a release-notes reader wants."
```

The same rule covers everything else the release owns — the workspace version in
`Cargo.toml`, the `[workspace.dependencies]` pins, and `Cargo.lock`'s version
entries are all set by `cargo set-version` during a release. Never bump a version
by hand to "prepare" a release.

Files under a `build.rs`'s `OUT_DIR` (the generated `nessemble-isa` opcode table)
are likewise outputs: change the CSV in `crates/nessemble-isa/data/`, not the
table.

## Plans

Substantial features are designed in `plans/NNN-name.md` before they are built,
and the plan is updated to match what shipped once they are. If you are
implementing a numbered plan, read it first — the decisions in its final section
are settled, not suggestions.
