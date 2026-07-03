# Version & Release-Notes Pipeline — Plan A (in-app) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Surface the app version in-app and show curated release notes ("What's New") sourced from a single hand-authored `CHANGELOG.md`, bundled at build time.

**Architecture:** `CHANGELOG.md` (Keep-a-Changelog) is parsed by a pure ESM function (`scripts/changelog-parse.mjs`). A build hook (`scripts/gen-changelog.mjs`, wired to npm `predev`/`prebuild`) writes `src/lib/generated/changelog.json` into the frontend bundle. Two new Svelte modals — `About` (version + metadata) and `WhatsNew` (notes for versions newer than the last-seen one) — are opened from a version label in the Sidebar footer and auto-shown once per version bump (tracked in `localStorage`). Version string comes from Tauri `getVersion()`.

**Tech Stack:** Svelte 5 (runes), TypeScript, Vite 6, `@tauri-apps/api/app`, vitest (new, for the parser).

**Spec:** `docs/superpowers/specs/2026-06-16-version-release-notes-pipeline-design.md`

**Reference patterns to follow:**
- Modal opened via a UI store + `bind:open`: `LessonExtractModal` in `src/App.svelte:811` (`<LessonExtractModal bind:open={$extractModalOpen} />`), store in `src/lib/stores/ui.ts`.
- Modal markup/overlay CSS: `src/lib/components/AccountSettings.svelte` (`.settings-overlay` / `.settings-modal`, `onClose` prop).
- Sidebar footer markup: `src/lib/components/Sidebar.svelte:1195` (`.sidebar-footer`, the account chip).

---

## File Structure

- Create `CHANGELOG.md` (repo root) — single source of truth for notes.
- Create `scripts/changelog-parse.mjs` — pure parser: markdown → structured entries + raw-section extractor. Shared by Plan B.
- Create `scripts/gen-changelog.mjs` — build hook: reads `CHANGELOG.md`, writes `src/lib/generated/changelog.json`.
- Create `scripts/changelog-parse.test.mjs` — vitest unit tests for the parser.
- Create `src/lib/generated/changelog.json` — generated (gitignored). Not hand-edited.
- Create `src/lib/components/About.svelte` — version + metadata modal.
- Create `src/lib/components/WhatsNew.svelte` — release-notes modal.
- Modify `src/lib/stores/ui.ts` — add `aboutModalOpen`, `whatsNewOpen` stores.
- Modify `src/lib/components/Sidebar.svelte` — version label in footer → opens About.
- Modify `src/App.svelte` — render the two modals; auto-show What's New on mount.
- Modify `package.json` — `predev`/`prebuild` hooks, `vitest` dev dep, `test` script.
- Modify `.gitignore` — ignore `src/lib/generated/`.

---

### Task 1: Changelog parser (pure ESM) + tests

**Files:**
- Create: `scripts/changelog-parse.mjs`
- Test: `scripts/changelog-parse.test.mjs`
- Modify: `package.json` (add vitest + test script)

- [ ] **Step 1: Add vitest and a test script to `package.json`**

In `package.json`, add `"test": "vitest run"` to `scripts` and `"vitest": "^2"` to `devDependencies`, then run `npm install`. Result `scripts` block:

```json
  "scripts": {
    "dev": "vite",
    "build": "vite build",
    "preview": "vite preview",
    "tauri": "tauri",
    "test": "vitest run"
  },
```

Run: `npm install`
Expected: vitest added under node_modules, no errors.

- [ ] **Step 2: Write the failing parser test**

Create `scripts/changelog-parse.test.mjs`:

```js
import { describe, it, expect } from 'vitest';
import { parseChangelog, sectionRawText } from './changelog-parse.mjs';

const SAMPLE = `# Changelog

## [Unreleased]
### Added
- work in progress

## [0.17.1] - 2026-06-16
### Added
- Slug links rewrite their displayed text when the target note is renamed.
### Fixed
- Stale link text after rename.

