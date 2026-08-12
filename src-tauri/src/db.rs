// SQLite local working replica for Jodd notes.
//
// Architecture note: this database is a LOCAL REPLICA, not a master. The
// source of truth is the model + sync rules across all replicas (this DB,
// Gmail, Apple Notes, iOS). When a write reaches Gmail successfully, that
// becomes the version other replicas eventually observe. A local write that
// hasn't synced yet is *also* a valid state for this replica — we just
// track that it's `dirty` until it's pushed.
//
// The state machine for each note:
//
//   clean        local matches remote (no pending push or pull)
//   dirty        local was edited; not yet pushed to Gmail
//   pull_needed  remote changed; not yet applied locally
//   conflict     local AND remote both changed since last sync — keep both
//
// remote_version is the Gmail message id of the last server state we know
// for this note. Gmail's insert-then-trash pattern means every Gmail-side
// change produces a new id, so the id alone is a sufficient version token.
//
// local_version is a monotonic local-edit counter. Compared against
// last_synced_at to determine `dirty` (any local edit after last sync).

use rusqlite::{params, Connection, OptionalExtension, Result as SqlResult};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

/// One row in the local notes cache. Mirrors gmail::Note plus sync metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedNote {
    pub uuid: String,
    pub account_id: String,
    pub id: String, // remote Gmail message id (most recently known)
    pub title: String,
    pub body_html: String,
    pub date: String,                       // RFC 2822 Date header (last-modified)
    pub x_mail_created_date: Option<String>,
    pub label: String,

    // ─── Sync metadata ────────────────────────────────────────────────────
    pub local_version: i64,                 // monotonic counter, ticks on every local edit
    pub remote_version: Option<String>,     // Gmail message id at last known sync point
    pub sync_state: SyncState,
    pub last_synced_at: Option<i64>,        // ms since epoch
    pub last_local_modified_at: i64,        // ms since epoch
    pub last_remote_modified_at: Option<i64>, // ms since epoch — Date header parsed

    // ─── Jodd-managed pin state (synced via meta-label sidecar) ───────────
    //
    // Pin lives in the SQLite replica AND in a small Jodd-managed sidecar
    // message under the account's meta_label (see accounts::Account). The
    // sidecar exists so multiple Jodd instances signed into the same Gmail
    // account see the same pins; Apple Notes ignores the meta_label
    // entirely (it isn't under "Notes/").
    //
    // pinned          — canonical local value, drives sort and UI indicator.
    // meta_msg_id     — Gmail message id of the current sidecar, None when
    //                   no sidecar exists yet (never pinned, or unpinned
    //                   and the sidecar was trashed).
    // pin_dirty       — true when `pinned` has changed locally but the
    //                   sidecar hasn't been updated yet. Drained by the
    //                   sync worker via `list_pin_dirty`. Orthogonal to
    //                   `sync_state` (see SyncState type comment).
    pub pinned: bool,
    pub meta_msg_id: Option<String>,
    pub pin_dirty: bool,

    // ─── Jodd-managed tag state (synced via meta-label sidecar) ───────────
    //
    // Tags live in three places: the `note_tags` join table (per-row
    // canonical local state), this row's `tags_meta_msg_id` (the Gmail
    // sidecar that mirrors the set), and `tags_dirty` (push backlog flag).
    //
    // tags_meta_msg_id — Gmail message id of the current tag sidecar
    //                    (subject `tags___<UUID>`, body
    //                    `{"tags":["a","b",…]}`). None when no sidecar
    //                    exists yet (note has never had tags, or all tags
    //                    were removed and the sidecar was trashed).
    // tags_dirty       — true when the local tag set has changed but the
    //                    sidecar hasn't been updated. Orthogonal to
    //                    sync_state AND pin_dirty — a single note can be
    //                    content-dirty + pin-dirty + tags-dirty at once.
    //                    Drained by the worker via list_tags_dirty.
    pub tags_meta_msg_id: Option<String>,
    pub tags_dirty: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncState {
    Clean,
    Dirty,
    PullNeeded,
    Conflict,
    /// Locally deleted; waiting for the sync worker to trash on Gmail.
    /// After successful trash, the row is removed from the cache entirely.
    /// We don't immediately DROP because the user might be offline — the
    /// delete intent needs to survive until it can be propagated.
    DeletedPending,
}

// Pin dirtiness is tracked OUTSIDE of `sync_state` because the two are
// orthogonal: a row can be content-dirty AND pin-dirty at the same time
// (user edits the body, then toggles the pin, before the worker drains
// either). A single sync_state column couldn't express that without a
// combinatorial explosion. See the `pin_dirty` column added in migration
// #4 — true means "the sidecar for this note doesn't match SQLite's
// `pinned` column; worker must push." The push path is `labels.modify`
// equivalent (write/trash a sidecar message), independent of any content
// push the worker does for the same uuid in the same tick.

impl SyncState {
    fn as_str(&self) -> &'static str {
        match self {
            SyncState::Clean => "clean",
            SyncState::Dirty => "dirty",
            SyncState::PullNeeded => "pull_needed",
            SyncState::Conflict => "conflict",
            SyncState::DeletedPending => "deleted_pending",
        }
    }
    fn from_str(s: &str) -> Self {
        match s {
            "dirty" => SyncState::Dirty,
            "pull_needed" => SyncState::PullNeeded,
            "conflict" => SyncState::Conflict,
            "deleted_pending" => SyncState::DeletedPending,
            _ => SyncState::Clean,
        }
    }
}

pub struct Db {
    conn: Mutex<Connection>,
}

/// Outstanding work the worker still owes the backend for one account.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct PendingPushes {
    pub notes: usize,
    pub deletes: usize,
    pub pins: usize,
    pub tags: usize,
    /// Folder creates, renames and deletes. Present because
    /// `has_pending_pushes` counts them: without it an account whose only
    /// outstanding work is a renamed folder reports "0 left" while the worker
    /// correctly keeps it draining, and the number the user reads is a lie.
    pub folders: usize,
}

/// `AND n.account_id NOT IN (?k, ?k+1, …)`, or an empty string when there is
/// nothing to exclude.
///
/// Empty means empty: with every account active the SQL is byte-identical to
/// the pre-feature query, no extra clause and no extra binding. `search_notes`
/// runs on every navigation and CLAUDE.md edge #8 records that path being
/// deliberately left alone once already.
fn exclude_clause(exclude: &[String], first_param: usize) -> String {
    if exclude.is_empty() {
        return String::new();
    }
    let ph: Vec<String> = (0..exclude.len())
        .map(|i| format!("?{}", first_param + i))
        .collect();
    format!(" AND n.account_id NOT IN ({})", ph.join(", "))
}

