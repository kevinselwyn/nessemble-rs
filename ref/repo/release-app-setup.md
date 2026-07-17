# Release App setup (`nessemble-release[bot]`)

The changeset-driven **Release** workflow (`.github/workflows/version.yml`) pushes
the version-bump commit directly to `main`, authenticated as a dedicated GitHub
App, `nessemble-release[bot]`. This document records the one-time, out-of-band
setup that the workflow depends on. See
[`plans/004-release-orchestration.md`](../../plans/004-release-orchestration.md)
for the overall design and [`RELEASING.md`](../../RELEASING.md) for how a release
is cut.

## The contract the workflow expects

| Thing | Value |
|-------|-------|
| App slug | `nessemble-release` (commit author `nessemble-release[bot]`) |
| Repo secret | `RELEASE_APP_ID` — the App's numeric ID |
| Repo secret | `RELEASE_APP_PRIVATE_KEY` — the full `.pem` contents |

The workflow mints a short-lived installation token per run with
`actions/create-github-app-token` from those two secrets; the App's slug and bot
user-id are read at run time (so nothing else is hard-coded).

## Why a GitHub App (not `GITHUB_TOKEN`)

One mechanism solves two problems at once:

1. **Branch protection.** `GITHUB_TOKEN` cannot be granted a bypass on `main`; a
   GitHub App can.
2. **Triggering the build.** A push made by `GITHUB_TOKEN` does **not** trigger
   other workflows, so the Publish pipeline (`release.yml`) would never fire. The
   App is a distinct identity, so its push triggers Publish normally.

The same `GITHUB_TOKEN` limitation is why the Publish pipeline's `release` job
also creates the GitHub Release with the App token: a release cut by
`GITHUB_TOKEN` emits no workflow-triggering event, so the Pages pipeline
(`pages.yml`, which listens for `release: published`) would never deploy. The App
identity makes the release publish normally and Pages runs. Creating a release
uses the App's **Contents: write** permission — no additional grant is needed.

The App is granted **Contents: write** and nothing else — least privilege. It
does not need Pull requests (the bump is a direct push, not a PR) or Workflows
(the bump commit only touches `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`, and
`.changeset/` — never `.github/workflows/`).

## Setup steps (done)

1. **Create the App** at <https://github.com/settings/apps/new> (personal
   account):
   - Name: `nessemble-release`
   - Homepage URL: the repo URL
   - Webhook: **Active** unchecked
   - Repository permissions: **Contents: Read and write**; Metadata: Read-only
     (mandatory). Everything else *No access*.
   - Where can this App be installed: **Only on this account**.
2. **Record the App ID** (App → General) → secret `RELEASE_APP_ID`.
3. **Generate a private key** (App → Private keys → Generate). The downloaded
   `.pem` contents (including the `BEGIN`/`END` lines) → secret
   `RELEASE_APP_PRIVATE_KEY`. Treat the key like a password.
4. **Install the App** on `kevinselwyn/nessemble-rs` (App → Install App → Only
   select repositories).
5. **Store the secrets** at Settings → Secrets and variables → Actions.
6. **Grant the bypass:** Settings → Rules → Rulesets → the ruleset targeting
   `main` → Bypass list → add the `nessemble-release` App. (If `main` uses
   classic branch protection instead, migrate it to a ruleset, or use "Allow
   specified actors to bypass required pull requests" and add the App there.)

## Verifying / rotating

- **Verify:** run Actions → *Release* → *Run workflow*. A correct setup mints the
  token, runs `xtask changeset version`, and pushes a `Release X.Y.Z` commit
  authored by `nessemble-release[bot]` to `main`, which starts the Publish
  pipeline. With no release-impacting changesets it stops early with "nothing to
  release" — expected.
- **Rotate the key:** generate a new private key on the App, replace
  `RELEASE_APP_PRIVATE_KEY`, then delete the old key. The App ID never changes.
