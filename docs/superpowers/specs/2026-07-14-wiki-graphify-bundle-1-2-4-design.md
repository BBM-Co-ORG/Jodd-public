# Structured citations, orphan/staleness lint, and MCP graph exposure — design

> Status: **design / approved** (2026-07-14). Bundles backlog items #1, #2, and #4
> from [`docs/LLM-WIKI-GRAPHIFY-ROADMAP.md`](../LLM-WIKI-GRAPHIFY-ROADMAP.md) — the
> roadmap's own "cheap, independent, no LLM needed" starting point. Item #3 (note
> `type`/`kind`) was dropped from this bundle in brainstorming (no concrete
> consumer yet); item #5+ (auto-link) stays a separate, larger spec per the
> roadmap's own sequencing.

## Problem

Three gaps identified against the Karpathy LLM-Wiki / Graphify / OKF patterns (see
the roadmap doc's "Origin & framing" for the full comparison):

1. **Citations are opaque.** Extract's source block is one `<details><pre>` dump —
   a URL inside the pasted source is buried, unclickable, and invisible to any
   query. There is also no way to detect "I already extracted this exact source"
   short of re-reading old notes.
2. **No hygiene view.** The `edges` table already knows which notes have zero
   backlinks; nothing surfaces that. Notes also silently go stale with no signal.
3. **The graph is trapped inside the app.** `search_notes` and the backlink/
   outgoing-link queries are Tauri-command-only — an external AI agent (e.g. a
   Claude Code session in another project) can't query "what does the user
   already know about X" against Jodd's SQLite cache without the user manually
   copy-pasting old notes into the conversation.

## Decisions (locked in brainstorming)

1. **Citations scan every note, every save** — not just Extract ingests. Follows
   the same `reconcile_*_from_body_conn` full-replace-on-every-write pattern
   already used for `note_tags` and `edges` (`mentions`/`child_of`/`tagged`), so
   any URL typed or pasted anywhere becomes a clickable citation, not just ones
   arriving through the Extract pipeline.
2. **Duplicate-URL detection is in scope**, but only at Extract time (new-note or
   append), not on every keystroke of manual editing — it's a soft warning
   ("already extracted from this URL in **{note}**"), never a hard block.
3. **Smart Folders ship as exactly two, per-account, fixed thresholds**:
   "Orphaned" (zero backlinks) and "Stale (30d+)" (`last_local_modified_at` older
   than a hardcoded 30 days). No settings UI, no configurable threshold.
4. **Smart Folders are fully virtual** — no row in the `folders` table, despite
   `folders.kind` already reserving a `'smart_query'` value for exactly this
   purpose (`db.rs:583`). Deliberate: folders are tightly coupled to Gmail-label
   sync (`dirty_new`/rename/move/delete state machine); Smart Folders never need
   any of that, and forcing them through the folders table would mean guarding
   every one of those code paths against a folder that must never sync, rename,
   or be deleted. A separate, smaller frontend selection concept is cheaper and
   safer than carving exceptions into folder sync state.
5. **The MCP server is a new Cargo workspace member** (`jodd-mcp/`), not a binary
   under `src-tauri/src/bin` or `src-tauri/examples/`. CLAUDE.md's defect #5
   documents why extra binaries under `src-tauri/src/bin` break the macOS bundle
   (Tauri's bundler copies every discovered `[[bin]]` into `Contents/MacOS/`, and
   Tahoe refuses to launch multi-binary bundles); `examples/` avoids that but is
   documented as dev/debug scratch space, not a persistent process a real MCP
   client shells out to. A workspace member sidesteps both: it's structurally
   invisible to Tauri's bundler and is the correct shape for an independently
   distributed, independently versioned binary.
6. **`jodd-mcp` exposes exactly two tools**: `search_notes` and
   `note_connections` (backlinks + outgoing links combined, mirroring the
   existing Tauri command of the same name). `list_folders`/`list_folder_kinds`
   and a citations lookup were considered and cut from this pass — narrower
   surface, easy to add later since both are one more thin wrapper over an
   existing `Db` method.
7. **DB path resolution**: default to `dirs::data_dir().join("jodd")` — the exact
   call `lib.rs:3949` already uses to open the app's own cache (not
   Tauri-specific; just the `dirs` crate) — joined with `jodd.sqlite3`,
   overridable via `--db-path` or `JODD_DB_PATH` for non-standard setups.