impl Db {
    /// Open (or create) the cache database. `app_data_dir` is the Tauri
    /// app's user-data directory — `~/Library/Application Support/jodd` on
    /// macOS, `%APPDATA%/jodd` on Windows.
    pub fn open(app_data_dir: &PathBuf) -> SqlResult<Self> {
        std::fs::create_dir_all(app_data_dir).ok();
        let db_path = app_data_dir.join("jodd.sqlite3");
        let conn = Connection::open(&db_path)?;
        // WAL gives us concurrent reads while a background writer pushes
        // dirty notes. Foreign keys are off by default — we don't have any
        // referential integrity to enforce here (single-table cache).
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA temp_store = MEMORY;
             PRAGMA busy_timeout = 5000;",
        )?;
        let db = Db { conn: Mutex::new(conn) };
        db.migrate()?;
        db.fts_backfill_if_empty()?;
        db.migrate_tags_to_body()?;
        db.edges_backfill_if_empty()?;
        Ok(db)
    }

    /// Populate the `edges` table from existing note bodies when empty — runs
    /// once after the migration that creates it. Idempotent (no-op once
    /// populated). Same shape as fts_backfill_if_empty.
    fn edges_backfill_if_empty(&self) -> SqlResult<()> {
        let mut conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row("SELECT count(*) FROM edges", [], |r| r.get(0))?;
        if count > 0 {
            return Ok(());
        }
        let rows: Vec<(String, String, String, String)> = {
            let mut stmt = conn.prepare("SELECT uuid, account_id, label, body_html FROM notes")?;
            let it = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
            it.filter_map(|x| x.ok()).collect()
        };
        if rows.is_empty() {
            return Ok(());
        }
        let tx = conn.transaction()?;
        for (uuid, account_id, label, body) in &rows {
            reconcile_edges_from_body_conn(&tx, account_id, uuid, label, body)?;
            reconcile_citations_from_body_conn(&tx, account_id, uuid, body)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// One-time, idempotent: ensure every tag in `note_tags` also exists as an
    /// inline `#hashtag` in its note's body, so tags round-trip to Apple Notes
    /// and the body can be the single source of truth (see
    /// `reconcile_tags_from_body_conn`). For each note carrying tags not already
    /// in its body, append a trailing `<div>#a #b</div>` and mark it
    /// content-dirty so the worker pushes it. Runs at open BEFORE any sync, so
    /// legacy sidecar tags reach the body before reconcile could prune them.
    /// Idempotent: once the body carries the tags, nothing is missing → no-op.
    fn migrate_tags_to_body(&self) -> SqlResult<()> {
        let mut conn = self.conn.lock().unwrap();
        let rows: Vec<(String, String, String)> = {
            let mut stmt = conn.prepare(
                "SELECT n.account_id, n.uuid, n.body_html FROM notes n
                 WHERE EXISTS (SELECT 1 FROM note_tags t
                               WHERE t.account_id = n.account_id AND t.uuid = n.uuid)",
            )?;
            let it = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            it.filter_map(|x| x.ok()).collect()
        };
        if rows.is_empty() {
            return Ok(());
        }
        let tx = conn.transaction()?;
        for (account_id, uuid, body_html) in &rows {
            let current: Vec<String> = {
                let mut s =
                    tx.prepare("SELECT tag FROM note_tags WHERE account_id = ?1 AND uuid = ?2")?;
                let it = s.query_map(params![account_id, uuid], |r| r.get::<_, String>(0))?;
                it.filter_map(|x| x.ok()).collect()
            };
            let in_body: std::collections::HashSet<String> =
                tags_from_body(body_html).into_iter().collect();
            let missing: Vec<String> =
                current.into_iter().filter(|t| !in_body.contains(t)).collect();
            if missing.is_empty() {
                continue;
            }
            let hashtags = missing
                .iter()
                .map(|t| format!("#{}", t))
                .collect::<Vec<_>>()
                .join(" ");
            let new_body = format!("{}<div>{}</div>", body_html, hashtags);
            tx.execute(
                "UPDATE notes SET body_html = ?1,
                     local_version = local_version + 1,
                     sync_state = CASE sync_state WHEN 'clean' THEN 'dirty' ELSE sync_state END,
                     last_local_modified_at = ?2
                 WHERE uuid = ?3 AND account_id = ?4",
                params![new_body, now_ms(), uuid, account_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Populate the FTS index from existing notes when it's empty — runs once
    /// after the migration that creates `notes_fts` (or on a fresh install with
    /// pre-existing cached notes). Idempotent: no-op once populated. Single
    /// transaction for speed (thousands of notes index in well under a second).
    fn fts_backfill_if_empty(&self) -> SqlResult<()> {
        let mut conn = self.conn.lock().unwrap();
        let fts_count: i64 = conn.query_row("SELECT count(*) FROM notes_fts", [], |r| r.get(0))?;
        if fts_count > 0 {
            return Ok(());
        }
        let rows: Vec<(String, String, String, String)> = {
            let mut stmt = conn.prepare("SELECT uuid, account_id, title, body_html FROM notes")?;
            let it = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
            it.filter_map(|x| x.ok()).collect()
        };
        if rows.is_empty() {
            return Ok(());
        }
        let tx = conn.transaction()?;
        for (uuid, account_id, title, body_html) in &rows {
            fts_index_conn(&tx, uuid, account_id, title, body_html)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Schema migration. Idempotent — runs every startup, only applies
    /// statements that haven't been recorded in the migrations table.
    /// Keeps the version history visible in code for future diffs.
    fn migrate(&self) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS migrations (
                version INTEGER PRIMARY KEY,
                applied_at INTEGER NOT NULL
            );",
        )?;
        let applied: Vec<i64> = {
            let mut stmt = conn.prepare("SELECT version FROM migrations ORDER BY version")?;
            let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?
                .collect::<SqlResult<Vec<_>>>()?;
            rows
        };
        let migrations: &[(i64, &str)] = &[
            (
                1,
                // PRIMARY KEY on (uuid, account_id) — Apple Notes' X-UUID is
                // globally unique BUT the same uuid could in principle exist
                // across separate Gmail accounts (e.g., user copied a note
                // between accounts). Keying by both keeps them distinct.
                "CREATE TABLE notes (
                    uuid TEXT NOT NULL,
                    account_id TEXT NOT NULL,
                    id TEXT NOT NULL,
                    title TEXT NOT NULL,
                    body_html TEXT NOT NULL,
                    date TEXT NOT NULL,
                    x_mail_created_date TEXT,
                    label TEXT NOT NULL,
                    local_version INTEGER NOT NULL DEFAULT 0,
                    remote_version TEXT,
                    sync_state TEXT NOT NULL DEFAULT 'clean',
                    last_synced_at INTEGER,
                    last_local_modified_at INTEGER NOT NULL,
                    last_remote_modified_at INTEGER,
                    PRIMARY KEY (uuid, account_id)
                );
                CREATE INDEX idx_notes_account_label ON notes (account_id, label);
                CREATE INDEX idx_notes_sync_state ON notes (sync_state) WHERE sync_state != 'clean';",
            ),
            (
                2,
                // Folders table — local-first folder ops (create/rename/
                // delete/move) write here first; sync worker pushes to Gmail.
                //
                // Folder identity = (account_id, path). label_id is Gmail's
                // numeric id, NULL for folders we created locally but haven't
                // pushed yet. Once pushed, label_id is permanent — Gmail
                // renames don't change it.
                //
                // sync_state values:
                //   clean           = mirrors Gmail
                //   dirty_new       = created locally, not yet pushed
                //   dirty_renamed   = renamed locally (path is the desired
                //                     new name; label_id points to the
                //                     Gmail label we'll rename)
                //   deleted_pending = deleted locally, worker will trash on Gmail
                "CREATE TABLE folders (
                    account_id TEXT NOT NULL,
                    path TEXT NOT NULL,
                    label_id TEXT,
                    sync_state TEXT NOT NULL DEFAULT 'clean',
                    last_local_modified_at INTEGER NOT NULL,
                    last_synced_at INTEGER,
                    PRIMARY KEY (account_id, path)
                );
                CREATE INDEX idx_folders_sync_state ON folders (sync_state) WHERE sync_state != 'clean';",
            ),
            (
                3,
                // Pin support. INTEGER 0/1 — sqlite's BOOLEAN is just an
                // INTEGER affinity, and serde will map to/from Rust's bool
                // via the row_to_note read. The partial index covers the
                // "pinned DESC" sort: only rows with pinned=1 matter, so
                // we don't pay for an index over the typically-zero
                // majority. Default 0 keeps every existing row unpinned.
                "ALTER TABLE notes ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;
                 CREATE INDEX idx_notes_pinned ON notes (account_id, pinned) WHERE pinned = 1;",
            ),
            (
                4,
                // Cross-Jodd-instance pin sync support.
                //
                //   meta_msg_id — Gmail message id of the current sidecar
                //     for this note. NULL means "no sidecar exists on
                //     Gmail" (the note has never been pinned, or it was
                //     unpinned and we trashed the sidecar). Lets the
                //     worker do insert-new + trash-old on update without
                //     a per-toggle list-meta round-trip.
                //
                //   pin_dirty — local pin state hasn't been pushed to
                //     Gmail. Orthogonal to sync_state (see the type
                //     comment above SyncState). Worker drains via the
                //     partial index below.
                //
                // Migration is idempotent: pre-#4 rows get meta_msg_id=NULL
                // and pin_dirty=0, which means "no Gmail sidecar yet" and
                // "we believe the sidecar matches local" — correct for
                // freshly-upgraded rows that have never had a sidecar.
                // Their first pin toggle will create one.
                "ALTER TABLE notes ADD COLUMN meta_msg_id TEXT;
                 ALTER TABLE notes ADD COLUMN pin_dirty INTEGER NOT NULL DEFAULT 0;
                 CREATE INDEX idx_notes_pin_dirty ON notes (pin_dirty) WHERE pin_dirty = 1;",
            ),
            (
                5,
                // Tag support. Jodd-local only (like Pin wave 1) — tags are NOT
                // stored in the note body and never round-trip to Apple Notes
                // (which has no tagging). Many-per-note, so a join table rather
                // than a column. Keyed by (account_id, uuid) to align with the
                // notes PK (uuid, account_id). `tag` is pre-normalized by the
                // write path: trimmed, leading '#' stripped, lowercased,
                // charset [a-z0-9_-]. The (account_id, tag) index covers both
                // the "count notes per tag" sidebar query and the "notes
                // carrying tag X" filter.
                "CREATE TABLE note_tags (
                    account_id TEXT NOT NULL,
                    uuid TEXT NOT NULL,
                    tag TEXT NOT NULL,
                    PRIMARY KEY (account_id, uuid, tag)
                );
                CREATE INDEX idx_note_tags_tag ON note_tags (account_id, tag);",
            ),
            (
                6,
                // Tag tombstones — recovery buffer for the prune_clean race.
                //
                // Background: list_notes treats Gmail's response as authoritative
                // and prune_clean drops any cache row whose uuid wasn't returned.
                // Gmail's q=label:Notes is eventually consistent; a transient
                // omission would silently destroy the note's tags before. Now
                // the orphan-tag step moves rows here instead of deleting, and
                // upsert_from_remote restores tombstoned tags whenever the note
                // reappears. Old tombstones are swept after TOMBSTONE_TTL_MS.
                //
                // Identity matches note_tags so a tombstone can replace itself
                // on a repeat prune. deleted_at drives the TTL sweep.
                "CREATE TABLE tag_tombstones (
                    account_id TEXT NOT NULL,
                    uuid TEXT NOT NULL,
                    tag TEXT NOT NULL,
                    deleted_at INTEGER NOT NULL,
                    PRIMARY KEY (account_id, uuid, tag)
                );
                CREATE INDEX idx_tag_tombstones_uuid ON tag_tombstones (account_id, uuid);
                CREATE INDEX idx_tag_tombstones_age ON tag_tombstones (deleted_at);",
            ),
            (
                7,
                // Cross-Jodd-instance tag sync support — mirrors migration #4
                // for pin, but with one important twist: pin is a binary
                // (sidecar exists = pinned), tags are a variable-length set,
                // so the sidecar carries a JSON body `{"tags":["a","b",…]}`
                // and `list_tag_sidecars` fetches with body (not metadata-
                // only like list_meta_sidecars). Subject convention is
                // `tags___<UUID>` — leading `tags` makes the prefix disjoint
                // from pin's `___<UUID>` so each sync's reader rejects the
                // other's sidecars by prefix match alone.
                //
                //   tags_meta_msg_id — Gmail message id of the current tag
                //     sidecar. NULL means "no sidecar yet" (note has never
                //     had tags, or all tags were removed and the sidecar
                //     was trashed). Lets the worker do insert-new + trash-
                //     old without a per-edit listing round-trip.
                //
                //   tags_dirty — local tag set hasn't been pushed. Orthogonal
                //     to sync_state AND pin_dirty (see CachedNote comment).
                //     Drained by the worker via list_tags_dirty.
                //
                // Pre-#7 rows get tags_meta_msg_id=NULL and tags_dirty=0,
                // meaning "no sidecar, and we believe none is needed yet" —
                // correct for upgraded rows. The first tag mutation flips
                // tags_dirty=1; the worker then creates the sidecar.
                "ALTER TABLE notes ADD COLUMN tags_meta_msg_id TEXT;
                 ALTER TABLE notes ADD COLUMN tags_dirty INTEGER NOT NULL DEFAULT 0;
                 CREATE INDEX idx_notes_tags_dirty ON notes (tags_dirty) WHERE tags_dirty = 1;",
            ),
            (
                8,
                // One-shot backfill: mark every existing tagged note as
                // tags_dirty so the worker creates a sidecar for each. Without
                // this, notes tagged before the v0.14.4 upgrade have local
                // tags but no Gmail sidecar — and cross-instance sync silently
                // does nothing because there's no remote state to pull.
                //
                // Runs ONCE per install (the migrations table records #8 as
                // applied). On a fresh install with no pre-existing tags this
                // is a no-op. On an upgrading install it bulk-flips dirty for
                // every uuid that has at least one note_tags row.
                //
                // Side effect on cross-instance: if both Mac and Windows run
                // #8 they'll BOTH try to push their (different) tag sets to
                // Gmail. Last-write-wins on the sidecar — whichever device
                // pushes second overwrites. Local-wins in apply_remote_tags
                // means the OTHER device's tags_dirty rows block the inbound
                // remote from clobbering them locally, but the SIDECAR on
                // Gmail will reflect whoever pushed second. Acceptable
                // because (a) two truly-divergent tag sets pre-existed only
                // because there was no sync, (b) future edits converge
                // through normal sync, and (c) the user can always re-add
                // missing tags after seeing the converged state — and now
                // those edits WILL propagate.
                "UPDATE notes SET tags_dirty = 1
                 WHERE (account_id, uuid) IN
                     (SELECT DISTINCT account_id, uuid FROM note_tags);",
            ),
            (
                9,
                // Attachments (inline images, etc.) carried in a note's
                // multipart/related body. Stored as a BLOB so the save path can
                // re-emit them instead of stripping them on re-save (the
                // data-loss bug). Keyed by (account_id, note_uuid, content_id);
                // content_id is Apple's cid, stable across edits, that the
                // body's <object data="cid:…"> references.
                //
                // These bytes also live in Gmail (the message itself), so the
                // BLOB is a cache/replica re-derivable on refetch — EXCEPT a
                // not-yet-pushed attachment, sole-copy until the worker pushes
                // it. Any future backup/export must capture those. See
                // docs/DIRECTION.md §6.
                "CREATE TABLE attachments (
                    account_id TEXT NOT NULL,
                    note_uuid TEXT NOT NULL,
                    content_id TEXT NOT NULL,
                    mime_type TEXT NOT NULL,
                    filename TEXT,
                    x_apple_part_url TEXT,
                    data BLOB NOT NULL,
                    PRIMARY KEY (account_id, note_uuid, content_id)
                );
                CREATE INDEX idx_attachments_note ON attachments (account_id, note_uuid);",
            ),
            (
                10,
                // Full-text search index over title + plain-text body. The
                // `trigram` tokenizer makes Thai/CJK (which have no word spaces)
                // substring-searchable — verified `ใบเสร็จ` matches a Thai body.
                // uuid/account_id are UNINDEXED, just for joining back to notes.
                // Maintained from Rust (fts_upsert/fts_delete) because the body
                // must be HTML-stripped first, which SQL triggers can't do.
                // Backfilled in Db::open when empty.
                "CREATE VIRTUAL TABLE notes_fts USING fts5(
                    uuid UNINDEXED,
                    account_id UNINDEXED,
                    title,
                    body,
                    tokenize = 'trigram'
                );",
            ),
            (
                11,
                // Fact-schema edges: directed relationships between notes. First
                // consumer is `mentions` — a [[wikilink]] in a note's body links
                // it to another note BY TITLE (dst_title, normalized) so forward
                // references and not-yet-loaded targets still resolve at query
                // time. Derived from the body on every write (like note_tags) and
                // queried for backlinks. The table is general (more `rel`s — e.g.
                // child_of, tagged — can be backfilled later).
                "CREATE TABLE edges (
                    account_id TEXT NOT NULL,
                    src_uuid   TEXT NOT NULL,
                    dst_title  TEXT NOT NULL,
                    rel        TEXT NOT NULL,
                    PRIMARY KEY (account_id, src_uuid, dst_title, rel)
                );
                CREATE INDEX idx_edges_dst ON edges (account_id, rel, dst_title);
                CREATE INDEX idx_edges_src ON edges (account_id, rel, src_uuid);",
            ),
            (
                12,
                // Slug-based links: add dst_id alongside dst_title so a
                // [[title-slug-<uuid8>]] resolves by the stable id (unambiguous
                // across duplicate titles) while plain [[Title]] still resolves
                // by title. edges is fully derived, so drop + recreate is safe —
                // edges_backfill_if_empty repopulates it on this same open.
                "DROP TABLE IF EXISTS edges;
                CREATE TABLE edges (
                    account_id TEXT NOT NULL,
                    src_uuid   TEXT NOT NULL,
                    dst_id     TEXT NOT NULL DEFAULT '',
                    dst_title  TEXT NOT NULL DEFAULT '',
                    rel        TEXT NOT NULL,
                    PRIMARY KEY (account_id, src_uuid, dst_id, dst_title, rel)
                );
                CREATE INDEX idx_edges_dst_id ON edges (account_id, rel, dst_id);
                CREATE INDEX idx_edges_dst_title ON edges (account_id, rel, dst_title);
                CREATE INDEX idx_edges_src ON edges (account_id, rel, src_uuid);",
            ),
            (
                13,
                // Clear edges so edges_backfill_if_empty repopulates it with the
                // newly-derived child_of (note→folder) and tagged (note→#tag)
                // rels, not just 'mentions'. edges is fully derived → safe.
                "DELETE FROM edges;",
            ),
            (
                14,
                // Folder kind — distinguishes user-created folders from Jodd
                // auto-created system folders for workflow output (Lessons,
                // future Summaries, etc.). Added on the feat/lesson-extraction
                // branch; merged after v0.15.1.
                //
                //   'user'            = user-managed folder (default for every
                //                       existing row — Apple-Notes-compatible
                //                       behavior is preserved).
                //   'system_workflow' = Jodd auto-created folder for workflow
                //                       output. Treated as a sink by the
                //                       workflow features; UI may render
                //                       differently (e.g. under a "Workflows"
                //                       sidebar group).
                //   'smart_query'     = reserved for the planned smart-folders
                //                       feature (query-defined folders backed
                //                       by SQLite predicates, no Gmail label).
                //
                // NOT NULL DEFAULT 'user' so existing rows backfill correctly
                // and future inserts that omit the column still get a sane
                // value. Jodd-local: never round-trips to Apple (which has
                // no per-folder metadata) — kind lives in SQLite only.
                "ALTER TABLE folders ADD COLUMN kind TEXT NOT NULL DEFAULT 'user';",
            ),
            // NOTE: an earlier v0.16.5 iteration added migration #15 as a
            // one-shot UPDATE to backfill misclassified rows. That migration
            // was dropped in favor of the self-heal step inside
            // `upsert_folder_from_remote` (see its body), which re-derives
            // kind from the path on every sync tick — a more robust pattern
            // than a single migration because it survives any future drift
            // and removes the need to reason about migration ordering. If
            // you later need a true schema change, add it as #16 and update
            // this comment.
            (
                15,
                // Citations (wiki-graphify bundle) added a new rel='cites'
                // edge type, derived by reconcile_citations_from_body_conn
                // and wired into edges_backfill_if_empty alongside the
                // existing mentions/child_of/tagged derivation. But
                // edges_backfill_if_empty only runs its derivation loop when
                // `edges` is EMPTY (see its early `if count > 0 { return
                // Ok(()); }` guard) — and on any existing install, `edges`
                // is already populated from the pre-existing mentions/
                // tagged/child_of feature. So the guard silently blocked
                // 'cites' from ever being backfilled for pre-existing notes;
                // only notes edited/saved after upgrade would get 'cites'
                // edges, via the normal write-path hook. Same precedent as
                // migration #13 above: edges is fully derived (never user-
                // authored), so clearing it is always safe and forces
                // edges_backfill_if_empty to see count == 0 on next startup
                // and fully repopulate, this time including 'cites'.
                "DELETE FROM edges;",
            ),
        ];
        for (v, sql) in migrations {
            if !applied.contains(v) {
                conn.execute_batch(sql)?;
                conn.execute(
                    "INSERT INTO migrations (version, applied_at) VALUES (?1, ?2)",
                    params![v, now_ms()],
                )?;
            }
        }
        Ok(())
    }

    /// Read cached notes for one account, EXCLUDING those pending deletion.
    /// The frontend should never see `deleted_pending` rows — they're
    /// logically gone, just waiting for the worker to propagate the delete.
    pub fn list_notes(&self, account_id: &str) -> SqlResult<Vec<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT uuid, account_id, id, title, body_html, date, x_mail_created_date,
                    label, local_version, remote_version, sync_state,
                    last_synced_at, last_local_modified_at, last_remote_modified_at,
                    pinned, meta_msg_id, pin_dirty, tags_meta_msg_id, tags_dirty
             FROM notes WHERE account_id = ?1 AND sync_state != 'deleted_pending'",
        )?;
        let rows = stmt.query_map(params![account_id], row_to_note)?;
        rows.collect()
    }

    /// Cache-first read scoped to one label. Used by the navigation path so
    /// clicking a folder paints from SQLite instantly — no token refresh,
    /// no label_map lookup, no Gmail round-trip. The sweep tick handles
    /// the eventual reconciliation against Gmail via the separate
    /// `list_notes_in_folder` command.
    pub fn list_notes_by_label(&self, account_id: &str, label: &str) -> SqlResult<Vec<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT uuid, account_id, id, title, body_html, date, x_mail_created_date,
                    label, local_version, remote_version, sync_state,
                    last_synced_at, last_local_modified_at, last_remote_modified_at,
                    pinned, meta_msg_id, pin_dirty, tags_meta_msg_id, tags_dirty
             FROM notes WHERE account_id = ?1 AND label = ?2 AND sync_state != 'deleted_pending'",
        )?;
        let rows = stmt.query_map(params![account_id, label], row_to_note)?;
        rows.collect()
    }

    /// Full-text search over title + body for one account. Returns full notes
    /// (so the UI can show results that aren't loaded into memory). Excludes
    /// deleted_pending. The trigram tokenizer needs >=3 chars; shorter queries
    /// fall back to a LIKE scan so 1-2 char searches still work.
    /// `account_id`/`label` are optional filters (None = don't filter), so the
    /// caller picks the scope: a folder (both set), one account (account set),
    /// or everything across all accounts (both None). Implemented with
    /// `?n IS NULL OR col = ?n` so the SQL stays static.
    pub fn search_notes(
        &self,
        account_id: Option<&str>,
        label: Option<&str>,
        query: &str,
        exclude_accounts: &[String],
    ) -> SqlResult<Vec<CachedNote>> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().unwrap();
        let cols = Self::NOTE_COLS;

        if q.chars().count() < 3 {
            // trigram can't index <3 chars — LIKE fallback over title + body.
            let like = format!("%{}%", q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
            // The LIMIT makes the ordering load-bearing: it decides WHICH 200
            // notes the user ever sees, and the frontend's own date sort can
            // only reorder what already came back.
            let order = Self::NEWEST_FIRST;
            let excl = exclude_clause(exclude_accounts, 4);
            let sql = format!(
                "SELECT {cols} FROM notes n
                 WHERE n.sync_state != 'deleted_pending'
                   AND (n.title LIKE ?1 ESCAPE '\\' OR n.body_html LIKE ?1 ESCAPE '\\')
                   AND (?2 IS NULL OR n.account_id = ?2)
                   AND (?3 IS NULL OR n.label = ?3){excl}
                 ORDER BY {order} LIMIT 200"
            );
            let mut binds: Vec<Box<dyn rusqlite::ToSql>> = vec![
                Box::new(like),
                Box::new(account_id.map(str::to_string)),
                Box::new(label.map(str::to_string)),
            ];
            for a in exclude_accounts {
                binds.push(Box::new(a.clone()));
            }
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(binds.iter()), row_to_note)?;
            return rows.collect();
        }

        // FTS5 phrase query (quoted → substring match; double quotes escaped).
        let match_q = format!("\"{}\"", q.replace('"', "\"\""));
        let excl = exclude_clause(exclude_accounts, 4);
        let sql = format!(
            "SELECT {cols} FROM notes n
             JOIN notes_fts f ON f.uuid = n.uuid AND f.account_id = n.account_id
             WHERE notes_fts MATCH ?1
               AND n.sync_state != 'deleted_pending'
               AND (?2 IS NULL OR n.account_id = ?2)
               AND (?3 IS NULL OR n.label = ?3){excl}
             ORDER BY rank LIMIT 200"
        );
        let mut binds: Vec<Box<dyn rusqlite::ToSql>> = vec![
            Box::new(match_q),
            Box::new(account_id.map(str::to_string)),
            Box::new(label.map(str::to_string)),
        ];
        for a in exclude_accounts {
            binds.push(Box::new(a.clone()));
        }
        let hits: Vec<CachedNote> = {
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(binds.iter()), row_to_note)?;
            rows.collect::<SqlResult<Vec<_>>>()?
        };
        drop(conn);

        // Additive fallback: the phrase match above requires the ENTIRE query
        // to appear verbatim as one substring. A caller combining several
        // search terms in one call (e.g. an MCP client passing "credential
        // password api key token secret auth") gets a silent, correct-but-
        // useless `[]`, because that six-word sequence never appears
        // anywhere. Only retry when the strict match found nothing AND there
        // is more than one token — a single-word query is already a phrase
        // of one token, so retrying it would be redundant, and a query that
        // already hit must never be widened by this branch.
        let tokens: Vec<&str> = q.split_whitespace().collect();
        if hits.is_empty() && tokens.len() > 1 {
            // FTS5's MATCH must be the sole predicate against the virtual
            // table per query — `notes_fts MATCH ?1 OR notes_fts MATCH ?2`
            // is not valid, so instead of one OR'd MATCH expression we run
            // one query per token and union the results in Rust, deduping
            // by (uuid, account_id) (`search_notes_or_of_terms` below).
            return self.search_notes_or_of_terms(account_id, label, &tokens, exclude_accounts);
        }
        Ok(hits)
    }

    /// OR-of-terms fallback used by `search_notes` when the strict phrase
    /// match returns nothing for a multi-word query. FTS5's `MATCH` operator
    /// must be the sole predicate against the virtual table (it can't be
    /// OR'd against other `MATCH`es the way this needs), so instead of OR
    /// inside one MATCH expression we UNION one query per token and dedupe
    /// by `(uuid, account_id)` — the table's real primary key — preserving
    /// the same `ORDER BY rank LIMIT 200` shape.
    fn search_notes_or_of_terms(
        &self,
        account_id: Option<&str>,
        label: Option<&str>,
        tokens: &[&str],
        exclude_accounts: &[String],
    ) -> SqlResult<Vec<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let cols = Self::NOTE_COLS;
        let excl = exclude_clause(exclude_accounts, 4);
        let sql = format!(
            "SELECT {cols} FROM notes n
             JOIN notes_fts f ON f.uuid = n.uuid AND f.account_id = n.account_id
             WHERE notes_fts MATCH ?1
               AND n.sync_state != 'deleted_pending'
               AND (?2 IS NULL OR n.account_id = ?2)
               AND (?3 IS NULL OR n.label = ?3){excl}
             ORDER BY rank LIMIT 200"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for t in tokens {
            let match_q = format!("\"{}\"", t.replace('"', "\"\""));
            let mut binds: Vec<Box<dyn rusqlite::ToSql>> = vec![
                Box::new(match_q),
                Box::new(account_id.map(str::to_string)),
                Box::new(label.map(str::to_string)),
            ];
            for a in exclude_accounts {
                binds.push(Box::new(a.clone()));
            }
            let rows: Vec<CachedNote> = stmt
                .query_map(rusqlite::params_from_iter(binds.iter()), row_to_note)?
                .collect::<SqlResult<Vec<_>>>()?;
            for n in rows {
                // Dedupe on the notes table's REAL primary key,
                // `(uuid, account_id)` — not uuid alone. The same UUID
                // legitimately exists in two accounts (a note moved or
                // duplicated across them; see CLAUDE.md roadmap #6), and a
                // cross-account search — `account_id = None`, the MCP
                // server's default — would silently drop one of the two,
                // reintroducing one layer down the exact silent-partial-
                // result class this fallback exists to fix. The primary
                // phrase-match path has no dedup at all and never had this.
                if seen.insert((n.uuid.clone(), n.account_id.clone())) {
                    out.push(n);
                }
            }
        }
        out.truncate(200);
        Ok(out)
    }

    /// ORDER BY expression for every "newest first" note list.
    ///
    /// `notes.date` is the raw RFC822 `Date` header string
    /// ("Wed, 9 Sep 2020 14:23:15 +0700"), so `ORDER BY date DESC` sorts
    /// LEXICALLY — by weekday name first — and puts a 2020 note above a 2026
    /// one. The bug is near-invisible on a small account and grossly wrong on a
    /// large one (found at 6,655 notes). Never order by `date`.
    ///
    /// `last_remote_modified_at` is epoch-ms and is the real sort key: both
    /// verticals populate it via `CachedNote::from_remote` (RFC2822 parse of
    /// the Date header) and `mark_pushed` refreshes it on every push. It is
    /// NULL only for a note created locally and not yet pushed
    /// (`insert_local_new`, e.g. the Extract workflow), so fall back to
    /// `last_local_modified_at`, which the schema declares NOT NULL — that
    /// keeps a brand-new note at the TOP of the list rather than the bottom.
    ///
    /// Requires the `notes` table to be aliased `n` (see `NOTE_COLS`).
    const NEWEST_FIRST: &'static str =
        "COALESCE(n.last_remote_modified_at, n.last_local_modified_at) DESC";

    /// The 19 `notes` columns, in the exact order `row_to_note` reads them.
    ///
    /// ONE definition for every query that yields a `CachedNote`, including
    /// `search_notes` and its OR-of-terms fallback, which each held their own
    /// verbatim copy until this was hoisted: adding a column to one copy and
    /// not the other silently mis-maps every field after it, with no compile
    /// error — `row_to_note` reads by index.
    ///
    /// Requires the `notes` table to be aliased `n`.
    const NOTE_COLS: &'static str = "n.uuid, n.account_id, n.id, n.title, n.body_html, \
        n.date, n.x_mail_created_date, n.label, n.local_version, n.remote_version, \
        n.sync_state, n.last_synced_at, n.last_local_modified_at, n.last_remote_modified_at, \
        n.pinned, n.meta_msg_id, n.pin_dirty, n.tags_meta_msg_id, n.tags_dirty";

    /// Every note in `label` and below. Jodd's folder tree is derived by
    /// splitting the label string on '/', with no containment relation in the
    /// data, so "descendant" has to be reconstructed by prefix matching. The
    /// `|| '/'` before the wildcard is load-bearing: a bare LIKE 'Notes/A%'
    /// would also match 'Notes/AB'.
    ///
    /// Read-only. Excludes deleted_pending. Ask Jodd only — `search_notes`
    /// keeps its exact-label semantics for the search box (spec §5.6).
    pub fn list_notes_in_subtree(
        &self,
        account_id: &str,
        label: &str,
    ) -> SqlResult<Vec<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT {} FROM notes n
             WHERE n.account_id = ?1
               AND n.sync_state != 'deleted_pending'
               AND (n.label = ?2 OR n.label LIKE ?2 || '/%')
             ORDER BY {}",
            Self::NOTE_COLS,
            Self::NEWEST_FIRST
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![account_id, label], row_to_note)?;
        rows.collect()
    }

    /// Notes whose body contains at least one checklist row. Feeds
    /// `list_tasks` (jodd-mcp spec §4.6(B)): the SQL `LIKE` pre-filter is
    /// mandatory, not an optimization — the measured production vault has
    /// 6,655 notes and 18MB of body_html in a single account, so parsing
    /// every body to find checkboxes is not viable. `label`, when given, is
    /// the recursive-subtree filter (see `list_notes_in_subtree` / gotcha
    /// #1): `Notes/A` matches itself and every descendant, never the
    /// lexical-prefix sibling `Notes/AB`.
    ///
    /// Read-only. Excludes deleted_pending.
    pub fn list_notes_with_checkboxes(
        &self,
        account_id: &str,
        label: Option<&str>,
    ) -> SqlResult<Vec<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT {} FROM notes n
             WHERE n.account_id = ?1
               AND n.sync_state != 'deleted_pending'
               AND n.body_html LIKE '%type=\"checkbox\"%'
               AND (?2 IS NULL OR n.label = ?2 OR n.label LIKE ?2 || '/%')
             ORDER BY {}",
            Self::NOTE_COLS,
            Self::NEWEST_FIRST
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![account_id, label], row_to_note)?;
        rows.collect()
    }

    /// The `limit` most recently modified notes, newest first.
    ///
    /// Ordered by `last_remote_modified_at` (epoch ms), NEVER by `date` —
    /// `date` is an RFC822 string and sorts by weekday name, putting 2013
    /// above 2020 (spec F4). Falls back to `last_local_modified_at` for rows
    /// whose remote timestamp is NULL (local-only notes never yet pushed).
    pub fn list_recent_notes(
        &self,
        account_id: Option<&str>,
        limit: usize,
        exclude_accounts: &[String],
    ) -> SqlResult<Vec<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let excl = exclude_clause(exclude_accounts, 3);
        let sql = format!(
            "SELECT {} FROM notes n
             WHERE n.sync_state != 'deleted_pending'
               AND (?1 IS NULL OR n.account_id = ?1){excl}
             ORDER BY {}
             LIMIT ?2",
            Self::NOTE_COLS,
            Self::NEWEST_FIRST
        );
        let mut binds: Vec<Box<dyn rusqlite::ToSql>> = vec![
            Box::new(account_id.map(str::to_string)),
            Box::new(limit as i64),
        ];
        for a in exclude_accounts {
            binds.push(Box::new(a.clone()));
        }
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(binds.iter()), row_to_note)?;
        rows.collect()
    }

    /// How many notes a scope contains — the denominator Ask Jodd shows the
    /// user ("6,655 in scope → 400 considered → 9 read"). `label_subtree` is
    /// recursive, matching list_notes_in_subtree.
    pub fn count_notes_in_scope(
        &self,
        account_id: Option<&str>,
        label_subtree: Option<&str>,
        exclude_accounts: &[String],
    ) -> SqlResult<usize> {
        let conn = self.conn.lock().unwrap();
        let excl = exclude_clause(exclude_accounts, 3);
        let sql = format!(
            "SELECT COUNT(*) FROM notes n
             WHERE n.sync_state != 'deleted_pending'
               AND (?1 IS NULL OR n.account_id = ?1)
               AND (?2 IS NULL OR n.label = ?2 OR n.label LIKE ?2 || '/%'){excl}"
        );
        let mut binds: Vec<Box<dyn rusqlite::ToSql>> = vec![
            Box::new(account_id.map(str::to_string)),
            Box::new(label_subtree.map(str::to_string)),
        ];
        for a in exclude_accounts {
            binds.push(Box::new(a.clone()));
        }
        let mut stmt = conn.prepare(&sql)?;
        let n: i64 = stmt.query_row(rusqlite::params_from_iter(binds.iter()), |r| r.get(0))?;
        Ok(n as usize)
    }

    /// One note by identity, or None if it is absent or locally deleted.
    ///
    /// Returns None rather than erroring for deleted_pending: Ask Jodd may
    /// select a note that a sync tick removes moments later, and dropping one
    /// source beats failing the whole turn.
    pub fn note_by_uuid(&self, account_id: &str, uuid: &str) -> SqlResult<Option<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT {} FROM notes n
             WHERE n.account_id = ?1 AND n.uuid = ?2
               AND n.sync_state != 'deleted_pending'",
            Self::NOTE_COLS
        );
        let mut stmt = conn.prepare(&sql)?;
        stmt.query_row(params![account_id, uuid], row_to_note).optional()
    }

    /// Notes that link TO this note via a `[[title]]` wikilink (backlinks).
    /// Matched by this note's normalized title against edges.dst_title.
    pub fn backlinks(&self, account_id: &str, uuid: &str) -> SqlResult<Vec<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let title: Option<String> = conn
            .query_row(
                "SELECT title FROM notes WHERE account_id = ?1 AND uuid = ?2",
                params![account_id, uuid],
                |r| r.get(0),
            )
            .optional()?;
        // Match links that target THIS note by its id (slug links) OR its title
        // (plain links).
        let id8 = uuid_short(uuid);
        let norm_title = normalize_title(&title.unwrap_or_default());
        let sql = format!(
            "SELECT DISTINCT {} FROM edges e
             JOIN notes n ON n.account_id = e.account_id AND n.uuid = e.src_uuid
             WHERE e.account_id = ?1 AND e.rel = 'mentions'
               AND ( (e.dst_id <> '' AND e.dst_id = ?2)
                  OR (e.dst_title <> '' AND e.dst_title = ?3) )
               AND n.uuid != ?4 AND n.sync_state != 'deleted_pending'
             ORDER BY {}",
            Self::NOTE_COLS,
            Self::NEWEST_FIRST
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![account_id, id8, norm_title, uuid], row_to_note)?;
        rows.collect()
    }

    /// Notes this note links TO — its `[[..]]` targets resolved to real notes by
    /// id (slug links) or title (plain links). Unresolved links don't appear.
    pub fn outgoing_links(&self, account_id: &str, uuid: &str) -> SqlResult<Vec<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT DISTINCT {} FROM edges e
             JOIN notes n ON n.account_id = e.account_id AND (
                    (e.dst_id <> '' AND substr(lower(replace(n.uuid, '-', '')), 1, 8) = e.dst_id)
                 OR (e.dst_title <> '' AND lower(trim(n.title)) = e.dst_title) )
             WHERE e.account_id = ?1 AND e.rel = 'mentions' AND e.src_uuid = ?2
               AND n.uuid != ?2 AND n.sync_state != 'deleted_pending'
             ORDER BY {}",
            Self::NOTE_COLS,
            Self::NEWEST_FIRST
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![account_id, uuid], row_to_note)?;
        rows.collect()
    }

    /// URLs cited by this note (`rel='cites'` edges), sorted for stable
    /// display order.
    pub fn citations(&self, account_id: &str, uuid: &str) -> SqlResult<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT dst_id FROM edges WHERE account_id = ?1 AND src_uuid = ?2 AND rel = 'cites' ORDER BY dst_id",
        )?;
        let rows = stmt.query_map(params![account_id, uuid], |r| r.get::<_, String>(0))?;
        rows.collect()
    }

    /// The first note (if any) that already cites `url`, excluding
    /// `exclude_uuid` (used when checking an append target against its own
    /// prior citations so re-appending into a note that already cites the
    /// same URL doesn't self-flag). Backs Extract's duplicate-source warning.
    pub fn find_citation_owner(
        &self,
        account_id: &str,
        url: &str,
        exclude_uuid: Option<&str>,
    ) -> SqlResult<Option<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT DISTINCT {} FROM edges e
             JOIN notes n ON n.account_id = e.account_id AND n.uuid = e.src_uuid
             WHERE e.account_id = ?1 AND e.rel = 'cites' AND e.dst_id = ?2
               AND n.sync_state != 'deleted_pending'
               AND (?3 IS NULL OR n.uuid != ?3)
             LIMIT 1",
            Self::NOTE_COLS
        );
        let mut stmt = conn.prepare(&sql)?;
        stmt.query_row(params![account_id, url, exclude_uuid], row_to_note)
            .optional()
    }

    /// Notes with zero incoming `mentions` edges — nothing else references
    /// them. Backs the "Orphaned" Smart Folder (fully virtual — no `folders`
    /// table row; see design spec decision 4).
    pub fn list_orphaned_notes(&self, account_id: &str) -> SqlResult<Vec<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT {} FROM notes n
             WHERE n.account_id = ?1 AND n.sync_state != 'deleted_pending'
               AND NOT EXISTS (
                 SELECT 1 FROM edges e
                 WHERE e.account_id = n.account_id AND e.rel = 'mentions'
                   AND ( (e.dst_id <> '' AND e.dst_id = substr(lower(replace(n.uuid,'-','')),1,8))
                      OR (e.dst_title <> '' AND e.dst_title = lower(trim(n.title))) )
               )
             ORDER BY {}",
            Self::NOTE_COLS,
            Self::NEWEST_FIRST
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![account_id], row_to_note)?;
        rows.collect()
    }

    /// Notes whose `last_local_modified_at` is older than 30 days — a
    /// hardcoded threshold, no settings UI (design spec decision 3). Backs
    /// the "Stale" Smart Folder.
    pub fn list_stale_notes(&self, account_id: &str) -> SqlResult<Vec<CachedNote>> {
        const STALE_THRESHOLD_MS: i64 = 30 * 24 * 60 * 60 * 1000;
        let conn = self.conn.lock().unwrap();
        let cutoff = now_ms() - STALE_THRESHOLD_MS;
        let sql = format!(
            "SELECT {} FROM notes n
             WHERE n.account_id = ?1 AND n.sync_state != 'deleted_pending'
               AND n.last_local_modified_at < ?2
             ORDER BY n.last_local_modified_at ASC",
            Self::NOTE_COLS
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![account_id, cutoff], row_to_note)?;
        rows.collect()
    }

    /// Autocomplete source for the `[[` link picker: notes whose title matches
    /// `query` (substring, case-insensitive), newest first. Returns (uuid,
    /// title, label) — the frontend builds the slug via note_slug.
    pub fn search_titles(
        &self,
        account_id: &str,
        query: &str,
        limit: i64,
    ) -> SqlResult<Vec<(String, String, String)>> {
        let conn = self.conn.lock().unwrap();
        let like = format!("%{}%", query.trim().replace('%', "\\%").replace('_', "\\_"));
        // Aliased `n` so NEWEST_FIRST (which qualifies its columns) applies.
        let sql = format!(
            "SELECT n.uuid, n.title, n.label FROM notes n
             WHERE n.account_id = ?1 AND n.sync_state != 'deleted_pending'
               AND n.title LIKE ?2 ESCAPE '\\'
             ORDER BY {} LIMIT ?3",
            Self::NEWEST_FIRST
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![account_id, like, limit], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
        rows.collect()
    }

    /// Upsert a fresh-from-Gmail note. Caller's responsibility: only call
    /// this with notes that came from a server fetch, NOT for local edits
    /// (local edits go through `apply_local_edit` so the dirty flag is set).
    /// Marks the row clean — the server state is what we just stored.
    pub fn upsert_from_remote(&self, n: &CachedNote) -> SqlResult<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            // `pinned` deliberately omitted from the ON CONFLICT SET clause:
            // pin is Jodd-local metadata that Gmail/Apple don't carry, so a
            // remote pull must preserve whatever the local replica has. On
            // a fresh INSERT we use the value from the CachedNote (which
            // from_remote stamps as false), so new-to-us notes arrive
            // unpinned, as expected.
            // `pinned`, `tags_meta_msg_id`, `tags_dirty` deliberately omitted
            // from ON CONFLICT SET: all Jodd-local, never authoritative from
            // remote. On a fresh INSERT (new uuid) they come from the
            // CachedNote (from_remote stamps tags_meta_msg_id=None,
            // tags_dirty=false), so new-to-us notes arrive untagged-locally
            // — `apply_remote_tags` is what writes the sidecar-derived state
            // afterward, and only when the local row isn't tags_dirty.
            "INSERT INTO notes (uuid, account_id, id, title, body_html, date,
                x_mail_created_date, label, local_version, remote_version,
                sync_state, last_synced_at, last_local_modified_at,
                last_remote_modified_at, pinned, meta_msg_id, pin_dirty, tags_meta_msg_id, tags_dirty)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
             ON CONFLICT(uuid, account_id) DO UPDATE SET
                id = excluded.id,
                title = excluded.title,
                body_html = excluded.body_html,
                date = excluded.date,
                x_mail_created_date = excluded.x_mail_created_date,
                label = excluded.label,
                remote_version = excluded.remote_version,
                sync_state = excluded.sync_state,
                last_synced_at = excluded.last_synced_at,
                last_remote_modified_at = excluded.last_remote_modified_at
             ",
            params![
                n.uuid,
                n.account_id,
                n.id,
                n.title,
                n.body_html,
                n.date,
                n.x_mail_created_date,
                n.label,
                n.local_version,
                n.remote_version,
                n.sync_state.as_str(),
                n.last_synced_at,
                n.last_local_modified_at,
                n.last_remote_modified_at,
                n.pinned as i64,
                n.meta_msg_id,
                n.pin_dirty as i64,
                n.tags_meta_msg_id,
                n.tags_dirty as i64,
            ],
        )?;
        // Tombstone restoration: if this uuid had tags wiped by a past
        // prune_clean and is now reappearing, bring those tags back. INSERT
        // OR IGNORE so a tag that ALSO survived as a live note_tags row
        // (shouldn't happen, but defensive) doesn't double-insert. The
        // DELETE empties the tombstones for this uuid either way — either
        // the tags are now live again, or they were already live.
        tx.execute(
            "INSERT OR IGNORE INTO note_tags (account_id, uuid, tag)
             SELECT account_id, uuid, tag FROM tag_tombstones
             WHERE account_id = ?1 AND uuid = ?2",
            params![n.account_id, n.uuid],
        )?;
        tx.execute(
            "DELETE FROM tag_tombstones WHERE account_id = ?1 AND uuid = ?2",
            params![n.account_id, n.uuid],
        )?;
        fts_index_conn(&tx, &n.uuid, &n.account_id, &n.title, &n.body_html)?;
        reconcile_tags_from_body_conn(&tx, &n.account_id, &n.uuid, &n.body_html)?;
        reconcile_edges_from_body_conn(&tx, &n.account_id, &n.uuid, &n.label, &n.body_html)?;
        reconcile_citations_from_body_conn(&tx, &n.account_id, &n.uuid, &n.body_html)?;
        tx.commit()?;
        Ok(())
    }

    /// Persist one inline attachment for a note. Upsert (insert-or-replace by
    /// content_id) — additive: a read never DELETES attachments, because Gmail's
    /// eventual consistency could transiently omit a part and we must not drop
    /// the cached bytes. Orphan rows (cid no longer referenced by any body) are
    /// harmless and pruned separately if needed.
    pub fn upsert_attachment(
        &self,
        account_id: &str,
        note_uuid: &str,
        att: &crate::backend::gmail::wire::Attachment,
    ) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO attachments
                (account_id, note_uuid, content_id, mime_type, filename, x_apple_part_url, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(account_id, note_uuid, content_id) DO UPDATE SET
                mime_type = excluded.mime_type,
                filename = excluded.filename,
                x_apple_part_url = excluded.x_apple_part_url,
                data = excluded.data",
            params![
                account_id,
                note_uuid,
                att.content_id,
                att.mime_type,
                att.filename,
                att.x_apple_part_url,
                att.data,
            ],
        )?;
        Ok(())
    }

    /// Load every stored attachment for a note (used by the save path to
    /// re-emit them, and later by the editor to render them).
    pub fn list_attachments(
        &self,
        account_id: &str,
        note_uuid: &str,
    ) -> SqlResult<Vec<crate::backend::gmail::wire::Attachment>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT content_id, mime_type, filename, x_apple_part_url, data
             FROM attachments WHERE account_id = ?1 AND note_uuid = ?2
             ORDER BY content_id",
        )?;
        let rows = stmt.query_map(params![account_id, note_uuid], |r| {
            Ok(crate::backend::gmail::wire::Attachment {
                content_id: r.get(0)?,
                mime_type: r.get(1)?,
                filename: r.get(2)?,
                x_apple_part_url: r.get(3)?,
                data: r.get(4)?,
            })
        })?;
        rows.collect()
    }

    /// Apply a local edit (from the user typing). Bumps local_version,
    /// transitions sync_state per the rules. last_local_modified_at = now.
    ///
    /// State transitions:
    ///   clean        → dirty           (normal edit)
    ///   pull_needed  → conflict        (remote already changed; editing makes it a conflict)
    ///   conflict     → dirty           (user is resolving the conflict by editing; push intent)
    ///   dirty        → dirty           (no change, keep pushing)
    ///   deleted_pending → deleted_pending (don't resurrect via edit)
    fn apply_local_edit_inner(
        &self,
        uuid: &str,
        account_id: &str,
        title: &str,
        body_html: &str,
        label: &str,
        expected_local_version: Option<i64>,
    ) -> SqlResult<bool> {
        let conn = self.conn.lock().unwrap();
        // Capture the note's previous title before the UPDATE so we can detect a
        // title-slug change and rewrite inbound slug-links (see below). A missing
        // row (brand-new uuid) means no prior title and no inbound links yet.
        let prev_title: Option<String> = conn
            .query_row(
                "SELECT title FROM notes WHERE uuid = ?1 AND account_id = ?2",
                params![uuid, account_id],
                |r| r.get(0),
            )
            .optional()?;
        let rows_changed = match expected_local_version {
            None => conn.execute(
                "UPDATE notes
                 SET title = ?1, body_html = ?2, label = ?3,
                     local_version = local_version + 1,
                     sync_state = CASE sync_state
                         WHEN 'clean' THEN 'dirty'
                         WHEN 'pull_needed' THEN 'conflict'
                         WHEN 'conflict' THEN 'dirty'
                         ELSE sync_state
                     END,
                     last_local_modified_at = ?4
                 WHERE uuid = ?5 AND account_id = ?6",
                params![title, body_html, label, now_ms(), uuid, account_id],
            )?,
            // Same UPDATE, plus a compare-and-swap on local_version — the same
            // shape mark_pushed already uses to refuse a stale write instead of
            // silently overwriting a concurrent writer's edit.
            Some(expected) => conn.execute(
                "UPDATE notes
                 SET title = ?1, body_html = ?2, label = ?3,
                     local_version = local_version + 1,
                     sync_state = CASE sync_state
                         WHEN 'clean' THEN 'dirty'
                         WHEN 'pull_needed' THEN 'conflict'
                         WHEN 'conflict' THEN 'dirty'
                         ELSE sync_state
                     END,
                     last_local_modified_at = ?4
                 WHERE uuid = ?5 AND account_id = ?6 AND local_version = ?7",
                params![title, body_html, label, now_ms(), uuid, account_id, expected],
            )?,
        };
        if rows_changed == 0 {
            return Ok(false);
        }
        fts_index_conn(&conn, uuid, account_id, title, body_html)?;
        reconcile_tags_from_body_conn(&conn, account_id, uuid, body_html)?;
        reconcile_edges_from_body_conn(&conn, account_id, uuid, label, body_html)?;
        reconcile_citations_from_body_conn(&conn, account_id, uuid, body_html)?;
        // If the title-slug actually changed, refresh the DISPLAYED text of every
        // slug-form `[[*-uuid8]]` link pointing at this note in OTHER notes'
        // bodies (resolution is by uuid8 and never broke; only the text was
        // stale). Synchronous-to-disk with the edit (local-first doctrine); the
        // carriers go clean→dirty so the worker re-syncs them.
        if let Some(prev) = prev_title {
            if slugify(&prev) != slugify(title) {
                rewrite_links_to_renamed_note_conn(
                    &conn,
                    account_id,
                    uuid,
                    &note_slug(title, uuid),
                )?;
            }
        }
        Ok(true)
    }

    pub fn apply_local_edit(
        &self,
        uuid: &str,
        account_id: &str,
        title: &str,
        body_html: &str,
        label: &str,
    ) -> SqlResult<()> {
        self.apply_local_edit_inner(uuid, account_id, title, body_html, label, None)
            .map(|_| ())
    }

    /// Same as `apply_local_edit`, but refuses (returns `Ok(false)`, row
    /// untouched) if `expected_local_version` no longer matches the row's
    /// current `local_version` — a concurrent writer (the App's own editor,
    /// `jodd-mcp`, Extract, auto-link) landed an edit first. Same
    /// compare-and-swap shape as `mark_pushed`
    /// ([db.rs:1367](../../../src-tauri/src/db.rs#L1367)). Callers that can
    /// safely recompute their write should re-read the row and retry; the
    /// one caller that can't (a full-body editor replace) should surface the
    /// conflict instead of retrying blindly. See `cross_process_safety_tests`.
    pub fn apply_local_edit_versioned(
        &self,
        uuid: &str,
        account_id: &str,
        title: &str,
        body_html: &str,
        label: &str,
        expected_local_version: i64,
    ) -> SqlResult<bool> {
        self.apply_local_edit_inner(uuid, account_id, title, body_html, label, Some(expected_local_version))
    }

    /// Insert a brand-new local note. UUID supplied by the caller (real
    /// Apple-format UUID generated frontend or backend). Starts dirty —
    /// will be pushed on first sync.
    pub fn insert_local_new(&self, n: &CachedNote) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO notes (uuid, account_id, id, title, body_html, date,
                x_mail_created_date, label, local_version, remote_version,
                sync_state, last_synced_at, last_local_modified_at,
                last_remote_modified_at, pinned, meta_msg_id, pin_dirty, tags_meta_msg_id, tags_dirty)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
            params![
                n.uuid,
                n.account_id,
                n.id,
                n.title,
                n.body_html,
                n.date,
                n.x_mail_created_date,
                n.label,
                n.local_version,
                n.remote_version,
                n.sync_state.as_str(),
                n.last_synced_at,
                n.last_local_modified_at,
                n.last_remote_modified_at,
                n.pinned as i64,
                n.meta_msg_id,
                n.pin_dirty as i64,
                n.tags_meta_msg_id,
                n.tags_dirty as i64,
            ],
        )?;
        fts_index_conn(&conn, &n.uuid, &n.account_id, &n.title, &n.body_html)?;
        reconcile_tags_from_body_conn(&conn, &n.account_id, &n.uuid, &n.body_html)?;
        reconcile_edges_from_body_conn(&conn, &n.account_id, &n.uuid, &n.label, &n.body_html)?;
        reconcile_citations_from_body_conn(&conn, &n.account_id, &n.uuid, &n.body_html)?;
        Ok(())
    }

    /// After a successful push to Gmail, record the new remote id, the exact
    /// date/body we sent, and mark clean.
    ///
    /// Why date/body here: the pushed message becomes Gmail's current state for
    /// this UUID. Future pulls compare cached `date` against fresh remote Date
    /// headers — leaving date stale (Apple's original) made dedupe pick wrong
    /// versions and broke "Last updated" in the UI. Same for body_html: the
    /// cache-aware fan-out reuses cached body when an id is in the listing, so
    /// stale body bytes would survive every pull. Now both reflect what we just
    /// put on Gmail.
    ///
    /// Applies a completed push, but only if the row's `local_version` still
    /// matches what the caller read before pushing. If a second writer (the
    /// MCP server, or this same worker on a later tick) edited the note in
    /// between, `local_version` has moved past `expected_local_version` and
    /// this UPDATE is a no-op — the row stays `dirty` with the newer edit
    /// intact, and the next push cycle picks it up. Without this guard a
    /// stale push would stamp `sync_state = 'clean'` over unsent local
    /// changes, silently dropping them.
    pub fn mark_pushed(
        &self,
        uuid: &str,
        account_id: &str,
        new_remote_id: &str,
        pushed_date: &str,
        pushed_body_html: &str,
        expected_local_version: i64,
    ) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        let now = now_ms();
        conn.execute(
            "UPDATE notes
             SET id = ?1, remote_version = ?1,
                 date = ?5,
                 body_html = ?6,
                 sync_state = 'clean',
                 last_synced_at = ?2,
                 last_remote_modified_at = ?2
             WHERE uuid = ?3 AND account_id = ?4 AND local_version = ?7",
            params![new_remote_id, now, uuid, account_id, pushed_date, pushed_body_html,
                    expected_local_version],
        )?;
        Ok(())
    }

    /// Drop a row entirely. Used when the user deletes locally AND we've
    /// successfully trashed the message on Gmail.
    pub fn delete(&self, uuid: &str, account_id: &str) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM notes WHERE uuid = ?1 AND account_id = ?2",
            params![uuid, account_id],
        )?;
        // Tags are Jodd-local metadata keyed by (account_id, uuid); drop them
        // with the note so they don't linger as orphans.
        conn.execute(
            "DELETE FROM note_tags WHERE uuid = ?1 AND account_id = ?2",
            params![uuid, account_id],
        )?;
        // Attachment BLOBs are keyed by (account_id, note_uuid); drop them too
        // so deleting a note reclaims its image bytes instead of leaking them.
        conn.execute(
            "DELETE FROM attachments WHERE note_uuid = ?1 AND account_id = ?2",
            params![uuid, account_id],
        )?;
        fts_delete_conn(&conn, uuid, account_id)?;
        conn.execute(
            "DELETE FROM edges WHERE account_id = ?1 AND src_uuid = ?2",
            params![account_id, uuid],
        )?;
        Ok(())
    }

    /// Wipe every cached row for one account — both notes and folders.
    /// Called when an account is removed via the UI or its Keychain entry
    /// is gone (auth-loss recovery): the rows are useless without a token
    /// to reconcile against and shouldn't keep occupying disk or leaking
    /// note bodies after the user thought they "signed out".
    ///
    /// Returns (notes_deleted, folders_deleted) for logging. Best-effort:
    /// if one DELETE fails the other is still attempted, because partial
    /// cleanup beats no cleanup when the user's intent is "remove this".
    pub fn delete_account(&self, account_id: &str) -> SqlResult<(usize, usize)> {
        let conn = self.conn.lock().unwrap();
        let notes_deleted = conn
            .execute("DELETE FROM notes WHERE account_id = ?1", params![account_id])
            .unwrap_or(0);
        let folders_deleted = conn
            .execute("DELETE FROM folders WHERE account_id = ?1", params![account_id])
            .unwrap_or(0);
        // Tag tables aren't covered by the (notes, folders) return tuple so
        // failures here are silent — best-effort wipe so account removal
        // doesn't leave Jodd-local metadata behind on disk after the user
        // intended "remove this account". Without these, note_tags rows
        // also block future re-add: a re-imported note sharing the same
        // uuid would hit INSERT OR IGNORE in add_tag and the new tag would
        // silently no-op against the zombie row.
        let _ = conn.execute(
            "DELETE FROM note_tags WHERE account_id = ?1",
            params![account_id],
        );
        let _ = conn.execute(
            "DELETE FROM tag_tombstones WHERE account_id = ?1",
            params![account_id],
        );
        // Reclaim attachment BLOBs for the removed account (can be large).
        let _ = conn.execute(
            "DELETE FROM attachments WHERE account_id = ?1",
            params![account_id],
        );
        let _ = conn.execute(
            "DELETE FROM notes_fts WHERE account_id = ?1",
            params![account_id],
        );
        let _ = conn.execute(
            "DELETE FROM edges WHERE account_id = ?1",
            params![account_id],
        );
        Ok((notes_deleted, folders_deleted))
    }

    /// Find all notes still pending a push. Background worker reads this
    /// and tries each one. Order by last_local_modified_at so the user's
    /// most-recent edits go first.
    pub fn list_dirty(&self) -> SqlResult<Vec<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT uuid, account_id, id, title, body_html, date, x_mail_created_date,
                    label, local_version, remote_version, sync_state,
                    last_synced_at, last_local_modified_at, last_remote_modified_at,
                    pinned, meta_msg_id, pin_dirty, tags_meta_msg_id, tags_dirty
             FROM notes WHERE sync_state = 'dirty'
             ORDER BY last_local_modified_at DESC",
        )?;
        let rows = stmt.query_map([], row_to_note)?;
        rows.collect()
    }

    /// Batch label update — moves N notes to a new label in one transaction.
    /// Each touched row transitions through the same state machine as
    /// `apply_local_edit` (clean → dirty, pull_needed → conflict, etc.).
    /// Local_version bumps and last_local_modified_at refresh for every row.
    ///
    /// Returns the count of rows actually updated (silently skips uuids
    /// the cache doesn't know about, which keeps the call idempotent if
    /// the frontend optimistically passed a uuid that hasn't synced yet).
    pub fn move_notes_batch(
        &self,
        account_id: &str,
        uuids: &[String],
        new_label: &str,
    ) -> SqlResult<usize> {
        if uuids.is_empty() {
            return Ok(0);
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let now = now_ms();
        let mut touched = 0usize;
        {
            let mut stmt = tx.prepare(
                "UPDATE notes
                 SET label = ?1,
                     local_version = local_version + 1,
                     sync_state = CASE sync_state
                         WHEN 'clean' THEN 'dirty'
                         WHEN 'pull_needed' THEN 'conflict'
                         WHEN 'conflict' THEN 'dirty'
                         ELSE sync_state
                     END,
                     last_local_modified_at = ?2
                 WHERE uuid = ?3 AND account_id = ?4",
            )?;
            for uuid in uuids {
                touched += stmt.execute(params![new_label, now, uuid, account_id])?;
            }
        }
        tx.commit()?;
        Ok(touched)
    }

    /// Batch delete — marks N notes `deleted_pending` in one transaction.
    /// Same per-row semantics as `mark_deleted`: row stays in the cache
    /// (so the worker can retry the Gmail trash if offline) but the
    /// frontend treats it as gone via the deleted_pending filter in
    /// `list_notes`. Returns the count of rows actually updated.
    pub fn delete_notes_batch(
        &self,
        account_id: &str,
        uuids: &[String],
    ) -> SqlResult<usize> {
        if uuids.is_empty() {
            return Ok(0);
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let now = now_ms();
        let mut touched = 0usize;
        {
            let mut stmt = tx.prepare(
                "UPDATE notes SET sync_state = 'deleted_pending',
                    last_local_modified_at = ?1
                 WHERE uuid = ?2 AND account_id = ?3",
            )?;
            for uuid in uuids {
                touched += stmt.execute(params![now, uuid, account_id])?;
            }
        }
        {
            // Same as mark_deleted: drop each deleted note's Jodd-local tags so
            // the sidebar reflects the deletion immediately.
            let mut tstmt = tx.prepare(
                "DELETE FROM note_tags WHERE uuid = ?1 AND account_id = ?2",
            )?;
            for uuid in uuids {
                tstmt.execute(params![uuid, account_id])?;
            }
        }
        tx.commit()?;
        Ok(touched)
    }

    /// Toggle the pin column for one note. Bumps `last_local_modified_at`
    /// so the UI re-sorts (NoteList sorts by pinned DESC, then date DESC,
    /// and `date` mirrors `last_local_modified_at` on edits). Marks
    /// `pin_dirty = 1` so the sync worker propagates the change to a
    /// Jodd-managed sidecar message in the account's meta_label — that's
    /// what makes pin state visible to other Jodd instances sharing the
    /// same Gmail account. Does NOT touch `sync_state`: pin push is
    /// orthogonal to content push and runs through its own worker path.
    /// Idempotent against value; still marks pin_dirty so a same-value
    /// retry after a previous push failure re-tries the push.
    pub fn set_pin(&self, uuid: &str, account_id: &str, pinned: bool) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE notes SET pinned = ?1, pin_dirty = 1, last_local_modified_at = ?2
             WHERE uuid = ?3 AND account_id = ?4",
            params![pinned as i64, now_ms(), uuid, account_id],
        )?;
        Ok(())
    }

    /// Batch pin/unpin — same shape as `set_pin` over N uuids in one
    /// transaction. Marks every touched row `pin_dirty = 1`. The worker
    /// pushes each sidecar independently; rows that fail to push retry
    /// on the next tick without rolling back the SQLite write.
    pub fn set_pin_batch(
        &self,
        account_id: &str,
        uuids: &[String],
        pinned: bool,
    ) -> SqlResult<usize> {
        if uuids.is_empty() {
            return Ok(0);
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let now = now_ms();
        let mut touched = 0usize;
        {
            let mut stmt = tx.prepare(
                "UPDATE notes SET pinned = ?1, pin_dirty = 1, last_local_modified_at = ?2
                 WHERE uuid = ?3 AND account_id = ?4",
            )?;
            for uuid in uuids {
                touched += stmt.execute(params![pinned as i64, now, uuid, account_id])?;
            }
        }
        tx.commit()?;
        Ok(touched)
    }

    /// Apply remote pin state. Called by the pull-side reconciliation
    /// when `list_notes` fetches sidecars from the meta_label and learns
    /// the authoritative pin state from Gmail. CRITICAL: this does NOT
    /// set pin_dirty — the remote IS the authority for what we just
    /// observed, so writing it back would be a write-amplification loop
    /// where two Jodd instances perpetually push the same value at each
    /// other. Also updates `meta_msg_id` so the next local pin toggle
    /// knows which sidecar to trash. last_local_modified_at intentionally
    /// NOT bumped: that timestamp drives the UI date column, and a remote
    /// pin observation isn't a local edit.
    pub fn apply_remote_pin(
        &self,
        uuid: &str,
        account_id: &str,
        pinned: bool,
        meta_msg_id: &str,
    ) -> SqlResult<usize> {
        let conn = self.conn.lock().unwrap();
        // Only apply when not locally pin-dirty. If the user has flipped
        // pin since the last sync, the local intent wins until the worker
        // pushes — applying remote here would clobber the user's recent
        // toggle. Worker push will overwrite the remote sidecar shortly.
        let touched = conn.execute(
            "UPDATE notes SET pinned = ?1, meta_msg_id = ?2
             WHERE uuid = ?3 AND account_id = ?4 AND pin_dirty = 0",
            params![pinned as i64, meta_msg_id, uuid, account_id],
        )?;
        Ok(touched)
    }

    /// Used during pull-side reconciliation to drop ghost sidecars: when
    /// the meta_label listing no longer contains a sidecar we previously
    /// observed, AND the local row isn't pin_dirty (i.e. the user hasn't
    /// just re-pinned), clear the local pin. Sidecar absence is the
    /// authoritative signal that the note is unpinned everywhere.
    ///
    /// The bulk variant — pass every uuid that DID appear in the latest
    /// meta_label fetch; this clears pin on every clean row not in that
    /// set. Returns the count cleared.
    pub fn clear_pins_not_in(
        &self,
        account_id: &str,
        keep_uuids: &[String],
    ) -> SqlResult<usize> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS _keep_pin_uuids (uuid TEXT PRIMARY KEY);
             DELETE FROM _keep_pin_uuids;",
        )?;
        {
            let mut ins = conn.prepare("INSERT OR IGNORE INTO _keep_pin_uuids (uuid) VALUES (?1)")?;
            for u in keep_uuids {
                ins.execute(params![u])?;
            }
        }
        let cleared = conn.execute(
            "UPDATE notes
             SET pinned = 0, meta_msg_id = NULL
             WHERE account_id = ?1
               AND pin_dirty = 0
               AND pinned = 1
               AND uuid NOT IN (SELECT uuid FROM _keep_pin_uuids)",
            params![account_id],
        )?;
        Ok(cleared)
    }

    /// Rows pending pin sidecar push. Worker drains these alongside
    /// (independently of) the content-dirty list. Cheap because the
    /// partial index `idx_notes_pin_dirty` covers exactly the candidates.
    pub fn list_pin_dirty(&self) -> SqlResult<Vec<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT uuid, account_id, id, title, body_html, date, x_mail_created_date,
                    label, local_version, remote_version, sync_state,
                    last_synced_at, last_local_modified_at, last_remote_modified_at,
                    pinned, meta_msg_id, pin_dirty, tags_meta_msg_id, tags_dirty
             FROM notes WHERE pin_dirty = 1
             ORDER BY last_local_modified_at DESC",
        )?;
        let rows = stmt.query_map([], row_to_note)?;
        rows.collect()
    }

    /// After a successful sidecar push, clear pin_dirty and record the
    /// new sidecar message id (or NULL if the sidecar was trashed
    /// because pinned=false). Returns rows touched (0 if the local
    /// pin state changed between push start and push completion — the
    /// pin_dirty flag will have been re-set, and the worker will pick
    /// it up on the next tick).
    pub fn mark_pin_pushed(
        &self,
        uuid: &str,
        account_id: &str,
        new_meta_msg_id: Option<&str>,
        pushed_pinned: bool,
    ) -> SqlResult<usize> {
        let conn = self.conn.lock().unwrap();
        // Only clear pin_dirty if the SQLite-side `pinned` still equals
        // the value we just pushed. If it changed (user re-toggled
        // mid-push) leave pin_dirty=1 so the worker re-pushes.
        let touched = conn.execute(
            "UPDATE notes
             SET meta_msg_id = ?1,
                 pin_dirty = 0
             WHERE uuid = ?2 AND account_id = ?3 AND pinned = ?4",
            params![new_meta_msg_id, uuid, account_id, pushed_pinned as i64],
        )?;
        Ok(touched)
    }

    // ─── Tag sync (mirrors pin sync above) ────────────────────────────────
    //
    // Pin is binary (sidecar exists = pinned). Tags are a variable-length
    // set so the sidecar carries `{"tags":[…]}` and the read path fetches
    // bodies, not just subjects. Otherwise the local-wins doctrine, the
    // partial-index drain, and the "remote observation doesn't bump
    // last_local_modified_at" rules all carry over unchanged.

    /// Mark a note's tag set as needing a sidecar push. Called from
    /// add_tag / remove_tag / rename_tag / delete_tag at the Tauri command
    /// layer in lib.rs. Idempotent: a no-op if already dirty. Does NOT bump
    /// last_local_modified_at — tag edits aren't content edits and we don't
    /// want them to re-order the NoteList.
    pub fn set_tags_dirty(&self, account_id: &str, uuid: &str) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE notes SET tags_dirty = 1
             WHERE uuid = ?1 AND account_id = ?2",
            params![uuid, account_id],
        )?;
        Ok(())
    }

    /// Mark every note in an account as tags_dirty — used by rename_tag /
    /// delete_tag at the lib.rs level, which mutate `note_tags` globally
    /// (not per-uuid) and need every affected sidecar re-pushed. We
    /// over-mark: rows that didn't actually carry the renamed/deleted tag
    /// will end up pushing an unchanged set. The waste is one redundant
    /// sidecar insert per untouched note — acceptable in exchange for not
    /// having to compute the precise affected set up-front. The 0-tag
    /// case (a note that has no tags) is handled by push_one_tag_set:
    /// empty set + no existing sidecar = no work.
    pub fn set_all_tags_dirty(&self, account_id: &str, tag: &str) -> SqlResult<usize> {
        let conn = self.conn.lock().unwrap();
        let touched = conn.execute(
            "UPDATE notes SET tags_dirty = 1
             WHERE account_id = ?1
               AND uuid IN (SELECT uuid FROM note_tags
                            WHERE account_id = ?1 AND tag = ?2)",
            params![account_id, tag],
        )?;
        Ok(touched)
    }

    /// Apply a remote tag set to the local replica. Called by the pull-side
    /// reconciliation when sync_tag_state lists sidecars and reads each
    /// body. CRITICAL: does NOT set tags_dirty — the remote IS the authority
    /// for what we just observed. Also does NOT bump last_local_modified_at
    /// (same reason as apply_remote_pin).
    ///
    /// Skips if the local row is tags_dirty: the user has changed tags
    /// since the last push, local intent wins until the worker pushes the
    /// override. Returns rows touched (0 if skipped due to local dirty).
    ///
    /// Reconciliation is all-or-nothing: we delete every local tag for the
    /// uuid and insert the remote set. The note_tags table is the canonical
    /// local view; trying to compute add/remove deltas adds bugs without
    /// reducing wall-clock noticeably.
    pub fn apply_remote_tags(
        &self,
        account_id: &str,
        uuid: &str,
        tags: &[String],
        sidecar_msg_id: &str,
    ) -> SqlResult<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        // Local-wins guard — bail without touching anything if dirty.
        let dirty: i64 = tx.query_row(
            "SELECT COALESCE(tags_dirty, 0) FROM notes
             WHERE uuid = ?1 AND account_id = ?2",
            params![uuid, account_id],
            |r| r.get(0),
        ).unwrap_or(0);
        if dirty != 0 {
            return Ok(0);
        }
        tx.execute(
            "DELETE FROM note_tags WHERE account_id = ?1 AND uuid = ?2",
            params![account_id, uuid],
        )?;
        {
            let mut ins = tx.prepare(
                "INSERT OR IGNORE INTO note_tags (account_id, uuid, tag)
                 VALUES (?1, ?2, ?3)",
            )?;
            for t in tags {
                ins.execute(params![account_id, uuid, t])?;
            }
        }
        // Also drop any tombstones for this uuid — the remote sidecar is
        // now the authority. If a tag was tombstoned locally but not in
        // the sidecar, the remote didn't have it, so the tombstone is
        // moot.
        tx.execute(
            "DELETE FROM tag_tombstones WHERE account_id = ?1 AND uuid = ?2",
            params![account_id, uuid],
        )?;
        let touched = tx.execute(
            "UPDATE notes SET tags_meta_msg_id = ?1
             WHERE uuid = ?2 AND account_id = ?3",
            params![sidecar_msg_id, uuid, account_id],
        )?;
        tx.commit()?;
        Ok(touched)
    }

    /// Pull-side cleanup mirror of `clear_pins_not_in`: when sync_tag_state
    /// observes a uuid we previously had a sidecar for is gone from the
    /// meta_label listing, drop the local tags (assuming we're not dirty —
    /// the user might have just tagged something offline).
    ///
    /// Note we don't tombstone here: a remote-driven clear is the
    /// authoritative "this note has no tags anywhere" signal. Tombstones
    /// exist for the prune_clean race where the NOTE row vanished; this
    /// is the opposite case (note row alive, sidecar gone).
    pub fn clear_tags_not_in(
        &self,
        account_id: &str,
        keep_uuids: &[String],
    ) -> SqlResult<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS _keep_tag_uuids (uuid TEXT PRIMARY KEY);
             DELETE FROM _keep_tag_uuids;",
        )?;
        {
            let mut ins = tx.prepare("INSERT OR IGNORE INTO _keep_tag_uuids (uuid) VALUES (?1)")?;
            for u in keep_uuids {
                ins.execute(params![u])?;
            }
        }
        // Subquery target: rows that (a) had a sidecar (tags_meta_msg_id
        // IS NOT NULL — proves we believed there were tags), (b) aren't
        // locally dirty, (c) whose uuid didn't appear in the latest fetch.
        // Bulk-delete their note_tags + clear the sidecar id.
        let cleared = tx.execute(
            "DELETE FROM note_tags
             WHERE account_id = ?1
               AND uuid IN (SELECT uuid FROM notes
                            WHERE account_id = ?1
                              AND tags_meta_msg_id IS NOT NULL
                              AND tags_dirty = 0
                              AND uuid NOT IN (SELECT uuid FROM _keep_tag_uuids))",
            params![account_id],
        )?;
        tx.execute(
            "UPDATE notes SET tags_meta_msg_id = NULL
             WHERE account_id = ?1
               AND tags_meta_msg_id IS NOT NULL
               AND tags_dirty = 0
               AND uuid NOT IN (SELECT uuid FROM _keep_tag_uuids)",
            params![account_id],
        )?;
        tx.commit()?;
        Ok(cleared)
    }

    /// Rows pending tag sidecar push. Drained by the worker after pin
    /// (lowest priority — tag sync is UX-only, never blocks content or
    /// delete propagation). Cheap because `idx_notes_tags_dirty` covers
    /// exactly the candidate set.
    pub fn list_tags_dirty(&self) -> SqlResult<Vec<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT uuid, account_id, id, title, body_html, date, x_mail_created_date,
                    label, local_version, remote_version, sync_state,
                    last_synced_at, last_local_modified_at, last_remote_modified_at,
                    pinned, meta_msg_id, pin_dirty, tags_meta_msg_id, tags_dirty
             FROM notes WHERE tags_dirty = 1
             ORDER BY last_local_modified_at DESC",
        )?;
        let rows = stmt.query_map([], row_to_note)?;
        rows.collect()
    }

    /// After a successful tag sidecar push, clear tags_dirty and record
    /// the new sidecar message id (or NULL if the sidecar was trashed
    /// because the tag set became empty). Mirror of `mark_pin_pushed`.
    ///
    /// We can't reliably guard "pushed value still equals current value"
    /// like mark_pin_pushed does for `pinned`, because the tag set is
    /// many-row state in note_tags. Instead we accept the small window:
    /// if the user edited tags during push, tags_dirty will be re-set
    /// (the new add/remove call set_tags_dirty after we cleared it would
    /// race, but ordering is set_tags_dirty BEFORE the push completes →
    /// we'd see tags_dirty=1 on the next worker tick and re-push).
    pub fn mark_tags_pushed(
        &self,
        uuid: &str,
        account_id: &str,
        new_meta_msg_id: Option<&str>,
    ) -> SqlResult<usize> {
        let conn = self.conn.lock().unwrap();
        let touched = conn.execute(
            "UPDATE notes
             SET tags_meta_msg_id = ?1, tags_dirty = 0
             WHERE uuid = ?2 AND account_id = ?3",
            params![new_meta_msg_id, uuid, account_id],
        )?;
        Ok(touched)
    }

    /// Snapshot the current tag list for a uuid, for the worker's sidecar
    /// payload. Returns the tags sorted alphabetically so the JSON body is
    /// deterministic — two pushes with the same tag set produce identical
    /// payloads, which helps with debugging and with any future "did the
    /// remote actually change?" checks.
    pub fn list_tags_for(&self, account_id: &str, uuid: &str) -> SqlResult<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT tag FROM note_tags
             WHERE account_id = ?1 AND uuid = ?2
             ORDER BY tag",
        )?;
        let rows = stmt.query_map(params![account_id, uuid], |r| r.get::<_, String>(0))?;
        rows.collect()
    }

    /// Mark a note as in conflict. The original row's content is PRESERVED —
    /// conflict is just metadata; the user's local edits are not touched.
    /// The remote version lives separately as a duplicate row (see lib.rs'
    /// `handle_conflict_detection`) so the user can compare and choose.
    pub fn flag_conflict(&self, uuid: &str, account_id: &str) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE notes SET sync_state = 'conflict' WHERE uuid = ?1 AND account_id = ?2",
            params![uuid, account_id],
        )?;
        Ok(())
    }

    /// Mark a note for deletion. Row stays in the cache (so we can retry
    /// the trash if the network is down), but the frontend should treat
    /// `deleted_pending` rows as gone. After the sync worker successfully
    /// trashes the message on Gmail, it calls `delete()` to remove the row.
    pub fn mark_deleted(&self, uuid: &str, account_id: &str) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE notes SET sync_state = 'deleted_pending', last_local_modified_at = ?1
             WHERE uuid = ?2 AND account_id = ?3",
            params![now_ms(), uuid, account_id],
        )?;
        // Drop the note's tags now so the sidebar tag counts update the moment
        // the user deletes — the row itself lingers as deleted_pending until the
        // worker trashes it on Gmail, but its tags are Jodd-local and gone.
        conn.execute(
            "DELETE FROM note_tags WHERE uuid = ?1 AND account_id = ?2",
            params![uuid, account_id],
        )?;
        Ok(())
    }

    /// Prune `clean` rows for an account+label whose uuid isn't in `keep_uuids`.
    /// Used after a scoped fetch (list_notes_in_folder) — the fetch is
    /// authoritative for ONE label only, so we prune within that label
    /// rather than across the whole account.
    pub fn prune_clean_in_label(
        &self,
        account_id: &str,
        label: &str,
        keep_uuids: &[String],
    ) -> SqlResult<usize> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS _keep_uuids_label (uuid TEXT PRIMARY KEY);
             DELETE FROM _keep_uuids_label;",
        )?;
        {
            let mut ins = conn.prepare("INSERT OR IGNORE INTO _keep_uuids_label (uuid) VALUES (?1)")?;
            for u in keep_uuids {
                ins.execute(params![u])?;
            }
        }
        let deleted = conn.execute(
            "DELETE FROM notes
             WHERE account_id = ?1
               AND label = ?2
               AND sync_state = 'clean'
               AND uuid NOT IN (SELECT uuid FROM _keep_uuids_label)",
            params![account_id, label],
        )?;
        Ok(deleted)
    }

    /// Prune `clean` rows for an account whose uuid isn't in `keep_uuids`.
    /// Used after a full list_notes sweep to drop entries that were deleted
    /// on the remote (Gmail / Apple Notes) while we weren't looking.
    ///
    /// Safety:
    ///   - Only touches `clean` rows. Dirty / DeletedPending / Conflict rows
    ///     have pending local intent and survive — they're the rows the user
    ///     has been editing or deleting locally and we haven't reconciled yet.
    ///   - The caller should only invoke this when the fetch is KNOWN
    ///     complete (a full list_notes scan), never on a scoped fetch.
    pub fn prune_clean(&self, account_id: &str, keep_uuids: &[String]) -> SqlResult<usize> {
        let conn = self.conn.lock().unwrap();
        // Build a temp table of uuids to keep — simpler than building a giant
        // IN (...) clause that could blow past sqlite's parameter limit.
        conn.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS _keep_uuids (uuid TEXT PRIMARY KEY);
             DELETE FROM _keep_uuids;",
        )?;
        {
            let mut ins = conn.prepare("INSERT OR IGNORE INTO _keep_uuids (uuid) VALUES (?1)")?;
            for u in keep_uuids {
                ins.execute(params![u])?;
            }
        }
        let deleted = conn.execute(
            "DELETE FROM notes
             WHERE account_id = ?1
               AND sync_state = 'clean'
               AND uuid NOT IN (SELECT uuid FROM _keep_uuids)",
            params![account_id],
        )?;
        // Cascade to derived tables. note_tags and edges are derived from
        // the note body and must not outlive the note row they came from.
        conn.execute(
            "DELETE FROM note_tags
             WHERE account_id = ?1
               AND uuid NOT IN (SELECT uuid FROM _keep_uuids)",
            params![account_id],
        )?;
        conn.execute(
            "DELETE FROM edges
             WHERE account_id = ?1
               AND src_uuid NOT IN (SELECT uuid FROM _keep_uuids)",
            params![account_id],
        )?;
        Ok(deleted)
    }

    /// UUIDs of notes marked `deleted_pending` for one account. Used by
    /// the Gmail-touching list paths (list_notes, list_notes_in_folder,
    /// refetch_note) to filter out messages whose local cache says they're
    /// logically gone — Gmail's eventual consistency can return them for
    /// a few seconds after our worker calls trash, and without this filter
    /// they'd reappear in the UI as ghost notes (D8). UUID-only (not full
    /// rows) keeps this cheap enough to run on every fetch.
    pub fn list_deleted_pending_uuids(&self, account_id: &str) -> SqlResult<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT uuid FROM notes
             WHERE account_id = ?1 AND sync_state = 'deleted_pending'",
        )?;
        let rows = stmt.query_map(params![account_id], |r| r.get::<_, String>(0))?;
        rows.collect()
    }

    /// Notes pending trash on Gmail. Worker picks these up alongside dirty.
    pub fn list_deleted_pending(&self) -> SqlResult<Vec<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT uuid, account_id, id, title, body_html, date, x_mail_created_date,
                    label, local_version, remote_version, sync_state,
                    last_synced_at, last_local_modified_at, last_remote_modified_at,
                    pinned, meta_msg_id, pin_dirty, tags_meta_msg_id, tags_dirty
             FROM notes WHERE sync_state = 'deleted_pending'",
        )?;
        let rows = stmt.query_map([], row_to_note)?;
        rows.collect()
    }

    /// Does this account still have anything the worker owes the backend?
    ///
    /// Covers all four outbound queues, because "Inactive" is a promise that
    /// nothing is pending — a queue missed here is a note that never leaves
    /// the device while the UI says everything is finished.
    pub fn has_pending_pushes(&self, account_id: &str) -> SqlResult<bool> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row(
            "SELECT
               (SELECT COUNT(*) FROM notes
                 WHERE account_id = ?1 AND sync_state IN ('dirty', 'deleted_pending'))
             + (SELECT COUNT(*) FROM notes WHERE account_id = ?1 AND pin_dirty = 1)
             + (SELECT COUNT(*) FROM notes WHERE account_id = ?1 AND tags_dirty = 1)
             + (SELECT COUNT(*) FROM folders
                 WHERE account_id = ?1 AND sync_state != 'clean')",
            params![account_id],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Per-queue breakdown for the "N left" display and the Stop-waiting
    /// confirmation. Read-only.
    pub fn count_pending_pushes(&self, account_id: &str) -> SqlResult<PendingPushes> {
        let conn = self.conn.lock().unwrap();
        let one = |sql: &str| -> SqlResult<usize> {
            let n: i64 = conn.query_row(sql, params![account_id], |r| r.get(0))?;
            Ok(n as usize)
        };
        Ok(PendingPushes {
            notes: one("SELECT COUNT(*) FROM notes WHERE account_id = ?1 AND sync_state = 'dirty'")?,
            deletes: one(
                "SELECT COUNT(*) FROM notes WHERE account_id = ?1 AND sync_state = 'deleted_pending'",
            )?,
            pins: one("SELECT COUNT(*) FROM notes WHERE account_id = ?1 AND pin_dirty = 1")?,
            tags: one("SELECT COUNT(*) FROM notes WHERE account_id = ?1 AND tags_dirty = 1")?,
            folders: one(
                "SELECT COUNT(*) FROM folders WHERE account_id = ?1 AND sync_state != 'clean'",
            )?,
        })
    }

    /// Look up one note by (uuid, account_id).
    pub fn get(&self, uuid: &str, account_id: &str) -> SqlResult<Option<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT uuid, account_id, id, title, body_html, date, x_mail_created_date,
                    label, local_version, remote_version, sync_state,
                    last_synced_at, last_local_modified_at, last_remote_modified_at,
                    pinned, meta_msg_id, pin_dirty, tags_meta_msg_id, tags_dirty
             FROM notes WHERE uuid = ?1 AND account_id = ?2",
        )?;
        stmt.query_row(params![uuid, account_id], row_to_note).optional()
    }

    // ─── Tags (Jodd-local, mirrors Pin wave 1) ────────────────────────────
    // Tags live ONLY here, never in the note body. The `tag` argument is
    // assumed already normalized by the command layer (trim, strip leading
    // '#', lowercase, charset [a-z0-9_-]).

    /// Add one tag to a note. Idempotent.
    pub fn add_tag(&self, account_id: &str, uuid: &str, tag: &str) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO note_tags (account_id, uuid, tag) VALUES (?1, ?2, ?3)",
            params![account_id, uuid, tag],
        )?;
        Ok(())
    }

    /// Remove one tag from a note.
    pub fn remove_tag(&self, account_id: &str, uuid: &str, tag: &str) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM note_tags WHERE account_id = ?1 AND uuid = ?2 AND tag = ?3",
            params![account_id, uuid, tag],
        )?;
        Ok(())
    }

    /// Every tag for an account with the count of notes carrying it, ordered
    /// alphabetically. Drives the sidebar Tags section. The JOIN against
    /// `notes` excludes deleted-pending notes AND any orphan tag rows whose
    /// note no longer exists, so the cloud is always consistent with reality.
    pub fn list_all_tags(&self, account_id: &str) -> SqlResult<Vec<(String, i64)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT t.tag, COUNT(*)
             FROM note_tags t
             JOIN notes n ON n.account_id = t.account_id AND n.uuid = t.uuid
             WHERE t.account_id = ?1 AND n.sync_state != 'deleted_pending'
             GROUP BY t.tag
             ORDER BY t.tag",
        )?;
        let rows = stmt.query_map(params![account_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        rows.collect()
    }

    /// (uuid, tag) for every tagged, non-deleted note in an account. The
    /// frontend folds this into a uuid → tags[] map for rendering chips.
    pub fn list_all_note_tags(&self, account_id: &str) -> SqlResult<Vec<(String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT t.uuid, t.tag
             FROM note_tags t
             JOIN notes n ON n.account_id = t.account_id AND n.uuid = t.uuid
             WHERE t.account_id = ?1 AND n.sync_state != 'deleted_pending'
             ORDER BY t.uuid, t.tag",
        )?;
        let rows = stmt.query_map(params![account_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        rows.collect()
    }

    /// Cached notes carrying ANY of `tags` (the union), distinct. The
    /// multi-tag navigation loads this union into the store; the frontend
    /// then narrows to AND (all tags) or OR (any tag) per the match mode —
    /// the union is a superset of both, so one load serves either mode.
    /// `account_id = None` searches EVERY account (cross-account tag filter);
    /// `Some(id)` scopes to one. Tags are OR-unioned here; the caller narrows to
    /// AND/OR.
    pub fn list_notes_with_tags(
        &self,
        account_id: Option<&str>,
        tags: &[String],
        exclude_accounts: &[String],
    ) -> SqlResult<Vec<CachedNote>> {
        if tags.is_empty() {
            return Ok(vec![]);
        }
        let conn = self.conn.lock().unwrap();
        let placeholders = std::iter::repeat("?")
            .take(tags.len())
            .collect::<Vec<_>>()
            .join(", ");
        // Account filter (if any) comes first in the bind order, then the tags.
        let mut binds: Vec<String> = Vec::with_capacity(tags.len() + 1);
        let account_clause = if let Some(a) = account_id {
            binds.push(a.to_string());
            "AND n.account_id = ?"
        } else {
            ""
        };
        binds.extend(tags.iter().cloned());
        // Every "?" above is auto-numbered by SQLite in left-to-right order of
        // appearance (1..=binds.len()), so the next free explicit placeholder
        // is binds.len() + 1 — the same rule the fixed-position functions
        // above express as a literal number, just computed here because the
        // account clause's own presence already varies that count.
        let excl = exclude_clause(exclude_accounts, binds.len() + 1);
        let sql = format!(
            "SELECT DISTINCT n.uuid, n.account_id, n.id, n.title, n.body_html, n.date,
                    n.x_mail_created_date, n.label, n.local_version, n.remote_version,
                    n.sync_state, n.last_synced_at, n.last_local_modified_at,
                    n.last_remote_modified_at, n.pinned, n.meta_msg_id, n.pin_dirty,
                    n.tags_meta_msg_id, n.tags_dirty
             FROM notes n
             JOIN note_tags t ON t.account_id = n.account_id AND t.uuid = n.uuid
             WHERE n.sync_state != 'deleted_pending' {}
               AND t.tag IN ({}){excl}",
            account_clause, placeholders
        );
        let mut all_binds: Vec<Box<dyn rusqlite::ToSql>> =
            binds.iter().map(|b| Box::new(b.clone()) as Box<dyn rusqlite::ToSql>).collect();
        for a in exclude_accounts {
            all_binds.push(Box::new(a.clone()));
        }
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(all_binds.iter()), row_to_note)?;
        rows.collect()
    }

    /// Rename a tag across every note in an account. Merges into an existing
    /// tag if some notes already carry `new_tag`: UPDATE OR IGNORE renames the
    /// rows that can move, then the leftover `old_tag` rows (those that would
    /// have collided on the PK) are deleted — completing the merge with no
    /// duplicates.
    /// Rename a tag across an account by REWRITING the body of every note that
    /// carries it (`#old` → `#new`) — the body is the source of truth, so just
    /// renaming note_tags would be undone by the next reconcile, and editing the
    /// body also round-trips the rename to Apple. Each touched note is marked
    /// content-dirty, re-derived, and re-indexed. Returns the count rewritten.
    pub fn rename_tag(&self, account_id: &str, old_tag: &str, new_tag: &str) -> SqlResult<usize> {
        self.rewrite_tag_in_bodies(account_id, old_tag, Some(new_tag))
    }

    /// Delete a tag across an account by stripping `#tag` from every note's body
    /// (same body-as-truth reasoning as rename). Returns the count rewritten.
    pub fn delete_tag(&self, account_id: &str, tag: &str) -> SqlResult<usize> {
        self.rewrite_tag_in_bodies(account_id, tag, None)
    }

    fn rewrite_tag_in_bodies(
        &self,
        account_id: &str,
        old_tag: &str,
        new_tag: Option<&str>,
    ) -> SqlResult<usize> {
        let mut conn = self.conn.lock().unwrap();
        let rows: Vec<(String, String, String, String)> = {
            let mut s = conn.prepare(
                "SELECT n.uuid, n.title, n.label, n.body_html FROM notes n
                 JOIN note_tags t ON t.account_id = n.account_id AND t.uuid = n.uuid
                 WHERE n.account_id = ?1 AND t.tag = ?2
                   AND n.sync_state != 'deleted_pending'",
            )?;
            let it = s.query_map(params![account_id, old_tag], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?;
            it.filter_map(|x| x.ok()).collect()
        };
        if rows.is_empty() {
            return Ok(0);
        }
        let tx = conn.transaction()?;
        let mut count = 0usize;
        for (uuid, title, label, body_html) in &rows {
            let new_body = rewrite_hashtag_in_body(body_html, old_tag, new_tag);
            if &new_body == body_html {
                continue;
            }
            tx.execute(
                "UPDATE notes SET body_html = ?1,
                     local_version = local_version + 1,
                     sync_state = CASE sync_state WHEN 'clean' THEN 'dirty' ELSE sync_state END,
                     last_local_modified_at = ?2
                 WHERE uuid = ?3 AND account_id = ?4",
                params![new_body, now_ms(), uuid, account_id],
            )?;
            reconcile_tags_from_body_conn(&tx, account_id, uuid, &new_body)?;
            reconcile_edges_from_body_conn(&tx, account_id, uuid, label, &new_body)?;
            reconcile_citations_from_body_conn(&tx, account_id, uuid, &new_body)?;
            fts_index_conn(&tx, uuid, account_id, title, &new_body)?;
            count += 1;
        }
        tx.commit()?;
        Ok(count)
    }

    /// Move tag rows whose note no longer exists into `tag_tombstones`.
    /// Replaces the previous prune_orphan_tags (which hard-deleted), so that
    /// a transient Gmail omission followed by a prune doesn't permanently
    /// destroy the user's tag metadata. When the note reappears via
    /// `upsert_from_remote`, the tombstone is restored. After TOMBSTONE_TTL_MS
    /// (see `sweep_old_tombstones`) the tombstone is itself dropped — at that
    /// point we're confident the note is genuinely gone, not transiently
    /// hidden.
    ///
    /// `INSERT OR REPLACE` updates `deleted_at` for a tombstone that already
    /// existed (the same note pruned, restored, pruned again) so the TTL
    /// always counts from the most-recent disappearance.
    ///
    /// Runs as a single transaction: tombstone-insert + note_tags-delete are
    /// atomic, so a crash mid-sweep cannot lose tags.
    pub fn tombstone_orphan_tags(&self, account_id: &str) -> SqlResult<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let now = now_ms();
        let moved = tx.execute(
            "INSERT OR REPLACE INTO tag_tombstones (account_id, uuid, tag, deleted_at)
             SELECT account_id, uuid, tag, ?1 FROM note_tags
             WHERE account_id = ?2
               AND uuid NOT IN (SELECT uuid FROM notes WHERE account_id = ?2)",
            params![now, account_id],
        )?;
        tx.execute(
            "DELETE FROM note_tags
             WHERE account_id = ?1
               AND uuid NOT IN (SELECT uuid FROM notes WHERE account_id = ?1)",
            params![account_id],
        )?;
        tx.commit()?;
        Ok(moved)
    }

    /// Hard-delete tombstones older than `max_age_ms`. Called from list_notes
    /// after `tombstone_orphan_tags` so the TTL is enforced on every sweep.
    /// Notes that vanish from Gmail and stay vanished for the TTL window get
    /// their tags permanently dropped here — the assumption being that beyond
    /// that window the disappearance is real, not transient.
    pub fn sweep_old_tombstones(&self, account_id: &str, max_age_ms: i64) -> SqlResult<usize> {
        let conn = self.conn.lock().unwrap();
        let cutoff = now_ms() - max_age_ms;
        let deleted = conn.execute(
            "DELETE FROM tag_tombstones
             WHERE account_id = ?1 AND deleted_at < ?2",
            params![account_id, cutoff],
        )?;
        Ok(deleted)
    }

    /// Copy every tag on `from_uuid` onto `to_uuid` within the same account.
    /// Used when reconcile_one creates a conflict-copy: without this the copy
    /// would silently start untagged and the user would lose tag state on
    /// whichever side of the conflict they ultimately choose.
    pub fn copy_tags(&self, account_id: &str, from_uuid: &str, to_uuid: &str) -> SqlResult<usize> {
        let conn = self.conn.lock().unwrap();
        let copied = conn.execute(
            "INSERT OR IGNORE INTO note_tags (account_id, uuid, tag)
             SELECT account_id, ?3, tag FROM note_tags
             WHERE account_id = ?1 AND uuid = ?2",
            params![account_id, from_uuid, to_uuid],
        )?;
        Ok(copied)
    }
}

