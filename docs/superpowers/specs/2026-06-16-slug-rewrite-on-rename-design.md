# Slug links: rewrite-on-rename — design

> Status: **design / approved** (2026-06-16). Fixes the stale-link-text behavior
> of `[[<title-slug>-<uuid8>]]` wikilinks: when a note is renamed, the *displayed*
> link text in other notes stays frozen at the old title (resolution stays correct
> via `uuid8`, but the text the user reads is stale). This is the existing
> wikilink/edges feature — **independent of the LocalFS / Vertical work**.

## Problem

A slug link is `[[<title-slug>-<uuid8>]]` where `title-slug = slugify(title)` and
`uuid8 = uuid_short(uuid)` (first 8 hex of the note's UUID, stable forever). Links
**resolve by `uuid8`** (the `edges.dst_id` column), so renaming a note never breaks
navigation. But the slug text is stored verbatim in the carrying note's body and is
**never updated on rename** — so after renaming note B, every note that links to B
still *displays* `[[old-title-<uuid8>]]`. Confusing to read; the link text no longer
matches B's current title.

Confirmed in code: `db::note_slug` / `db::uuid_short`; migration #12 (`edges.dst_id`
= uuid8, slug links resolve by it); there is a `rewrite_tag_in_bodies` for tag
rename but **no equivalent for link/slug rename**.

## Decisions (locked in brainstorming)

1. **Scope: slug-form `[[*-uuid8]]` only.** Rewrite only links that carry the
   target's `uuid8` (unambiguous — they point at exactly one note). Plain
   `[[Title]]` links (no uuid8, `edges.dst_id` empty) are **left untouched** — they
   are title-keyed and ambiguous (multiple notes can share a title); updating them
   is a separate, riskier concern out of scope here.
2. **Trigger: on save when the slug changes.** Hook the note-save path; when a
   note's `slugify(new_title) != slugify(old_title)` (the title-slug prefix actually
   changed), rewrite every body that links to it. Mirrors Obsidian's
   "update links on rename". Autosave is already debounced, so this fires a bounded
   number of times, not per keystroke.
3. **Backend-agnostic.** The rewrite operates on the SQLite cache (body_html +
   re-derivation) and flips carriers `clean → dirty`; the existing sync worker then
   pushes each carrier through its vertical (Gmail or LocalFS). No vertical-specific
   code.

## Approach (mirror `rewrite_tag_in_bodies` exactly)

### 1. Pure body rewrite — `rewrite_wikilink_slug_in_body`

New free fn in `db.rs` (sibling of `rewrite_hashtag_in_body`):

```rust
/// Rewrite every slug-form wikilink targeting `uuid8` to use `new_slug`.
/// Finds `[[ ... -<uuid8>]]` occurrences (the uuid8 = the 8 hex chars
/// immediately before `]]`, after the final `-`) and replaces the WHOLE inner
/// slug with `new_slug`. Leaves plain `[[Title]]` (no `-<uuid8>` suffix) and
/// links to other uuids untouched. Returns the rewritten body (== input if no
/// change). HTML-aware to the same degree as rewrite_hashtag_in_body (operates
/// on the text; `[[..]]` never appears inside tags in practice).
fn rewrite_wikilink_slug_in_body(body_html: &str, uuid8: &str, new_slug: &str) -> String
```

Matching rule: scan for `[[`, find the closing `]]`, take the inner text; if the
inner ends with `-<uuid8>` **or equals `<uuid8>`** (titleless slug), replace the
entire `[[inner]]` with `[[new_slug]]`. `uuid8` comparison is case-insensitive
(`uuid_short` lowercases; be tolerant of a hand-typed uppercase). Only the trailing
8-hex token is significant, so slugs containing hyphens (`my-note-abc12345`) match
correctly.

### 2. DB method — `rewrite_links_to_renamed_note`

New method on `Db` (sibling of `rewrite_tag_in_bodies`), single transaction:

```rust
/// After a note's title (hence slug) changes, rewrite slug-form links to it in
/// every carrying note's body so the displayed text stays fresh. Carriers are
/// found via the edges index (rel='mentions', dst_id=uuid8) — no full scan.
/// Each rewritten carrier: body updated, local_version bumped, clean→dirty
/// (so the worker re-syncs it), tags/edges/fts re-derived. Returns # rewritten.
pub fn rewrite_links_to_renamed_note(&self, account_id: &str, target_uuid: &str, new_slug: &str) -> SqlResult<usize>
```

