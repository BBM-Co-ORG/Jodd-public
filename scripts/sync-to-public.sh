#!/usr/bin/env bash
#
# sync-to-public.sh — push a sanitized snapshot of the current commit
# to the Jodd-public repository as a single squash commit.
#
# Run from the private upstream repo, on a clean working tree, after
# you have tagged the release in private (e.g. v0.1.3).
#
# What this script does:
#   1. Creates a fresh temp worktree at the chosen commit.
#   2. Removes files that must not appear in public (CLAUDE.md,
#      internal docs, .env, prep folder, etc.) — see EXCLUDES below.
#   3. Copies the prepared public docs (LICENSE, README.md, etc.)
#      from public-mirror-prep/ into the worktree root.
#   4. Force-pushes the resulting tree as a single commit to the
#      public repository's main branch (rewinding it — the public repo
#      history is intentionally snapshot-style, not preserved).
#
# This script does NOT push tags. Tag the release separately on the
# public repo if you want a release page there:
#   gh release create v0.1.3 --repo BBM-Co-ORG/Jodd-public ...
#
# Prerequisites:
#   - The Jodd-public repository exists and your gh/git auth has push
#     rights to it.
#   - public-mirror-prep/ in this repo contains the latest LICENSE,
#     NOTICE, README.md, DISCLAIMER.md, CONTRIBUTING.md, SECURITY.md,
#     ARCHITECTURE.md.
#
# Usage:
#   ./public-mirror-prep/scripts/sync-to-public.sh              # snapshot HEAD
#   ./public-mirror-prep/scripts/sync-to-public.sh v0.1.3       # snapshot a tag
#
set -euo pipefail

PUBLIC_REMOTE="${PUBLIC_REMOTE:-git@github.com:BBM-Co-ORG/Jodd-public.git}"
PUBLIC_BRANCH="${PUBLIC_BRANCH:-main}"
SOURCE_REF="${1:-HEAD}"

# Files / directories that must never appear in public.
EXCLUDES=(
  "CLAUDE.md"
  "docs/SYNC-BUGS-2026-06-07.md"
  ".env"
  ".env.local"
  "accounts.json"
  "jodd.sqlite3"
  "public-mirror-prep"
  ".claude"
  "memory"
  "*.log"
)

# Files copied FROM public-mirror-prep/ INTO the snapshot root.
PUBLIC_DOCS=(
  "LICENSE"
  "NOTICE"
  "README.md"
  "DISCLAIMER.md"
  "CONTRIBUTING.md"
  "SECURITY.md"
  "ARCHITECTURE.md"
)

# Resolve repo root + sanity checks
REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

if [[ -n "$(git status --porcelain)" ]]; then
  echo "ERROR: working tree is dirty. Commit or stash first." >&2
  exit 1
fi

COMMIT_SHA="$(git rev-parse "$SOURCE_REF")"
COMMIT_SHORT="$(git rev-parse --short "$SOURCE_REF")"
TIMESTAMP="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

WORKTREE_DIR="$(mktemp -d -t jodd-public-snapshot-XXXXXX)"
trap 'git worktree remove --force "$WORKTREE_DIR" 2>/dev/null || true; rm -rf "$WORKTREE_DIR"' EXIT

echo "→ Creating temp worktree at $COMMIT_SHORT ..."
git worktree add --detach "$WORKTREE_DIR" "$COMMIT_SHA"

echo "→ Stripping excluded paths ..."
for path in "${EXCLUDES[@]}"; do
  # Use find for glob expansion; -path matches the full relative path
  find "$WORKTREE_DIR" -path "$WORKTREE_DIR/$path" -prune -exec rm -rf {} + 2>/dev/null || true
done

echo "→ Copying prepared public docs ..."
for doc in "${PUBLIC_DOCS[@]}"; do
  src="$REPO_ROOT/public-mirror-prep/$doc"
  if [[ ! -f "$src" ]]; then
    echo "ERROR: missing prepared doc $src" >&2
    exit 1
  fi
  cp "$src" "$WORKTREE_DIR/$doc"
done

echo "→ Sanity-check: scanning for likely secrets in snapshot ..."
# Very basic — refuse to push if anything that looks like a credential
# leaks through. Extend the pattern set as you learn what to look for.
if grep -rIE \
    -e 'AIza[0-9A-Za-z_-]{35}' \
    -e 'AKIA[0-9A-Z]{16}' \
    -e '-----BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY-----' \
    -e 'apps\.googleusercontent\.com' \
    --exclude='*.example' \
    --exclude-dir=node_modules \
    --exclude-dir=target \
    "$WORKTREE_DIR" >/dev/null 2>&1; then
  echo "ERROR: snapshot appears to contain credentials. Refusing to push." >&2
  echo "Matches:" >&2
  grep -rInE \
      -e 'AIza[0-9A-Za-z_-]{35}' \
      -e 'AKIA[0-9A-Z]{16}' \
      -e '-----BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY-----' \
      -e 'apps\.googleusercontent\.com' \
      --exclude='*.example' \
      --exclude-dir=node_modules \
      --exclude-dir=target \
      "$WORKTREE_DIR" >&2 || true
  exit 1
fi

echo "→ Building snapshot commit ..."
cd "$WORKTREE_DIR"
rm -rf .git           # detach from upstream history entirely
git init -q -b "$PUBLIC_BRANCH"
git remote add origin "$PUBLIC_REMOTE"
git add -A
git -c commit.gpgsign=false commit -q -m "Snapshot from upstream $COMMIT_SHORT ($TIMESTAMP)

This is a sanitized snapshot of the private upstream repository at
commit $COMMIT_SHA.

The public repository's history is intentionally not preserved between
snapshots — each push to main rewinds the branch to a single
self-contained commit. Tags and Releases on this repository serve as
the version history.

Authorship is preserved through tags/releases and the upstream commit
log; individual contributor credits for changes since the last
snapshot will appear in the next release notes."

echo "→ Force-pushing to $PUBLIC_REMOTE ($PUBLIC_BRANCH) ..."
read -r -p "Confirm push? [y/N] " confirm
if [[ "$confirm" != "y" && "$confirm" != "Y" ]]; then
  echo "Aborted by user. Snapshot left in $WORKTREE_DIR for inspection."
  trap - EXIT
  exit 1
fi

git push --force origin "$PUBLIC_BRANCH"

echo
echo "✓ Pushed snapshot $COMMIT_SHORT to $PUBLIC_REMOTE@$PUBLIC_BRANCH"
echo "  Next: create a release on the public repo if this snapshot"
echo "  corresponds to a tagged version, e.g.:"
echo "    gh release create v0.1.3 --repo BBM-Co-ORG/Jodd-public ..."