## Approach

### 1. Structured citations

**Schema** — no migration needed. New `rel='cites'` rows in the existing `edges`
table (`db.rs:549-559`, next migration number is 15 if any schema change were
needed, but this reuses the existing table as-is). `dst_id` = the raw matched URL
string, trimmed; `dst_title` left empty, mirroring how `tagged` rows leave
`dst_id` empty. **No normalization** (no trailing-slash stripping, no query-param
canonicalization) — dedup is exact-string match only, per the roadmap's own
"no LLM needed" framing for the cheap first cut.

**Detection** — new `extract_urls(text: &str) -> Vec<String>` in a location
alongside the existing hand-rolled scanners (`extract_wikilinks`, `tags_from_body`
in `db.rs`). No `regex` crate exists in this codebase's dependency tree; this
follows the same character-scan style. Algorithm: scan for `http://` or
`https://`, extend the match until whitespace or one of `)`, `]`, `"`, `'`, `<`,
then trim trailing `.`, `,`, `;`, `:` (common prose-sentence punctuation
immediately after a bare URL). Dedup within a single note before returning.
Operates on plain text, not raw HTML — same as `tags_from_body`'s existing
`strip_html_to_text(body_html)` pre-pass (`db.rs:2719`, `2799-2801`).

**Hook point** — new `reconcile_citations_from_body_conn(conn, account_id, uuid,
body_html)`, a third sibling to `reconcile_tags_from_body_conn` (`db.rs:3086-3103`)
and `reconcile_edges_from_body_conn` (`db.rs:2896-2942`), following the identical
full-delete-then-reinsert shape:

```rust
fn reconcile_citations_from_body_conn(conn: &Connection, account_id: &str, uuid: &str, body_html: &str) -> SqlResult<()> {
    conn.execute("DELETE FROM edges WHERE account_id = ?1 AND src_uuid = ?2 AND rel = 'cites'", params![account_id, uuid])?;
    for url in extract_urls(&strip_html_to_text(body_html)) {
        conn.execute(
            "INSERT OR IGNORE INTO edges (account_id, src_uuid, dst_id, dst_title, rel) VALUES (?1, ?2, ?3, '', 'cites')",
            params![account_id, uuid, url],
        )?;
    }
    Ok(())
}
```

Called from the same four write sites as the other two reconcilers:
`upsert_from_remote` (`db.rs:860`), `apply_local_edit` (`db.rs:968`),
`insert_local_new` (`db.rs:1022`), and the slug-rewrite-on-rename path
(`db.rs:1942`, `3072`). The existing `DELETE FROM edges WHERE ... rel IN
('mentions','child_of','tagged')` calls in `reconcile_edges_from_body_conn` do
**not** need `'cites'` added to their `IN (...)` list — citations get their own
dedicated delete/insert pair, kept as a separate reconciler function rather than
folded into `reconcile_edges_from_body_conn`, since it has a different source
(URL scan of stripped body text vs. wikilink/tag parsing) and a different
downstream consumer (dedup lookups, not backlink traversal).

**Sources panel** — new `Db::citations(account_id, uuid) -> SqlResult<Vec<String>>`
(`SELECT dst_id FROM edges WHERE account_id=?1 AND src_uuid=?2 AND rel='cites'
ORDER BY dst_id`), new Tauri command `note_citations` (sibling to
`note_connections`, `lib.rs:1573-1600`). `NoteEditor.svelte` fetches it alongside
`refreshConnections()` (same `connSeq`-guarded pattern, `NoteEditor.svelte:161-179`)
and renders a third `.conn-group` block ("📎 Sources") in the existing connections
panel (`NoteEditor.svelte:1954-1975`), each URL an `<a href={url} target="_blank"
rel="noopener">` opened in the OS default browser — no in-app browser chrome
exists to route through instead. The block only renders when non-empty, matching
how the Links-to/Linked-from groups already behave with zero entries.