impl CachedNote {
    /// Project a cached row into the frontend-facing `gmail::Note` shape.
    /// The sync metadata stays in Rust — UI doesn't need it directly.
    pub fn to_frontend_note(&self) -> crate::backend::gmail::wire::Note {
        crate::backend::gmail::wire::Note {
            id: self.id.clone(),
            uuid: self.uuid.clone(),
            title: self.title.clone(),
            body_html: self.body_html.clone(),
            date: self.date.clone(),
            x_mail_created_date: self.x_mail_created_date.clone(),
            label: self.label.clone(),
            account_id: Some(self.account_id.clone()),
            pinned: self.pinned,
            local_version: self.local_version,
            // Attachments live in their own table and aren't serialized to the
            // UI yet; the editor doesn't consume them. Empty here is correct.
            attachments: Vec::new(),
        }
    }

    /// Build a CachedNote from a server fetch result. Stamps `clean` with
    /// remote_version = the message id, last_synced_at = now, last_remote
    /// _modified_at parsed from the Date header. local_version stays at 0
    /// (caller hasn't edited locally).
    pub fn from_remote(account_id: &str, n: &crate::backend::gmail::wire::Note) -> Self {
        let now = now_ms();
        let last_remote = chrono::DateTime::parse_from_rfc2822(&n.date)
            .ok()
            .map(|d| d.timestamp_millis());
        CachedNote {
            uuid: n.uuid.clone(),
            account_id: account_id.to_string(),
            id: n.id.clone(),
            title: n.title.clone(),
            body_html: n.body_html.clone(),
            date: n.date.clone(),
            x_mail_created_date: n.x_mail_created_date.clone(),
            label: n.label.clone(),
            local_version: 0,
            remote_version: Some(n.id.clone()),
            sync_state: SyncState::Clean,
            last_synced_at: Some(now),
            last_local_modified_at: now,
            last_remote_modified_at: last_remote,
            // Pin / sidecar fields are Jodd-managed metadata, NOT part of
            // the remote Note shape. Brand-new rows from remote default to
            // unpinned with no sidecar; existing rows preserve their
            // values because upsert_from_remote's ON CONFLICT clause
            // intentionally omits these three columns from the SET list.
            // The pull-side pin reconciliation (apply_remote_pin /
            // clear_pins_not_in) is what propagates remote pin changes
            // from sidecars into the cache.
            pinned: false,
            meta_msg_id: None,
            pin_dirty: false,
            // Same rule as the pin trio above — tag-sidecar fields are
            // Jodd-managed metadata, not part of the remote Note shape.
            // New rows arrive with no sidecar; apply_remote_tags is what
            // populates these when a sidecar exists.
            tags_meta_msg_id: None,
            tags_dirty: false,
        }
    }
}

