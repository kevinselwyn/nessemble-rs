# nessemble-rs: A Plan for Changeset-Driven Release Orchestration

> Status: **Decisions settled (§9); Phase 0 (convention & docs) done —
> Phases 1–3 pending.** Phase 0 established the `.changeset/` convention, the
> `RELEASING.md` rewrite, and the PR-template update in this PR; the tooling, CI
> enforcement, and Release workflow land in follow-up PRs.

---

## 1. Goal

Make releases **intentional, auditable, and semantically versioned from the work
itself**:

1. **Every PR carries a changeset** — a small Markdown file, in the format the
   [`changesets`](https://crates.io/crates/changesets) Rust crate parses,
   declaring how the change affects the version (`major` / `minor` / `patch`)
   plus a human-readable changelog summary. CI **fails a PR that has neither a
   changeset nor an explicit opt-out**.
2. **Releases are triggered on demand** by a GitHub Action, not by hand-editing a
   version string.
3. **The next version is computed from the accumulated changesets** since the
   last release: the highest bump level among them wins (any `major` → major
   bump; else any `minor` → minor; else `patch`).
4. **The release action upversions the whole workspace** — the root
   `[workspace.package] version`, every crate (they inherit it), the internal
   `[workspace.dependencies]` version pins, and `Cargo.lock` — then consumes the
   changesets and updates the changelog.
5. **The existing release pipeline then produces the build assets** — the version
   bump lands on `main`, and today's `release.yml` sees a new, un-tagged version
   and builds/tags/publishes exactly as it does now. **No change to how artifacts
   are built.**

## 2. Why change the current model

Today (`RELEASING.md`, `.github/workflows/release.yml`) the release version *is*
the workspace version in the root `Cargo.toml`, and a release is cut by
**hand-editing that version on `main`**. Work-in-progress is parked behind a
`-dev` pre-release suffix so intermediate merges don't release, and the suffix is
dropped to cut the release.

This works but has friction the changeset model removes:

- **The version is chosen manually and up-front**, before anyone knows the full
  scope of what will ship. Deciding "is this a minor or a patch?" is deferred to
  release time and derived from the change log, not guessed at the first PR.
- **`-dev` juggling is bookkeeping.** Contributors must know to add/drop the
  suffix, and a forgotten suffix can cut an unintended release, while a stale one
  can silently suppress an intended one.
- **Release notes are PR-title-shaped, not author-shaped.** GitHub's
  auto-generated "What's Changed" lists PR titles; a changeset lets the author
  write the user-facing line at the moment they understand the change.

The core reframing: **stop encoding "unreleased work" in the Cargo version;
encode it in accumulated changeset files.** Between releases the workspace version
always equals the *last released* version, and the release action is the only
thing that advances it.

## 3. Current state — grounding

- **Single-version workspace.** All crates set `version.workspace = true`
  (`crates/*/Cargo.toml`, `xtask/Cargo.toml`); the one source of truth is
  `[workspace.package] version` in the root `Cargo.toml` (currently `2.8.1`).
- **Internal version pins exist and must move in lockstep.**
  `[workspace.dependencies]` in the root `Cargo.toml` hard-codes
  `version = "2.8.1"` for the six internal path crates (`nessemble-isa`,
  `-core`, `-media`, `-script`, `-i18n`, `-lsp`). A version bump that touches
  only `[workspace.package]` leaves these stale and **breaks the build** (a path
  dependency with a version requirement that no longer matches). `Cargo.lock`
  also records the versions and must be regenerated.
- **`release.yml` reads the version by grep** (`grep -m1 '^version = '
  Cargo.toml`), checks whether a `v<version>` tag exists, skips on a `-*`
  pre-release suffix, and otherwise builds all platform artifacts, tags
  `v<version>`, and publishes a GitHub Release. It triggers on **push to `main`**
  and on `workflow_dispatch`.
- **`ci.yml`** runs fmt / clippy / test / parity on every `pull_request` and push
  to `main`. There is a natural home here for a changeset-presence check.
- **`.github/release.yml`** configures GitHub's auto-generated note grouping by
  label.
- **`xtask`** is the project's Rust-native tooling harness (`fetch-oracle`,
  `verify-goldens`, `parity`, `wasm`, `dist`), dispatched by a `match` in
  `xtask/src/main.rs`. It is the idiomatic place to add changeset logic — **no
  Node/npm toolchain is introduced.**