**Duplicate warning** — new `Db::find_citation_owner(account_id, url, exclude_uuid:
Option<&str>) -> SqlResult<Option<CachedNote>>` (joins `edges` back to `notes` on
`src_uuid`, `rel='cites' AND dst_id=?url`, excludes the append target so appending
new source material into a note that already cites the same URL doesn't
self-flag). Wired into both `extract_lessons` and `append_extract_lessons`
(`lib.rs`): before calling the LLM, run `extract_urls` over the pasted
`source_text`, check each against `find_citation_owner`, and if any hit, return a
warning payload (`{ url, existing_note_title, existing_note_uuid }`) the modal
surfaces inline with a "Continue anyway" button that resubmits with a
`skip_duplicate_check: bool` flag. This mirrors the existing "target note not
found" inline-error precedent in `append_extract_lessons` (see the Phase 1 spec) —
same "surface it, don't silently proceed or silently block" shape.

### 2. Smart Folders (Orphaned / Stale)

**Backend** — two new `Db` methods:

```rust
pub fn list_orphaned_notes(&self, account_id: &str) -> SqlResult<Vec<CachedNote>>
pub fn list_stale_notes(&self, account_id: &str) -> SqlResult<Vec<CachedNote>>
```

`list_orphaned_notes` mirrors the anti-join shape of `backlinks` (`db.rs:712-738`)
inverted to `NOT EXISTS (SELECT 1 FROM edges e WHERE e.rel='mentions' AND
(e.dst_id = substr(lower(replace(n.uuid,'-','')),1,8) OR e.dst_title =
lower(trim(n.title))))` — the exact same uuid8/title-match expression
`backlinks`/`outgoing_links` already use, just inverted into a `NOT EXISTS`.
`list_stale_notes` filters `last_local_modified_at < ?threshold_ms`, where
`threshold_ms` is computed in Rust (`now_ms() - 30 * 86_400_000`) and bound as a
query param — `last_local_modified_at` is stored as `i64` **milliseconds** since
epoch (`db.rs:46`), not seconds, so this must not use SQLite's `unixepoch()`
(seconds) directly. Both exclude
`sync_state != 'deleted_pending'`, matching `search_notes`'s existing exclusion.
Two Tauri commands, `list_orphaned_notes` / `list_stale_notes`, same
`account_id: String, state: State<'_, AppState>` shape as other per-account
reads.

**Frontend** — new store `selectedSmartFolder: Writable<{ account: string; kind:
'orphaned' | 'stale' } | null>` in `notes.ts`, mutually exclusive with
`selectedFolder` (setting one clears the other, mirroring the existing
folder-selection-clears-note-selection convention). Sidebar renders two fixed
rows per account ("🔍 Orphaned", "🕰 Stale") in their own small block, positioned
after the Workflows group and before the folder tree proper — visually distinct
from both regular folders and the `__Extracts__`-style Workflows group, since
these aren't folders at all. Clicking sets `selectedSmartFolder` and clears
`selectedFolder`; `NoteList.svelte`'s note-fetch branches on
`$selectedSmartFolder` before falling through to the existing cached-folder path,
calling `list_orphaned_notes`/`list_stale_notes` instead. No context menu, no
drag-drop target, no rename — read-only, matching the "spring cleaning view"
framing from the roadmap.

### 3. `jodd-mcp` — agent-callable graph exposure

**Workspace layout** — repo root gains (or extends) a `Cargo.toml`:

```toml
[workspace]
members = ["src-tauri", "jodd-mcp"]
```

`jodd-mcp/Cargo.toml` depends on `jodd_lib` (the existing `rlib` target declared
at `src-tauri/Cargo.toml:9-11`) via a path dependency (`{ path = "../src-tauri" }`),
plus an MCP server SDK crate for stdio transport and tool-call dispatch (exact
crate/version pinned during planning — `rmcp`, the official Rust SDK, is the
leading candidate; confirm current published version at plan time rather than
guessing here), plus `dirs = "5"` (already a dependency of `src-tauri`, reused
here directly rather than round-tripping through `jodd_lib`).

**DB access** — `jodd-mcp`'s `main.rs` resolves the DB path per decision 7 above,
then calls `jodd_lib::db::Db::open(&path)` directly — the exact same function
the desktop app calls, zero duplicated SQL, zero Tauri runtime dependency (`Db::open`
takes only a `&PathBuf`, confirmed no `AppHandle`/`State` coupling — `db.rs:142`).
WAL mode (already enabled, `db.rs:150`) permits concurrent readers, so `jodd-mcp`
running alongside a live Jodd instance is safe as long as it only calls read
methods (it does — no write path is exposed).

