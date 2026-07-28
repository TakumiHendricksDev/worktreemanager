#!/usr/bin/env bash
# Install a bundled example config into a target repo's git directory.
#
# Deliberately writes to `git rev-parse --git-common-dir`, NOT the working tree:
#   * nothing is added to the target repo's tracked files, so no PR is needed
#   * --git-common-dir (not --git-dir) means all of that repo's worktrees share
#     the one config; inside a linked worktree `.git` is a *file*, so the naive
#     "<repo>/.git/..." path would be wrong
#
# To share the config with a team later, move it to a committed <repo>/wtm.toml —
# the repo layer sits directly above this local layer in the precedence chain.
set -euo pipefail

src="${1:?usage: install-example.sh <example.toml> <repo-path>}"
repo="${2:?usage: install-example.sh <example.toml> <repo-path>}"

[ -f "$src" ] || { printf '✗ no such example: %s\n' "$src" >&2; exit 1; }

common_dir="$(git -C "$repo" rev-parse --git-common-dir 2>/dev/null)" || {
    printf '✗ not a git repository: %s\n' "$repo" >&2; exit 1
}
# --git-common-dir can be relative to the repo, so resolve it.
case "$common_dir" in
    /*) ;;
    *) common_dir="$(cd "$repo" && cd "$common_dir" && pwd)" ;;
esac

dest="${common_dir}/wtm.local.toml"

if [ -e "$dest" ]; then
    printf '! %s already exists.\n' "$dest" >&2
    printf '  Diff against the example:\n\n' >&2
    diff -u "$dest" "$src" >&2 || true
    printf '\n  Not overwriting. Merge by hand, or remove it first.\n' >&2
    exit 1
fi

cp "$src" "$dest"
printf '✓ installed %s\n  → %s\n\n' "$src" "$dest"
printf 'wtm will ask you to review and trust this config the first time it loads\n'
printf 'it, because it declares shell commands. Read them before approving.\n'
