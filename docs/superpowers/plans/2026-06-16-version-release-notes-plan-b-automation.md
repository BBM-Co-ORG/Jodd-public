# Version & Release-Notes Pipeline — Plan B (release automation) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** On a `v*` tag push, automatically (a) set the GitHub release body on the private `Jodd` repo from `CHANGELOG.md`, and (b) publish the same binaries + notes as a release on the public `Jodd-public` repo — ending the manual, drifted (stuck at v0.14.5) public-release process.

**Architecture:** A new `changelog-section.mjs` CLI reuses the Plan A parser to print one version's notes. A new single-run `finalize` job in `release.yml` (`needs: build`, tag-only) edits the private draft release's body, downloads its built assets, and re-publishes them to `Jodd-public` via a cross-repo PAT (`PUBLIC_REPO_TOKEN`). Source code is never pushed to the public repo — releases only.

**Tech Stack:** GitHub Actions, `gh` CLI (preinstalled on runners), Node 20, the Plan A parser (`scripts/changelog-parse.mjs`).

**Spec:** `docs/superpowers/specs/2026-06-16-version-release-notes-pipeline-design.md`

**Depends on Plan A:** `CHANGELOG.md` and `scripts/changelog-parse.mjs` must already exist (Plan A Tasks 1–2).

**Reference:** existing build job is `jobs.build` in `.github/workflows/release.yml:35`; matrix platforms `macos-latest` + `windows-latest`; it uses `tauri-apps/tauri-action@v0` with `releaseDraft: true` and a hardcoded `releaseBody: "See the changelog for details."` (lines ~105–110).

---

## File Structure

- Create `scripts/changelog-section.mjs` — CLI: `node scripts/changelog-section.mjs <version>` prints that version's CHANGELOG body to stdout; exits 1 if the version is absent.
- Modify `.github/workflows/release.yml` — drop the hardcoded `releaseBody` to a neutral pointer, add the `finalize` job (body-from-changelog + Jodd-public sync).
- Documentation/manual: create the `PUBLIC_REPO_TOKEN` secret (Task 4).

---

### Task 1: `changelog-section.mjs` CLI

**Files:**
- Create: `scripts/changelog-section.mjs`

- [ ] **Step 1: Write the CLI**

Reuses `sectionRawText` from the Plan A parser. Create `scripts/changelog-section.mjs`:

```js
#!/usr/bin/env node
// Print the CHANGELOG.md body for one version (used by CI to fill release
// notes). Usage: node scripts/changelog-section.mjs 0.17.1
// Exits 1 (with a stderr message) if the version has no section — so a
// release can never go out with empty notes.
import { readFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { sectionRawText } from './changelog-parse.mjs';

const version = process.argv[2];
if (!version) {
  console.error('usage: changelog-section.mjs <version>');
  process.exit(2);
}
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const md = readFileSync(resolve(root, 'CHANGELOG.md'), 'utf8');
const body = sectionRawText(md, version);
if (!body) {
  console.error(`changelog-section: no CHANGELOG section for version "${version}"`);
  process.exit(1);
}
process.stdout.write(body + '\n');
```

- [ ] **Step 2: Verify the happy path**

Run: `node scripts/changelog-section.mjs 0.17.1`
Expected: prints the 0.17.1 notes body (e.g. `### Added` + the rename bullet), exit code 0.

- [ ] **Step 3: Verify the failure path (no notes → non-zero exit)**

Run: `node scripts/changelog-section.mjs 9.9.9; echo "exit=$?"`
Expected: stderr `no CHANGELOG section for version "9.9.9"`, `exit=1`.

- [ ] **Step 4: Commit**

```bash
git add scripts/changelog-section.mjs
git commit -m "feat(ci): changelog-section CLI (notes for one version, fails if absent)"
```

---

### Task 2: Neutralize the hardcoded release body in the build job

**Files:**
- Modify: `.github/workflows/release.yml` (the `tauri-action` `releaseBody`, ~lines 105–110)