**Tools**:
- `search_notes(account_id?: string, label?: string, query: string)` → wraps
  `Db::search_notes` (`db.rs:657-703`) directly, same three-way scope semantics
  (`None`/`None` = all accounts+folders) as the app's own search.
- `note_connections(account_id: string, uuid: string)` → wraps `Db::backlinks` +
  `Db::outgoing_links` (`db.rs:712-757`) together, same combined shape as the
  existing `note_connections` Tauri command (`lib.rs:1573-1600`).

Both return the note DTO already used for Tauri responses (title, body_html,
folder, uuid, etc.) serialized as MCP tool-call JSON results.

**Distribution** — not part of the Tauri bundle or its CI release pipeline. Built
via `cargo build --release -p jodd-mcp`; the user adds it to their Claude Code MCP
config pointing at the built binary path (`claude mcp add jodd -- /path/to/jodd-mcp
--db-path /path/to/jodd/jodd.sqlite3`, or relying on the `dirs::data_dir()`
default). Documented as a manual one-time setup step in this spec's Scope/files
section — no auto-install, no auto-registration with Claude Code's config from
inside the Jodd app itself.

## Error handling

- **Malformed/partial URLs in pasted text** (e.g. a URL cut off by a line wrap):
  `extract_urls` only requires the `http(s)://` prefix and a non-whitespace
  continuation — a truncated URL still gets captured as whatever substring was
  present. No validation that the result is a well-formed, resolvable URL; this
  is citation capture, not link-checking.
- **Duplicate-check false negative** (same source re-pasted with a tracking
  param added/removed, e.g. `?utm_source=...`): accepted trade-off of exact-string
  matching (decision 2 / roadmap's own framing) — a near-duplicate URL won't
  trigger the warning. Not a correctness bug, a scope boundary.
- **`jodd-mcp` launched with no DB at the resolved/given path**: `Db::open`
  already creates the parent dir and an empty SQLite file if none exists
  (`db.rs:143-144`) — `jodd-mcp` would surface an app with an empty schema/no
  tables if pointed at a fresh path rather than the real cache. No special
  handling beyond surfacing whatever `rusqlite` error results from querying
  tables that don't exist (a clear "no such table: notes" error is diagnosable
  as-is — pointed at the wrong path).
- **Extract's duplicate-URL check finds the LLM-in-flight note deleted before
  the "Continue anyway" resubmit**: falls through to the existing "target note
  not found" handling already specified in the Phase 1 append design — no new
  error path needed, the check happens before that existing validation runs.

## Edge cases

- **A note that cites a URL also mentioned in another note's plain prose** (not
  inside an `<a href>`, just typed as text): still captured — `extract_urls`
  scans the HTML-stripped text, not `<a>` tags specifically, so a bare pasted URL
  in a checklist item or bullet is captured the same as one in an Extract source
  block.
- **Orphaned AND Stale simultaneously**: independent predicates, a note can
  appear in both Smart Folders at once — no dedup or precedence between them,
  matching how the roadmap describes them as two separate lint signals.
  **Note counts shown next to folder rows are direct-label-only elsewhere in this
  app (CLAUDE.md, D11 close-out)** — Smart Folder rows show no count at all in
  this pass, avoiding a query-on-every-sidebar-render cost; open the folder to see
  contents, consistent with treating this as a lint/review view, not a live
  dashboard.
- **`jodd-mcp` and the Jodd app both running, one writes while the other reads
  mid-transaction**: WAL mode's reader/writer isolation covers this — a reader
  either sees the pre- or post-commit state, never a torn read. No additional
  locking needed since `jodd-mcp` never writes.

## Testing

- **Unit** (`db.rs`): `extract_urls` — bare URL, URL followed by punctuation, URL
  inside markdown-link syntax `[text](url)` (captures just the URL portion, not
  the brackets), multiple URLs in one body, no false match on non-URL text
  containing "http" as a substring. `reconcile_citations_from_body_conn` —
  re-running on an edited body drops removed URLs and keeps/adds current ones
  (full-replace semantics, mirroring existing `note_tags` reconciler tests if
  present). `list_orphaned_notes` / `list_stale_notes` against an in-memory
  fixture DB with a mix of linked/unlinked and old/new notes.