// ─── Folder operations ──────────────────────────────────────────────────────
//
// Folders mirror the same local-first + sync-worker pattern as notes, but
// with simpler shape: a folder is essentially (account, path, sync_state).
// No body content, no UUID — Gmail's label_id is the durable identifier
// once the label exists remotely.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedFolder {
    pub account_id: String,
    pub path: String,
    pub label_id: Option<String>,
    pub sync_state: FolderSyncState,
    pub last_local_modified_at: i64,
    pub last_synced_at: Option<i64>,
    /// Folder kind (migration #9): 'user' | 'system_workflow' | 'smart_query'.
    /// `user` is the only kind today — every existing folder, every folder
    /// created by the user, and every folder pulled from Gmail defaults to
    /// `user`. `system_workflow` and `smart_query` are reserved for future
    /// Jodd-managed folders (lesson extraction, smart/dynamic folders).
    /// Round-trip rule: remote upserts NEVER touch `kind` — a folder that
    /// was minted locally as `system_workflow` and round-tripped through
    /// Gmail stays `system_workflow`.
    pub kind: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderSyncState {
    Clean,
    DirtyNew,
    DirtyRenamed,
    DeletedPending,
}

impl FolderSyncState {
    fn as_str(&self) -> &'static str {
        match self {
            FolderSyncState::Clean => "clean",
            FolderSyncState::DirtyNew => "dirty_new",
            FolderSyncState::DirtyRenamed => "dirty_renamed",
            FolderSyncState::DeletedPending => "deleted_pending",
        }
    }
    fn from_str(s: &str) -> Self {
        match s {
            "dirty_new" => FolderSyncState::DirtyNew,
            "dirty_renamed" => FolderSyncState::DirtyRenamed,
            "deleted_pending" => FolderSyncState::DeletedPending,
            _ => FolderSyncState::Clean,
        }
    }
}