> The build job runs per-platform (matrix), so it must NOT each try to set the
> final notes (they would race). The real notes are set once by the `finalize`
> job (Task 3). Here we just make the interim draft body honest.

- [ ] **Step 1: Replace the `releaseBody` block**

Change the existing `tauri-action` `releaseBody` from:

```yaml
          releaseBody: |
            See the changelog for details.

            **Install notes:**
            - macOS: open with right-click → Open (first run only — Gatekeeper)
            - Windows: SmartScreen may warn — click "More info" → "Run anyway"
```

to:

```yaml
          releaseBody: |
            Release notes are set automatically from CHANGELOG.md by the finalize job.

            **Install notes:**
            - macOS: open with right-click → Open (first run only — Gatekeeper)
            - Windows: SmartScreen may warn — click "More info" → "Run anyway"
```

- [ ] **Step 2: Validate the workflow YAML parses**

Run: `python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/release.yml'))" && echo OK`
Expected: `OK` (no YAML parse error).

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "chore(ci): interim draft body honest (finalize job sets real notes)"
```

---

### Task 3: `finalize` job — set body from CHANGELOG + sync to Jodd-public

**Files:**
- Modify: `.github/workflows/release.yml` (add a new top-level job under `jobs:`)

- [ ] **Step 1: Append the `finalize` job**

Add this as a sibling of `build` under `jobs:` (same indentation as `build:`) in `.github/workflows/release.yml`:

```yaml
  # Runs once after all platform builds, on a real tag only. Sets the private
  # release's notes from CHANGELOG.md, then publishes the same binaries + notes
  # to the public download repo. Source is never pushed — releases only.
  finalize:
    needs: build
    if: github.ref_type == 'tag'
    runs-on: ubuntu-latest
    permissions:
      contents: write   # edit/read the release on THIS (private) repo
    steps:
      - uses: actions/checkout@v5

      - name: Install Node
        uses: actions/setup-node@v5
        with:
          node-version: '20'

      - name: Derive tag + version, build notes file
        id: notes
        run: |
          set -euo pipefail
          TAG="${GITHUB_REF_NAME}"
          VERSION="${TAG#v}"
          echo "tag=$TAG" >> "$GITHUB_OUTPUT"
          # Fails the job if CHANGELOG.md has no section for this version.
          node scripts/changelog-section.mjs "$VERSION" > notes.md
          echo "Notes for $TAG:"; cat notes.md

      - name: Set release notes on the private repo
        env:
          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
        run: |
          set -euo pipefail
          gh release edit "${{ steps.notes.outputs.tag }}" \
            --repo "$GITHUB_REPOSITORY" \
            --notes-file notes.md

      - name: Download built assets from the private release
        env:
          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
        run: |
          set -euo pipefail
          mkdir -p assets
          # Works on a draft release with the contents:write token.
          gh release download "${{ steps.notes.outputs.tag }}" \
            --repo "$GITHUB_REPOSITORY" \
            --dir assets
          echo "Downloaded:"; ls -la assets

      - name: Publish binaries + notes to Jodd-public
        env:
          GH_TOKEN: ${{ secrets.PUBLIC_REPO_TOKEN }}
        run: |
          set -euo pipefail
          TAG="${{ steps.notes.outputs.tag }}"
          PUB="BBM-Co-ORG/Jodd-public"
          # Create the public release if it doesn't exist yet, else upload
          # (clobbering) into the existing one. Published (not draft) so users
          # can download immediately — add `--draft` here if you want to gate it.
          if gh release view "$TAG" --repo "$PUB" >/dev/null 2>&1; then
            gh release upload "$TAG" assets/* --repo "$PUB" --clobber
            gh release edit "$TAG" --repo "$PUB" --notes-file notes.md
          else
            gh release create "$TAG" assets/* \
              --repo "$PUB" \
              --title "Jodd $TAG" \
              --notes-file notes.md
          fi
```

- [ ] **Step 2: Validate the workflow YAML parses**

Run: `python3 -c "import yaml,sys; d=yaml.safe_load(open('.github/workflows/release.yml')); assert 'finalize' in d['jobs']; print('OK')"`
Expected: `OK`.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "feat(ci): finalize job — notes from CHANGELOG + releases-only sync to Jodd-public"
```

---

### Task 4: Create the `PUBLIC_REPO_TOKEN` secret (manual, one-time)

**Files:** none (GitHub settings)

> The default `GITHUB_TOKEN` is scoped to the current repo only, so it cannot
> write releases on `Jodd-public`. A cross-repo PAT is required.

- [ ] **Step 1: Create a fine-grained PAT**

In GitHub → Settings → Developer settings → Fine-grained tokens → Generate:
- Resource owner: `BBM-Co-ORG`
- Repository access: only `Jodd-public`
- Permissions: **Contents → Read and write** (enough for releases + asset upload)
- Expiration: your choice (note the renewal date).

- [ ] **Step 2: Add it as an Actions secret on the private repo**

In `BBM-Co-ORG/Jodd` → Settings → Secrets and variables → Actions → New repository secret:
- Name: `PUBLIC_REPO_TOKEN`
- Value: the PAT from Step 1.

- [ ] **Step 3: Record the renewal**

Add the PAT expiry date to your release docs (or a calendar reminder) — an expired PAT silently fails only the public-sync step (the private release still succeeds).

---

### Task 5: End-to-end verification with a throwaway tag

**Files:** none (verification only)

- [ ] **Step 1: Ensure CHANGELOG has an entry for the test version**

Pick the current app version (`grep version package.json` → e.g. `0.17.1`) and confirm `CHANGELOG.md` has a `## [0.17.1]` section. If testing a fresh version, add its section first.

- [ ] **Step 2: Push a tag and watch the run**

```bash
git tag v0.17.1 2>/dev/null || true   # use the real version
git push origin v0.17.1
gh run watch --repo BBM-Co-ORG/Jodd
```
Expected: `build` matrix succeeds, then `finalize` runs.

- [ ] **Step 3: Verify the private release body**

Run: `gh release view v0.17.1 --repo BBM-Co-ORG/Jodd`
Expected: the body is the CHANGELOG section (not "Release notes are set automatically…").

- [ ] **Step 4: Verify the public release**

Run: `gh release view v0.17.1 --repo BBM-Co-ORG/Jodd-public`
Expected: a release exists with the same notes and the binary assets attached (the `.dmg`/`.app`/`.msi`/`.exe` from the build).

- [ ] **Step 5: Verify the missing-notes guard (optional but recommended)**

Push a tag whose version has NO CHANGELOG section (on a scratch branch). Expected: the `finalize` job fails at "Derive tag + version, build notes file" with `no CHANGELOG section for version …`, and nothing is published to `Jodd-public`.

---

## Self-Review notes (author)

- **Spec coverage:** CHANGELOG → GitHub release body (Task 3 "Set release notes"), releases-only sync to Jodd-public incl. binaries + same notes (Task 3 "Publish…"), PAT requirement (Task 4), fail-loud on missing section (Task 1 exit-1 + Task 3 step that runs it; verified Task 5 step 5), public-sync failure isolated from private release (separate job step; private release + body already done before the public step runs). All Plan-B spec items covered.
- **No source pushed to public:** the finalize job only calls `gh release …` against `Jodd-public` — it never pushes to its git tree. Matches the "releases-only" decision.
- **Type/name consistency:** `changelog-section.mjs` consumes the same `sectionRawText` exported by `scripts/changelog-parse.mjs` (Plan A Task 1). Secret name `PUBLIC_REPO_TOKEN` matches between Task 3 (`env`) and Task 4 (creation). Version-without-`v` (`VERSION="${TAG#v}"`) matches the CHANGELOG header format `## [0.17.1]` from Plan A Task 2.
- **Idempotency:** re-running a tag uses `gh release upload --clobber` + `release edit` if the public release already exists, so a re-run does not error.