## [0.16.6] - 2026-06-15
### Changed
- Internal cleanup.
`;

describe('parseChangelog', () => {
  it('returns entries newest-first in file order, including Unreleased', () => {
    const entries = parseChangelog(SAMPLE);
    expect(entries.map((e) => e.version)).toEqual(['Unreleased', '0.17.1', '0.16.6']);
  });

  it('parses date and grouped bullet sections', () => {
    const v = parseChangelog(SAMPLE).find((e) => e.version === '0.17.1');
    expect(v.date).toBe('2026-06-16');
    expect(v.sections.Added).toEqual([
      'Slug links rewrite their displayed text when the target note is renamed.',
    ]);
    expect(v.sections.Fixed).toEqual(['Stale link text after rename.']);
  });

  it('Unreleased has a null date', () => {
    const u = parseChangelog(SAMPLE).find((e) => e.version === 'Unreleased');
    expect(u.date).toBeNull();
  });
});

describe('sectionRawText', () => {
  it('returns the markdown body for a version, excluding its header', () => {
    const txt = sectionRawText(SAMPLE, '0.17.1');
    expect(txt).toContain('### Added');
    expect(txt).toContain('- Stale link text after rename.');
    expect(txt).not.toContain('## [0.17.1]');
    expect(txt).not.toContain('## [0.16.6]'); // stops at next version
  });

  it('returns null for a missing version', () => {
    expect(sectionRawText(SAMPLE, '9.9.9')).toBeNull();
  });
});
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `npx vitest run scripts/changelog-parse.test.mjs`
Expected: FAIL — `Failed to resolve import "./changelog-parse.mjs"` / module not found.

- [ ] **Step 4: Write the parser**

Create `scripts/changelog-parse.mjs`:

```js
// Pure Keep-a-Changelog parser. No I/O — used by both the build-time
// generator (gen-changelog.mjs) and the CI section extractor
// (changelog-section.mjs), and unit-tested directly.

const VERSION_HEADER = /^##\s+\[([^\]]+)\](?:\s*-\s*(.+))?\s*$/;
const GROUP_HEADER = /^###\s+(.+?)\s*$/;
const BULLET = /^[-*]\s+(.+?)\s*$/;

/**
 * Parse a CHANGELOG.md string into ordered entries (file order = newest first).
 * @returns {{version: string, date: string|null, sections: Record<string,string[]>}[]}
 */
export function parseChangelog(md) {
  const lines = md.split(/\r?\n/);
  const entries = [];
  let cur = null;
  let group = null;
  for (const line of lines) {
    const vh = line.match(VERSION_HEADER);
    if (vh) {
      cur = { version: vh[1].trim(), date: vh[2] ? vh[2].trim() : null, sections: {} };
      entries.push(cur);
      group = null;
      continue;
    }
    if (!cur) continue;
    const gh = line.match(GROUP_HEADER);
    if (gh) {
      group = gh[1].trim();
      cur.sections[group] ??= [];
      continue;
    }
    const b = line.match(BULLET);
    if (b && group) cur.sections[group].push(b[1].trim());
  }
  return entries;
}

/**
 * Return the raw markdown body for one version (everything after its header
 * line up to the next `## ` header), trimmed. null if the version is absent.
 */