- **Integration** (`jodd-mcp`): a fixture SQLite DB (same schema, seeded via the
  same `Db::open`/migration path used elsewhere), call both tools, assert JSON
  shape and content match the equivalent `Db` method's direct output.
- **Live verification** (per CLAUDE.md convention, scoped to a test folder):
  - Paste a source with a URL into Extract (new-note mode); confirm the note's
    Sources panel shows it, clicking opens the OS browser.
  - Re-paste the same source (or one containing the same URL) into a second
    Extract; confirm the inline duplicate warning appears naming the first note;
    confirm "Continue anyway" proceeds.
  - Manually type a bare URL into a regular hand-edited note's body; save;
    confirm it shows up in that note's Sources panel too (validates "all notes,
    every save" scope, not just Extract).
  - Create a note with no `[[wikilinks]]` pointing to it; confirm it appears
    under "Orphaned"; link to it from another note; confirm it drops out.
  - Confirm "Stale" populates for a note whose `last_local_modified_at` is
    artificially backdated past 30 days (via direct SQLite edit in a test DB, not
    literally waiting 30 days).
  - Build `jodd-mcp`, point it at the same `jodd.sqlite3` a running Jodd instance
    uses, register it in Claude Code's MCP config, and from a Claude Code session
    call `search_notes` and `note_connections` against real data — confirm results
    match what the Jodd app itself shows for the same account/note.

## Scope / files

- `src-tauri/src/db.rs` — `extract_urls`, `reconcile_citations_from_body_conn`
  (+ its four call sites), `Db::citations`, `Db::find_citation_owner`,
  `Db::list_orphaned_notes`, `Db::list_stale_notes`, plus unit tests for all of
  the above.
- `src-tauri/src/lib.rs` — `note_citations`, `list_orphaned_notes`,
  `list_stale_notes` Tauri commands; duplicate-URL check wired into
  `extract_lessons` and `append_extract_lessons`.
- `src/lib/stores/notes.ts` — `selectedSmartFolder` store.
- `src/lib/components/NoteEditor.svelte` — Sources panel block, citation fetch.
- `src/lib/components/Sidebar.svelte` — Orphaned/Stale rows per account.
- `src/lib/components/NoteList.svelte` — branch on `$selectedSmartFolder`.
- `src/lib/components/LessonExtractModal.svelte` — inline duplicate-URL warning
  + "Continue anyway" resubmit.
- **New**: `jodd-mcp/` workspace member (`Cargo.toml`, `src/main.rs`), repo-root
  `Cargo.toml` gaining/extending a `[workspace]` table.
- No changes to `lessons/prompt.rs`, `provider.rs`, `resolve.rs`, `http.rs`,
  `claude_code.rs`, the LLM system prompt, or the `edges` table schema (reuses
  the existing table/migration as-is).

## Deferred (not built)

- **URL normalization for fuzzier dedup** (stripping tracking params, trailing
  slashes, scheme case): exact-string matching only in this pass, per decision 2.
- **Configurable staleness threshold / additional Smart Folders**: only
  Orphaned + Stale(30d), no settings UI. Future Smart Folders (by tag, by
  pinned, by date range) are backlog item #2 in CLAUDE.md's own roadmap, a
  separate pass.
- **`list_folders`/`list_folder_kinds` and a citations-lookup MCP tool**:
  considered, cut from this pass (decision 6) — narrow, easy follow-on additions
  to `jodd-mcp` later.
- **Auto-registering `jodd-mcp` with Claude Code's config from inside the Jodd
  app**: manual `claude mcp add` setup only.
- **Note `type`/`kind` field (roadmap item #3)**: dropped from this bundle
  entirely per the "Decisions" above — no concrete consumer exists yet.
- **Real `folders`-table-backed smart folders** (`kind='smart_query'`): the
  schema comment reserving this value stays unused by this spec; decision 4
  chose the virtual approach instead. A future pass could still build the
  folders-table version if a stronger case for uniform folder-tree treatment
  emerges (e.g. drag-a-note-into-Orphaned-does-nothing needing the same
  guard-rail pattern already built for the Workflows folder).