## 4. Proposed architecture

### 4.1 Changeset files

- **Location:** a new `.changeset/` directory at the repo root.
- **Format:** exactly the one the [`changesets`](https://crates.io/crates/changesets)
  crate parses — a `.md` file whose front matter, delimited by `---` lines, is a
  set of `package: change_type` pairs, followed by a Markdown body that is the
  change **summary**:

  ```markdown
  ---
  nessemble: minor
  ---
  Add syntax highlighting to the in-browser assembler component.
  ```

  - `change_type` is `major`, `minor`, or `patch` (the crate also permits custom
    types, which it treats as `patch` for versioning but can distinguish in the
    changelog).
  - **Single-version workspace ⇒ a single umbrella package key** — `nessemble` —
    rather than a per-crate matrix. Because every crate shares the one workspace
    version, one pair per changeset is all that's meaningful; the release computes
    the highest `change_type` across every pending changeset. (This is the key
    simplification over the JS `changesets` per-package model, while still using
    the Rust crate's exact enforced file format.)
  - The body is the user-facing changelog summary (the crate treats it as plain
    text, not parsed Markdown).
- **Filename:** arbitrary and unique; a short random slug (e.g.
  `.changeset/brave-otters-sing.md`) avoids collisions across concurrent PRs.
  `xtask changeset add` can generate one.
- **No-release opt-out:** a changeset with the custom type `nessemble: none`
  documents an intentional no-version-impact change; xtask special-cases `none`
  as "no bump" (see §4.2 / §4.5). This is in addition to the `no-changeset` label
  (§4.5).
- **`.changeset/README.md`** documents the convention and is **excluded** from all
  scanning (it is not a changeset).

### 4.2 `xtask` subcommands (the brains)

A new `changeset` command group in `xtask` keeps the logic in Rust, tested like
the rest of the tooling. It uses the **`changesets` crate for parsing and format
validation** (so files match the crate's enforced format exactly, D1), and layers
the project's own version-decision policy on top (bump precedence + the `none`
opt-out):

- `xtask changeset add` — scaffold a new changeset file (prompt or flags for the
  change type and a summary). Convenience for contributors.
- `xtask changeset check` — validate that pending changesets parse (valid front
  matter, non-empty summary). Used by CI and locally.
- `xtask changeset status` — print the pending changesets and the version bump
  they would produce (dry run of the computation).
- `xtask changeset version` — the release-time mutation:
  1. Parse every `.changeset/*.md` (except `README.md`) via the `changesets`
     crate.
  2. Compute the next version: highest change type wins (`major` > `minor` >
     `patch`; a set that is `none`-only ⇒ no release, error out).
  3. Apply the bump to the workspace — see §4.3.
  4. Aggregate the changeset bodies into a new `CHANGELOG.md` section under the
     new version + date.
  5. **Delete the consumed changeset files.**
  6. Leave the working tree staged/ready for the workflow to commit.

### 4.3 The version mutation (the hands)

The actual Cargo edits are delegated to **`cargo-edit`'s `cargo set-version`**,
which already knows how to bump a workspace version *and* rewrite the internal
`[workspace.dependencies]` pins *and* refresh `Cargo.lock` in one shot:

```sh
cargo set-version --bump <level> --workspace
```

`xtask changeset version` computes `<level>` from the changesets and invokes it
(or the workflow does, using xtask's computed level). This avoids a bespoke,
error-prone TOML string-rewriter and guarantees the six internal pins + lockfile
stay consistent. (Alternative considered: hand-rolled edits in xtask — rejected
as reinventing `cargo-edit`. See §9.)

### 4.4 The Release workflow (`.github/workflows/version.yml`, new)

A new `workflow_dispatch` workflow — the "release trigger" the user asked for:

1. Checkout `main` (full history).
2. Install Rust + `cargo-edit`.
3. `xtask changeset status` → guard: if there are **no** consumable changesets,
   fail with a clear message (nothing to release).
4. `xtask changeset version` → bumps the workspace, writes `CHANGELOG.md`,
   deletes consumed changesets.
5. Commit the result and **push it directly to `main`** (D4 — direct push),
   authenticated as a **`nessemble-release[bot]` GitHub App**. The App token both
   satisfies branch protection (via a bypass allowance for the App) and — unlike
   the default `GITHUB_TOKEN` — lets the push trigger `release.yml` (§10). The
   commit is authored by the bot identity.
6. The push to `main` **chains into the existing `release.yml`**, which sees the
   new, un-tagged version and builds + tags + publishes the assets — unchanged.

### 4.5 The changeset-required CI check (`ci.yml`, extended)

A new job (or step) that runs **only on `pull_request`**:

- Diff the PR branch against its base and list files added under `.changeset/`.
- **Pass** if at least one new changeset file is present (and
  `xtask changeset check` validates it).
- **Escape hatch** for PRs that legitimately need no version bump (docs-only, CI,
  chores): either a `nessemble: none` changeset (explicit, self-documenting — the
  preferred path) **or** a `no-changeset` PR label that the job honors (D3 —
  both).

### 4.6 Release notes / changelog

- `CHANGELOG.md` (new, repo root) is the durable, curated history, written from
  changeset bodies by `xtask changeset version`.
- The GitHub Release body can additionally surface the new `CHANGELOG.md` section
  (replacing or augmenting today's `generate_release_notes`). See §9 decision D5.

## 5. Phased plan

*(Nothing below is implemented yet.)*

### Phase 0 — Convention & docs — ✅ done
- Created `.changeset/` with a `README.md` describing the format and workflow,
  plus a `nessemble: none` changeset for this planning PR.
- Rewrote `RELEASING.md` around the changeset model (the `-dev` mechanism is
  retired; documents the new "run the Release action" flow), with a rollout note
  since the automation lands in Phases 1–3.
- Updated `.github/pull_request_template.md` — replaced the `-dev`-based "Release
  impact" section with a "Changeset" section (which change type, or the
  `no-changeset` opt-out).

### Phase 1 — `xtask changeset` tooling
- Implement `add`, `check`, `status`, `version` with unit tests over fixture
  changeset dirs. Wire `cargo set-version` for the mutation.

### Phase 2 — CI enforcement
- Add the changeset-presence job to `ci.yml`, with the agreed escape hatch.
- Seed the repo's own convention: the implementing PRs each carry a changeset.

### Phase 3 — Release workflow
- **Set up the `nessemble-release[bot]` GitHub App** (D4): register with
  `contents: write`, install on the repo, store App ID + private key as secrets,
  and add the App to `main`'s branch-protection bypass list.
- Add `.github/workflows/version.yml` (`workflow_dispatch`) that mints an App
  token (`actions/create-github-app-token`), runs `xtask changeset version`,
  commits as the bot, and **pushes the bump directly to `main`** (D4) so
  `release.yml` fires.
- **Remove the `-dev` pre-release logic** (D6): drop the `*-*` branch from
  `release.yml`'s version-resolve step, leaving only the tag-existence check.
- Confirm end-to-end on a dry run / test tag before first real use.

### Phase 4 — Changelog surfacing
- Generate `CHANGELOG.md`; optionally feed the new section into the Release body.

## 6. Interaction with today's pipeline

- **`release.yml` is essentially untouched.** It keeps building assets on a
  new-version push to `main`. The only behavioral shift: the *only* thing that
  now advances the version on `main` is the Release action, so ordinary feature
  PRs never trip it.
- **The `-dev` pre-release guard is removed** (D6). With changesets, the version
  on `main` simply *stays at the last released version* between releases, so
  `release.yml` sees the existing tag and no-ops on ordinary pushes; there is
  nothing left to suppress, and the `*-*` guard in the version-resolve step is
  deleted.

## 7. Testing strategy

- **xtask unit tests** over fixture `.changeset/` directories: bump precedence
  (`major`/`minor`/`patch`/`none`), empty-set guard, malformed front-matter,
  changelog rendering, file consumption.
- **A `cargo set-version` integration check** on a throwaway copy of the manifests
  to confirm the six internal pins + `Cargo.lock` all move.
- **A workflow dry-run** (bump to a `-rc`/test version, or a fork) before the
  first production release.

## 8. Non-goals

- **Per-crate independent versions / crates.io publishing.** The workspace stays
  single-versioned and unpublished; changesets carry one bump for the whole repo.
- **Adopting the JS `@changesets/cli` toolchain.** The format is *inspired by* it,
  but the implementation is Rust-native via `xtask` — no Node dependency.
- **Changing how build artifacts are produced.** `release.yml`'s matrix and
  packaging are out of scope.
- **Automatic bump inference from commit messages** (Conventional Commits /
  semantic-release). Bumps are declared explicitly per PR via changesets.

## 9. Decisions

**All settled:**

1. **D1 — Changeset home & format.** A repo-root `.changeset/` dir; each changeset
   is a `.md` file in the **`changesets` crate's format** — `package: change_type`
   pairs between `---` delimiters, then a plain-text summary body. A single
   umbrella package key (`nessemble`) is used since the workspace is
   single-versioned. *Settled (§4.1).*
2. **D2 — Tooling in `xtask`, mutation via `cargo-edit`.** Logic lives in a Rust
   `xtask changeset` command group (parsing via the `changesets` crate); the
   actual manifest/lockfile edit is done by
   `cargo set-version --bump <level> --workspace`. No Node/`@changesets/cli`.
   *Settled.*
3. **D3 — No-changeset escape hatch.** Support a `nessemble: none` changeset as
   the self-documenting "no release impact" marker **and** honor a `no-changeset`
   PR label as a lighter override. *Settled: both.*
4. **D4 — How the bump lands on `main`.** **Direct push** from the Release action
   to `main`, authenticated as a **`nessemble-release[bot]` GitHub App**, which
   immediately chains into `release.yml`. The App is granted a branch-protection
   bypass on `main`; its token (minted per run via
   `actions/create-github-app-token`) both authorizes the push and, being a
   non-`GITHUB_TOKEN` identity, lets the resulting push trigger `release.yml`
   (§10). *Settled.*
5. **D5 — Release-notes source.** Write a curated `CHANGELOG.md` from changeset
   summaries and surface that section as the GitHub Release body, *replacing*
   today's `generate_release_notes`. *Settled.*
6. **D6 — Fate of the `-dev` guard.** **Removed.** Delete the `*-*` pre-release
   branch from `release.yml`'s version-resolve step; the changeset flow makes it
   unnecessary. *Settled.*

**Settled by the framing:** single-version workspace; Rust-native tooling; no
crates.io publishing; artifact build path unchanged.

## 10. Risks & constraints

- **Direct push to `main` under branch protection (D4).** Handled by the
  **`nessemble-release[bot]` GitHub App**: the App is added to `main`'s
  branch-protection **bypass** list, and the workflow mints a short-lived
  installation token per run with `actions/create-github-app-token`. Operational
  prerequisites to set up before Phase 3: (a) create/register the App with
  `contents: write` on this repo, (b) install it, (c) store its App ID + private
  key as repo secrets, (d) add it to the `main` bypass list.
- **Chained-workflow trigger.** A push made with the default `GITHUB_TOKEN` does
  **not** trigger other workflows, which would leave `release.yml` unfired. The
  App-token push is a distinct identity, so it triggers `release.yml` normally —
  no explicit re-dispatch needed.
- **Concurrent changesets.** Two PRs adding changesets never conflict (distinct
  random filenames). The Release action consumes and deletes them atomically in
  one commit; changesets added after that commit simply belong to the next
  release.
- **`cargo set-version` availability & correctness.** The workflow installs
  `cargo-edit`; the plan's Phase 1 test confirms a bump moves the root version,
  the six internal `[workspace.dependencies]` pins, and `Cargo.lock` together.
