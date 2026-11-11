# Version & Release-Notes Pipeline — design

> Status: **design / approved** (2026-06-16). Gives Jodd a single source of truth
> for release notes (`CHANGELOG.md`) that flows automatically to three places:
> the in-app **About / What's New** UI, the **GitHub Release** body on the private
> `Jodd` repo, and the **public download repo** `Jodd-public`. Closes the gap where
> the app shows no version anywhere and the public repo's releases drifted 3
> versions behind dev (stuck at v0.14.5 while dev is at v0.17.1).

## Problem

1. **In-app:** Jodd never surfaces its own version. There is no app-level About /
   Settings menu (only the per-account ⚙ `AccountSettings` and `LlmProviderSettings`
   modals). `getVersion()` is never called. A user cannot tell which version they run
   or what changed.
2. **Release notes:** there is no `CHANGELOG.md`. `release.yml` hardcodes the release
   body to the literal string `"See the changelog for details."` — pointing at a file
   that does not exist.
3. **Public distribution drift:** releases are cut as **drafts** on the private
   `BBM-Co-ORG/Jodd` repo (latest v0.17.1 draft), but the public download repo
   `BBM-Co-ORG/Jodd-public` is updated by hand and has fallen behind — its latest
   release is **v0.14.5** (2026-06-10). Users downloading from the public repo get a
   stale build.

These are three links of one missing chain: **author notes once → propagate to GitHub
release + public repo + in-app**.

## Decisions (locked in brainstorming)

1. **Source of truth = hand-authored `CHANGELOG.md`** (Keep-a-Changelog format), CI
   propagates downstream. User-facing wording stays human-curated (friendly, not raw
   commit messages); everything *downstream* of the file is automated.
2. **In-app delivery = bundled at build.** The app reads release notes from a
   CHANGELOG parsed into the bundle at build time — works offline, always matches the
   running version, no GitHub-API coupling. Version comes from Tauri `getVersion()`.
3. **What's New auto-shows once per version bump.** Compares `getVersion()` to a
   `lastSeenVersion` in `localStorage`; shows once when they differ, then records the
   current version. Re-openable from About.
4. **Public sync = releases-only.** CI copies the built **binaries + the same release
   notes** to a GitHub Release on `Jodd-public`. It does **not** push source — the
   public repo's source tree is left untouched, so no private material leaks and the
   sync stays cheap.

## Architecture — data-flow spine

```
CHANGELOG.md  (root, hand-authored, Keep-a-Changelog)
   │
   ├─ [build time] scripts/gen-changelog.mjs
   │        parses → src/lib/generated/changelog.json (gitignored)
   │        → About modal (version + metadata) + What's New modal
   │
   └─ [git tag v* push] release.yml
            scripts/changelog-section.mjs <version> → notes text
              ├─▶ GitHub Release body on BBM-Co-ORG/Jodd (private)
              └─▶ gh release create/upload on BBM-Co-ORG/Jodd-public
                     (binaries + same notes; needs PUBLIC_REPO_TOKEN PAT)
```

One input file; three derived outputs. Each output is a pure function of the file, so
a single parser test covers correctness for all three paths.

## Unit 1 — `CHANGELOG.md` (repo root)

Keep-a-Changelog structure:

```markdown
# Changelog

## [Unreleased]
### Added
- ...

## [0.17.1] - 2026-06-16
### Added
- Slug links now rewrite their displayed text when the target note is renamed.
### Fixed
- ...
```

- One `## [Unreleased]` section at the top, filled as work lands.
- On release, the human (or Claude, drafting from commits for the user to review) moves
  `Unreleased` → a dated `## [x.y.z] - YYYY-MM-DD` section. Wording is end-user-facing,
  not commit-speak.
- This file is the **only** place release notes are written.

## Unit 2 — In-app About / What's New (frontend, Svelte 5)

### Entry point
A `Jodd vX.Y.Z` label pinned at the **bottom of `Sidebar.svelte`** — doubles as an
always-visible version indicator and the click target to open About. (Fills the absent
app-level menu; per-account settings stay where they are.)

### `About.svelte` (new modal, mirrors `AccountSettings.svelte` pattern)
- App version from `@tauri-apps/api/app` `getVersion()`.
- App name, developer (BBM Media), license, repo link (`Jodd-public`).
- "What's New" button → opens the What's New modal for the current version.

### `WhatsNew.svelte` (new modal)
- Renders the parsed CHANGELOG entry for the running version (Added / Fixed / Changed
  groups). If multiple versions are newer than `lastSeenVersion`, shows all of them
  newest-first (covers a user jumping several versions, e.g. public v0.14.5 → v0.17.1).
- **Auto-show logic** (in `App.svelte` on mount): read `getVersion()`; read
  `lastSeenVersion` from `localStorage`; if different and a changelog entry exists,
  open WhatsNew once, then set `lastSeenVersion = current`. Dismiss = no re-show until
  next bump.