impl Db {
    /// Read all folders for one account (excluding pending-deleted, so the
    /// frontend doesn't see folders being trashed).
    pub fn list_folders(&self, account_id: &str) -> SqlResult<Vec<CachedFolder>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT account_id, path, label_id, sync_state,
                    last_local_modified_at, last_synced_at, kind
             FROM folders
             WHERE account_id = ?1 AND sync_state != 'deleted_pending'
             ORDER BY path",
        )?;
        let rows = stmt.query_map(params![account_id], row_to_folder)?;
        rows.collect()
    }

    /// Insert a folder we just created locally (no Gmail label yet).
    /// Idempotent on (account_id, path): if a row already exists, leave it
    /// alone — caller is responsible for checking before creating.
    ///
    /// **Ancestor invariant (D1 fix):** every path segment between the
    /// implicit "Notes" root and the leaf must have its own row, so the
    /// folder tree never has gaps. Any missing ancestor is inserted in
    /// the same transaction as `dirty_new` so the worker pushes it to
    /// Gmail on the next tick. Without this, a UI bug or a Gmail label
    /// anomaly could leave child rows whose parent the move-to submenu
    /// (which reads SQLite directly) can't find.
    pub fn insert_folder_local_new(&self, f: &CachedFolder) -> SqlResult<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        ensure_ancestors(&tx, &f.account_id, &f.path)?;
        tx.execute(
            "INSERT INTO folders (account_id, path, label_id, sync_state,
                last_local_modified_at, last_synced_at, kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(account_id, path) DO NOTHING",
            params![
                f.account_id,
                f.path,
                f.label_id,
                f.sync_state.as_str(),
                f.last_local_modified_at,
                f.last_synced_at,
                f.kind,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Creates a system workflow folder under `Notes/` if absent.
    /// `name` is the workflow leaf (e.g. "Lessons" → "Notes/Lessons").
    /// Returns the full label path. Idempotent: a second call with the
    /// same name leaves the existing row untouched (ON CONFLICT DO
    /// NOTHING — we do NOT downgrade an already-clean system folder back
    /// to dirty_new, nor flip its kind).
    ///
    /// Goes through `ensure_ancestors` so the implicit `Notes` root
    /// segment is materialized if missing — same path used by
    /// `insert_folder_local_new` for user folders. Strict ancestors
    /// default to kind='user'; only the leaf is `system_workflow`.
    pub fn ensure_workflow_folder(
        &self,
        account_id: &str,
        name: &str,
    ) -> SqlResult<String> {
        let path = format!("Notes/{}", name);
        let now = now_ms();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        ensure_ancestors(&tx, account_id, &path)?;
        tx.execute(
            "INSERT INTO folders (account_id, path, label_id, sync_state,
                last_local_modified_at, last_synced_at, kind)
             VALUES (?1, ?2, NULL, 'dirty_new', ?3, NULL, 'system_workflow')
             ON CONFLICT(account_id, path) DO NOTHING",
            params![account_id, path, now],
        )?;
        tx.commit()?;
        Ok(path)
    }

    /// Local-first folder create for an arbitrary full path (MCP write
    /// tools). Unlike the sidebar command (kind='user' always), the kind is
    /// derived from the leaf so __name__ folders classify as
    /// system_workflow — gotcha #4's convention is the contract.
    pub fn create_folder_local_new(&self, account_id: &str, path: &str) -> SqlResult<()> {
        let now = now_ms();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        ensure_ancestors(&tx, account_id, path)?;
        let kind = derive_workflow_kind(path);
        tx.execute(
            "INSERT INTO folders (account_id, path, label_id, sync_state,
                last_local_modified_at, last_synced_at, kind)
             VALUES (?1, ?2, NULL, 'dirty_new', ?3, NULL, ?4)
             ON CONFLICT(account_id, path) DO NOTHING",
            params![account_id, path, now, kind],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Upsert a folder we learned about from Gmail. Used by pull
    /// reconciliation. If a local row exists in a pending state, we
    /// preserve it (caller decides what to do).
    ///
    /// **Ancestor invariant (D1 fix):** Gmail allows label names like
    /// `Notes/A/B` to exist without a corresponding `Notes/A` label —
    /// the slash is just a character in the label name. If the pull
    /// returns such a "headless" child, insert any missing ancestors
    /// as `dirty_new` so (a) the tree never has gaps locally and (b)
    /// the sync worker creates the matching Gmail labels on the next
    /// tick, healing the remote tree to match. Ancestors are NOT
    /// inserted as `clean` because they don't actually exist on Gmail
    /// yet — `dirty_new` is the honest state.
    pub fn upsert_folder_from_remote(
        &self,
        account_id: &str,
        path: &str,
        label_id: &str,
    ) -> SqlResult<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        ensure_ancestors(&tx, account_id, path)?;
        // NOTE on `kind`: derived from the path leaf via `derive_workflow_kind`
        // (see its doc). Pre-v0.16.5 this was hardcoded 'user', which caused
        // a cross-device Sidebar-grouping bug: a `__Extracts__` label minted
        // on device A as `system_workflow` and pulled to device B via this
        // function got stamped 'user' on B, so the same folder appeared in
        // different sidebar groups. The INSERT path now derives kind from
        // the path. The ON CONFLICT clause intentionally does NOT touch
        // `kind` directly — we handle re-classification in the self-heal
        // UPDATE below, which is easier to read than embedding a CASE in
        // ON CONFLICT. Migration #14 added the column.
        let kind = derive_workflow_kind(path);
        tx.execute(
            "INSERT INTO folders (account_id, path, label_id, sync_state,
                last_local_modified_at, last_synced_at, kind)
             VALUES (?1, ?2, ?3, 'clean', ?4, ?4, ?5)
             ON CONFLICT(account_id, path) DO UPDATE SET
                label_id = excluded.label_id,
                sync_state = 'clean',
                last_synced_at = excluded.last_synced_at
             WHERE folders.sync_state = 'clean'",
            params![account_id, path, label_id, now_ms(), kind],
        )?;
        // Self-heal: if a pre-existing row is misclassified (e.g., stamped
        // 'user' by a pre-v0.16.5 Jodd, or by a future regression), promote
        // it here. Re-derived from the path on every sync tick — drift
        // cannot persist past the next sync. Idempotent: once kind already
        // matches, the WHERE excludes the row. We only PROMOTE 'user' →
        // 'system_workflow'; we never demote, because a `__*__` folder that
        // somehow has a different kind (e.g. `smart_query`) was explicitly
        // set and should be preserved.
        //
        // Why derive-not-migrate: the `__*__` leaf IS the contract (see
        // CLAUDE.md edge #6). The `kind` column is a cache; we re-derive
        // on every reconciliation rather than relying on a one-shot
        // migration that can be defeated by any future code path that
        // forgets the rule. See the migration list above (the comment
        // preceding #15) for the dropped folder-kind backfill that was
        // originally proposed as #15 — superseded by this self-heal.
        // (#15 itself is now a live, unrelated migration — the citations
        // 'cites' edges backfill — so "dropped #15" no longer refers to
        // a real migration slot; this note is purely historical context.)
        if kind == "system_workflow" {
            tx.execute(
                "UPDATE folders SET kind = 'system_workflow'
                 WHERE account_id = ?1 AND path = ?2 AND kind = 'user'",
                params![account_id, path],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Update a folder's path (rename or move). Transitions:
    ///   clean → dirty_renamed (push the new name)
    ///   dirty_new → dirty_new with new path (still pending create with the
    ///     latest name — no need to two-phase since we haven't pushed yet)
    ///   dirty_renamed → stays dirty_renamed (overwrite with newest desired name)
    pub fn rename_folder(
        &self,
        account_id: &str,
        old_path: &str,
        new_path: &str,
    ) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE folders
             SET path = ?1,
                 sync_state = CASE sync_state
                     WHEN 'clean' THEN 'dirty_renamed'
                     WHEN 'dirty_new' THEN 'dirty_new'
                     WHEN 'dirty_renamed' THEN 'dirty_renamed'
                     ELSE sync_state
                 END,
                 last_local_modified_at = ?2
             WHERE account_id = ?3 AND path = ?4",
            params![new_path, now_ms(), account_id, old_path],
        )?;
        Ok(())
    }

    /// Mark a folder for deletion. Row stays so the worker can retry
    /// the trash if needed (offline). For dirty_new folders that haven't
    /// been pushed yet, we can drop the row entirely — no remote work needed.
    pub fn mark_folder_deleted(&self, account_id: &str, path: &str) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        // If the folder was never pushed (dirty_new), just drop the row.
        // Otherwise mark deleted_pending so the worker handles Gmail.
        let row: Option<String> = conn.query_row(
            "SELECT sync_state FROM folders WHERE account_id = ?1 AND path = ?2",
            params![account_id, path],
            |r| r.get(0),
        ).optional()?;
        match row.as_deref() {
            Some("dirty_new") => {
                conn.execute(
                    "DELETE FROM folders WHERE account_id = ?1 AND path = ?2",
                    params![account_id, path],
                )?;
            }
            _ => {
                conn.execute(
                    "UPDATE folders SET sync_state = 'deleted_pending',
                        last_local_modified_at = ?1
                     WHERE account_id = ?2 AND path = ?3",
                    params![now_ms(), account_id, path],
                )?;
            }
        }
        Ok(())
    }

    /// After successful push of a new folder, store the assigned label_id
    /// and mark clean.
    pub fn mark_folder_created(
        &self,
        account_id: &str,
        path: &str,
        label_id: &str,
    ) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE folders SET label_id = ?1, sync_state = 'clean',
                last_synced_at = ?2
             WHERE account_id = ?3 AND path = ?4",
            params![label_id, now_ms(), account_id, path],
        )?;
        Ok(())
    }

    /// After successful push of a rename, mark clean.
    pub fn mark_folder_renamed(&self, account_id: &str, path: &str) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE folders SET sync_state = 'clean', last_synced_at = ?1
             WHERE account_id = ?2 AND path = ?3",
            params![now_ms(), account_id, path],
        )?;
        Ok(())
    }

    /// After successful Gmail trash, remove the folder row.
    pub fn drop_folder_row(&self, account_id: &str, path: &str) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM folders WHERE account_id = ?1 AND path = ?2",
            params![account_id, path],
        )?;
        Ok(())
    }

    /// All folders pending push (any non-clean state). Worker drains these.
    /// Returned ordered by: dirty_new first (so new folders exist before
    /// renames cascade onto them), then dirty_renamed (parents before
    /// children — using path length as a rough proxy), then deleted_pending
    /// (deepest first so children are gone before parents).
    pub fn list_dirty_folders(&self) -> SqlResult<Vec<CachedFolder>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT account_id, path, label_id, sync_state,
                    last_local_modified_at, last_synced_at, kind
             FROM folders
             WHERE sync_state != 'clean'
             ORDER BY
                CASE sync_state
                    WHEN 'dirty_new' THEN 1
                    WHEN 'dirty_renamed' THEN 2
                    WHEN 'deleted_pending' THEN 3
                    ELSE 4
                END,
                CASE sync_state
                    WHEN 'deleted_pending' THEN -length(path)
                    ELSE length(path)
                END",
        )?;
        let rows = stmt.query_map([], row_to_folder)?;
        rows.collect()
    }

    /// Prune `clean` folder rows for an account whose path isn't in
    /// `keep_paths`. Used after a full list_notes (which knows the complete
    /// remote folder list via label_map) to drop folders that were deleted
    /// externally — same idea as `prune_clean` for notes, but on folders.
    /// Pending-state rows (dirty_new/dirty_renamed/deleted_pending) are
    /// untouched — the worker owns their lifecycle.
    pub fn prune_clean_folders(
        &self,
        account_id: &str,
        keep_paths: &[String],
    ) -> SqlResult<usize> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS _keep_folders (path TEXT PRIMARY KEY);
             DELETE FROM _keep_folders;",
        )?;
        {
            let mut ins = conn.prepare("INSERT OR IGNORE INTO _keep_folders (path) VALUES (?1)")?;
            for p in keep_paths {
                ins.execute(params![p])?;
            }
        }
        let deleted = conn.execute(
            "DELETE FROM folders
             WHERE account_id = ?1
               AND sync_state = 'clean'
               AND path NOT IN (SELECT path FROM _keep_folders)",
            params![account_id],
        )?;
        Ok(deleted)
    }

    /// Count notes currently labeled with `label` for an account, excluding
    /// rows pending deletion. Used by delete_folder to enforce "must be
    /// empty before delete" against the local cache.
    pub fn count_notes_in_label(&self, account_id: &str, label: &str) -> SqlResult<i64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM notes
             WHERE account_id = ?1 AND label = ?2 AND sync_state != 'deleted_pending'",
            params![account_id, label],
            |r| r.get(0),
        )
    }

    /// Cheap existence check: does the cache hold ANY note for this account?
    /// Backs the offline-safe readiness gate (`is_authenticated`). Pure SQLite,
    /// sub-ms, no network. Counts rows in any sync_state (a deleted_pending row
    /// still means we have something to show until the worker prunes it).
    pub fn has_cached_notes(&self, account_id: &str) -> SqlResult<bool> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM notes WHERE account_id = ?1)",
            params![account_id],
            |r| r.get(0),
        )
    }

    /// Look up one folder by (account_id, path).
    pub fn get_folder(&self, account_id: &str, path: &str) -> SqlResult<Option<CachedFolder>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT account_id, path, label_id, sync_state,
                    last_local_modified_at, last_synced_at, kind
             FROM folders WHERE account_id = ?1 AND path = ?2",
        )?;
        stmt.query_row(params![account_id, path], row_to_folder).optional()
    }

    /// Rename a folder AND all its descendants in one transaction, AND
    /// update all notes' label field to match. Used for move/rename ops
    /// that affect a subtree. Returns the count of folder rows touched.
    pub fn rename_subtree(
        &self,
        account_id: &str,
        old_path: &str,
        new_path: &str,
    ) -> SqlResult<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let old_prefix = format!("{}/", old_path);
        // Folders: the exact match + everything under it. We rewrite by
        // splicing the prefix. SQLite's substr() is 1-indexed.
        let folder_count = tx.execute(
            "UPDATE folders
             SET path = CASE
                     WHEN path = ?1 THEN ?2
                     ELSE ?2 || substr(path, length(?1) + 1)
                 END,
                 sync_state = CASE sync_state
                     WHEN 'clean' THEN 'dirty_renamed'
                     WHEN 'dirty_new' THEN 'dirty_new'
                     WHEN 'dirty_renamed' THEN 'dirty_renamed'
                     ELSE sync_state
                 END,
                 last_local_modified_at = ?3
             WHERE account_id = ?4
               AND (path = ?1 OR path LIKE ?5)",
            params![old_path, new_path, now_ms(), account_id, format!("{}%", old_prefix)],
        )?;

        // Notes: update label field same way. We DO NOT mark notes dirty
        // here — Gmail keeps the label_id stable across renames, so the
        // server-side notes are already pointing at the renamed label.
        // The label field on each note is just our local name mirror.
        tx.execute(
            "UPDATE notes
             SET label = CASE
                     WHEN label = ?1 THEN ?2
                     ELSE ?2 || substr(label, length(?1) + 1)
                 END
             WHERE account_id = ?3
               AND (label = ?1 OR label LIKE ?4)",
            params![old_path, new_path, account_id, format!("{}%", old_prefix)],
        )?;

        tx.commit()?;
        Ok(folder_count)
    }

    /// After a LocalFs folder is physically renamed on disk, update note `id`
    /// fields so the cached path matches the new file location. For Gmail the
    /// `id` is a message-ID (e.g. `18e2d5f3b4a`) and never matches the path
    /// prefix, so this is a safe no-op on Gmail accounts.
    pub fn rename_note_ids_for_folder(
        &self,
        account_id: &str,
        old_path: &str,
        new_path: &str,
    ) -> SqlResult<usize> {
        self.conn.lock().unwrap().execute(
            "UPDATE notes
             SET id = ?3 || substr(id, length(?2) + 1)
             WHERE account_id = ?1 AND id LIKE ?2 || '/%'",
            params![account_id, old_path, new_path],
        )
    }
}

fn row_to_folder(r: &rusqlite::Row) -> SqlResult<CachedFolder> {
    Ok(CachedFolder {
        account_id: r.get(0)?,
        path: r.get(1)?,
        label_id: r.get(2)?,
        sync_state: FolderSyncState::from_str(&r.get::<_, String>(3)?),
        last_local_modified_at: r.get(4)?,
        last_synced_at: r.get(5)?,
        kind: r.get(6)?,
    })
}

fn row_to_note(r: &rusqlite::Row) -> SqlResult<CachedNote> {
    Ok(CachedNote {
        uuid: r.get(0)?,
        account_id: r.get(1)?,
        id: r.get(2)?,
        title: r.get(3)?,
        body_html: r.get(4)?,
        date: r.get(5)?,
        x_mail_created_date: r.get(6)?,
        label: r.get(7)?,
        local_version: r.get(8)?,
        remote_version: r.get(9)?,
        sync_state: SyncState::from_str(&r.get::<_, String>(10)?),
        last_synced_at: r.get(11)?,
        last_local_modified_at: r.get(12)?,
        last_remote_modified_at: r.get(13)?,
        pinned: r.get::<_, i64>(14)? != 0,
        meta_msg_id: r.get(15)?,
        pin_dirty: r.get::<_, i64>(16)? != 0,
        tags_meta_msg_id: r.get(17)?,
        tags_dirty: r.get::<_, i64>(18)? != 0,
    })
}

/// Walk every strict ancestor of `path` BELOW the implicit "Notes" root
/// and insert a `dirty_new` row for any that doesn't already exist. Runs
/// inside the caller's transaction so either every missing ancestor lands
/// or none of them do.
///
/// The "Notes" root itself is intentionally NOT inserted here — Apple Notes
/// creates that label on the Gmail side as part of account setup, and the
/// sidebar synthesizes a client-side row when the cache hasn't seen it yet
/// (see `list_folders` in lib.rs). Inserting it as `dirty_new` would make
/// the worker call `gmail::create_label("Notes")`, which fails because
/// the label already exists, and the row would stay stuck dirty forever.
///
/// `dirty_new` is chosen deliberately for genuine ancestors (depth ≥ 2):
/// in BOTH callsites (local create where the user's parent might have only
/// existed in a UI buildRows synthesis, and remote pull where Gmail
/// returned a headless `Notes/A/B` without `Notes/A`) the honest state is
/// "we want this label to exist on Gmail." The worker's next tick creates
/// it. If the label already exists on Gmail under a different code path,
/// the next list_notes label_map will upsert it back to `clean` with the
/// real label_id.
/// Derive folder `kind` from the path leaf.
///
/// Jodd reserves the `__name__` syntax for system-managed workflow folders
/// (Extracts, future Summaries, etc.) — see CLAUDE.md "Active edges" #5
/// and the `validate_folder_segment` rejection rule. Folders whose leaf
/// matches this pattern are auto-classified `system_workflow` so the
/// Sidebar renders them under the Workflows group regardless of which
/// device first minted the folder. Before v0.16.5 the classification was
/// only set on the device that ran `ensure_workflow_folder`, so a second
/// device that pulled the same Gmail label via `upsert_folder_from_remote`
/// stamped it `'user'` and the same `Notes/__Extracts__` appeared in
/// different Sidebar groups on different devices.
pub(crate) fn derive_workflow_kind(path: &str) -> &'static str {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    // len >= 5 → at least one char between the `__` prefix and suffix
    // (`__a__`). Without this `____` would falsely match.
    if leaf.len() >= 5 && leaf.starts_with("__") && leaf.ends_with("__") {
        "system_workflow"
    } else {
        "user"
    }
}

