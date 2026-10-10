# Search matches folder names — design

Date: 2026-10-09 · Status: approved in chat (sections 1–2 + approach), the rest
decided under `/goal` and recorded here for review.

## Problem

`Db::search_notes` matches title and body only. `notes_fts` has no folder
column, and `label` is used purely as an exact-match *filter*. So:

- typing a folder name in Jodd search finds nothing unless a note's text
  contains it;
- Ask Jodd's FTS source (`ask::pool`, one `search_notes` call per question
  term) cannot pull a folder's notes in by name;
- jodd-mcp `search_notes` is the same function and has the same blind spot.

This holds for **every backend**: `notes.label` is the backend-neutral folder
path (Gmail label, iCloud `ParentFolder` path, SSH/LocalFs directory), and the
index is built by one function, `fts_index_conn`, for all of them. Microsoft
is barely affected only because Graph cannot discover folders (gotcha #12).

## Success criteria

1. Searching `Clients` finds notes in `Notes/Work/Clients` whose text never
   says "Clients" — Thai folder names included (trigram, substring).
2. An Ask Jodd question naming a folder pulls that folder's notes into the
   candidate pool.
3. Every result today's search returns for a title/body match is still
   returned, in the same order, ahead of any folder-only match.
4. A note's folder text in the index is never stale: a move, a folder rename
   (whole subtree) or a remote relocation is visible to the next search.

## Decisions

| # | Decision | Who |
|---|---|---|
| D1 | Index the path **minus its first segment** (`Notes/Work/Clients` → `Work/Clients`). Every backend roots at one segment (`Notes`, or Gmail's configured `notes_label`); indexing it would make `note`/`notes`/`ote` match every note in the vault. Strip the first segment structurally, never by comparing to the literal `Notes`. A root-level note indexes `''`. | user (A), mechanism mine |
| D2 | Folder matches rank **below** title/body matches. | user (A) |
| D3 | Keep the folder column fresh with **SQLite triggers** on `notes`, not by re-indexing from every Rust path that changes `label`. | user (approach 1) |
| D4 | Implement D2 as **two tiers**, not bm25 column weights. | mine — see below |
| D5 | One Rust helper, `folder_text_sql(col)`, is the only definition of the strip rule; triggers, `fts_index_conn` and the LIKE fallback are all built from it. | mine |

### Why tiers instead of bm25 weights (D4)

The approach as first proposed set a persistent FTS5 `rank` of
`bm25(…, folder=0.2)`. That cannot *guarantee* D2: bm25 normalises by column
length, and a folder column is a handful of trigrams, so a down-weighted short
column can still outscore a long body. It would also perturb today's ordering
of pure title/body hits (criterion 3) for no reason. Two tiers give both
guarantees by construction:

- **Tier 1** — `notes_fts MATCH '{title body} : "q"'` `ORDER BY rank`: exactly
  today's query (today's MATCH spans title+body, the only indexed columns), so
  today's results come back unchanged and first.
- **Tier 2** — only if tier 1 returned fewer than 200 rows: `MATCH 'folder :
  "q"'`, skipping keys already in tier 1, **newest first** (a folder-name hit
  is effectively "show me that folder", and recency is the folder view's own
  order), filling up to 200.

The UI re-sorts results by date (`NoteList.filteredNotes`), so for the app the
tiers decide *which* 200 survive, not the display order. Ask Jodd's pool and
MCP's row cap consume the order directly.

## Design

### Schema — migration 24 (`Step::Rust`)

`Step::Rust` because the SQL is assembled from `folder_text_sql`.

1. `DROP TABLE notes_fts;` then recreate as
   `fts5(uuid UNINDEXED, account_id UNINDEXED, title, body, folder, tokenize='trigram')`.
2. `notes_fts_folder_ai AFTER INSERT ON notes` → `UPDATE notes_fts SET folder =
   <folder_text_sql(NEW.label)> WHERE uuid = NEW.uuid AND account_id = NEW.account_id`.
3. `notes_fts_folder_au AFTER UPDATE OF label ON notes WHEN OLD.label IS NOT
   NEW.label` → same body. The `WHEN` is load-bearing: the pull upsert writes
   `label = ?3` on every row every pull, and `UPDATE OF` fires on any SET of
   the column, changed or not.

No backfill inside the migration: `Db::open` runs `fts_backfill_if_empty()`
right after `migrate()`, finds the new table empty and rebuilds it.

`folder_text_sql(col)` =
`CASE WHEN instr(col,'/') > 0 THEN substr(col, instr(col,'/') + 1) ELSE '' END`.

### `fts_index_conn`

Signature unchanged. Its INSERT gains
`folder = COALESCE((SELECT <folder_text_sql(label)> FROM notes WHERE uuid = ?1
AND account_id = ?2), '')`. Together with the insert trigger this is correct
whichever of `notes` / `notes_fts` a call site writes first.

### Search (`Db::search_notes`)

- FTS path (≥3 chars): tier 1 then tier 2 as above, same filters
  (`account_id`, exact `label`, `exclude_accounts`, not `deleted_pending`).
- OR-of-terms fallback (multi-word query, nothing matched): the same two tiers,
  per token — every token's title/body hits first, then every token's
  folder-only hits; dedupe on `(uuid, account_id)`; cap 200.
- LIKE fallback (<3 chars): also matches `folder_text_sql(n.label)`, ordered
  `title/body hit first, then NEWEST_FIRST`.

### Migration 19 must not name `folder` (found while implementing)

Migration 19 re-derives `notes_fts` through `fts_index_conn`. On a vault older
than 19 it runs while `notes_fts` still has its migration-10 shape, so an
`fts_index_conn` that names `folder` would abort the migration and the vault
would not open. 19's loop therefore calls `fts_index_title_body_conn` (the old
four-column insert); 24 rebuilds the index straight after. Pinned by
`migration_19_reindexes_a_pre_24_fts_table`.