### Bundling mechanism
- `scripts/gen-changelog.mjs` parses `CHANGELOG.md` → `src/lib/generated/changelog.json`
  — an array of `{ version, date, sections: { Added: [...], Fixed: [...], ... } }`.
- Output path is **gitignored**; the script runs in the `predev` and `prebuild` npm
  hooks so the bundle is always fresh and the app never imports a stale file.
- `About`/`WhatsNew` import the generated JSON (static import → Vite bundles it).

## Unit 3 — CI automation (`.github/workflows/release.yml`)

### Replace the hardcoded body
- New `scripts/changelog-section.mjs <version>` prints the CHANGELOG section for a
  version (text between its `## [x.y.z]` header and the next `## [`).
- A release-step reads that into the GitHub release body (`tauri-action`'s
  `releaseBody`, or `gh release edit --notes-file`). **Fail the job** if the section is
  missing — never publish a release with no notes.

### Public sync (releases-only)
- After the build + the private release exist, a new job/step runs:
  `gh release create <tag> <artifact-paths> --repo BBM-Co-ORG/Jodd-public
   --title "Jodd <tag>" --notes-file <section.md>`
  (or `gh release upload` if the release already exists).
- Auth: the default `GITHUB_TOKEN` cannot write to a *different* repo, so this needs a
  **PAT secret `PUBLIC_REPO_TOKEN`** (scoped to `Jodd-public`, `contents:write`),
  consumed as `GH_TOKEN`. Setting the secret is a one-time manual step (documented in
  the plan).
- This step touches only Releases on `Jodd-public` — never its source tree.

## Error handling

- **CHANGELOG section missing for a tag** → CI fails loudly (guard against a
  note-less release).
- **App can't parse / find a changelog entry** → About still shows the version;
  WhatsNew shows a graceful "No release notes for this version." — never crashes.
- **Public-sync step fails** (PAT expired, network) → it is a **separate step**, so the
  private release still succeeds; the public copy can be retried manually. The job logs
  the failure clearly rather than silently skipping.

## Testing

- **Unit (parser):** the CHANGELOG parser is a pure function. Add a minimal **vitest**
  case (`changelog.parse.test.ts`) covering: a normal versioned section, the
  `Unreleased` section, multiple sections, and a missing-version lookup. This also
  bootstraps the project's currently-absent frontend test setup (kept scoped to the
  parser — no broader test sprawl).
- **CI-section script:** a sample-CHANGELOG assertion that `changelog-section.mjs 0.17.1`
  emits exactly that section.
- **Manual:** push a throwaway tag → verify (a) private release body, (b) `Jodd-public`
  release created with binaries + notes, (c) open the app → About shows the version,
  WhatsNew auto-shows once.

## Scope / files

- `CHANGELOG.md` (new, root)
- `scripts/gen-changelog.mjs` (new — build-time parse → JSON)
- `scripts/changelog-section.mjs` (new — CI section extractor)
- `src/lib/generated/changelog.json` (generated, gitignored)
- `src/lib/components/About.svelte`, `src/lib/components/WhatsNew.svelte` (new)
- `src/lib/components/Sidebar.svelte` (add version label + entry point)
- `src/App.svelte` (What's New auto-show on mount)
- `.github/workflows/release.yml` (section-extract body + public-sync step)
- `package.json` (predev/prebuild hooks; vitest dev dep + test script)
- `.gitignore` (ignore the generated changelog json)

## Implementation split (two plans)

- **Plan A — in-app:** `CHANGELOG.md` + `gen-changelog.mjs` + parser/test + `About` +
  `WhatsNew` + Sidebar version label + auto-show. Ships visible value immediately,
  independent of CI.
- **Plan B — automation:** `changelog-section.mjs` + `release.yml` body extraction +
  `Jodd-public` releases-only sync + `PUBLIC_REPO_TOKEN`. Depends only on `CHANGELOG.md`
  existing (from Plan A).

## Deferred / explicitly out of scope

- **macOS intermittent "process starts, window never appears."** Observed today on
  Tahoe but the user reports it is **intermittent** ("happens sometimes, not always"),
  **new** (never seen across a month+ of Jodd dev), and **not unique to Jodd** (a
  separate Tauri project never hit it). That profile points to a non-deterministic
  OS-level launch/AMFI/WebKit-spawn gremlin, **not** a deterministic `/Applications` +
  ad-hoc rule — so code signing / notarization is **not a guaranteed fix**. The
  realistic lever is *observability* (startup logging to a file + a watchdog/relaunch),
  not prevention. Tracked as a separate investigation; deliberately kept out of this
  spec so it does not stall the version/release pipeline.
- **Auto-updater** (`tauri-plugin-updater`): the natural future pairing with What's New
  (show notes on "update available"), but a separate feature.
- **BYO-credentials UI** and **Google OAuth verification / signing**: separate
  release-readiness tracks discussed elsewhere.
