# Claude Code configuration

## Stop hook: CI gate (`hooks/stop-ci-checks.sh`)

Registered in [`settings.json`](settings.json) as a `Stop` hook, this runs the
**exact checks the CI pipeline runs** every time Claude finishes a turn. The
goal: an agent can never end a turn — and therefore never open a PR — in a state
that [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) would reject.

It mirrors both CI jobs:

| CI step (`ci.yml`)                                     | Hook command |
| ----------------------------------------------------- | ------------ |
| Format                                                | `cargo fmt --all --check` |
| Clippy                                                 | `cargo clippy --all-targets --all-features -- -D warnings` |
| Test                                                  | `cargo test --all-features` |
| Parity                                                | `cargo run -p xtask -- parity` |
| Changeset validate                                    | `cargo run -p xtask -- changeset check` |
| `changeset` job (PR must add a changeset)             | checks for a new `.changeset/*.md` on the branch |

On failure the hook writes the failing check and its output to stderr and exits
`2`, which blocks the Stop event and feeds the failure back to Claude so it fixes
the problem before stopping. When everything passes it is silent and exits `0`.

### Escape hatches

- `NESSEMBLE_SKIP_CI_HOOK=1` — skip the whole hook (e.g. mid-refactor).
- `NESSEMBLE_NO_CHANGESET=1` — skip only the changeset-presence gate; the local
  equivalent of applying the `no-changeset` label to a PR.

> The hook runs the full test/clippy suite, so a turn that leaves the tree
> changed takes as long as CI to finish. Cargo's incremental cache keeps repeat
> runs fast when nothing relevant changed.