### What does not change

- `label` stays an exact-match *filter* in `search_notes` (gotcha #1); scope
  semantics for Ask/DB/MCP are untouched (gotcha #33) — this changes what a
  query *matches*, not which notes are *in scope*.
- MCP hidden folders: `do_search_notes` already filters hidden rows before its
  row cap, so a hidden folder's name surfaces nothing new.
- uuid rekey (gotcha #16) already re-indexes under the new uuid; migration 19's
  account-id rewrite re-derives FTS from `notes`.
- Downgrade: an older build's named-column INSERT into `notes_fts` still works
  (`folder` NULL) and the triggers keep running.

### Cost

`uuid`/`account_id` are `UNINDEXED`, so every `WHERE uuid = …` on `notes_fts`
is a table scan. `fts_index_conn` already pays one per note write; the
triggers add one per note whose folder actually changes. A folder rename of N
notes is N scans. Measured (`folder_rename_timing`, debug build): renaming a 400-note subtree
in a 7 000-note vault takes **0.83 s**. Acceptable for a rare user action; if
it bites, a `rowid` side-map is the follow-up, not part of this change.

## Non-goals

- Showing *why* a result matched (no "matched folder" badge). Clicking a result
  already lights up its folder in the sidebar.
- Searching folders as entities (a "Folders" result group).
- Changing `extract_query_terms` or the Ask catalog (it already shows `label`).

## Testing

`db.rs`:
- folder-only match is found; Thai folder name found by substring;
- root segment is not indexed (`Notes` query does not match every note);
- a title/body hit precedes a newer folder-only hit; tier 1 order equals the
  pre-change order;
- `move_notes_batch`, `rename_subtree`, `upsert_from_remote` with a new label:
  searchable under the new folder, not the old;
- `<3`-char LIKE fallback matches folder text, title/body hits first;
- OR-of-terms fallback reaches folder-only hits;
- `label` filter is still exact;
- upgrade: a DB at migration 23 (old 4-column `notes_fts`) opens, runs 24,
  and every note is indexed with its folder;
- timing: rename a 400-note subtree in a 7 000-note DB, print the duration.

`ask::pool`: a question naming a folder pools that folder's notes via FTS.
