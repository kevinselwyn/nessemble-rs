#!/usr/bin/env bash
#
# Stop hook: run the exact checks CI runs, so an agent never finishes a turn —
# and therefore never opens a PR — in a state the CI pipeline would reject.
#
# This mirrors .github/workflows/ci.yml:
#   * the `check` job   -> cargo fmt / clippy / test / xtask parity / changeset check
#   * the `changeset`   -> a PR must add a changeset under .changeset/
#
# On any failure the hook writes an explanation to stderr and exits 2, which
# blocks the Stop event and feeds the failure back to Claude so it fixes the
# problem before stopping. When every check passes the hook is silent and
# exits 0, letting the turn end normally.
#
# Escape hatches (for when the gate is genuinely not wanted right now):
#   NESSEMBLE_SKIP_CI_HOOK=1   -> skip the whole hook
#   NESSEMBLE_NO_CHANGESET=1   -> skip only the changeset-presence gate
#                                 (the local equivalent of the `no-changeset` PR label)
#
set -uo pipefail

ROOT="${CLAUDE_PROJECT_DIR:-$(git rev-parse --show-toplevel 2>/dev/null || pwd)}"
cd "$ROOT" 2>/dev/null || exit 0

[ "${NESSEMBLE_SKIP_CI_HOOK:-}" = "1" ] && exit 0

# Drain the Stop payload on stdin (avoids SIGPIPE); we don't need any field.
cat >/dev/null 2>&1 || true

log="$(mktemp "${TMPDIR:-/tmp}/nessemble-stop-hook.XXXXXX")"
trap 'rm -f "$log"' EXIT

# fail <label> <fix-command> — surface the captured output to Claude and block.
fail() {
  {
    echo "CI gate failed: $1"
    echo
    echo "This mirrors the CI pipeline in .github/workflows/ci.yml, which would"
    echo "reject a PR opened in the current state. Fix it before finishing —"
    echo "reproduce locally with:"
    echo "    $2"
    echo
    echo "----- captured output (tail) -----"
    tail -n 200 "$log"
  } >&2
  exit 2
}

# run <label> <fix-command> -- <cmd...> — run a check, capturing combined output.
run() {
  local label="$1" fix="$2"
  shift 2
  [ "$1" = "--" ] && shift
  if ! "$@" >"$log" 2>&1; then
    fail "$label" "$fix"
  fi
}

# --- `check` job: fmt / clippy / test / parity / changeset check ---------------
run "Format"           "cargo fmt --all"                                   -- cargo fmt --all --check
run "Clippy"           "cargo clippy --all-targets --all-features -- -D warnings" -- cargo clippy --all-targets --all-features -- -D warnings
run "Test"             "cargo test --all-features"                          -- cargo test --all-features
run "Parity"           "cargo run -p xtask -- parity"                       -- cargo run -p xtask -- parity
run "Changeset validate" "cargo run -p xtask -- changeset check"            -- cargo run -p xtask -- changeset check

# --- `changeset` job: a PR must add a changeset (unless opted out) -------------
if [ "${NESSEMBLE_NO_CHANGESET:-}" != "1" ]; then
  base=""
  for ref in origin/main main; do
    if git rev-parse --verify --quiet "${ref}^{commit}" >/dev/null 2>&1; then
      base="$ref"
      break
    fi
  done

  if [ -n "$base" ]; then
    mergebase="$(git merge-base "$base" HEAD 2>/dev/null)" || mergebase="$base"

    # Only gate when this branch actually carries changes that would form a PR.
    committed_changes="$(git diff --name-only "${mergebase}...HEAD" 2>/dev/null || true)"
    worktree_changes="$(git status --porcelain 2>/dev/null || true)"

    if [ -n "$committed_changes" ] || [ -n "$worktree_changes" ]; then
      # Changeset .md files added since the base — committed or still in the
      # working tree (the agent may not have committed yet). README.md is ignored.
      added_committed="$(git diff --name-only --diff-filter=A "${mergebase}...HEAD" -- .changeset/ 2>/dev/null \
        | { grep -E '\.md$' || true; } | { grep -vx '.changeset/README.md' || true; })"
      added_worktree="$(git status --porcelain -- .changeset/ 2>/dev/null \
        | { grep -E '^(\?\?|A[ M]|M[ M]| A|AM) ' || true; } | cut -c4- \
        | { grep -E '\.md$' || true; } | { grep -vx '.changeset/README.md' || true; })"

      if [ -z "$added_committed" ] && [ -z "$added_worktree" ]; then
        {
          echo "CI gate failed: Changeset required"
          echo
          echo "The CI \`changeset\` job requires every PR that changes shipped"
          echo "behavior to add a changeset under .changeset/ (see .changeset/README.md)."
          echo "This branch has changes but no new changeset. Add one before finishing:"
          echo "    cargo run -p xtask -- changeset add <major|minor|patch|none> \"summary\""
          echo
          echo "A change with no release impact (docs, CI, chores) uses \`none\`. If the"
          echo "PR will instead carry the \`no-changeset\` label, set NESSEMBLE_NO_CHANGESET=1."
        } >&2
        exit 2
      fi
    fi
  fi
fi

exit 0