fn ensure_ancestors(tx: &rusqlite::Transaction<'_>, account_id: &str, path: &str) -> SqlResult<()> {
    let segs: Vec<&str> = path.split('/').collect();
    // segs.len() < 3 means path is either "Notes" or "Notes/leaf" — no
    // strict ancestor below the root to materialize.
    if segs.len() < 3 {
        return Ok(());
    }
    let now = now_ms();
    // Start at i=2 so the first ancestor inserted is "Notes/<seg1>".
    // i=1 would attempt to insert just "Notes" — see why we skip above.
    for i in 2..segs.len() {
        let ancestor = segs[..i].join("/");
        // Most ancestors are user-shaped folders ('user'); the exception is
        // a nested workflow folder like `Notes/__Outer__/leaf` where the
        // ancestor `Notes/__Outer__` itself matches the workflow pattern.
        // Derive per-ancestor so that corner case is correct.
        let kind = derive_workflow_kind(&ancestor);
        tx.execute(
            "INSERT INTO folders (account_id, path, label_id, sync_state,
                last_local_modified_at, last_synced_at, kind)
             VALUES (?1, ?2, NULL, 'dirty_new', ?3, NULL, ?4)
             ON CONFLICT(account_id, path) DO NOTHING",
            params![account_id, ancestor, now, kind],
        )?;
    }
    Ok(())
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests_workflow_folder {
    use super::*;
    use tempfile::tempdir;

    fn temp_db() -> Db {
        let dir = tempdir().unwrap();
        // Db::open takes the app_data_dir and appends jodd.sqlite3 itself.
        let path = dir.path().to_path_buf();
        // Leak the dir so the path stays valid for the test's lifetime.
        std::mem::forget(dir);
        Db::open(&path).expect("open temp db")
    }

    #[test]
    fn ensure_workflow_folder_creates_when_absent() {
        let db = temp_db();
        let acct = "test@example.com";
        let path = db
            .ensure_workflow_folder(acct, "Lessons")
            .expect("ensure");
        assert_eq!(path, "Notes/Lessons");

        let folders = db.list_folders(acct).expect("list");
        let lessons = folders
            .iter()
            .find(|f| f.path == "Notes/Lessons")
            .expect("Notes/Lessons exists");
        assert_eq!(lessons.kind, "system_workflow");
        assert_eq!(lessons.sync_state, FolderSyncState::DirtyNew);
    }

    #[test]
    fn ensure_workflow_folder_idempotent() {
        let db = temp_db();
        let acct = "test@example.com";
        db.ensure_workflow_folder(acct, "Lessons").unwrap();
        let path = db.ensure_workflow_folder(acct, "Lessons").unwrap();
        assert_eq!(path, "Notes/Lessons");
        let count = db
            .list_folders(acct)
            .unwrap()
            .iter()
            .filter(|f| f.path == "Notes/Lessons")
            .count();
        assert_eq!(count, 1, "should not duplicate");
    }

    #[test]
    fn create_folder_local_new_derives_kind_and_ancestors() {
        let db = temp_db();
        let acct = "acc";

        db.create_folder_local_new(acct, "Notes/__Claude__").unwrap();
        let f = db.get_folder(acct, "Notes/__Claude__").unwrap().unwrap();
        assert_eq!(
            f.kind, "system_workflow",
            "__name__ leaf must derive workflow kind (gotcha #4)"
        );

        db.create_folder_local_new(acct, "Notes/A/B").unwrap();
        let f = db.get_folder(acct, "Notes/A/B").unwrap().unwrap();
        assert_eq!(f.kind, "user");
        assert!(
            db.get_folder(acct, "Notes/A").unwrap().is_some(),
            "ancestors must be ensured"
        );

        // Idempotent: second call is a no-op, not an error.
        db.create_folder_local_new(acct, "Notes/A/B").unwrap();
        let count = db
            .list_folders(acct)
            .unwrap()
            .iter()
            .filter(|f| f.path == "Notes/A/B")
            .count();
        assert_eq!(count, 1, "should not duplicate");
    }
}

/// Strip HTML tags to plain text for the FTS index (so searches match note
/// content, not tag/attribute names). Collapses runs of whitespace.
pub(crate) fn strip_html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Extract Apple-style inline hashtags from a note's PLAIN TEXT (caller strips
/// HTML first so `#fff` inside `style="color:#fff"` isn't mistaken for a tag).
/// A hashtag is a `#` at a word boundary followed by a run of letters / digits /
/// underscore that contains at least one letter — so `#2024` alone is not a tag,
/// matching Apple Notes. Unicode-aware (Thai `#งาน` works). Returns normalized
/// (lowercased) tags, de-duplicated, in first-seen order.
/// A tag char is a letter/digit/underscore/HYPHEN OR a combining mark.
///
/// Hyphen (added 2026-06-13) lets compound tags like `#database-migrations` or
/// `#local-first` parse as one tag instead of truncating at the dash. Multi-word
/// tags by kebab-case convention — spaces remain unsupported because the inline
/// `#hashtag` parser uses non-tag-chars (including space) to terminate a tag,
/// so `#data migration` would be ambiguous (1 tag or 2?).
///
/// Thai (and many scripts) glue tone/vowel marks onto letters (e.g. the ์ in
/// "โปรเจกต์"), and those marks are NOT is_alphanumeric, so without the
/// matches!() arm a tag word would be truncated at the first mark.
pub(crate) fn is_tag_word_char(c: char) -> bool {
    c.is_alphanumeric()
        || c == '_'
        || c == '-'
        || matches!(c as u32,
            0x0300..=0x036F           // Latin combining diacritics
            | 0x0483..=0x0489         // Cyrillic
            | 0x0591..=0x05BD         // Hebrew points
            | 0x0610..=0x061A | 0x064B..=0x065F | 0x0670 // Arabic
            | 0x0900..=0x0903 | 0x093A..=0x094F | 0x0951..=0x0957 // Devanagari
            | 0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E // Thai
            | 0x0EB1 | 0x0EB4..=0x0EBC | 0x0EC8..=0x0ECD // Lao
            | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
}

pub fn extract_hashtags(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let is_word = is_tag_word_char;
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '#' && (i == 0 || !is_word(chars[i - 1])) {
            let start = i + 1;
            let mut j = start;
            while j < chars.len() && is_word(chars[j]) {
                j += 1;
            }
            if j > start {
                let tag: String = chars[start..j].iter().collect();
                if tag.chars().any(|c| c.is_alphabetic()) {
                    let norm = tag.to_lowercase();
                    if !out.contains(&norm) {
                        out.push(norm);
                    }
                }
            }
            i = j;
            continue;
        }
        i += 1;
    }
    out
}

/// Derive a note's tag set from its body HTML: strip to text, then pull inline
/// hashtags. The single place that maps "what's in the body" → "the tag set".
pub fn tags_from_body(body_html: &str) -> Vec<String> {
    extract_hashtags(&strip_html_to_text(body_html))
}

/// Extract `[[wikilink]]` targets from plain text — the trimmed inner text of
/// each `[[ ... ]]`. First-seen order, de-duplicated, empty links skipped. Not
/// normalized (caller lowercases for title matching).
pub fn extract_wikilinks(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i + 1 < chars.len() {
        if chars[i] == '[' && chars[i + 1] == '[' {
            let start = i + 2;
            let mut j = start;
            while j + 1 < chars.len() && !(chars[j] == ']' && chars[j + 1] == ']') {
                j += 1;
            }
            if j + 1 < chars.len() {
                let inner: String = chars[start..j].iter().collect();
                let t = inner.trim();
                if !t.is_empty() && !out.iter().any(|x| x == t) {
                    out.push(t.to_string());
                }
                i = j + 2;
                continue;
            } else {
                break; // unterminated [[
            }
        }
        i += 1;
    }
    out
}

/// Decode the handful of HTML/XML entities that show up in real note bodies
/// (a contenteditable's `innerHTML` serialization escapes `&` to `&amp;`,
/// `<` to `&lt;`, etc. on save) so a URL extracted from stored `body_html`
/// compares equal, byte-for-byte, to the same URL scanned from raw,
/// unescaped text (e.g. Extract's pasted source). `&amp;` is decoded LAST
/// so a genuinely double-escaped `&amp;lt;` (literal text "&lt;") decodes to
/// exactly one level, not two.
fn decode_html_entities(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.contains('&') {
        return std::borrow::Cow::Borrowed(s);
    }
    std::borrow::Cow::Owned(
        s.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

/// Extract `http(s)://` URLs from plain text, in first-seen order,
/// deduplicated. Hand-rolled (no `regex` dependency in this crate) — matches
/// the scanning style of `extract_wikilinks`/`extract_hashtags`. A match
/// starts at `http://` or `https://` and extends until whitespace or one of
/// `)]"'<`, then trims trailing sentence punctuation (`.,;:`) so a URL at the
/// end of a sentence doesn't capture the period. HTML entities in the match
/// (`&amp;` etc.) are decoded before dedup/return — see `decode_html_entities`.
pub(crate) fn extract_urls(text: &str) -> Vec<String> {
    const PREFIXES: [&str; 2] = ["http://", "https://"];
    const STOP_CHARS: [char; 5] = [')', ']', '"', '\'', '<'];
    const TRIM_CHARS: [char; 4] = ['.', ',', ';', ':'];

    let mut out: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < text.len() {
        let rest = &text[i..];
        let matched_prefix = PREFIXES.iter().find(|p| rest.starts_with(**p));
        if let Some(prefix) = matched_prefix {
            let start = i;
            let mut end = i + prefix.len();
            for c in rest[prefix.len()..].chars() {
                if c.is_whitespace() || STOP_CHARS.contains(&c) {
                    break;
                }
                end += c.len_utf8();
            }
            let raw = text[start..end].trim_end_matches(|c| TRIM_CHARS.contains(&c));
            let url = decode_html_entities(raw);
            if !url.is_empty() && !out.iter().any(|u| u == url.as_ref()) {
                out.push(url.into_owned());
            }
            i = end;
        } else {
            let c = rest.chars().next().unwrap_or(' ');
            i += c.len_utf8();
        }
    }
    out
}

/// Normalized form of a note title for edge matching (trim + lowercase).
fn normalize_title(s: &str) -> String {
    s.trim().to_lowercase()
}

/// First 8 hex chars of a note's UUID (hyphens stripped, lowercased) — the
/// stable id segment embedded in a note's slug. Round-trips because the UUID
/// lives in the message header, so any Jodd instance derives the same id.
pub(crate) fn uuid_short(uuid: &str) -> String {
    uuid.chars()
        .filter(|c| c.is_ascii_hexdigit())
        .take(8)
        .collect::<String>()
        .to_lowercase()
}

/// URL-ish slug from a title — lowercased, runs of non-word chars become a
/// single '-'. Keeps Unicode letters/marks so Thai survives (#โน้ต → โน้ต).
fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for c in s.trim().chars() {
        if c.is_alphanumeric() || is_tag_word_char(c) {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.extend(c.to_lowercase());
        } else {
            pending_dash = true;
        }
    }
    out
}

/// A note's stable, readable, round-trip-safe link slug: `<title-slug>-<uuid8>`.
/// The trailing id makes it UNIQUE (titles can collide) and durable (re-derived
/// from the UUID), while the title part keeps it readable. Resolved by the id.
pub fn note_slug(title: &str, uuid: &str) -> String {
    let id = uuid_short(uuid);
    let base = slugify(title);
    if base.is_empty() {
        id
    } else {
        format!("{}-{}", base, id)
    }
}

/// Interpret a `[[...]]` link target: if it ends in `-<8 hex>` it's a slug →
/// resolve by that id; otherwise it's a plain title (manual link) → resolve by
/// normalized title. Returns (dst_id, dst_title) with exactly one non-empty.
fn parse_link_key(inner: &str) -> (String, String) {
    let t = inner.trim();
    if let Some(pos) = t.rfind('-') {
        let suffix = &t[pos + 1..];
        if suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_hexdigit()) {
            return (suffix.to_lowercase(), String::new());
        }
    }
    (String::new(), normalize_title(t))
}

/// Reconcile a note's outgoing `mentions` edges to exactly the [[wikilinks]] in
/// its body (full replace). Each link is stored as a slug-id (unambiguous) or a
/// plain title (manual link); both resolve at query time. Caller-held conn.
fn reconcile_edges_from_body_conn(
    conn: &rusqlite::Connection,
    account_id: &str,
    uuid: &str,
    label: &str,
    body_html: &str,
) -> SqlResult<()> {
    // Full-replace this note's derived edges across all three rels.
    conn.execute(
        "DELETE FROM edges WHERE account_id = ?1 AND src_uuid = ?2
           AND rel IN ('mentions', 'child_of', 'tagged')",
        params![account_id, uuid],
    )?;
    // mentions — [[wikilinks]] in the body (note → note).
    for link in extract_wikilinks(&strip_html_to_text(body_html)) {
        let (dst_id, dst_title) = parse_link_key(&link);
        if dst_id.is_empty() && dst_title.is_empty() {
            continue;
        }
        conn.execute(
            "INSERT OR IGNORE INTO edges (account_id, src_uuid, dst_id, dst_title, rel)
             VALUES (?1, ?2, ?3, ?4, 'mentions')",
            params![account_id, uuid, dst_id, dst_title],
        )?;
    }
    // child_of — note → its folder (dst_title = label path).
    let folder = label.trim();
    if !folder.is_empty() {
        conn.execute(
            "INSERT OR IGNORE INTO edges (account_id, src_uuid, dst_id, dst_title, rel)
             VALUES (?1, ?2, '', ?3, 'child_of')",
            params![account_id, uuid, folder],
        )?;
    }
    // tagged — note → each inline #hashtag (dst_title = tag).
    for tag in tags_from_body(body_html) {
        conn.execute(
            "INSERT OR IGNORE INTO edges (account_id, src_uuid, dst_id, dst_title, rel)
             VALUES (?1, ?2, '', ?3, 'tagged')",
            params![account_id, uuid, tag],
        )?;
    }
    Ok(())
}

/// Reconcile a note's `cites` edges to exactly the URLs found in its body
/// (full replace, mirroring `reconcile_tags_from_body_conn`). Runs on every
/// write so any URL typed or pasted anywhere becomes a citation, not just
/// ones arriving through Extract. Caller-held connection.
fn reconcile_citations_from_body_conn(
    conn: &rusqlite::Connection,
    account_id: &str,
    uuid: &str,
    body_html: &str,
) -> SqlResult<()> {
    conn.execute(
        "DELETE FROM edges WHERE account_id = ?1 AND src_uuid = ?2 AND rel = 'cites'",
        params![account_id, uuid],
    )?;
    for url in extract_urls(&strip_html_to_text(body_html)) {
        conn.execute(
            "INSERT OR IGNORE INTO edges (account_id, src_uuid, dst_id, dst_title, rel)
             VALUES (?1, ?2, ?3, '', 'cites')",
            params![account_id, uuid, url],
        )?;
    }
    Ok(())
}

