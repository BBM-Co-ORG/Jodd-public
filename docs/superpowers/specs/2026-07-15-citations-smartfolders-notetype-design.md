# Structured citations + Smart Folders (Orphaned/Stale) + auto note type — design

> Status: **design / approved** (2026-07-15). Bundles three items from
> `docs/LLM-WIKI-GRAPHIFY-ROADMAP.md` (items #1, #2, #3) into one spec since they
> all touch the main app's Rust/Svelte code directly and were locked together in
> the roadmap's "ship as a bundle" recommendation. Item #4 (MCP server) is
> deliberately **out of scope** for this spec — it's a separate Cargo workspace
> member with no shared code path, tracked as its own spec/plan cycle.

## Problem

Three gaps identified against the LLM Wiki / Graphify / OKF patterns (full
background in the roadmap doc):

1. **No structured citations.** Extract's source block is an opaque
   `<details><pre>` dump — any URL in a pasted source is buried, unclickable,
   and invisible to search or dedup checks.
2. **No hygiene view.** Notes with zero backlinks or untouched for a long time
   are invisible — nothing surfaces them.
3. **No note classification distinct from tags.** Extract's LLM already
   implicitly classifies its source ("Debugging session", "Meeting transcript",
   etc. — `lessons/prompt.rs:33-35`) to shape `lessons_markdown`, but that
   classification is discarded once it's done its job.

## Decisions (locked in brainstorming)

1. **Citations are a `cites` edge, not a new table.** `(account_id, src_uuid,
   dst_id=<url>, dst_title='', rel='cites')` in the existing `edges` table.
   `dst_id`/`dst_title` are already `NOT NULL DEFAULT ''` (migration #12) —
   **zero schema migration** needed, just a new `rel` value.
2. **Citations apply to every note, not just Extract output.** Derived in the
   same reconciliation pass that already derives `mentions`/`tagged` on every
   write (`reconcile_edges_from_body_conn`, called from `apply_local_edit` /
   `insert_local_new`), not scoped to Extract's source-text functions.
3. **URL matching is exact-after-light-normalization**, not full
   canonicalization: strip a trailing `/` and any `#fragment`. No `utm_`-style
   query-param stripping. Explicit YAGNI call — revisit only if false-negatives
   prove annoying in real use.
4. **Smart Folders (Orphaned/Stale) are fully virtual — no `folders`-table
   rows.** `folders.kind='smart_query'` is reserved for this in `db.rs:583-585`,
   but real rows in the same table as syncable folders risk the sync
   worker/prune pass/context-menu mishandling them (this codebase's own
   documented defect class: D1/D8/D11 in CLAUDE.md). Two new read-only
   queries instead; no new table rows.
5. **Two fixed folders, hardcoded 30-day staleness threshold, no config UI.**
   Matches this item's own "ship fast" rationale — add a settings knob later
   only if 30 days ever proves wrong in practice.
6. **Per-account, not cross-account.** `edges`/backlinks never cross accounts
   (UUIDs are namespaced by `account_id`), so this is a presentation choice.
   Per-account reuses existing patterns; a combined cross-account view (like
   Tags' scope selector) is a bigger, separate investment for later.
7. **Sidebar: new "SMART FOLDERS" group, visually parallel to "WORKFLOWS" but
   not built on the same mechanism.** Workflows entries are real `folders`
   rows filtered by `kind='system_workflow'` (`splitRowsByAccount` /
   `isWorkflowPath` in `Sidebar.svelte`). Smart Folders are two static,
   always-rendered buttons per account with no backing table row — do not
   route them through the Workflows-splitting logic. **Placement:** per
   account, right after the conditional Workflows `folder-group` block and
   before "Recently Deleted" (`Sidebar.svelte:1147-1171`) — unlike Workflows,
   this group is unconditional (always rendered, not gated on data existing).
8. **Clicking a Smart Folder reuses `NoteList.svelte` with one new branch**,
   not a parallel component. A new `selectedSmartFolder` store; when set,
   `NoteList` fetches via `list_orphaned_notes`/`list_stale_notes` instead of
   `list_cached_notes_in_folder` — search, multi-select, and the context menu
   all keep working unmodified since the result is still plain `Note[]`.
9. **Note type is narrowly scoped to Extract's auto-derived classification**,
   not a general note-typing system. `ExtractEnvelope` gains
   `source_type: Option<String>`; `extract_lessons`/`append_extract_lessons`
   write it into a new `notes.note_type` column at write time. Manually-created
   or synced notes stay `NULL` unless the user sets it by hand — this spec does
   **not** build automatic classification for non-Extract notes.
10. **Note type shown/edited via a badge in the editor's context bar**
    (`NoteEditor.svelte`, next to the existing folder/slug buttons around line
    1868), not a new settings surface.

## Approach

### 1. Structured citations

**Backend — `src-tauri/src/db.rs`:**

```rust
/// Extract distinct http(s) URLs from a note body (HTML or raw pasted text),
/// normalized (trailing slash and #fragment stripped) and deduplicated.
/// Pure function — no DB access — so it's unit-testable in isolation.
fn extract_urls(body: &str) -> Vec<String>
```

Hook into `reconcile_edges_from_body_conn` (db.rs:2899) alongside its existing
`mentions`/`child_of`/`tagged` derivation: for the note being reconciled, compute
`extract_urls(body_html)`, diff against the note's current `rel='cites'` edges
(`DELETE` ones no longer present, `INSERT OR IGNORE` new ones) — same
diff-and-upsert shape the function already uses for `mentions`.

```rust
/// A note's outgoing citations — URLs found in its body, not other notes.
/// Distinct return shape from backlinks/outgoing_links (Vec<CachedNote>)
/// because a citation target is an external URL with no corresponding note row.
pub fn note_citations(&self, account_id: &str, uuid: &str) -> SqlResult<Vec<String>>

/// Notes (if any) that already cite this exact URL — used for the
/// "you already have a note citing this" dedup check before an Extract/append
/// submit. Matches the same normalization extract_urls applies.
pub fn notes_citing_url(&self, account_id: &str, url: &str) -> SqlResult<Vec<CachedNote>>
```

New Tauri commands: `note_citations(account_id, uuid)` (called by
`NoteEditor.svelte` alongside the existing `note_connections` call) and
`check_citation_dedup(account_id, urls: Vec<String>)` wrapping
`notes_citing_url` for each URL found in a pasted source (called from
`LessonExtractModal.svelte` before submit, surfaced as a non-blocking inline
notice — never blocks the Extract, since a repeat citation may be intentional).

**Frontend — `NoteEditor.svelte`:** extend the existing `connections` state
shape to also hold `citations: string[]`, fetched in the same
`refreshConnections()` call. New "📎 Sources" section in the Connections panel
markup, alongside the existing outgoing/backlinks sections (`connections.svelte`
markup around line 1954) — same list-with-icon visual pattern, each entry a
plain external link (opens via the OS default handler, not in-app navigation).

### 2. Smart Folders (Orphaned/Stale)

**Backend — `src-tauri/src/db.rs`:**

```rust
/// Notes with zero incoming 'mentions' edges (no backlinks). Pure SQL,
/// no LLM. Excludes deleted_pending, same filter every other list path uses.
pub fn list_orphaned_notes(&self, account_id: &str) -> SqlResult<Vec<CachedNote>>

/// Notes not modified in the last STALE_DAYS days (hardcoded constant).
pub fn list_stale_notes(&self, account_id: &str) -> SqlResult<Vec<CachedNote>>

const STALE_DAYS: i64 = 30;
```

New Tauri commands `list_orphaned_notes(account_id)` /
`list_stale_notes(account_id)`, thin wrappers mapping to frontend `Note[]` the
same way every existing list command does.

**Frontend — `Sidebar.svelte`:** new `selectedSmartFolder` store
(`{account_id, kind: 'orphaned' | 'stale'} | null`) in `stores/notes.ts`,
sibling to `selectedFolder`. Render a new, unconditional "SMART FOLDERS"
`folder-group` per account — same `group-header` markup pattern as the
existing Workflows group (`Sidebar.svelte:1149`) — positioned right after the
Workflows block and before "Recently Deleted" (between lines 1154 and 1156
today). Two static buttons ("🔗‍💥 Orphaned", "🕰 Stale"), no folder-row
derivation, each setting `selectedSmartFolder` and clearing
`selectedFolder`/`searchQuery` on click (mirroring the existing folder-click
handler's mutual-exclusivity with search mode).

**Frontend — `NoteList.svelte`:** one new reactive branch: when
`$selectedSmartFolder` is set, fetch via `list_orphaned_notes`/
`list_stale_notes` instead of `list_cached_notes_in_folder`; render into the
same list — multi-select, context menu (move/delete), and per-note click-to-open
all keep working since the result is still `Note[]`. Clicking a real folder or
starting a search clears `selectedSmartFolder` (same mutual-exclusivity
`searchQuery` already has with `selectedFolder`).

### 3. Auto-derived note type

**Backend:**

- `src-tauri/src/lessons/provider.rs`: add `source_type: Option<String>` to
  `ExtractEnvelope`.
- `src-tauri/src/lessons/prompt.rs`: one addition to the JSON shape description
  telling the LLM to also emit the classification it already reasons about
  (e.g. `"source_type": "meeting" | "debugging" | "article" | "conversation" |
  "tutorial" | null`) — reusing the exact categories already implied by the
  existing "shape of the points adapts to the source" section (lines 33-39).
- New migration #15: `ALTER TABLE notes ADD COLUMN note_type TEXT;` (highest
  existing migration is #14; nullable, no
  default — absence means "unclassified", rendered as no badge).
- `extract_lessons`/`append_extract_lessons` (lib.rs) write
  `envelope.source_type` into the new column on insert/append.
- New Tauri command `set_note_type(account_id, uuid, note_type: Option<String>)`
  for the manual-override path — direct column write, no body/edges
  re-derivation needed since this isn't body-derived data.

**Frontend — `NoteEditor.svelte`:** small badge button next to the existing
`.ctx-slug` button (context bar, ~line 1868) — e.g. "📝 Meeting ▾" if
`note_type` is set, a plain "+ Type" affordance if not. Click opens a small
dropdown (the fixed category list above, plus a free-text option) that calls
`set_note_type`.

## Error handling

- **URL extraction finds nothing:** normal case, not an error — most notes
  (debugging sessions, meeting transcripts, day-to-day writing) have no URLs.
  Sources section simply doesn't render (same pattern as the existing
  Connections panel hiding when there's nothing to show).
- **Citation dedup check fails (DB error):** non-fatal — the Extract/append
  submit proceeds without the dedup notice rather than blocking on it; log and
  swallow, matching the existing `console.warn` pattern used for
  `post-extract cache paint failed` and similar non-critical post-save steps.
- **`list_orphaned_notes`/`list_stale_notes` return empty:** normal case (no
  orphans/stale notes) — Smart Folder shows the existing NoteList empty state
  ("No notes in this folder" equivalent), not an error.
- **`set_note_type` fails:** surfaced the same way other inline editor writes
  report failure today (no new error UI pattern needed).

## Edge cases

- **Citations + Phase 1's append mode:** when appending a new source into an
  existing note, `reconcile_edges_from_body_conn` re-scans the *entire* new
  body (existing content + appended content) for URLs — new `cites` edges get
  added for URLs in the newly-appended text; existing `cites` edges for URLs
  still present in the old content are untouched (same diff-and-upsert
  behavior as `mentions` already has today, no special-casing needed for
  append specifically).
- **A note with `sync_state='deleted_pending'` appearing in Orphaned/Stale:**
  excluded by the same `sync_state != 'deleted_pending'` filter every other
  list query already applies (D8 doctrine — Gmail's eventual consistency must
  never resurrect a just-deleted note as a UI ghost).
- **Two accounts, same URL cited in each:** citations are `account_id`-scoped
  in the `edges` table exactly like `mentions` — no cross-account dedup
  detection, matching decision 6 (per-account, not cross-account) applied
  consistently across all three items.
- **`note_type` on a note synced in from Apple Notes/Gmail with no Extract
  involved:** stays `NULL` forever unless the user manually sets it — this
  spec does not attempt to backfill/guess types for non-Extract notes.

## Testing

- **Unit** (`db.rs`, in-memory `Db`, mirroring the Phase 1 test style):
  - `extract_urls`: finds multiple URLs, dedupes repeats, normalizes trailing
    slash and `#fragment`, returns empty for URL-less text.
  - `reconcile_edges_from_body_conn` citations path: a note with 2 URLs in its
    body gets 2 `cites` edges after reconciliation; editing the body to remove
    one URL and add a different one leaves exactly the new set (diff-and-upsert,
    not additive-only).
  - `list_orphaned_notes`: a note with an inbound `mentions` edge is excluded; a
    note with none is included; `deleted_pending` notes never appear.
  - `list_stale_notes`: a note older than 30 days appears; one modified today
    doesn't.
  - `notes_citing_url`: exact match after normalization finds a prior citer;
    an unrelated URL finds nothing.
- **Live verification** (per CLAUDE.md convention, scoped to a disposable test
  folder):
  - Paste a source containing a URL via Extract → confirm the Sources section
    appears in the Connections panel with a clickable link.
  - Paste the same URL again in a second Extract → confirm the dedup notice
    appears before submit.
  - Open Orphaned/Stale in a test account with a mix of linked/orphaned and
    recent/stale notes → confirm the right notes appear in each, and that
    context-menu move/delete work identically to a real folder's note list.
  - Run an Extract → confirm the context-bar badge shows the LLM's classified
    type; click it and manually override → confirm it persists.

## Scope / files

- `src-tauri/src/db.rs` — `extract_urls`, citations derivation inside
  `reconcile_edges_from_body_conn`, `note_citations`, `notes_citing_url`,
  `list_orphaned_notes`, `list_stale_notes`, new migration for `notes.note_type`,
  tests.
- `src-tauri/src/lessons/provider.rs` — `ExtractEnvelope.source_type`.
- `src-tauri/src/lessons/prompt.rs` — prompt addition for `source_type`.
- `src-tauri/src/lib.rs` — `extract_lessons`/`append_extract_lessons` write
  `note_type`; new commands `note_citations`, `check_citation_dedup`,
  `list_orphaned_notes`, `list_stale_notes`, `set_note_type`.
- `src/lib/stores/notes.ts` — new `selectedSmartFolder` store.
- `src/lib/components/Sidebar.svelte` — "SMART FOLDERS" group (two static
  buttons per account).
- `src/lib/components/NoteList.svelte` — `selectedSmartFolder` fetch branch.
- `src/lib/components/NoteEditor.svelte` — Sources section in Connections
  panel; note-type badge in the context bar.
- `src/lib/components/LessonExtractModal.svelte` — dedup notice before submit.

## Deferred (not built)

- **Item #4 (MCP server)** — separate spec/plan cycle entirely, no shared code
  with this spec.
- **URL canonicalization beyond trailing-slash/fragment stripping** (decision 3)
  — revisit only if real use shows false-negatives.
- **Configurable staleness threshold** (decision 5) — hardcoded 30 days for now.
- **Cross-account Smart Folders / citation dedup** (decision 6) — per-account
  only in this spec.
- **Automatic `note_type` classification for non-Extract notes** (decision 9) —
  out of scope; those notes stay unclassified unless set by hand.