Implementation (mirrors `rewrite_tag_in_bodies`):
- `uuid8 = uuid_short(target_uuid)`.
- Select carriers: `SELECT n.uuid, n.title, n.label, n.body_html FROM notes n JOIN
  edges e ON e.account_id = n.account_id AND e.src_uuid = n.uuid WHERE
  n.account_id = ?1 AND e.rel = 'mentions' AND e.dst_id = ?2 AND n.sync_state !=
  'deleted_pending'` (uses `idx_edges_dst_id`). DISTINCT on uuid (a note may link
  multiple times). Includes self-links (src == target) — correct, the self-link
  text should update too.
- For each carrier: `new_body = rewrite_wikilink_slug_in_body(body, uuid8, new_slug)`;
  skip if unchanged; else `UPDATE notes SET body_html, local_version+1,
  sync_state = CASE 'clean' THEN 'dirty' ELSE sync_state END, last_local_modified_at`
  + `reconcile_tags_from_body_conn` + `reconcile_edges_from_body_conn` +
  `fts_index_conn` (all `_conn` variants inside the tx). Commit. Return count.

### 3. Trigger — hook the save path in `apply_local_edit`

`db::apply_local_edit` (the local-first write for an edited note) is where a note's
new title lands in the cache. Capture the note's **previous title** (read the
existing row before the upsert), and after the edit is applied, if
`slugify(prev_title) != slugify(new_title)`, call
`rewrite_links_to_renamed_note(account_id, uuid, note_slug(new_title, uuid))` within
the same logical step. (If the row is new — no previous title — there are no
inbound links yet, so skip.) This keeps the rewrite synchronous-to-disk with the
edit (data doctrine), and the carriers' `dirty` flag drives the async push.

Note: `apply_local_edit` already re-derives the EDITED note's own tags/edges/fts.
The new call additionally fixes the *inbound* links from OTHER notes.

## Edge cases

- **uuid8 collision** (two notes share the first 8 hex): ~1-in-4-billion; a carrier
  of the renamed note could in theory also match a colliding note's uuid8. Accepted
  (uuid8 is 32-bit; the whole slug design already relies on its practical
  uniqueness). Not mitigated.
- **Slugs containing hyphens** (`weekly-review-abc12345`): matched correctly — only
  the final 8-hex token before `]]` is compared.
- **Plain `[[Title]]`**: untouched (dst_id empty; not selected by the query).
- **Carrier is dirty/conflict**: the rewrite still updates the cached body and keeps
  the row dirty (CASE leaves non-clean states as-is) — same as `rewrite_tag_in_bodies`.
- **Round-trip**: the rewritten `[[new-slug]]` is plain body text → round-trips to
  Apple/Gmail and re-derives identically; no special handling.

## Testing

- **Unit** (`rewrite_wikilink_slug_in_body`): `[[old-title-abc12345]]` →
  `[[new-title-abc12345]]`; slug with hyphens; titleless `[[abc12345]]` →
  `[[new-slug]]`; a DIFFERENT uuid8 `[[x-def67890]]` left untouched; plain
  `[[Some Title]]` left untouched; multiple links in one body all rewritten.
- **Integration** (`rewrite_links_to_renamed_note`, in-memory Db): insert note B
  (uuid known) + note A whose body contains `[[oldB-<uuid8B>]]` (so edges has A→B
  via dst_id); call with B's new slug; assert A's body now has `[[newB-<uuid8B>]]`,
  A is `dirty`, B is unchanged, and edges for A still resolve to B.
- Existing suite stays green (87 tests).

## Scope / files

- `src-tauri/src/db.rs` — `rewrite_wikilink_slug_in_body` (fn) +
  `rewrite_links_to_renamed_note` (method) + the `apply_local_edit` hook + tests.
- No frontend changes, no vertical changes (Gmail/LocalFS), no schema change
  (`edges.dst_id` already exists).

## Deferred (not built)

- Rewriting plain `[[Title]]` links on rename (ambiguous; separate concern).
- Live re-derivation of link DISPLAY at render time (the alternative to
  rewrite-on-rename; not chosen — rewrite keeps the body as the single source).