/// Rename (`new = Some`) or remove (`new = None`) an inline hashtag inside a
/// note's body HTML, touching only TEXT content — never inside `<tags>` or
/// attributes (so `#fff` in a style or `#section` in an href is left alone).
/// Word-boundary + case-insensitive match so `#work` doesn't also hit
/// `#working` or `#Work`. Returns the rewritten HTML (== input if not present).
fn rewrite_hashtag_in_body(body_html: &str, old: &str, new: Option<&str>) -> String {
    let chars: Vec<char> = body_html.chars().collect();
    let old_lower = old.to_lowercase();
    let mut out = String::with_capacity(body_html.len());
    let mut in_tag = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '<' {
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
        } else if !in_tag
            && c == '#'
            && (i == 0 || !is_tag_word_char(chars[i - 1]))
        {
            let start = i + 1;
            let mut j = start;
            while j < chars.len() && is_tag_word_char(chars[j]) {
                j += 1;
            }
            let word: String = chars[start..j].iter().collect();
            if word.to_lowercase() == old_lower {
                if let Some(t) = new {
                    out.push('#');
                    out.push_str(t);
                }
                // None → drop the whole "#word" (removal).
                i = j;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Rewrite every slug-form wikilink that targets `uuid8` to use `new_slug`.
/// A slug link is `[[<title-slug>-<uuid8>]]` (or titleless `[[<uuid8>]]`), where
/// uuid8 is the 8 hex chars immediately before `]]` (after the final `-`).
/// Only links whose trailing 8-hex token equals `uuid8` (case-insensitive) are
/// rewritten — links to other notes and plain `[[Title]]` (no `-uuid8` suffix)
/// are left untouched. Returns the body (== input if nothing matched).
fn rewrite_wikilink_slug_in_body(body_html: &str, uuid8: &str, new_slug: &str) -> String {
    let want = uuid8.to_lowercase();
    let bytes = body_html.as_bytes();
    let mut out = String::with_capacity(body_html.len());
    let mut i = 0;
    while i < body_html.len() {
        // Look for the start of a wikilink `[[`.
        if bytes[i] == b'[' && i + 1 < body_html.len() && bytes[i + 1] == b'[' {
            // Find the closing `]]`.
            if let Some(rel_end) = body_html[i + 2..].find("]]") {
                let inner = &body_html[i + 2..i + 2 + rel_end];
                // Trailing token after the last '-' (or the whole inner if none):
                // that's the uuid8 for a slug link.
                let tail = inner.rsplit('-').next().unwrap_or(inner);
                if tail.len() == 8
                    && tail.bytes().all(|b| b.is_ascii_hexdigit())
                    && tail.eq_ignore_ascii_case(&want)
                {
                    out.push_str("[[");
                    out.push_str(new_slug);
                    out.push_str("]]");
                    i = i + 2 + rel_end + 2; // past the closing ]]
                    continue;
                }
            }
        }
        // Default: copy this char (advance by full UTF-8 width).
        let ch_len = body_html[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        out.push_str(&body_html[i..i + ch_len]);
        i += ch_len;
    }
    out
}

/// After a note's title (hence slug) changes, rewrite slug-form `[[*-uuid8]]`
/// links to it in every carrying note's body so the displayed link text stays
/// fresh. Carriers are found via the edges index (rel='mentions', dst_id=uuid8 —
/// uses `idx_edges_dst_id`), so no full scan. Each rewritten carrier: body
/// updated, local_version bumped, clean→dirty (so the worker re-syncs it),
/// tags/edges/fts re-derived. Self-links (carrier == target) are included —
/// their text should update too. Caller-held connection (non-reentrant Mutex):
/// called from within `apply_local_edit`, which already holds the lock and runs
/// no explicit transaction, so this mirrors that (statements directly on `conn`,
/// no nested transaction — `Connection::transaction` needs `&mut`). Returns the
/// number of carriers rewritten.
fn rewrite_links_to_renamed_note_conn(
    conn: &rusqlite::Connection,
    account_id: &str,
    target_uuid: &str,
    new_slug: &str,
) -> SqlResult<usize> {
    let uuid8 = uuid_short(target_uuid);
    let rows: Vec<(String, String, String, String)> = {
        let mut s = conn.prepare(
            "SELECT DISTINCT n.uuid, n.title, n.label, n.body_html FROM notes n
             JOIN edges e ON e.account_id = n.account_id AND e.src_uuid = n.uuid
             WHERE n.account_id = ?1 AND e.rel = 'mentions' AND e.dst_id = ?2
               AND n.sync_state != 'deleted_pending'",
        )?;
        let it = s.query_map(params![account_id, uuid8], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
        it.filter_map(|x| x.ok()).collect()
    };
    let mut count = 0usize;
    for (uuid, title, label, body_html) in &rows {
        let new_body = rewrite_wikilink_slug_in_body(body_html, &uuid8, new_slug);
        if &new_body == body_html {
            continue;
        }
        conn.execute(
            "UPDATE notes SET body_html = ?1,
                 local_version = local_version + 1,
                 sync_state = CASE sync_state WHEN 'clean' THEN 'dirty' ELSE sync_state END,
                 last_local_modified_at = ?2
             WHERE uuid = ?3 AND account_id = ?4",
            params![new_body, now_ms(), uuid, account_id],
        )?;
        reconcile_tags_from_body_conn(conn, account_id, uuid, &new_body)?;
        reconcile_edges_from_body_conn(conn, account_id, uuid, label, &new_body)?;
        reconcile_citations_from_body_conn(conn, account_id, uuid, &new_body)?;
        fts_index_conn(conn, uuid, account_id, title, &new_body)?;
        count += 1;
    }
    Ok(count)
}

/// Reconcile a note's note_tags rows to EXACTLY its body-derived hashtags
/// (full replace). The body is the single source of truth for tags now, so this
/// runs on every note write — an Apple-added or in-editor #hashtag becomes a
/// tag, and removing the #hashtag removes the tag. Caller-held connection
/// (non-reentrant Mutex). NOTE: the one-time `migrate_tags_to_body` pass must
/// have run first so legacy sidecar-only tags are present in the body before
/// this can prune them.
fn reconcile_tags_from_body_conn(
    conn: &rusqlite::Connection,
    account_id: &str,
    uuid: &str,
    body_html: &str,
) -> SqlResult<()> {
    conn.execute(
        "DELETE FROM note_tags WHERE account_id = ?1 AND uuid = ?2",
        params![account_id, uuid],
    )?;
    for tag in tags_from_body(body_html) {
        conn.execute(
            "INSERT OR IGNORE INTO note_tags (account_id, uuid, tag) VALUES (?1, ?2, ?3)",
            params![account_id, uuid, tag],
        )?;
    }
    Ok(())
}

/// Index (or re-index) one note in the FTS table using a caller-held connection
/// (avoids re-locking the Mutex, which is non-reentrant). Body is HTML-stripped
/// first. FTS5 has no UPSERT → delete-then-insert.
fn fts_index_conn(
    conn: &rusqlite::Connection,
    uuid: &str,
    account_id: &str,
    title: &str,
    body_html: &str,
) -> SqlResult<()> {
    let body = strip_html_to_text(body_html);
    conn.execute(
        "DELETE FROM notes_fts WHERE uuid = ?1 AND account_id = ?2",
        params![uuid, account_id],
    )?;
    conn.execute(
        "INSERT INTO notes_fts (uuid, account_id, title, body) VALUES (?1, ?2, ?3, ?4)",
        params![uuid, account_id, title, body],
    )?;
    Ok(())
}

fn fts_delete_conn(conn: &rusqlite::Connection, uuid: &str, account_id: &str) -> SqlResult<()> {
    conn.execute(
        "DELETE FROM notes_fts WHERE uuid = ?1 AND account_id = ?2",
        params![uuid, account_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        extract_hashtags, extract_urls, extract_wikilinks, note_slug, parse_link_key,
        rewrite_hashtag_in_body, rewrite_wikilink_slug_in_body, slugify,
    };

    #[test]
    fn slug_and_note_slug() {
        assert_eq!(slugify("Hello World!"), "hello-world");
        assert_eq!(slugify("  multi   space "), "multi-space");
        assert_eq!(
            note_slug("Meeting Notes", "48A58D89-FE95-4A78-8835-41420134D25C"),
            "meeting-notes-48a58d89"
        );
        // Empty title → just the id.
        assert_eq!(note_slug("   ", "48A58D89-FE95-..."), "48a58d89");
    }

    #[test]
    fn rewrite_wikilink_slug_updates_matching_uuid8() {
        let b = "see [[old-title-abc12345]] and [[other-def67890]] and [[Plain Title]] end";
        let out = rewrite_wikilink_slug_in_body(b, "abc12345", "new-title-abc12345");
        assert_eq!(
            out,
            "see [[new-title-abc12345]] and [[other-def67890]] and [[Plain Title]] end",
            "only the matching uuid8 link is rewritten; other uuid8 + plain title untouched"
        );
    }

    #[test]
    fn rewrite_wikilink_slug_handles_hyphenated_titles_and_titleless() {
        let b = "[[weekly-review-notes-abc12345]] [[abc12345]]";
        let out = rewrite_wikilink_slug_in_body(b, "abc12345", "renamed-abc12345");
        assert_eq!(out, "[[renamed-abc12345]] [[renamed-abc12345]]");
    }

    #[test]
    fn rewrite_wikilink_slug_uuid8_case_insensitive_and_noop_when_absent() {
        let b = "[[t-ABC12345]] [[nope-99999999]]";
        let out = rewrite_wikilink_slug_in_body(b, "abc12345", "x-abc12345");
        assert_eq!(out, "[[x-abc12345]] [[nope-99999999]]");
        assert_eq!(
            rewrite_wikilink_slug_in_body("[[a-11111111]]", "abc12345", "x-abc12345"),
            "[[a-11111111]]"
        );
    }

    #[test]
    fn parse_link_id_vs_title() {
        assert_eq!(
            parse_link_key("meeting-notes-48a58d89"),
            ("48a58d89".to_string(), String::new())
        );
        assert_eq!(
            parse_link_key("Note3"),
            (String::new(), "note3".to_string())
        );
        // trailing segment not 8 hex → treated as a plain title.
        assert_eq!(
            parse_link_key("plain-title"),
            (String::new(), "plain-title".to_string())
        );
    }

    #[test]
    fn wikilinks_basic_dedup_trim() {
        assert_eq!(
            extract_wikilinks("see [[Note3]] and [[ Meeting Notes ]] then [[Note3]]"),
            vec!["Note3", "Meeting Notes"]
        );
    }

    #[test]
    fn wikilinks_empty_and_unterminated() {
        assert_eq!(extract_wikilinks("[[]] plain [[no close"), Vec::<String>::new());
        assert_eq!(extract_wikilinks("none here"), Vec::<String>::new());
    }

    #[test]
    fn wikilinks_thai() {
        assert_eq!(extract_wikilinks("ดู [[บันทึกประชุม]]"), vec!["บันทึกประชุม"]);
    }

    #[test]
    fn rewrite_rename_in_text() {
        assert_eq!(
            rewrite_hashtag_in_body("<div>a #work b</div>", "work", Some("job")),
            "<div>a #job b</div>"
        );
    }

    #[test]
    fn rewrite_delete_in_text() {
        assert_eq!(
            rewrite_hashtag_in_body("<div>#work done</div>", "work", None),
            "<div> done</div>"
        );
    }

    #[test]
    fn rewrite_skips_tag_interiors() {
        // #work inside the href attribute must stay; only the body text changes.
        assert_eq!(
            rewrite_hashtag_in_body(r##"<a href="#work">x</a> #work"##, "work", Some("job")),
            r##"<a href="#work">x</a> #job"##
        );
    }

    #[test]
    fn rewrite_word_boundary() {
        assert_eq!(
            rewrite_hashtag_in_body("#working #work", "work", Some("job")),
            "#working #job"
        );
    }

    #[test]
    fn rewrite_thai_and_case_insensitive() {
        assert_eq!(rewrite_hashtag_in_body("x #งาน", "งาน", None), "x ");
        assert_eq!(rewrite_hashtag_in_body("#Work", "work", Some("job")), "#job");
    }

    #[test]
    fn basic_and_dedup_and_order() {
        assert_eq!(
            extract_hashtags("a #work item #urgent then #work again"),
            vec!["work", "urgent"]
        );
    }

    #[test]
    fn lowercased() {
        assert_eq!(extract_hashtags("#Work #WORK #work"), vec!["work"]);
    }

    #[test]
    fn thai_tags() {
        assert_eq!(extract_hashtags("จด #งาน และ #โปรเจกต์"), vec!["งาน", "โปรเจกต์"]);
    }

    #[test]
    fn hyphenated_tags_stay_one_tag() {
        // Compound kebab-case tags (LLM extraction emits these) must parse as
        // one tag, not truncate at the hyphen. Regression test for the
        // database-migrations → "database" bug.
        assert_eq!(
            extract_hashtags("#database-migrations and #local-first"),
            vec!["database-migrations", "local-first"]
        );
    }

    #[test]
    fn hyphen_at_word_boundary_does_not_eat_following_text() {
        // Make sure the hyphen rule doesn't accidentally absorb trailing
        // non-tag characters via the body parser.
        assert_eq!(
            extract_hashtags("hyphenated #foo-bar, then a comma"),
            vec!["foo-bar"]
        );
    }

    #[test]
    fn leading_hyphen_after_hash_is_not_a_tag() {
        // `#-foo` has no alphabetic char before the dash so the result starts
        // with a hyphen. We keep this because: (a) the alphabetic-content
        // check inside extract_hashtags still requires at least one letter, and
        // (b) leading-hyphen tags are weird but not actively harmful.
        // Just document the behavior: returns the tag, lowercased.
        assert_eq!(extract_hashtags("#-foo"), vec!["-foo"]);
    }

    #[test]
    fn requires_a_letter_not_all_digits() {
        // #2024 is not a tag; #q1 is (has a letter).
        assert_eq!(extract_hashtags("due #2024 in #q1"), vec!["q1"]);
    }

    #[test]
    fn needs_word_boundary() {
        // mid-word # (foo#bar, color hex) is not a tag.
        assert_eq!(extract_hashtags("foo#bar baz"), Vec::<String>::new());
        assert_eq!(extract_hashtags("see #real tag"), vec!["real"]);
    }

    #[test]
    fn underscores_allowed_stops_at_punct() {
        assert_eq!(extract_hashtags("#my_tag, #next."), vec!["my_tag", "next"]);
    }

    #[test]
    fn empty_and_bare_hash() {
        assert_eq!(extract_hashtags("nothing here"), Vec::<String>::new());
        assert_eq!(extract_hashtags("a # b"), Vec::<String>::new());
    }

    #[test]
    fn extract_urls_basic_dedup() {
        assert_eq!(
            extract_urls("see https://example.com/a and https://example.com/a again"),
            vec!["https://example.com/a".to_string()]
        );
    }

    #[test]
    fn extract_urls_decodes_html_entities() {
        // Reproduces a real bug found in live testing: a contenteditable's
        // innerHTML serialization escapes `&` to `&amp;` on save, so a
        // hand-typed note's stored body_html contains `&amp;t=2831s` for a
        // pasted URL like `...&t=2831s`. Without decoding, extract_urls
        // would capture the literal "&amp;t=2831s" text, which never
        // string-equals the same URL scanned from Extract's raw (unescaped)
        // pasted source — silently defeating duplicate-citation detection
        // for any URL containing `&`, i.e. most real multi-param URLs
        // (YouTube, Google, etc.).
        assert_eq!(
            extract_urls("https://www.youtube.com/watch?v=ZXg70X9RhKs&amp;t=2831s"),
            vec!["https://www.youtube.com/watch?v=ZXg70X9RhKs&t=2831s".to_string()]
        );
    }

    #[test]
    fn extract_urls_html_escaped_and_raw_forms_match() {
        // The actual bug scenario end-to-end: the SAME url, once as it would
        // be stored in body_html (entity-escaped) and once as it would
        // appear in Extract's raw pasted source_text (unescaped), must
        // produce identical extract_urls() output so find_citation_owner's
        // exact-string dst_id match succeeds.
        let stored_in_body = extract_urls(
            "<div>https://www.youtube.com/watch?v=ZXg70X9RhKs&amp;t=2831s</div>",
        );
        let from_raw_source =
            extract_urls("https://www.youtube.com/watch?v=ZXg70X9RhKs&t=2831s");
        assert_eq!(stored_in_body, from_raw_source);
    }

    #[test]
    fn extract_urls_trailing_punctuation() {
        assert_eq!(
            extract_urls("check this out: https://example.com/page."),
            vec!["https://example.com/page".to_string()]
        );
    }

    #[test]
    fn extract_urls_markdown_link_syntax() {
        assert_eq!(
            extract_urls("[the docs](https://example.com/docs) explain it"),
            vec!["https://example.com/docs".to_string()]
        );
    }

    #[test]
    fn extract_urls_multiple_distinct() {
        assert_eq!(
            extract_urls("https://a.example and https://b.example/path?q=1"),
            vec!["https://a.example".to_string(), "https://b.example/path?q=1".to_string()]
        );
    }

    #[test]
    fn extract_urls_no_false_positive_on_substring() {
        assert_eq!(
            extract_urls("this note discusses http caching headers"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn extract_urls_empty_text() {
        assert_eq!(extract_urls(""), Vec::<String>::new());
    }
}

#[cfg(test)]
mod tests_slug_rewrite {
    use super::*;
    use tempfile::tempdir;

    fn temp_db() -> Db {
        let dir = tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::mem::forget(dir); // keep the temp path alive for the test
        Db::open(&path).expect("open temp db")
    }

    /// Build a minimal clean local note (sync metadata zeroed, no pin/tags).
    fn mk_note(account_id: &str, uuid: &str, title: &str, body_html: &str) -> CachedNote {
        CachedNote {
            uuid: uuid.to_string(),
            account_id: account_id.to_string(),
            id: String::new(),
            title: title.to_string(),
            body_html: body_html.to_string(),
            date: "Thu, 4 Jun 2026 01:19:50 +0700".to_string(),
            x_mail_created_date: None,
            label: "Notes".to_string(),
            local_version: 0,
            remote_version: None,
            sync_state: SyncState::Clean,
            last_synced_at: None,
            last_local_modified_at: 0,
            last_remote_modified_at: None,
            pinned: false,
            meta_msg_id: None,
            pin_dirty: false,
            tags_meta_msg_id: None,
            tags_dirty: false,
        }
    }

    /// Renaming note B (via apply_local_edit) rewrites the DISPLAYED slug text of
    /// the `[[*-uuid8B]]` link in carrier note A, flips A clean→dirty, and leaves
    /// B's own row otherwise as the edit made it. The link still resolves to B
    /// (edge dst_id == uuid8B unchanged).
    #[test]
    fn rename_rewrites_inbound_slug_link_text() {
        let db = temp_db();
        let acct = "test@example.com";
        let b_uuid = "AAAAAAAA-1111-2222-3333-444444444444"; // uuid8 = "aaaaaaaa"
        let uuid8 = uuid_short(b_uuid);
        assert_eq!(uuid8, "aaaaaaaa");

        // B exists with its original title.
        db.insert_local_new(&mk_note(acct, b_uuid, "Old Title B", "<div>body of B</div>"))
            .unwrap();
        // A links to B by the slug-form wikilink (so edges has A -> dst_id=uuid8).
        let a_uuid = "BBBBBBBB-9999-8888-7777-666666666666";
        db.insert_local_new(&mk_note(
            acct,
            a_uuid,
            "Note A",
            "<div>see [[old-title-b-aaaaaaaa]] here</div>",
        ))
        .unwrap();

        // Sanity: the edge resolves to B's uuid8 before the rename.
        let dst_before: String = {
            let conn = db.conn.lock().unwrap();
            conn.query_row(
                "SELECT dst_id FROM edges WHERE account_id=?1 AND src_uuid=?2 AND rel='mentions'",
                params![acct, a_uuid],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(dst_before, "aaaaaaaa");

        // Rename B: new title -> new slug. This is the trigger.
        db.apply_local_edit(b_uuid, acct, "New Title B", "<div>body of B</div>", "Notes")
            .unwrap();

        // A's body now shows the fresh slug; B's uuid8 (hence resolution) is unchanged.
        let (a_body, a_state): (String, String) = {
            let conn = db.conn.lock().unwrap();
            conn.query_row(
                "SELECT body_html, sync_state FROM notes WHERE account_id=?1 AND uuid=?2",
                params![acct, a_uuid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
        };
        assert!(
            a_body.contains("[[new-title-b-aaaaaaaa]]"),
            "A's link text should be rewritten to the new slug, got: {a_body}"
        );
        assert!(
            !a_body.contains("old-title-b"),
            "stale slug text must be gone, got: {a_body}"
        );
        assert_eq!(a_state, "dirty", "carrier A must be marked dirty for re-sync");

        // Edge still resolves to B by uuid8 (navigation never broke).
        let dst_after: String = {
            let conn = db.conn.lock().unwrap();
            conn.query_row(
                "SELECT dst_id FROM edges WHERE account_id=?1 AND src_uuid=?2 AND rel='mentions'",
                params![acct, a_uuid],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(dst_after, "aaaaaaaa", "link still resolves to B");
    }

    /// A title edit that does NOT change the slug (e.g. trailing whitespace) must
    /// NOT touch carriers — no spurious clean->dirty churn.
    #[test]
    fn no_slug_change_leaves_carriers_clean() {
        let db = temp_db();
        let acct = "test@example.com";
        let b_uuid = "CCCCCCCC-1111-2222-3333-444444444444"; // uuid8 = "cccccccc"
        db.insert_local_new(&mk_note(acct, b_uuid, "Title B", "<div>b</div>")).unwrap();
        let a_uuid = "DDDDDDDD-9999-8888-7777-666666666666";
        db.insert_local_new(&mk_note(
            acct,
            a_uuid,
            "Note A",
            "<div>[[title-b-cccccccc]]</div>",
        ))
        .unwrap();

        // Re-save B with a title whose slug is identical ("Title B " slugifies the same).
        db.apply_local_edit(b_uuid, acct, "Title B ", "<div>b</div>", "Notes").unwrap();

        let a_state: String = {
            let conn = db.conn.lock().unwrap();
            conn.query_row(
                "SELECT sync_state FROM notes WHERE account_id=?1 AND uuid=?2",
                params![acct, a_uuid],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(a_state, "clean", "carrier must stay clean when slug is unchanged");
    }
}

#[cfg(test)]
mod tests_citations {
    use super::*;
    use tempfile::tempdir;

    fn temp_db() -> Db {
        let dir = tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::mem::forget(dir);
        Db::open(&path).expect("open temp db")
    }

    fn mk_note(account_id: &str, uuid: &str, title: &str, body_html: &str) -> CachedNote {
        CachedNote {
            uuid: uuid.to_string(),
            account_id: account_id.to_string(),
            id: String::new(),
            title: title.to_string(),
            body_html: body_html.to_string(),
            date: "Thu, 4 Jun 2026 01:19:50 +0700".to_string(),
            x_mail_created_date: None,
            label: "Notes".to_string(),
            local_version: 0,
            remote_version: None,
            sync_state: SyncState::Clean,
            last_synced_at: None,
            last_local_modified_at: 0,
            last_remote_modified_at: None,
            pinned: false,
            meta_msg_id: None,
            pin_dirty: false,
            tags_meta_msg_id: None,
            tags_dirty: false,
        }
    }

    #[test]
    fn insert_local_new_derives_cites_edges() {
        let db = temp_db();
        let acct = "test@example.com";
        let uuid = "AAAAAAAA-1111-2222-3333-444444444444";
        db.insert_local_new(&mk_note(
            acct,
            uuid,
            "Note A",
            "<div>see https://example.com/source and https://example.com/source again</div>",
        ))
        .unwrap();

        let conn = db.conn.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM edges WHERE account_id=?1 AND src_uuid=?2 AND rel='cites'",
                params![acct, uuid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "dedup within one body must produce one edge row");
    }

    #[test]
    fn apply_local_edit_full_replaces_cites_edges() {
        let db = temp_db();
        let acct = "test@example.com";
        let uuid = "BBBBBBBB-1111-2222-3333-444444444444";
        db.insert_local_new(&mk_note(acct, uuid, "Note B", "<div>https://old.example</div>"))
            .unwrap();
        db.apply_local_edit(uuid, acct, "Note B", "<div>https://new.example</div>", "Notes")
            .unwrap();

        let conn = db.conn.lock().unwrap();
        let urls: Vec<String> = conn
            .prepare("SELECT dst_id FROM edges WHERE account_id=?1 AND src_uuid=?2 AND rel='cites'")
            .unwrap()
            .query_map(params![acct, uuid], |r| r.get(0))
            .unwrap()
            .collect::<SqlResult<Vec<String>>>()
            .unwrap();
        assert_eq!(urls, vec!["https://new.example".to_string()], "old URL must be dropped, not accumulated");
    }

    #[test]
    fn citations_returns_sorted_urls() {
        let db = temp_db();
        let acct = "test@example.com";
        let uuid = "CCCCCCCC-1111-2222-3333-444444444444";
        db.insert_local_new(&mk_note(
            acct,
            uuid,
            "Note C",
            "<div>https://z.example and https://a.example</div>",
        ))
        .unwrap();
        let urls = db.citations(acct, uuid).unwrap();
        assert_eq!(urls, vec!["https://a.example".to_string(), "https://z.example".to_string()]);
    }

    #[test]
    fn find_citation_owner_locates_and_excludes() {
        let db = temp_db();
        let acct = "test@example.com";
        let owner_uuid = "DDDDDDDD-1111-2222-3333-444444444444";
        db.insert_local_new(&mk_note(acct, owner_uuid, "Owner", "<div>https://shared.example</div>"))
            .unwrap();

        let found = db.find_citation_owner(acct, "https://shared.example", None).unwrap();
        assert_eq!(found.map(|n| n.uuid), Some(owner_uuid.to_string()));

        let excluded = db
            .find_citation_owner(acct, "https://shared.example", Some(owner_uuid))
            .unwrap();
        assert!(excluded.is_none(), "excluding the owner's own uuid must suppress the match");

        let none = db.find_citation_owner(acct, "https://not-cited.example", None).unwrap();
        assert!(none.is_none());
    }
}

#[cfg(test)]
mod tests_smart_folders {
    use super::*;
    use tempfile::tempdir;

    fn temp_db() -> Db {
        let dir = tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::mem::forget(dir);
        Db::open(&path).expect("open temp db")
    }

    fn mk_note(account_id: &str, uuid: &str, title: &str, body_html: &str) -> CachedNote {
        CachedNote {
            uuid: uuid.to_string(),
            account_id: account_id.to_string(),
            id: String::new(),
            title: title.to_string(),
            body_html: body_html.to_string(),
            date: "Thu, 4 Jun 2026 01:19:50 +0700".to_string(),
            x_mail_created_date: None,
            label: "Notes".to_string(),
            local_version: 0,
            remote_version: None,
            sync_state: SyncState::Clean,
            last_synced_at: None,
            last_local_modified_at: now_ms(),
            last_remote_modified_at: None,
            pinned: false,
            meta_msg_id: None,
            pin_dirty: false,
            tags_meta_msg_id: None,
            tags_dirty: false,
        }
    }

    #[test]
    fn orphaned_excludes_linked_notes() {
        let db = temp_db();
        let acct = "test@example.com";
        let linked_uuid = "EEEEEEEE-1111-2222-3333-444444444444"; // uuid8 = "eeeeeeee"
        let orphan_uuid = "FFFFFFFF-1111-2222-3333-444444444444";
        db.insert_local_new(&mk_note(acct, linked_uuid, "Linked Note", "<div>no links</div>"))
            .unwrap();
        db.insert_local_new(&mk_note(acct, orphan_uuid, "Orphan Note", "<div>no links</div>"))
            .unwrap();
        // A third note links to "Linked Note" by title.
        db.insert_local_new(&mk_note(
            acct,
            "AAAAAAAA-9999-8888-7777-666666666666",
            "Linker",
            "<div>[[Linked Note]]</div>",
        ))
        .unwrap();

        let orphans = db.list_orphaned_notes(acct).unwrap();
        let orphan_uuids: Vec<&str> = orphans.iter().map(|n| n.uuid.as_str()).collect();
        assert!(orphan_uuids.contains(&orphan_uuid), "unlinked note must appear as orphaned");
        assert!(!orphan_uuids.contains(&linked_uuid), "linked note must NOT appear as orphaned");
    }

    #[test]
    fn stale_respects_30_day_threshold() {
        let db = temp_db();
        let acct = "test@example.com";
        let fresh_uuid = "11111111-1111-2222-3333-444444444444";
        let stale_uuid = "22222222-1111-2222-3333-444444444444";
        db.insert_local_new(&mk_note(acct, fresh_uuid, "Fresh", "<div>x</div>")).unwrap();
        db.insert_local_new(&mk_note(acct, stale_uuid, "Stale", "<div>x</div>")).unwrap();

        // Backdate the "Stale" note's last_local_modified_at to 31 days ago.
        let thirty_one_days_ago_ms = now_ms() - 31 * 24 * 60 * 60 * 1000;
        {
            let conn = db.conn.lock().unwrap();
            conn.execute(
                "UPDATE notes SET last_local_modified_at = ?1 WHERE account_id = ?2 AND uuid = ?3",
                params![thirty_one_days_ago_ms, acct, stale_uuid],
            )
            .unwrap();
        }

        let stale = db.list_stale_notes(acct).unwrap();
        let stale_uuids: Vec<&str> = stale.iter().map(|n| n.uuid.as_str()).collect();
        assert!(stale_uuids.contains(&stale_uuid), "backdated note must appear as stale");
        assert!(!stale_uuids.contains(&fresh_uuid), "freshly-created note must NOT appear as stale");
    }
}

/// `notes.date` is the raw RFC822 `Date` header string ("Wed, 9 Sep 2020 …"),
/// so `ORDER BY date DESC` sorts lexically by weekday name — "Wed" beats "Thu"
/// and a 2020 note outranks a 2026 one. Every list query must instead order by
/// the epoch-ms `last_remote_modified_at` (falling back to
/// `last_local_modified_at`, which is NOT NULL, for rows created locally and
/// not yet pushed).
#[cfg(test)]
mod tests_chronological_order {
    use super::*;
    use tempfile::tempdir;

    fn temp_db() -> Db {
        let dir = tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::mem::forget(dir);
        Db::open(&path).expect("open temp db")
    }

    // Deliberately adversarial pair: lexically "Wed…2020" > "Thu…2026", so any
    // string sort puts the OLD note first while the epochs say the opposite.
    const OLD_DATE: &str = "Wed, 9 Sep 2020 14:23:15 +0700";
    const OLD_MS: i64 = 1_599_636_195_000;
    const NEW_DATE: &str = "Thu, 4 Jun 2026 01:19:50 +0700";
    const NEW_MS: i64 = 1_780_251_590_000;

    const OLD_UUID: &str = "AAAAAAAA-1111-2222-3333-444444444444";
    const NEW_UUID: &str = "BBBBBBBB-1111-2222-3333-444444444444";

    fn mk_note(
        account_id: &str,
        uuid: &str,
        title: &str,
        body_html: &str,
        date: &str,
        remote_ms: i64,
    ) -> CachedNote {
        CachedNote {
            uuid: uuid.to_string(),
            account_id: account_id.to_string(),
            id: String::new(),
            title: title.to_string(),
            body_html: body_html.to_string(),
            date: date.to_string(),
            x_mail_created_date: None,
            label: "Notes".to_string(),
            local_version: 0,
            remote_version: None,
            sync_state: SyncState::Clean,
            last_synced_at: None,
            // Deliberately the INVERSE of the remote order, so a query that
            // accidentally falls back to the local clock still fails the test.
            last_local_modified_at: if remote_ms == OLD_MS { 2 } else { 1 },
            last_remote_modified_at: Some(remote_ms),
            pinned: false,
            meta_msg_id: None,
            pin_dirty: false,
            tags_meta_msg_id: None,
            tags_dirty: false,
        }
    }

    /// Seeds one old + one new note carrying the `zq` marker and a wikilink to
    /// "Target", plus the "Target" note itself.
    fn seed(db: &Db, acct: &str) {
        db.insert_local_new(&mk_note(
            acct,
            OLD_UUID,
            "zq carrier old",
            "<div>zq body [[Target]]</div>",
            OLD_DATE,
            OLD_MS,
        ))
        .unwrap();
        db.insert_local_new(&mk_note(
            acct,
            NEW_UUID,
            "zq carrier new",
            "<div>zq body [[Target]]</div>",
            NEW_DATE,
            NEW_MS,
        ))
        .unwrap();
    }

    fn uuids(notes: &[CachedNote]) -> Vec<&str> {
        notes.iter().map(|n| n.uuid.as_str()).collect()
    }

    #[test]
    fn search_notes_like_fallback_is_newest_first() {
        let db = temp_db();
        let acct = "test@example.com";
        seed(&db, acct);
        // <3 chars → the LIKE fallback branch, which is the one that sorts by date.
        let hits = db.search_notes(Some(acct), None, "zq", &[]).unwrap();
        assert_eq!(uuids(&hits), vec![NEW_UUID, OLD_UUID]);
    }

    #[test]
    fn search_titles_is_newest_first() {
        let db = temp_db();
        let acct = "test@example.com";
        seed(&db, acct);
        let hits = db.search_titles(acct, "carrier", 10).unwrap();
        let ids: Vec<&str> = hits.iter().map(|(u, _, _)| u.as_str()).collect();
        assert_eq!(ids, vec![NEW_UUID, OLD_UUID]);
    }

    #[test]
    fn list_orphaned_notes_is_newest_first() {
        let db = temp_db();
        let acct = "test@example.com";
        seed(&db, acct);
        // Neither carrier has an incoming mention, so both are orphaned.
        let orphans = db.list_orphaned_notes(acct).unwrap();
        assert_eq!(uuids(&orphans), vec![NEW_UUID, OLD_UUID]);
    }

    #[test]
    fn backlinks_are_newest_first() {
        let db = temp_db();
        let acct = "test@example.com";
        let target_uuid = "CCCCCCCC-1111-2222-3333-444444444444";
        db.insert_local_new(&mk_note(
            acct,
            target_uuid,
            "Target",
            "<div>target body</div>",
            OLD_DATE,
            OLD_MS,
        ))
        .unwrap();
        seed(&db, acct);
        let back = db.backlinks(acct, target_uuid).unwrap();
        assert_eq!(uuids(&back), vec![NEW_UUID, OLD_UUID]);
    }

    #[test]
    fn outgoing_links_are_newest_first() {
        let db = temp_db();
        let acct = "test@example.com";
        // A source note linking to both carriers by plain title.
        let src_uuid = "DDDDDDDD-1111-2222-3333-444444444444";
        seed(&db, acct);
        db.insert_local_new(&mk_note(
            acct,
            src_uuid,
            "Source",
            "<div>[[zq carrier old]] and [[zq carrier new]]</div>",
            NEW_DATE,
            NEW_MS,
        ))
        .unwrap();
        let out = db.outgoing_links(acct, src_uuid).unwrap();
        assert_eq!(uuids(&out), vec![NEW_UUID, OLD_UUID]);
    }
}

#[cfg(test)]
mod tests_search_or_fallback {
    use crate::test_support::{note, temp_db};

    fn insert_note(db: &crate::db::Db, uuid: &str, account_id: &str, title: &str, body_html: &str, label: &str) {
        note(account_id, uuid).title(title).body(body_html).label(label).insert(db);
    }

    #[test]
    fn multi_term_query_falls_back_to_or_when_phrase_match_is_empty() {
        let db = temp_db();
        insert_note(&db, "u1", "acc", "Password note", "<p>my password is hunter2</p>", "Notes/A");
        insert_note(&db, "u2", "acc", "API note", "<p>the api key is xyz</p>", "Notes/A");
        insert_note(&db, "u3", "acc", "Unrelated", "<p>nothing relevant here</p>", "Notes/A");

        // The exact failure shape observed live: terms combined in one query,
        // never appearing as that literal six-word phrase anywhere.
        let hits = db
            .search_notes(None, None, "password api key token secret auth", &[])
            .unwrap();
        let uuids: Vec<&str> = hits.iter().map(|n| n.uuid.as_str()).collect();
        assert!(uuids.contains(&"u1"), "password hit missing: {uuids:?}");
        assert!(uuids.contains(&"u2"), "api key hit missing: {uuids:?}");
        assert!(!uuids.contains(&"u3"), "unrelated note should not match: {uuids:?}");
    }

    #[test]
    fn phrase_match_that_already_hits_is_not_widened() {
        // A query that DOES match as a literal phrase must return exactly
        // that match, not the (broader, noisier) OR-of-terms result — the
        // fallback must only fire when the strict match is empty.
        let db = temp_db();
        insert_note(&db, "u1", "acc", "Exact", "<p>the quick brown fox</p>", "Notes/A");
        insert_note(&db, "u2", "acc", "Partial", "<p>quick something fox somewhere</p>", "Notes/A");
        let hits = db.search_notes(None, None, "quick brown fox", &[]).unwrap();
        assert_eq!(hits.len(), 1, "phrase match must stay exact when it already hits: {hits:?}");
        assert_eq!(hits[0].uuid, "u1");
    }

    #[test]
    fn single_term_query_is_unaffected() {
        // Single-word queries are already a phrase of one token — the
        // fallback path must not change their (already-correct) behavior.
        let db = temp_db();
        insert_note(&db, "u1", "acc", "T", "<p>credential exposure</p>", "Notes/A");
        let hits = db.search_notes(None, None, "credential", &[]).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn same_uuid_in_two_accounts_both_survive_the_or_fallback() {
        // The notes PK is (uuid, account_id): the SAME uuid legitimately
        // exists in two accounts (CLAUDE.md roadmap #6 — a cross-account
        // move keeps the uuid). A cross-account search (account_id omitted,
        // the MCP server's default) must return BOTH rows.
        //
        // Deduping on `uuid` alone drops the second one silently. Note the
        // two rows differ in body as well as account, so a uuid-keyed
        // HashSet returns exactly one row here while a
        // (uuid, account_id)-keyed one returns two — that is the whole
        // discriminating power of this test, and why both must match the
        // query rather than one being a decoy.
        let db = temp_db();
        let shared = "SHARED-UUID-1234";
        insert_note(&db, shared, "work@x.com", "Work copy", "<p>my password lives here</p>", "Notes/A");
        insert_note(&db, shared, "home@x.com", "Home copy", "<p>the api key lives here</p>", "Notes/A");
        // A third, single-account note guards against the fallback silently
        // not firing at all (which would also return "2 rows" from a plain
        // phrase match if the phrase ever did hit).
        insert_note(&db, "other", "work@x.com", "Other", "<p>nothing relevant here</p>", "Notes/A");

        // Multi-token, matches nothing as a literal phrase → OR fallback.
        let hits = db
            .search_notes(None, None, "password api key token secret auth", &[])
            .unwrap();
        let ids: Vec<(&str, &str)> = hits
            .iter()
            .map(|n| (n.uuid.as_str(), n.account_id.as_str()))
            .collect();
        assert!(
            ids.contains(&(shared, "work@x.com")),
            "work account's copy dropped: {ids:?}"
        );
        assert!(
            ids.contains(&(shared, "home@x.com")),
            "home account's copy dropped — dedup keyed on uuid alone: {ids:?}"
        );
        assert!(!ids.contains(&("other", "work@x.com")), "unrelated note matched: {ids:?}");
    }

    #[test]
    fn or_fallback_still_dedupes_a_row_matched_by_two_tokens() {
        // The counterpart to the test above: widening the dedup key must not
        // stop it deduping. One note containing BOTH tokens is returned by
        // both per-token queries and must still appear exactly once.
        let db = temp_db();
        insert_note(&db, "u1", "acc", "Both", "<p>password and api key together</p>", "Notes/A");
        let hits = db
            .search_notes(None, None, "password api key token secret auth", &[])
            .unwrap();
        assert_eq!(hits.len(), 1, "row matched by several tokens must not repeat: {hits:?}");
    }
}

#[cfg(test)]
mod tests_ask_queries {
    use crate::test_support::{note, temp_db};

    #[test]
    fn subtree_includes_descendants_but_not_prefix_siblings() {
        let db = temp_db();
        let a = "acct@x";
        for label in ["Notes/A", "Notes/A/B", "Notes/A/B/C", "Notes/AB", "Notes/Other"] {
            // uuid doubles as a readable id; "Notes/AB" is the prefix sibling
            // that a bare LIKE 'Notes/A%' would wrongly match.
            note(a, label).label(label).insert(&db);
        }

        let got = db.list_notes_in_subtree(a, "Notes/A").unwrap();
        let mut labels: Vec<&str> = got.iter().map(|n| n.label.as_str()).collect();
        labels.sort();
        assert_eq!(
            labels,
            vec!["Notes/A", "Notes/A/B", "Notes/A/B/C"],
            "Notes/AB is a prefix sibling, not a descendant"
        );
    }

    #[test]
    fn recency_orders_by_epoch_not_by_the_rfc822_date_string() {
        let db = temp_db();
        let a = "acct@x";
        // The trap: lexically "Wed, ..." sorts above "Mon, ...", but Mon is the
        // NEWER note. A query that orders by `date` returns these backwards.
        note(a, "old").modified_ms(1_000).date("Wed, 9 Oct 2013 07:49:21 +0700").insert(&db);
        note(a, "new").modified_ms(9_000).date("Mon, 3 Aug 2020 11:39:07 +0700").insert(&db);

        let got = db.list_recent_notes(Some(a), 10, &[]).unwrap();
        assert_eq!(got[0].uuid, "new", "newest first, by last_remote_modified_at");
        assert_eq!(got[1].uuid, "old");
    }

    #[test]
    fn recency_respects_the_limit_and_spans_accounts_when_unscoped() {
        let db = temp_db();
        note("a1", "x1").modified_ms(5_000).insert(&db);
        note("a2", "x2").modified_ms(6_000).insert(&db);

        assert_eq!(db.list_recent_notes(None, 10, &[]).unwrap().len(), 2);
        assert_eq!(db.list_recent_notes(None, 1, &[]).unwrap().len(), 1);
        assert_eq!(db.list_recent_notes(Some("a1"), 10, &[]).unwrap().len(), 1);
    }

    #[test]
    fn deleted_pending_notes_are_excluded_everywhere() {
        let db = temp_db();
        let a = "acct@x";
        note(a, "keep").label("Notes/A").insert(&db);
        note(a, "gone").label("Notes/A").insert(&db);
        // NOTE the argument order: mark_deleted(uuid, account_id), not the
        // (account_id, uuid) order used by almost every other Db method
        // (db.rs:1736). Verified against the source - do not "fix" it.
        db.mark_deleted("gone", a).expect("mark deleted");

        assert!(db.list_notes_in_subtree(a, "Notes/A").unwrap().iter().all(|n| n.uuid != "gone"));
        assert!(db.list_recent_notes(Some(a), 10, &[]).unwrap().iter().all(|n| n.uuid != "gone"));
        assert_eq!(db.count_notes_in_scope(Some(a), Some("Notes/A"), &[]).unwrap(), 1);
    }

    #[test]
    fn count_in_scope_counts_the_subtree_and_the_account() {
        let db = temp_db();
        let a = "acct@x";
        note(a, "u1").label("Notes/A").insert(&db);
        note(a, "u2").label("Notes/A/B").insert(&db);
        note(a, "u3").label("Notes/Other").insert(&db);

        assert_eq!(db.count_notes_in_scope(Some(a), Some("Notes/A"), &[]).unwrap(), 2);
        assert_eq!(db.count_notes_in_scope(Some(a), None, &[]).unwrap(), 3);
        assert_eq!(db.count_notes_in_scope(None, None, &[]).unwrap(), 3);
    }

    #[test]
    fn note_by_uuid_returns_the_note_and_none_for_absent_or_deleted() {
        let db = temp_db();
        let a = "acct@x";
        note(a, "here").title("Found me").insert(&db);
        note(a, "gone").insert(&db);
        db.mark_deleted("gone", a).expect("mark deleted");

        assert_eq!(db.note_by_uuid(a, "here").unwrap().unwrap().title, "Found me");
        assert!(db.note_by_uuid(a, "gone").unwrap().is_none(), "deleted_pending reads as absent");
        assert!(db.note_by_uuid(a, "never-existed").unwrap().is_none());
    }

    /// list_tasks (spec 4.6(B)) must not scan every note's body_html to find
    /// checkboxes — the pre-filter is mandatory on a 6,655-note/18MB vault —
    /// and the label scope must be the recursive-subtree filter (gotcha #1),
    /// not a bare LIKE prefix that also matches the sibling "Notes/AB".
    #[test]
    fn list_notes_with_checkboxes_prefilters_and_scopes() {
        let db = temp_db();
        let a = "acc";
        db.create_folder_local_new(a, "Notes/A").unwrap();
        // one note with a task, one without, one in a sibling folder
        note(a, "u1").label("Notes/A").body(r#"<div><input type="checkbox">a</div>"#).insert(&db);
        note(a, "u2").label("Notes/A").body("<p>no tasks here</p>").insert(&db);
        note(a, "u3").label("Notes/AB").body(r#"<div><input type="checkbox">c</div>"#).insert(&db);

        let all = db.list_notes_with_checkboxes(a, None).unwrap();
        assert_eq!(all.len(), 2, "only checkbox-bearing notes");

        let scoped = db.list_notes_with_checkboxes(a, Some("Notes/A")).unwrap();
        assert_eq!(scoped.len(), 1, "Notes/AB is a sibling, not a descendant");
        assert_eq!(scoped[0].uuid, "u1");
    }
}

#[cfg(test)]
mod drain_complete_tests {
    use super::*;
    use crate::test_support::{note, temp_db};

    const DATE: &str = "Thu, 4 Jun 2026 01:19:50 +0700";

    /// The predicate that decides when a Draining account is finished. If it
    /// misses a queue, an account goes silent with work still unsent — the
    /// exact failure this whole design exists to prevent.
    #[test]
    fn has_pending_pushes_sees_every_queue() {
        let db = temp_db();
        let acct = "a@example.com";

        assert!(!db.has_pending_pushes(acct).unwrap(), "empty db has nothing pending");

        note(acct, "u1").insert(&db);
        db.apply_local_edit("u1", acct, "t", "<div>b</div>", "Notes").unwrap();
        assert!(db.has_pending_pushes(acct).unwrap(), "a dirty note is pending");

        db.mark_pushed("u1", acct, "mid-1", DATE, "<div>b</div>", 2).unwrap();
        assert!(!db.has_pending_pushes(acct).unwrap(), "pushed note is not pending");

        db.set_pin("u1", acct, true).unwrap();
        assert!(db.has_pending_pushes(acct).unwrap(), "a pin_dirty row is pending");
        db.mark_pin_pushed("u1", acct, Some("meta-1"), true).unwrap();
        assert!(!db.has_pending_pushes(acct).unwrap());

        db.mark_deleted("u1", acct).unwrap();
        assert!(db.has_pending_pushes(acct).unwrap(), "a deleted_pending row is pending");
    }

    /// Another account's queue must not keep this one draining forever.
    #[test]
    fn has_pending_pushes_is_scoped_to_one_account() {
        let db = temp_db();
        note("a@x", "u1").insert(&db);
        db.apply_local_edit("u1", "a@x", "t", "<div>b</div>", "Notes").unwrap();
        assert!(db.has_pending_pushes("a@x").unwrap());
        assert!(!db.has_pending_pushes("b@x").unwrap());
    }

    /// The `tags_dirty` term, on its own, with no other queue involved. This
    /// is the legacy tag-sidecar backlog flag (tags now ride inline with
    /// content, so in practice this queue rarely has rows — but the column,
    /// the worker drain, and this predicate's term over it all still exist,
    /// so a Draining account with a stray tags_dirty row must not go
    /// Inactive out from under it).
    #[test]
    fn has_pending_pushes_sees_tags_dirty() {
        let db = temp_db();
        let acct = "a@example.com";
        note(acct, "u1").insert(&db);
        assert!(!db.has_pending_pushes(acct).unwrap(), "freshly inserted note is clean");

        db.set_tags_dirty(acct, "u1").unwrap();
        assert!(db.has_pending_pushes(acct).unwrap(), "a tags_dirty row is pending");

        db.mark_tags_pushed("u1", acct, None).unwrap();
        assert!(!db.has_pending_pushes(acct).unwrap(), "cleared tags_dirty is not pending");
    }

    /// The folders term, on its own, with no note-side queue involved.
    #[test]
    fn has_pending_pushes_sees_dirty_folders() {
        let db = temp_db();
        let acct = "a@example.com";
        assert!(!db.has_pending_pushes(acct).unwrap(), "empty db has nothing pending");

        db.insert_folder_local_new(&CachedFolder {
            account_id: acct.to_string(),
            path: "Notes/Test".to_string(),
            label_id: None,
            sync_state: FolderSyncState::DirtyNew,
            last_local_modified_at: now_ms(),
            last_synced_at: None,
            kind: "user".to_string(),
        })
        .unwrap();
        assert!(db.has_pending_pushes(acct).unwrap(), "a dirty_new folder is pending");

        db.mark_folder_created(acct, "Notes/Test", "label-1").unwrap();
        assert!(!db.has_pending_pushes(acct).unwrap(), "a clean folder is not pending");
    }

    /// The numbers behind "finishing sync — 3 left". A wrong count is worse
    /// than none: it tells the user work is finished when it is not.
    #[test]
    fn count_pending_pushes_separates_the_four_kinds() {
        let db = temp_db();
        let acct = "a@example.com";

        for u in ["u1", "u2", "u3"] {
            note(acct, u).insert(&db);
        }
        db.apply_local_edit("u1", acct, "t", "<div>b</div>", "Notes").unwrap();
        db.apply_local_edit("u2", acct, "t", "<div>b</div>", "Notes").unwrap();
        db.mark_deleted("u3", acct).unwrap();
        db.mark_pushed("u1", acct, "mid-1", DATE, "<div>b</div>", 2).unwrap();
        db.set_pin("u1", acct, true).unwrap();

        let c = db.count_pending_pushes(acct).unwrap();
        assert_eq!(c.notes, 1, "u2 is still dirty");
        assert_eq!(c.deletes, 1, "u3 is deleted_pending");
        assert_eq!(c.pins, 1, "u1 is pin_dirty");
        assert_eq!(c.tags, 0);
        assert_eq!(c.folders, 0, "no folder rows were touched");
    }

    /// The asymmetry that made this field necessary: has_pending_pushes counts
    /// folders, so a count that did not would report "0 left" on an account the
    /// worker is still, correctly, refusing to silence.
    #[test]
    fn count_and_has_pending_agree_when_only_a_folder_is_dirty() {
        let db = temp_db();
        let acct = "a@example.com";
        db.insert_folder_local_new(&CachedFolder {
            account_id: acct.to_string(),
            path: "Notes/NewFolder".to_string(),
            label_id: None,
            sync_state: FolderSyncState::DirtyNew,
            last_local_modified_at: now_ms(),
            last_synced_at: None,
            kind: "user".to_string(),
        })
        .unwrap();

        assert!(db.has_pending_pushes(acct).unwrap());
        let c = db.count_pending_pushes(acct).unwrap();
        assert_eq!(c.folders, 1);
        assert!(
            c.notes + c.deletes + c.pins + c.tags + c.folders > 0,
            "the figure the UI shows must not be 0 while the account still drains"
        );
    }
}

#[cfg(test)]
mod cross_process_safety_tests {
    use super::*;

    /// `test_support::note()` always inserts `sync_state = Clean`, with no
    /// setter to start a row already `dirty` at a known `local_version` —
    /// exactly the state needed to simulate "the worker already read this
    /// row and is mid-push" before a second writer touches it. Built
    /// directly rather than extending the shared builder for one test.
    fn test_note(uuid: &str, account_id: &str, title: &str, body_html: &str, label: &str) -> CachedNote {
        CachedNote {
            uuid: uuid.to_string(),
            account_id: account_id.to_string(),
            id: String::new(),
            title: title.to_string(),
            body_html: body_html.to_string(),
            date: "Thu, 4 Jun 2026 01:19:50 +0700".to_string(),
            x_mail_created_date: None,
            label: label.to_string(),
            local_version: 1,
            remote_version: None,
            sync_state: SyncState::Dirty,
            last_synced_at: None,
            last_local_modified_at: now_ms(),
            last_remote_modified_at: None,
            pinned: false,
            meta_msg_id: None,
            pin_dirty: false,
            tags_meta_msg_id: None,
            tags_dirty: false,
        }
    }

    /// Reproduces the race a second writer (the MCP server, or a worker
    /// retry) makes real: the worker reads a dirty row at `local_version`
    /// N, starts pushing it, and — before the push completes — someone
    /// else edits the same row, bumping it to N+1 and still dirty. When the
    /// worker's push finishes and calls `mark_pushed` with the version it
    /// read (N), the UPDATE must be a no-op: the newer edit must survive,
    /// not get silently overwritten by `sync_state = 'clean'` and the
    /// stale pushed body.
    #[test]
    fn mark_pushed_with_stale_version_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().to_path_buf()).unwrap();
        let n = test_note("u1", "acc", "Title", "<p>v1</p>", "Notes");
        db.insert_local_new(&n).unwrap();

        // Worker reads the note (local_version = 1) and starts pushing.
        // Meanwhile a second process appends: local_version -> 2, still dirty.
        db.apply_local_edit("u1", "acc", "Title", "<p>v1</p><p>appended</p>", "Notes").unwrap();

        // Worker's push completes and calls mark_pushed with the version it READ.
        db.mark_pushed("u1", "acc", "msg-1", "Mon, 11 Aug 2026 10:00:00 +0700", "<p>v1</p>", 1).unwrap();

        let row = db.get("u1", "acc").unwrap().unwrap();
        assert_eq!(row.sync_state, SyncState::Dirty, "stale mark_pushed must not clean the row");
        assert_eq!(row.body_html, "<p>v1</p><p>appended</p>", "stale mark_pushed must not revert the body");

        // With the CURRENT version it applies normally.
        db.mark_pushed("u1", "acc", "msg-2", "Mon, 11 Aug 2026 10:00:05 +0700", "<p>v1</p><p>appended</p>", row.local_version).unwrap();
        let row = db.get("u1", "acc").unwrap().unwrap();
        assert_eq!(row.sync_state, SyncState::Clean);
        assert_eq!(row.id, "msg-2");
    }

    /// The sibling race `apply_local_edit` itself is exposed to: two local
    /// writers (the App's own editor, `jodd-mcp`, Extract's re-ingest, or
    /// auto-link's appends) can both read the same row, then both write.
    /// Plain `apply_local_edit` has no guard at all — whichever commits
    /// second silently discards the first. `apply_local_edit_versioned`
    /// must refuse (no-op) when the row moved since the caller's read,
    /// exactly like `mark_pushed` already does.
    #[test]
    fn apply_local_edit_versioned_refuses_stale_write() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().to_path_buf()).unwrap();
        let n = test_note("u2", "acc", "Title", "<p>base</p>", "Notes");
        db.insert_local_new(&n).unwrap();

        // Writer A reads the row at local_version = 1.
        let read_by_a = db.get("u2", "acc").unwrap().unwrap();
        assert_eq!(read_by_a.local_version, 1);

        // Writer B (a second process) commits first, bumping local_version to 2.
        db.apply_local_edit("u2", "acc", "Title", "<p>base</p><p>B's edit</p>", "Notes").unwrap();

        // Writer A's write now lands with the STALE version it read (1) — must
        // be refused, not silently applied over B's edit.
        let applied = db
            .apply_local_edit_versioned("u2", "acc", "Title", "<p>base</p><p>A's edit</p>", "Notes", read_by_a.local_version)
            .unwrap();
        assert!(!applied, "a write against a stale local_version must be refused");

        let row = db.get("u2", "acc").unwrap().unwrap();
        assert_eq!(row.body_html, "<p>base</p><p>B's edit</p>", "B's edit must survive untouched");

        // With the CURRENT version, A's (recomputed) write applies normally.
        let applied2 = db
            .apply_local_edit_versioned("u2", "acc", "Title", "<p>base</p><p>B's edit</p><p>A's edit</p>", "Notes", row.local_version)
            .unwrap();
        assert!(applied2, "a write against the current local_version must succeed");
        assert_eq!(db.get("u2", "acc").unwrap().unwrap().body_html, "<p>base</p><p>B's edit</p><p>A's edit</p>");
    }
}

#[cfg(test)]
mod exclusion_tests {
    use super::*;

    use crate::test_support::{note, temp_db};

    fn seeded() -> Db {
        let db = temp_db();
        for (acct, uuid) in [("a@x", "u1"), ("b@x", "u2")] {
            note(acct, uuid).title("findme").insert(&db);
        }
        db
    }

    /// Same two-account/two-note shape as `seeded()`, but each note's body
    /// also carries the inline hashtag `#t` — `note_tags` is derived from the
    /// body on write (`reconcile_tags_from_body_conn`, wired into
    /// `insert_local_new`), so there is no direct `note_tags` INSERT path
    /// reachable from a test; a body hashtag is the only way to get a row
    /// there, same as every other tag test in this codebase.
    fn seeded_with_tag() -> Db {
        let db = temp_db();
        for (acct, uuid) in [("a@x", "u1"), ("b@x", "u2")] {
            note(acct, uuid).title("findme").body("#t").insert(&db);
        }
        db
    }

    #[test]
    fn search_excludes_the_listed_accounts() {
        let db = seeded();
        let all = db.search_notes(None, None, "findme", &[]).unwrap();
        assert_eq!(all.len(), 2);

        let filtered = db
            .search_notes(None, None, "findme", &["b@x".to_string()])
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].account_id, "a@x");
    }

    /// The LIKE fallback branch (queries under 3 chars — trigram FTS can't
    /// index them) is a separate code path from the FTS branch above, with
    /// its own `exclude_clause` call and its own bind vector. "fi" is 2
    /// chars, so this exercises exactly that branch — a regression here
    /// would leave short searches leaking a dismissed account's notes while
    /// every 3+ char search behaved correctly.
    #[test]
    fn search_like_branch_excludes_the_listed_accounts() {
        let db = seeded();
        let all = db.search_notes(None, None, "fi", &[]).unwrap();
        assert_eq!(all.len(), 2);

        let filtered = db.search_notes(None, None, "fi", &["b@x".to_string()]).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].account_id, "a@x");
    }

    /// The zero-cost rule. An empty exclusion list must behave exactly as the
    /// pre-feature query did — search_notes runs on every navigation.
    #[test]
    fn an_empty_exclusion_list_changes_nothing() {
        let db = seeded();
        let a = db.search_notes(None, None, "findme", &[]).unwrap();
        let b = db.search_notes(Some("a@x"), None, "findme", &[]).unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn recent_and_scope_count_and_tags_all_honour_the_exclusion() {
        let db = seeded();
        assert_eq!(db.list_recent_notes(None, 10, &[]).unwrap().len(), 2);
        assert_eq!(
            db.list_recent_notes(None, 10, &["b@x".to_string()]).unwrap().len(),
            1
        );
        assert_eq!(db.count_notes_in_scope(None, None, &[]).unwrap(), 2);
        assert_eq!(
            db.count_notes_in_scope(None, None, &["b@x".to_string()]).unwrap(),
            1
        );
    }

    /// `list_notes_with_tags` is the one function of the four whose exclusion
    /// start index is COMPUTED (`binds.len() + 1`) rather than a literal,
    /// because the account clause is only present when `account_id.is_some()`
    /// — the account-scoped sub-case below.
    #[test]
    fn list_notes_with_tags_excludes_the_listed_accounts_when_account_scoped() {
        let db = seeded_with_tag();
        let all = db.list_notes_with_tags(Some("a@x"), &["t".to_string()], &[]).unwrap();
        assert_eq!(all.len(), 1);

        let filtered = db
            .list_notes_with_tags(Some("a@x"), &["t".to_string()], &["b@x".to_string()])
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].account_id, "a@x");
    }

    /// The unscoped (`account_id = None`) sub-case — precisely where the
    /// brief's original `2 + tags.len()` formula diverges from the correct
    /// `binds.len() + 1`: with no account clause, `binds` (tag names only)
    /// is one shorter than the formula assumes, so the exclude placeholder
    /// numbering is off by one. Verified empirically (see the task-7 fix
    /// report): reverting to the wrong formula makes rusqlite reject the
    /// query outright with `InvalidParameterCount(2, 3)` rather than
    /// silently misbehaving — still a real, user-visible bug (every
    /// unscoped tag search with an active exclusion would error), just not
    /// the silent-NULL failure mode one might first guess at.
    #[test]
    fn list_notes_with_tags_excludes_the_listed_accounts_when_unscoped() {
        let db = seeded_with_tag();
        let all = db.list_notes_with_tags(None, &["t".to_string()], &[]).unwrap();
        assert_eq!(all.len(), 2);

        let filtered = db
            .list_notes_with_tags(None, &["t".to_string()], &["b@x".to_string()])
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].account_id, "a@x");
    }
}
