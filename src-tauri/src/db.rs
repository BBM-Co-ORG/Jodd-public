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
             PRAGMA temp_store = MEMORY;",
        )?;
        let db = Db { conn: Mutex::new(conn) };
        db.migrate()?;
        Ok(db)
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
        tx.commit()?;
        Ok(())
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
    pub fn apply_local_edit(
        &self,
        uuid: &str,
        account_id: &str,
        title: &str,
        body_html: &str,
        label: &str,
    ) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
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
        )?;
        Ok(())
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
    pub fn mark_pushed(
        &self,
        uuid: &str,
        account_id: &str,
        new_remote_id: &str,
        pushed_date: &str,
        pushed_body_html: &str,
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
             WHERE uuid = ?3 AND account_id = ?4",
            params![new_remote_id, now, uuid, account_id, pushed_date, pushed_body_html],
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
    pub fn list_notes_with_tags(
        &self,
        account_id: &str,
        tags: &[String],
    ) -> SqlResult<Vec<CachedNote>> {
        if tags.is_empty() {
            return Ok(vec![]);
        }
        let conn = self.conn.lock().unwrap();
        let placeholders = std::iter::repeat("?")
            .take(tags.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT DISTINCT n.uuid, n.account_id, n.id, n.title, n.body_html, n.date,
                    n.x_mail_created_date, n.label, n.local_version, n.remote_version,
                    n.sync_state, n.last_synced_at, n.last_local_modified_at,
                    n.last_remote_modified_at, n.pinned, n.meta_msg_id, n.pin_dirty,
                    n.tags_meta_msg_id, n.tags_dirty
             FROM notes n
             JOIN note_tags t ON t.account_id = n.account_id AND t.uuid = n.uuid
             WHERE n.account_id = ? AND n.sync_state != 'deleted_pending'
               AND t.tag IN ({})",
            placeholders
        );
        // First bind param is account_id, then every tag. Uniform String type
        // so params_from_iter can take them all.
        let mut binds: Vec<String> = Vec::with_capacity(tags.len() + 1);
        binds.push(account_id.to_string());
        binds.extend(tags.iter().cloned());
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(binds), row_to_note)?;
        rows.collect()
    }

    /// Rename a tag across every note in an account. Merges into an existing
    /// tag if some notes already carry `new_tag`: UPDATE OR IGNORE renames the
    /// rows that can move, then the leftover `old_tag` rows (those that would
    /// have collided on the PK) are deleted — completing the merge with no
    /// duplicates.
    pub fn rename_tag(&self, account_id: &str, old_tag: &str, new_tag: &str) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE OR IGNORE note_tags SET tag = ?3 WHERE account_id = ?1 AND tag = ?2",
            params![account_id, old_tag, new_tag],
        )?;
        conn.execute(
            "DELETE FROM note_tags WHERE account_id = ?1 AND tag = ?2",
            params![account_id, old_tag],
        )?;
        Ok(())
    }

    /// Delete a tag from every note in an account.
    pub fn delete_tag(&self, account_id: &str, tag: &str) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM note_tags WHERE account_id = ?1 AND tag = ?2",
            params![account_id, tag],
        )?;
        Ok(())
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
    pub fn to_frontend_note(&self) -> crate::gmail::Note {
        crate::gmail::Note {
            id: self.id.clone(),
            uuid: self.uuid.clone(),
            title: self.title.clone(),
            body_html: self.body_html.clone(),
            date: self.date.clone(),
            x_mail_created_date: self.x_mail_created_date.clone(),
            label: self.label.clone(),
            account_id: Some(self.account_id.clone()),
            pinned: self.pinned,
        }
    }

    /// Build a CachedNote from a server fetch result. Stamps `clean` with
    /// remote_version = the message id, last_synced_at = now, last_remote
    /// _modified_at parsed from the Date header. local_version stays at 0
    /// (caller hasn't edited locally).
    pub fn from_remote(account_id: &str, n: &crate::gmail::Note) -> Self {
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
                    last_local_modified_at, last_synced_at
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
                last_local_modified_at, last_synced_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(account_id, path) DO NOTHING",
            params![
                f.account_id,
                f.path,
                f.label_id,
                f.sync_state.as_str(),
                f.last_local_modified_at,
                f.last_synced_at,
            ],
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
        tx.execute(
            "INSERT INTO folders (account_id, path, label_id, sync_state,
                last_local_modified_at, last_synced_at)
             VALUES (?1, ?2, ?3, 'clean', ?4, ?4)
             ON CONFLICT(account_id, path) DO UPDATE SET
                label_id = excluded.label_id,
                sync_state = 'clean',
                last_synced_at = excluded.last_synced_at
             WHERE folders.sync_state = 'clean'",
            params![account_id, path, label_id, now_ms()],
        )?;
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
                    last_local_modified_at, last_synced_at
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

    /// Look up one folder by (account_id, path).
    pub fn get_folder(&self, account_id: &str, path: &str) -> SqlResult<Option<CachedFolder>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT account_id, path, label_id, sync_state,
                    last_local_modified_at, last_synced_at
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
}

fn row_to_folder(r: &rusqlite::Row) -> SqlResult<CachedFolder> {
    Ok(CachedFolder {
        account_id: r.get(0)?,
        path: r.get(1)?,
        label_id: r.get(2)?,
        sync_state: FolderSyncState::from_str(&r.get::<_, String>(3)?),
        last_local_modified_at: r.get(4)?,
        last_synced_at: r.get(5)?,
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
        tx.execute(
            "INSERT INTO folders (account_id, path, label_id, sync_state,
                last_local_modified_at, last_synced_at)
             VALUES (?1, ?2, NULL, 'dirty_new', ?3, NULL)
             ON CONFLICT(account_id, path) DO NOTHING",
            params![account_id, ancestor, now],
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
