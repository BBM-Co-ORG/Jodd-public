#!/usr/bin/env bash
#
# release-dispatch.sh — build a release on Jodd-public, then sign it (and, for
# a real release, publish it) from the private repository.
#
# Run it from the private repository, AFTER:
#   1. the release commit is on main and its tag vX.Y.Z is pushed, and
#   2. public-mirror-prep/scripts/sync-to-public.sh vX.Y.Z has pushed that
#      tag's snapshot to Jodd-public's main (interactive — it force-pushes).
#
# Usage:
#   scripts/release-dispatch.sh v0.39.0 --dry-run   # draft v0.39.0-dryrun.N, signed, NOT published
#   scripts/release-dispatch.sh v0.39.0             # real release: builds, signs, PUBLISHES
#
# Running it without --dry-run is the publication approval, as pushing the tag
# was before 2026-10-26; the gates in release.yml and release-sign.yml still
# stop anything that fails them. The waiting happens here, with `gh run watch`
# on your machine, so no runner is billed for it.
#
# Needs: gh, authenticated as someone who can dispatch workflows on both
# repositories. Dispatches nothing until every local check has passed.
set -euo pipefail

PUB="${PUB:-BBM-Co-ORG/Jodd-public}"
PRIV="${PRIV:-BBM-Co-ORG/Jodd}"

die() { echo "ERROR: $*" >&2; exit 1; }
usage() { echo "usage: $0 vX.Y.Z [--dry-run]" >&2; exit 2; }

TAG="${1:-}"
[[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || usage
case "${2:-}" in
  "") DRY=false ;;
  --dry-run) DRY=true ;;
  *) usage ;;
esac
[[ $# -le 2 ]] || usage

# --- 1. What is on Jodd-public's main, and is it the release asked for? -----
SNAP_SHA="$(gh api "repos/$PUB/commits/main" --jq .sha)"
UPSTREAM="$(gh api "repos/$PUB/commits/$SNAP_SHA" --jq .commit.message \
  | grep -oE 'commit [0-9a-f]{40}' | head -1 | cut -d' ' -f2 || true)"
[[ -n "$UPSTREAM" ]] || die "$PUB main ($SNAP_SHA) is not a sync-to-public.sh snapshot."
echo "→ $PUB main $SNAP_SHA is a snapshot of $UPSTREAM"

if [[ "$DRY" == false ]]; then
  TAG_SHA="$(gh api "repos/$PRIV/commits/$TAG" --jq .sha 2>/dev/null || true)"
  [[ -n "$TAG_SHA" ]] || die "$TAG is not on $PRIV. Push it first: git push origin $TAG"
  [[ "$TAG_SHA" == "$UPSTREAM" ]] \
    || die "$TAG is $TAG_SHA, but $PUB main is a snapshot of $UPSTREAM. Run: public-mirror-prep/scripts/sync-to-public.sh $TAG"
else
  echo "  (dry run: the private tag is not checked; the build is of that snapshot)"
fi

# Runs that existed before we dispatch, so the new one can be told apart
# without trusting clocks.
existing_ids() { gh run list -R "$1" -w "$2" -e workflow_dispatch -L 50 --json databaseId --jq '[.[].databaseId]'; }

# find_new_run <repo> <workflow> <ids-before> <title-prefix>
find_new_run() {
  local repo="$1" wf="$2" before="$3" prefix="$4" id=""
  for _ in $(seq 1 30); do
    id="$(gh run list -R "$repo" -w "$wf" -e workflow_dispatch -L 20 \
      --json databaseId,displayTitle \
      --jq "[.[] | select((.databaseId as \$i | $before | index(\$i)) == null) | select(.displayTitle | startswith(\"$prefix\"))] | first | .databaseId // empty")"
    [[ -n "$id" ]] && { echo "$id"; return 0; }
    sleep 4
  done
  return 1
}

# --- 2. Build on Jodd-public --------------------------------------------------
BEFORE="$(existing_ids "$PUB" release.yml)"
echo "→ Dispatching release.yml on $PUB main (tag $TAG, dry_run $DRY)"
gh workflow run release.yml -R "$PUB" --ref main \
  -f tag="$TAG" -f upstream_sha="$UPSTREAM" -f dry_run="$DRY"
RUN="$(find_new_run "$PUB" release.yml "$BEFORE" "Release $TAG")" \
  || die "dispatched, but no new run appeared on $PUB within 2 minutes — check its Actions tab."
echo "  https://github.com/$PUB/actions/runs/$RUN"
gh run watch "$RUN" -R "$PUB" --exit-status --interval 30 \
  || die "the build failed: https://github.com/$PUB/actions/runs/$RUN — nothing was published."
echo "✓ Built and staged"

if [[ "$DRY" == true ]]; then
  NUMBER="$(gh run view "$RUN" -R "$PUB" --json number --jq .number)"
  RELEASE_TAG="$TAG-dryrun.$NUMBER"
  PUBLISH=false
else
  RELEASE_TAG="$TAG"
  PUBLISH=true
fi

# --- 3. Sign (and publish) from the private repository ------------------------
BEFORE="$(existing_ids "$PRIV" release-sign.yml)"
echo "→ Dispatching release-sign.yml on $PRIV main (tag $RELEASE_TAG, publish $PUBLISH)"
gh workflow run release-sign.yml -R "$PRIV" --ref main \
  -f tag="$RELEASE_TAG" -f public_run_id="$RUN" -f publish="$PUBLISH"
# release-sign.yml's run-name.
if [[ "$PUBLISH" == true ]]; then
  SIGN_TITLE="Sign + publish $RELEASE_TAG"
else
  SIGN_TITLE="Sign + stage $RELEASE_TAG"
fi
SIGN_RUN="$(find_new_run "$PRIV" release-sign.yml "$BEFORE" "$SIGN_TITLE")" \
  || die "dispatched, but no new run appeared on $PRIV within 2 minutes — check its Actions tab."
echo "  https://github.com/$PRIV/actions/runs/$SIGN_RUN"
gh run watch "$SIGN_RUN" -R "$PRIV" --exit-status --interval 15 \
  || die "signing failed: https://github.com/$PRIV/actions/runs/$SIGN_RUN — read its summary before re-running."

if [[ "$PUBLISH" == true ]]; then
  echo "✓ Published: https://github.com/$PUB/releases/tag/$RELEASE_TAG"
  echo "  Check both Actions tabs, and that the release lists 7 assets."
else
  echo "✓ Signed into draft $RELEASE_TAG (not published): https://github.com/$PUB/releases"
  echo "  Delete it when done: gh release delete $RELEASE_TAG -R $PUB --yes"
fi