export function sectionRawText(md, version) {
  const lines = md.split(/\r?\n/);
  let start = -1;
  for (let i = 0; i < lines.length; i++) {
    const vh = lines[i].match(VERSION_HEADER);
    if (vh && vh[1].trim() === version) { start = i + 1; break; }
  }
  if (start === -1) return null;
  let end = lines.length;
  for (let i = start; i < lines.length; i++) {
    if (/^##\s+/.test(lines[i])) { end = i; break; }
  }
  return lines.slice(start, end).join('\n').trim() || null;
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `npx vitest run scripts/changelog-parse.test.mjs`
Expected: PASS — 5 tests pass.

- [ ] **Step 6: Commit**

```bash
git add package.json package-lock.json scripts/changelog-parse.mjs scripts/changelog-parse.test.mjs
git commit -m "feat(changelog): pure Keep-a-Changelog parser + vitest"
```

---

### Task 2: Seed `CHANGELOG.md` + build-time generator + gitignore

**Files:**
- Create: `CHANGELOG.md`
- Create: `scripts/gen-changelog.mjs`
- Modify: `.gitignore`
- Modify: `package.json` (predev/prebuild hooks)

- [ ] **Step 1: Create `CHANGELOG.md` (root)**

Seed with the current and recent versions. (Notes are user-facing wording, not commit messages.)

```markdown
# Changelog

All notable changes to Jodd are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/); versions follow the app version.

## [Unreleased]

## [0.17.1] - 2026-06-16
### Added
- Links to a note now update their displayed text automatically when you rename that note.

## [0.16.6] - 2026-06-15
### Changed
- Internal stability improvements.
```

- [ ] **Step 2: Write the generator script**

Create `scripts/gen-changelog.mjs`:

```js
// Build hook: parse CHANGELOG.md → src/lib/generated/changelog.json so the
// frontend can bundle release notes (offline, version-matched). Run via the
// npm predev/prebuild hooks. Never hand-edit the output.
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseChangelog } from './changelog-parse.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const srcPath = resolve(root, 'CHANGELOG.md');
const outPath = resolve(root, 'src/lib/generated/changelog.json');

const md = readFileSync(srcPath, 'utf8');
const entries = parseChangelog(md);
mkdirSync(dirname(outPath), { recursive: true });
writeFileSync(outPath, JSON.stringify(entries, null, 2) + '\n', 'utf8');
console.log(`gen-changelog: wrote ${entries.length} entries → ${outPath}`);
```

- [ ] **Step 3: Wire the generator into npm hooks**

In `package.json` add `predev` and `prebuild` (npm runs `pre<script>` automatically before `dev`/`build`; `tauri.conf.json` `beforeDevCommand`/`beforeBuildCommand` already call `npm run dev`/`npm run build`, so these fire for `npm run tauri dev|build` too):

```json
  "scripts": {
    "predev": "node scripts/gen-changelog.mjs",
    "dev": "vite",
    "prebuild": "node scripts/gen-changelog.mjs",
    "build": "vite build",
    "preview": "vite preview",
    "tauri": "tauri",
    "test": "vitest run"
  },
```

- [ ] **Step 4: Ignore the generated file**

Append to `.gitignore`:

```
# Generated from CHANGELOG.md at build time (do not commit)
src/lib/generated/
```

- [ ] **Step 5: Run the generator and verify output**

Run: `node scripts/gen-changelog.mjs && cat src/lib/generated/changelog.json`
Expected: prints `gen-changelog: wrote 3 entries …` and a JSON array whose first element is `{"version":"Unreleased",...}` and includes `0.17.1` with an `Added` array.

- [ ] **Step 6: Commit**

```bash
git add CHANGELOG.md scripts/gen-changelog.mjs package.json .gitignore
git commit -m "feat(changelog): CHANGELOG.md + build-time generator wired to predev/prebuild"
```

---

### Task 3: UI store toggles for the two modals

**Files:**
- Modify: `src/lib/stores/ui.ts`

- [ ] **Step 1: Add the stores**

Append to `src/lib/stores/ui.ts`:

```ts
// App-level "About" modal — opened by the Sidebar footer version label.
export const aboutModalOpen = writable(false);

// "What's New" modal — opened from About, and auto-shown once per version
// bump by App.svelte (compares getVersion() to a localStorage last-seen value).
export const whatsNewOpen = writable(false);
```

- [ ] **Step 2: Type-check**

Run: `npm run check` (svelte-check) — if no `check` script exists, run `npx svelte-check --tsconfig ./tsconfig.json`
Expected: no new errors referencing `ui.ts`.

- [ ] **Step 3: Commit**

```bash
git add src/lib/stores/ui.ts
git commit -m "feat(ui): aboutModalOpen + whatsNewOpen stores"
```

---

### Task 4: `WhatsNew.svelte` modal

**Files:**
- Create: `src/lib/components/WhatsNew.svelte`

- [ ] **Step 1: Create the component**

Mirrors the `AccountSettings.svelte` overlay/modal structure. Takes `versions` (entries to show) and `open` (bindable). Imports the generated changelog only via the parent (kept prop-driven so it is trivially testable / reusable).

Create `src/lib/components/WhatsNew.svelte`:

```svelte
<script lang="ts">
  // One changelog entry: a version, its date, and grouped bullet lists.
  type Entry = { version: string; date: string | null; sections: Record<string, string[]> };

  export let open = false;
  export let versions: Entry[] = [];

  function close() {
    open = false;
  }
</script>

{#if open}
  <div class="wn-overlay" role="presentation" onclick={close}>
    <div
      class="wn-modal"
      role="dialog"
      aria-modal="true"
      aria-labelledby="wn-title"
      onclick={(e) => e.stopPropagation()}
    >
      <h2 id="wn-title">What's New</h2>
      {#if versions.length === 0}
        <p class="wn-empty">No release notes for this version.</p>
      {:else}
        {#each versions as v (v.version)}
          <section class="wn-version">
            <h3>
              {v.version}{#if v.date}<span class="wn-date"> · {v.date}</span>{/if}
            </h3>
            {#each Object.entries(v.sections) as [group, items] (group)}
              <h4>{group}</h4>
              <ul>
                {#each items as item}<li>{item}</li>{/each}
              </ul>
            {/each}
          </section>
        {/each}
      {/if}
      <div class="wn-actions">
        <button class="wn-close" onclick={close}>Close</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .wn-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.35);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
  }
  .wn-modal {
    background: #fdfcf7;
    border-radius: 10px;
    padding: 20px 24px;
    width: min(520px, 90vw);
    max-height: 80vh;
    overflow-y: auto;
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.25);
  }
  h2 { margin: 0 0 12px; font-size: 18px; }
  .wn-version { margin-bottom: 16px; }
  .wn-version h3 { margin: 0 0 6px; font-size: 15px; }
  .wn-date { color: #8a8madd; font-weight: 400; }
  .wn-version h4 { margin: 8px 0 2px; font-size: 13px; color: #5a5a52; }
  .wn-version ul { margin: 0 0 4px; padding-left: 20px; }
  .wn-version li { font-size: 13px; line-height: 1.5; }
  .wn-empty { color: #777; font-size: 13px; }
  .wn-actions { display: flex; justify-content: flex-end; margin-top: 8px; }
  .wn-close {
    padding: 6px 14px;
    border: 1px solid #cfcabb;
    border-radius: 6px;
    background: #efece2;
    cursor: pointer;
  }
</style>
```

- [ ] **Step 2: Type-check**

Run: `npx svelte-check --tsconfig ./tsconfig.json`
Expected: no errors in `WhatsNew.svelte`.

- [ ] **Step 3: Commit**

```bash
git add src/lib/components/WhatsNew.svelte
git commit -m "feat(ui): WhatsNew modal (prop-driven entries)"
```

---

### Task 5: `About.svelte` modal

**Files:**
- Create: `src/lib/components/About.svelte`

- [ ] **Step 1: Create the component**

Reads the version via `getVersion()` and the bundled changelog; "What's New" button opens the WhatsNew store. Uses `@tauri-apps/plugin-opener` (already a dependency) to open the repo link in the browser.

Create `src/lib/components/About.svelte`:

```svelte
<script lang="ts">
  import { onMount } from 'svelte';
  import { getVersion } from '@tauri-apps/api/app';
  import { open as openUrl } from '@tauri-apps/plugin-opener';
  import { aboutModalOpen, whatsNewOpen } from '../stores/ui';

  const REPO_URL = 'https://github.com/BBM-Co-ORG/Jodd-public';
  let version = '';

  onMount(async () => {
    try {
      version = await getVersion();
    } catch (e) {
      console.error('getVersion failed', e);
      version = 'unknown';
    }
  });

  function close() {
    aboutModalOpen.set(false);
  }
  function showWhatsNew() {
    aboutModalOpen.set(false);
    whatsNewOpen.set(true);
  }
</script>

{#if $aboutModalOpen}
  <div class="about-overlay" role="presentation" onclick={close}>
    <div
      class="about-modal"
      role="dialog"
      aria-modal="true"
      aria-labelledby="about-title"
      onclick={(e) => e.stopPropagation()}
    >
      <h2 id="about-title">Jodd</h2>
      <div class="about-version">Version {version}</div>
      <p class="about-desc">Apple Notes, anywhere — by BBM Media.</p>
      <div class="about-actions">
        <button class="about-link" onclick={showWhatsNew}>What's New</button>
        <button class="about-link" onclick={() => openUrl(REPO_URL)}>Project page</button>
        <button class="about-close" onclick={close}>Close</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .about-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.35);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
  }
  .about-modal {
    background: #fdfcf7;
    border-radius: 10px;
    padding: 20px 24px;
    width: min(380px, 90vw);
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.25);
    text-align: center;
  }
  h2 { margin: 0 0 4px; font-size: 20px; }
  .about-version { color: #5a5a52; font-size: 13px; margin-bottom: 8px; }
  .about-desc { font-size: 13px; color: #444; margin: 0 0 16px; }
  .about-actions { display: flex; gap: 8px; justify-content: center; flex-wrap: wrap; }
  .about-link, .about-close {
    padding: 6px 14px;
    border: 1px solid #cfcabb;
    border-radius: 6px;
    background: #efece2;
    cursor: pointer;
    font-size: 13px;
  }
</style>
```

- [ ] **Step 2: Type-check**

Run: `npx svelte-check --tsconfig ./tsconfig.json`
Expected: no errors in `About.svelte`.

- [ ] **Step 3: Commit**

```bash
git add src/lib/components/About.svelte
git commit -m "feat(ui): About modal (version via getVersion + What's New entry)"
```

---

### Task 6: Sidebar version label (entry point)

**Files:**
- Modify: `src/lib/components/Sidebar.svelte`

- [ ] **Step 1: Import version + store at the top of the `<script>`**

Add near the other imports in `src/lib/components/Sidebar.svelte` (the file already imports from `../stores/...`):

```ts
import { onMount } from 'svelte';
import { getVersion } from '@tauri-apps/api/app';
import { aboutModalOpen } from '../stores/ui';

let appVersion = '';
onMount(async () => {
  try { appVersion = await getVersion(); } catch { appVersion = ''; }
});
```

(If `onMount` is already imported in this file, do not duplicate the import — reuse it.)

- [ ] **Step 2: Add the version label inside `.sidebar-footer`**

In the `.sidebar-footer` block (starts at `src/lib/components/Sidebar.svelte:1195`), add a button after the account chip / panel, still inside the footer `<div>`:

```svelte
    <button
      class="version-label"
      onclick={() => aboutModalOpen.set(true)}
      title="About Jodd"
    >
      Jodd{appVersion ? ` v${appVersion}` : ''}
    </button>
```

- [ ] **Step 3: Add styling for `.version-label` in the Sidebar `<style>`**

```css
  .version-label {
    display: block;
    width: 100%;
    text-align: center;
    padding: 4px 0;
    margin-top: 2px;
    border: none;
    background: none;
    color: #9a978c;
    font-size: 11px;
    cursor: pointer;
  }
  .version-label:hover { color: #5a5a52; text-decoration: underline; }
```

- [ ] **Step 4: Type-check**

Run: `npx svelte-check --tsconfig ./tsconfig.json`
Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add src/lib/components/Sidebar.svelte
git commit -m "feat(ui): Sidebar footer version label opens About"
```

---

### Task 7: Render modals + auto-show What's New in `App.svelte`

**Files:**
- Modify: `src/App.svelte`

- [ ] **Step 1: Import the components, store, version, and changelog**

Add to the imports in `src/App.svelte` (it already imports `onMount` and `getVersion` is from a new module):

```ts
import About from './lib/components/About.svelte';
import WhatsNew from './lib/components/WhatsNew.svelte';
import { aboutModalOpen, whatsNewOpen, extractModalOpen } from './lib/stores/ui';
import { getVersion } from '@tauri-apps/api/app';
import changelog from './lib/generated/changelog.json';

type Entry = { version: string; date: string | null; sections: Record<string, string[]> };
const LAST_SEEN_KEY = 'jodd:lastSeenVersion';
let whatsNewVersions: Entry[] = [];
```

(The file currently imports `extractModalOpen` from `./lib/stores/ui` on line 13 — merge the new names into that existing import line instead of adding a duplicate import.)

- [ ] **Step 2: Add the auto-show logic inside the existing `onMount`**

In `src/App.svelte` the `onMount` starts at line 382. Add this block near the start of the `onMount` callback body (it does not depend on auth/account loading):

```ts
    // What's New: show release notes once per version bump. Compare the running
    // app version to the last value we recorded in localStorage; surface every
    // changelog entry strictly newer than it (so a user who jumped several
    // versions sees them all), then record the current version.
    try {
      const current = await getVersion();
      const lastSeen = localStorage.getItem(LAST_SEEN_KEY);
      const all = (changelog as Entry[]).filter((e) => e.version !== 'Unreleased');
      if (current !== lastSeen) {
        whatsNewVersions = lastSeen
          ? all.filter((e) => cmpVersion(e.version, lastSeen) > 0)
          : all.filter((e) => e.version === current);
        if (whatsNewVersions.length > 0) whatsNewOpen.set(true);
        localStorage.setItem(LAST_SEEN_KEY, current);
      }
    } catch (e) {
      console.error('whats-new check failed', e);
    }
```

- [ ] **Step 3: Add a semver compare helper in the `<script>` (top-level, not inside onMount)**

```ts
  // Compare two dotted version strings numerically. >0 if a is newer than b.
  function cmpVersion(a: string, b: string): number {
    const pa = a.split('.').map((n) => parseInt(n, 10) || 0);
    const pb = b.split('.').map((n) => parseInt(n, 10) || 0);
    for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
      const d = (pa[i] ?? 0) - (pb[i] ?? 0);
      if (d !== 0) return d;
    }
    return 0;
  }
```

- [ ] **Step 4: Render the modals in the markup**

Next to the existing `<LessonExtractModal bind:open={$extractModalOpen} />` (line 811), add:

```svelte
<About />
<WhatsNew bind:open={$whatsNewOpen} versions={whatsNewVersions} />
```

- [ ] **Step 5: Generate the changelog so the import resolves, then type-check + build**

Run: `node scripts/gen-changelog.mjs && npx svelte-check --tsconfig ./tsconfig.json && npm run build`
Expected: changelog json written; svelte-check passes; `vite build` succeeds (the `import changelog from './lib/generated/changelog.json'` resolves).

- [ ] **Step 6: Commit**

```bash
git add src/App.svelte
git commit -m "feat(ui): render About/WhatsNew + auto-show What's New on version bump"
```

---

### Task 8: Manual verification in the running app

**Files:** none (verification only)

- [ ] **Step 1: Build and launch**

Run: `npm run tauri build -- --bundles app` then launch the bundle from a non-`/Applications` path (per the macOS startup note in the spec — e.g. `open src-tauri/target/release/bundle/macos/Jodd.app`).

- [ ] **Step 2: Verify About**

Click the `Jodd v0.17.1` label at the bottom of the sidebar. Expected: About modal opens showing "Version 0.17.1". "Project page" opens the Jodd-public GitHub page. "What's New" switches to the WhatsNew modal listing the 0.17.1 note.

- [ ] **Step 3: Verify What's New auto-show**

In the running app, open devtools console and run `localStorage.removeItem('jodd:lastSeenVersion')`, then relaunch the app. Expected: What's New auto-opens once showing 0.17.1; closing it and relaunching does NOT re-open it (localStorage now records 0.17.1).

- [ ] **Step 4: Verify graceful empty state**

Temporarily set `localStorage.setItem('jodd:lastSeenVersion','0.17.1')` and relaunch. Expected: no auto-popup (already seen). Open About → What's New manually still shows the 0.17.1 entry.

---

## Self-Review notes (author)

- **Spec coverage:** CHANGELOG source (Task 2), bundled-at-build delivery (Task 2 generator + Task 7 import), version via getVersion (Tasks 5/6), About entry point in Sidebar footer (Task 6), What's New auto-show once per bump with multi-version jump (Task 7), graceful empty state (Task 4 markup + Task 8 step 4). Parser tested (Task 1). All Plan-A spec items covered.
- **Out of scope here (Plan B):** CI release-body extraction + Jodd-public sync — see the Plan B file. Plan B depends only on `CHANGELOG.md` + `scripts/changelog-parse.mjs` existing (delivered in Tasks 1–2).
- **Type consistency:** the `Entry` shape (`{version, date, sections}`) is identical in the parser test (Task 1), WhatsNew props (Task 4), and App.svelte (Task 7). The store names `aboutModalOpen`/`whatsNewOpen` match across Tasks 3, 5, 6, 7. `LAST_SEEN_KEY = 'jodd:lastSeenVersion'` matches between Task 7 code and Task 8 verification.
