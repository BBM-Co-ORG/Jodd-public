# public-mirror-prep/ — staging area for the public repo

This folder contains the docs and scripts that **only live in the
public mirror** (`BBM-Co-ORG/Jodd-public`). They are kept here in the
private upstream so you can edit them with full context, run them
through review, and version-control them alongside the code they
describe.

**This folder itself never goes into the public snapshot** — see the
`EXCLUDES` list in `scripts/sync-to-public.sh`.

## What's in here

| File | Purpose |
|---|---|
| `LICENSE` | Apache 2.0 license text. |
| `NOTICE` | Required by Apache 2.0 §4(d). Copyright + trademark notice. |
| `README.md` | Public-facing README — landing page, build steps, status. |
| `DISCLAIMER.md` | No-warranty + trademark + data-risk + scope-of-trust statement. |
| `CONTRIBUTING.md` | How outside contributors submit PRs into the snapshot model. |
| `SECURITY.md` | Vulnerability reporting policy. |
| `ARCHITECTURE.md` | Sanitized version of CLAUDE.md — architecture only, no defect IDs, no internal commit hashes, no internal dev memory. |
| `scripts/sync-to-public.sh` | Build a snapshot of the current commit, strip secrets/internal docs, force-push to the public repo's `main`. |

## First-time setup

```bash
# 1. Create the public repo (empty, no auto-init) on GitHub:
#    https://github.com/organizations/BBM-Co-ORG/repositories/new
#    Name: Jodd-public
#    Description: "Open-source mirror of Jodd — Apple Notes for non-Apple devices"
#    Visibility: Public
#    Do NOT initialize with README/license/.gitignore — we push our own first commit.

# 2. Push the first snapshot:
./public-mirror-prep/scripts/sync-to-public.sh

# 3. After the push lands, on the GitHub web UI for Jodd-public:
#    - Settings → General → Features → uncheck "Wikis" (we use docs/ instead)
#    - Settings → General → Pull Requests → check "Allow squash merging" only
#    - Settings → Branches → add protection rule on `main`:
#        - Restrict deletions
#        - Allow force pushes ONLY by maintainers (the snapshot script needs it)
#        - Require pull request before merging (so outside PRs can't land directly)
#    - Settings → Code security → enable Dependabot alerts
#    - Settings → Secrets and variables → Actions →
#        - Add GOOGLE_CLIENT_ID + GOOGLE_CLIENT_SECRET (test client, NOT prod)
#          if you want release CI to work on the public repo too. (Optional —
#          you can run releases only from the private upstream.)
```

## Ongoing snapshot workflow

```bash
# After landing a fix in the private upstream:
git tag v0.1.3
git push origin v0.1.3

# Then push a sanitized snapshot to public:
./public-mirror-prep/scripts/sync-to-public.sh v0.1.3

# Then on the public repo, cut a release that points at the snapshot
# commit so users have a download page:
gh release create v0.1.3 \
  --repo BBM-Co-ORG/Jodd-public \
  --title "Jodd 0.1.3" \
  --notes-file public-mirror-prep/release-notes-v0.1.3.md \
  src-tauri/target/release/bundle/dmg/Jodd_0.1.3_aarch64.dmg \
  src-tauri/target/release/bundle/msi/Jodd_0.1.3_x64.msi
```

## Accepting outside PRs

When someone opens a PR on the public repo:

1. Review it on the public repo as you would any PR.
2. If you accept it, cherry-pick into the private upstream **with `-x`**
   so the original commit reference is preserved:

   ```bash
   # In the private upstream:
   git fetch git@github.com:BBM-Co-ORG/Jodd-public.git pull/123/head:pr-123
   git cherry-pick -x pr-123
   # Edit / amend / test as needed
   git push origin main
   ```

3. Close the public PR with a comment pointing to the cherry-pick
   commit on the upstream and confirming it will appear in the next
   snapshot. **Do not merge the PR through GitHub's UI** — that would
   write a merge commit onto the public main, and the next snapshot
   would overwrite it anyway.

4. When the next snapshot push happens, the contributor's commit will
   appear on the public main with their authorship intact (the `-x`
   trailer carries the link back to their PR commit).

## Editing the public docs

Edit the files in this folder as you would any other repo file. They
become part of the public repo the next time you run the snapshot
script. Treat them as project assets, not as scratch.

## Audit before every push

The `sync-to-public.sh` script does a basic regex sweep for common
credential patterns (Google API key, AWS access key, PEM private key,
OAuth client ID). Add new patterns to the script as the project
acquires new secret types.

The audit is not a substitute for **reading the diff yourself** before
the first few pushes. Run:

```bash
./public-mirror-prep/scripts/sync-to-public.sh HEAD
# When prompted, decline the push:
# Confirm push? [y/N] n
# Then inspect the temp worktree printed in the abort message.
```
