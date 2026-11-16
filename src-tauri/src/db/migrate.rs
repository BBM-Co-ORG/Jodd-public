//! Schema migrations (`Db::migrate`), the one-time backfills that run from
//! `open`, and the account-id migration (#19).
//! Split out of `db.rs` 2026-11-16.

use super::*;

/// Map a migration failure onto the error the caller can act on.
///
/// Only migration #19's fail-closed abort is singled out; everything else is
/// an ordinary SQLite failure and keeps its existing meaning.
pub(crate) fn classify_migrate_error(e: rusqlite::Error) -> crate::db_crypto::DbOpenError {
    if let rusqlite::Error::InvalidParameterName(msg) = &e {
        if msg.starts_with(ACCOUNT_ID_MIGRATION_ABORT) {
            return crate::db_crypto::DbOpenError::AccountIdMigration(msg.clone());
        }
    }
    crate::db_crypto::DbOpenError::Sqlite(e)
}

/// Whether this connection is the user's REAL cache rather than a test or
/// temp-dir database.
///
/// This gates one thing only: whether migration #19 is allowed to rewrite
/// `accounts.json`. Without it, every `cargo test` run would — `Db::open_*`
/// runs the migration, `accounts::save_accounts()` resolves through
/// `paths::config_base()`, and `paths.rs`'s `OnceLock` "cannot be reset
/// between tests", so the file it would rewrite is the developer's own
/// `~/Library/Application Support/jodd/accounts.json`. `#[cfg(test)]` is not
/// enough: `jodd-mcp`'s tests link this crate compiled WITHOUT `cfg(test)`
/// and open temp databases through the same path.
///
/// Both real consumers of the live cache pass this — the app
/// (`paths::data_base()/jodd`, `lib.rs`'s `.setup()`) and `jodd-mcp`, whose
/// default `resolve_db_path()` is the same directory. The desktop-only
/// last-resort temp-dir cache in `.setup()` does not, which is correct: that
/// cache is volatile, and account ids should not be rewritten on the strength
/// of one.
///
/// **`jodd-mcp` passing is intended, not an accident to be narrowed later.**
/// It opens the *same* real database through the same `Db::open`, so it runs
/// migration #19 for real and must migrate the account list along with it — a
/// jodd-mcp run that qualified the tables and left `accounts.json` bare would
/// reproduce exactly the split state this migration exists to close. Two
/// processes racing is safe: the migration is idempotent, resolves by `email`
/// (which neither process changes), and `save_accounts` renames atomically, so
/// whichever arrives first wins and the other finds the work already done and
/// no-ops.
///
/// Canonicalised on both sides so a symlinked home or `/var` -> `/private/var`
/// cannot make the live cache look like a stranger — a false negative here is
/// the expensive direction (the database would be rewritten and the account
/// list left behind).
///
/// **`live_base` is a parameter, not a call to `paths::data_base()`, purely so
/// the `true` branch is reachable from a test.** With the lookup inlined, the
/// only way to make this function say `true` was to make the connection the
/// developer's actual cache — so the branch that does the dangerous thing had
/// nothing watching it, and an earlier attempt to test it by forcing `true`
/// rewrote a real `accounts.json`. The single production call site passes
/// `crate::paths::data_base()`; tests pass a tempdir and exercise both
/// directions with zero exposure to the real data directory.
pub(crate) fn connection_is_the_live_user_cache(
    conn: &rusqlite::Connection,
    live_base: Option<PathBuf>,
) -> bool {
    let Some(path) = conn.path() else { return false };
    if path.is_empty() {
        return false; // in-memory or temporary database
    }
    let Some(live_dir) = live_base.map(|d| d.join("jodd")) else {
        return false;
    };
    let Some(parent) = std::path::Path::new(path).parent() else {
        return false;
    };
    match (parent.canonicalize(), live_dir.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => parent == live_dir,
    }
}

/// Migration #19 — rewrite every `account_id` to `{backend}:{email}`.
///
/// This is the registered `Step::Rust`. It is a thin shell around
/// [`migrate_account_ids_with`], which holds all of the database work and
/// touches no filesystem — the split is what makes the migration testable,
/// because `accounts::load_accounts()` / `save_accounts()` resolve through
/// `paths::config_base()`, a process-global `OnceLock` that cannot be pointed
/// at a tempdir from a unit test (`paths.rs`).
///
/// **`accounts.json` is written LAST, after `tx.commit()`.** Two properties
/// make that the safe order, and neither is a preference:
///
/// 1. **A failed `save_accounts` is retried in full, and retries cheaply.**
///    The error propagates out of this function, so `Db::migrate`'s loop never
///    reaches its `INSERT INTO migrations` and the whole migration re-runs on
///    the next launch. The already-qualified rows are then skipped by
///    `accounts::is_qualified`, so the retry is a no-op on the tables and only
///    the file write is actually re-attempted. (An earlier version of this
///    comment argued for save-first on the grounds that save-last would strand
///    a qualified database beside a bare account list "permanently, with
///    version 19 recorded". That was true only of a draft that swallowed the
///    error with `let _ =`. This code propagates it, so the premise is false.)
/// 2. **A fail-closed abort must not touch the file at all.**
///    `migrate_account_ids_with` refuses to guess a backend for an orphan id
///    and returns `Err` with the tables untouched (gotcha #2's orphan rows are
///    a real state). Under save-first, `accounts.json` had *already* been
///    rewritten by then — leaving a qualified account list beside a bare
///    database, and a migration that aborts identically on every subsequent
///    launch. Under save-last the abort happens before the file is opened.
///
/// The remaining crash window — process death between the commit and the
/// rename — is harmless in both directions, because resolution is by `email`,
/// which this migration never changes: the next run finds the tables already
/// qualified and rewrites only the list. `accounts::save_accounts` is itself
/// atomic (tmp file + `rename`), so the file is never left half-written.
///
/// **There is no rollback path.** A downgrade past this migration is
/// unsupported: nothing records what the previous ids were.
pub(crate) fn migrate_account_ids(conn: &rusqlite::Connection) -> SqlResult<()> {
    let accounts = crate::accounts::load_accounts();
    let updated: Vec<crate::accounts::Account> = accounts
        .iter()
        .cloned()
        .map(|mut a| {
            if !crate::accounts::is_qualified(&a.id) {
                a.id = crate::accounts::account_id_for(a.backend_kind, &a.email);
            }
            a
        })
        .collect();

    // Tables first. `updated` rather than `accounts` so the list this resolves
    // against matches what is about to be written to disk. Which one is passed
    // cannot change the outcome: resolution is by email, and email is the
    // field this migration never touches.
    migrate_account_ids_with(conn, &updated)?;

    // The COUNT THAT CHANGED, not the length of the list. This is the line a
    // maintainer reads while watching a live upgrade, so "qualifying 3 account
    // id(s)" on a profile where two were already qualified is not log noise —
    // it is a wrong answer to the question the log line exists to answer.
    let changed_count = updated
        .iter()
        .zip(accounts.iter())
        .filter(|(u, a)| u.id != a.id)
        .count();
    if changed_count > 0 {
        if connection_is_the_live_user_cache(conn, crate::paths::data_base()) {
            crate::log!("migration 19: qualifying {} account id(s)", changed_count);
            crate::accounts::save_accounts(&updated).map_err(|e| {
                rusqlite::Error::InvalidParameterName(format!(
                    "migration 19: tables are qualified but accounts.json could not be \
                     rewritten ({e}) — version 19 stays unrecorded, so the next launch \
                     retries (a no-op on the tables, which is_qualified now skips)"
                ))
            })?;
        } else {
            crate::log!(
                "migration 19: not the live user cache ({:?}) — rewriting the tables only",
                conn.path().unwrap_or("<none>")
            );
        }
    }
    Ok(())
}

/// The database half of migration #19. No filesystem access, by design.
///
/// Opens its own transaction. `Db::migrate` applies each migration in
/// autocommit and records the version in a *separate* `INSERT` afterwards, so
/// a crash between the two hands this function a database it has already
/// converted. Idempotency is therefore load-bearing rather than defensive, and
/// it comes from `accounts::is_qualified`: an id that already carries a backend
/// prefix is skipped, which is also what makes every `localfs:{uuid}` row a
/// no-op on the first run.
///
/// Resolution is **by email, not only by the old id**, because `accounts.json`
/// cannot join this transaction. A bare `account_id` equals the `email` of
/// exactly one account, and email never changes, so either half-finished state
/// (list rewritten, tables not — or the reverse) completes here.
///
/// **Fails closed — but only where failing closed protects something.** An
/// unresolvable `account_id` is not one situation, it is three, and the first
/// version of this function treated them alike, which turned an ordinary
/// orphan row into an app that opens an empty temp-dir cache on every launch:
///
/// 1. **The id carries user data** (`notes` or `folders`). Guessing a backend
///    would silently misfile someone's notes under the wrong account, so this
///    aborts the transaction — and, via [`classify_migrate_error`], surfaces
///    as `DbOpenError::AccountIdMigration` so the caller can say so instead of
///    substituting a fresh database.
/// 2. **The id appears only in satellite tables** (`note_tags`,
///    `tag_tombstones`, `attachments`, `edges`, `note_uuid_aliases`). That is
///    a provably-gone account: `remove_account` saves the shortened
///    `accounts.json` *before* wiping the database, and `delete_account`'s
///    satellite deletes are best-effort `let _ =` — and only grew to cover
///    `attachments`, `edges` and `note_uuid_aliases` in 2026-08, so existing
///    installs carry these today. There is no note to misfile, so the rows are
///    deleted rather than allowed to wedge the open.
/// 3. **The account list is EMPTY beside a non-empty database.** That
///    combination means *"the list could not be read"*, not *"every account
///    was removed"* — `accounts::load_accounts` swallows every error and
///    returns an empty `Vec`, so an unreadable or truncated `accounts.json`
///    looks exactly like a user with no accounts. Deleting or aborting on the
///    strength of it would destroy, or lock out, an intact cache. The
///    migration does nothing at all and returns `Ok`.
///
/// Case 3 returns `Ok`, so `Db::migrate` records version 19 and the tables
/// stay bare for good. That is deliberate: acting on an empty list is the only
/// option that can destroy an intact cache, so doing nothing is the safe
/// direction even though it means this profile never migrates.
///
/// Bare tables beside a bare `accounts.json` are **almost** self-consistent —
/// resolution is by id, the credential fallback already handles a bare id, and
/// every id minted afterwards is qualified without colliding with the bare
/// ones. **The exception is re-authentication, and it is the one thing this
/// state cannot do cleanly.** `complete_oauth` dedupes on the *qualified* id
/// (lib.rs), so a surviving bare account that later signs in again is not
/// recognised: the user gets a SECOND account row for the same mailbox, and
/// its `index_account` re-imports the whole mailbox beside the bare copy.
/// Nothing on this branch can reach that except case 3.
///
/// Converging them later would mean re-running this on every open, and the
/// cost is a `DISTINCT` over seven tables per launch on affected profiles.
/// That is the trade, stated plainly — an earlier version of this comment
/// argued the alternative would "make case 1's abort fire forever instead of
/// once", which was never true once a case-1 abort began refusing the launch
/// outright: it already fires every launch until the user repairs
/// `accounts.json`.
pub(crate) fn migrate_account_ids_with(
    conn: &rusqlite::Connection,
    accounts: &[crate::accounts::Account],
) -> SqlResult<()> {
    let tx = conn.unchecked_transaction()?;

    // Every table, not just notes+folders: an id surviving in, say, note_tags
    // alone would be orphaned by a rewrite that never looked there.
    let union = ACCOUNT_ID_TABLES
        .iter()
        .map(|t| format!("SELECT account_id FROM {t}"))
        .collect::<Vec<_>>()
        .join(" UNION ");
    // ORDER BY is not cosmetic: it makes the order ids are rewritten in — and
    // therefore how much work a fail-closed abort has to roll back — the same
    // on every run and in every test, instead of whatever the DISTINCT
    // implementation happens to produce.
    let existing: Vec<String> = {
        let mut stmt = tx.prepare(&format!(
            "SELECT DISTINCT account_id FROM ({union}) ORDER BY account_id"
        ))?;
        let ids = stmt.query_map([], |r| r.get(0))?.collect::<SqlResult<Vec<_>>>()?;
        ids
    };

    // Case 3 above: an empty list beside rows is an unreadable `accounts.json`,
    // not an install with no accounts. Nothing has been written yet, so
    // dropping the transaction unwritten is the whole of "do nothing".
    if accounts.is_empty() && !existing.is_empty() {
        crate::log!(
            "migration 19: {} account id(s) in the cache but the account list is empty \
             — treating that as an unreadable accounts.json and leaving the tables alone",
            existing.len()
        );
        return Ok(());
    }

    // Which ids actually carry user data. An id here that resolves to no
    // account aborts (case 1); an id absent from this set is satellite-only
    // and its rows are deleted (case 2).
    let ids_with_user_data: std::collections::HashSet<String> = {
        let mut stmt = tx.prepare(
            "SELECT account_id FROM notes UNION SELECT account_id FROM folders",
        )?;
        let ids = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<SqlResult<std::collections::HashSet<_>>>()?;
        ids
    };

    let mut rewrote_any = false;
    let mut purged_any = false;
    for old in existing {
        if crate::accounts::is_qualified(&old) {
            continue;
        }
        let acct = accounts
            .iter()
            .find(|a| a.id == old)
            .or_else(|| accounts.iter().find(|a| a.email == old));
        let Some(acct) = acct else {
            if ids_with_user_data.contains(&old) {
                return Err(rusqlite::Error::InvalidParameterName(format!(
                    "{ACCOUNT_ID_MIGRATION_ABORT}: account_id {old:?} matches no account \
                     by id or email but still holds notes or folders — refusing to guess \
                     a backend"
                )));
            }
            // Satellite-only leftovers of an account that is provably gone.
            // Deleting them is what keeps a stale `note_tags` row from wedging
            // the whole database open (case 2 above).
            crate::log!(
                "migration 19: dropping satellite-only rows for departed account {old:?}"
            );
            for table in ACCOUNT_ID_TABLES {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE account_id = ?1"),
                    params![old],
                )?;
            }
            purged_any = true;
            continue;
        };
        let new = crate::accounts::account_id_for(acct.backend_kind, &acct.email);
        for table in ACCOUNT_ID_TABLES {
            tx.execute(
                &format!("UPDATE {table} SET account_id = ?1 WHERE account_id = ?2"),
                params![new, old],
            )?;
        }
        rewrote_any = true;
    }

    // Re-derive FTS from `notes` rather than UPDATEing it: its content is
    // derived state and `notes` is now authoritative for account_id
    // (gotcha #4). Inside the same transaction, so the index can never be
    // observed disagreeing with the rows it indexes.
    // `purged_any` belongs here as much as `rewrote_any`: a purge leaves the
    // departed account's `notes_fts` rows behind (that table is not in
    // ACCOUNT_ID_TABLES, being derived), and the re-derive below is what
    // removes them — it rebuilds the index from `notes`, which no longer has
    // them.
    if rewrote_any || purged_any {
        tx.execute("DELETE FROM notes_fts", [])?;
        let rows: Vec<(String, String, String, String)> = {
            let mut stmt = tx.prepare("SELECT uuid, account_id, title, body_html FROM notes")?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<SqlResult<Vec<_>>>()?;
            rows
        };
        // Not `fts_index_conn`: this runs at migration 19, while `notes_fts`
        // still has its migration-10 shape, and that function names the
        // `folder` column migration 24 adds. 24 rebuilds the index anyway.
        for (uuid, account_id, title, body_html) in rows {
            fts_index_title_body_conn(&tx, &uuid, &account_id, &title, &body_html)?;
        }
    }

    tx.commit()
}

impl Db {
    /// Populate the `edges` table from existing note bodies when empty — runs
    /// once after the migration that creates it. Idempotent (no-op once
    /// populated). Same shape as fts_backfill_if_empty.
    pub(crate) fn edges_backfill_if_empty(&self) -> SqlResult<()> {
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
            reconcile_edges_from_body_conn(&tx, account_id, uuid, label, body, &tags_for_account(account_id, body))?;
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
    pub(crate) fn migrate_tags_to_body(&self) -> SqlResult<()> {
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
            // Never on iCloud: legacy sidecar tags never existed there, and
            // the appended `#tag` TEXT is not a tag under `InlineObjects`, so
            // this "repair" would write into Apple's note on every open
            // without ever converging (gotcha #38).
            if tag_model_for_account(account_id) == crate::backend::TagModel::InlineObjects {
                continue;
            }
            let current: Vec<String> = {
                let mut s =
                    tx.prepare("SELECT tag FROM note_tags WHERE account_id = ?1 AND uuid = ?2")?;
                let it = s.query_map(params![account_id, uuid], |r| r.get::<_, String>(0))?;
                it.filter_map(|x| x.ok()).collect()
            };
            let in_body: std::collections::HashSet<String> =
                tags_for_account(account_id, body_html).into_iter().collect();
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
    pub(crate) fn fts_backfill_if_empty(&self) -> SqlResult<()> {
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
    pub(crate) fn migrate(&self) -> SqlResult<()> {
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
        let migrations: &[(i64, Step)] = &[
            (
                1,
                // PRIMARY KEY on (uuid, account_id) — Apple Notes' X-UUID is
                // globally unique BUT the same uuid could in principle exist
                // across separate Gmail accounts (e.g., user copied a note
                // between accounts). Keying by both keeps them distinct.
                Step::Sql("CREATE TABLE notes (
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
                CREATE INDEX idx_notes_sync_state ON notes (sync_state) WHERE sync_state != 'clean';"),
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
                Step::Sql("CREATE TABLE folders (
                    account_id TEXT NOT NULL,
                    path TEXT NOT NULL,
                    label_id TEXT,
                    sync_state TEXT NOT NULL DEFAULT 'clean',
                    last_local_modified_at INTEGER NOT NULL,
                    last_synced_at INTEGER,
                    PRIMARY KEY (account_id, path)
                );
                CREATE INDEX idx_folders_sync_state ON folders (sync_state) WHERE sync_state != 'clean';"),
            ),
            (
                3,
                // Pin support. INTEGER 0/1 — sqlite's BOOLEAN is just an
                // INTEGER affinity, and serde will map to/from Rust's bool
                // via the row_to_note read. The partial index covers the
                // "pinned DESC" sort: only rows with pinned=1 matter, so
                // we don't pay for an index over the typically-zero
                // majority. Default 0 keeps every existing row unpinned.
                Step::Sql("ALTER TABLE notes ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;
                 CREATE INDEX idx_notes_pinned ON notes (account_id, pinned) WHERE pinned = 1;"),
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
                Step::Sql("ALTER TABLE notes ADD COLUMN meta_msg_id TEXT;
                 ALTER TABLE notes ADD COLUMN pin_dirty INTEGER NOT NULL DEFAULT 0;
                 CREATE INDEX idx_notes_pin_dirty ON notes (pin_dirty) WHERE pin_dirty = 1;"),
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
                Step::Sql("CREATE TABLE note_tags (
                    account_id TEXT NOT NULL,
                    uuid TEXT NOT NULL,
                    tag TEXT NOT NULL,
                    PRIMARY KEY (account_id, uuid, tag)
                );
                CREATE INDEX idx_note_tags_tag ON note_tags (account_id, tag);"),
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
                Step::Sql("CREATE TABLE tag_tombstones (
                    account_id TEXT NOT NULL,
                    uuid TEXT NOT NULL,
                    tag TEXT NOT NULL,
                    deleted_at INTEGER NOT NULL,
                    PRIMARY KEY (account_id, uuid, tag)
                );
                CREATE INDEX idx_tag_tombstones_uuid ON tag_tombstones (account_id, uuid);
                CREATE INDEX idx_tag_tombstones_age ON tag_tombstones (deleted_at);"),
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
                Step::Sql("ALTER TABLE notes ADD COLUMN tags_meta_msg_id TEXT;
                 ALTER TABLE notes ADD COLUMN tags_dirty INTEGER NOT NULL DEFAULT 0;
                 CREATE INDEX idx_notes_tags_dirty ON notes (tags_dirty) WHERE tags_dirty = 1;"),
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
                Step::Sql("UPDATE notes SET tags_dirty = 1
                 WHERE (account_id, uuid) IN
                     (SELECT DISTINCT account_id, uuid FROM note_tags);"),
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
                Step::Sql("CREATE TABLE attachments (
                    account_id TEXT NOT NULL,
                    note_uuid TEXT NOT NULL,
                    content_id TEXT NOT NULL,
                    mime_type TEXT NOT NULL,
                    filename TEXT,
                    x_apple_part_url TEXT,
                    data BLOB NOT NULL,
                    PRIMARY KEY (account_id, note_uuid, content_id)
                );
                CREATE INDEX idx_attachments_note ON attachments (account_id, note_uuid);"),
            ),
            (
                10,
                // Full-text search index over title + plain-text body. The
                // `trigram` tokenizer makes Thai/CJK (which have no word spaces)
                // substring-searchable — verified `ใบเสร็จ` matches a Thai body.
                // uuid/account_id are UNINDEXED, just for joining back to notes.
                // Maintained from Rust (fts_upsert/fts_delete) because the body
                // must be HTML-stripped first, which SQL triggers can't do.
                // Migration 24 recreates it with a `folder` column, which IS
                // trigger-maintained — see `migrate_fts_folder_column`.
                // Backfilled in Db::open when empty.
                Step::Sql("CREATE VIRTUAL TABLE notes_fts USING fts5(
                    uuid UNINDEXED,
                    account_id UNINDEXED,
                    title,
                    body,
                    tokenize = 'trigram'
                );"),
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
                Step::Sql("CREATE TABLE edges (
                    account_id TEXT NOT NULL,
                    src_uuid   TEXT NOT NULL,
                    dst_title  TEXT NOT NULL,
                    rel        TEXT NOT NULL,
                    PRIMARY KEY (account_id, src_uuid, dst_title, rel)
                );
                CREATE INDEX idx_edges_dst ON edges (account_id, rel, dst_title);
                CREATE INDEX idx_edges_src ON edges (account_id, rel, src_uuid);"),
            ),
            (
                12,
                // Slug-based links: add dst_id alongside dst_title so a
                // [[title-slug-<uuid8>]] resolves by the stable id (unambiguous
                // across duplicate titles) while plain [[Title]] still resolves
                // by title. edges is fully derived, so drop + recreate is safe —
                // edges_backfill_if_empty repopulates it on this same open.
                Step::Sql("DROP TABLE IF EXISTS edges;
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
                CREATE INDEX idx_edges_src ON edges (account_id, rel, src_uuid);"),
            ),
            (
                13,
                // Clear edges so edges_backfill_if_empty repopulates it with the
                // newly-derived child_of (note→folder) and tagged (note→#tag)
                // rels, not just 'mentions'. edges is fully derived → safe.
                Step::Sql("DELETE FROM edges;"),
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
                Step::Sql("ALTER TABLE folders ADD COLUMN kind TEXT NOT NULL DEFAULT 'user';"),
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
                Step::Sql("DELETE FROM edges;"),
            ),
            (
                16,
                // Microsoft M2 fix (whole-branch review, 2026-08-15, Critical
                // fix 2): the label Jodd last CONFIRMED the remote holds for
                // this note, distinct from `label` (the locally-desired
                // value, mutated immediately by `apply_local_edit`/local
                // moves). On Gmail and LocalFs a label change is an intrinsic
                // part of `save_note_full` itself (Gmail inserts under the
                // new label and trashes the old message; LocalFs writes into
                // the new folder's directory and removes the old file), but
                // Exchange's `PATCH` is a true in-place update that never
                // touches `parentFolderId` — so a label change on that
                // backend needs an EXPLICIT `POST /me/messages/{id}/move`
                // after the PATCH, or the remote note stays in its old
                // folder forever while the local cache believes it moved.
                // `push_one_dirty` (lib.rs) reads this column to decide
                // whether that move is necessary, comparing it against the
                // row's current `label` — a no-op when they already agree,
                // so a move is never issued on an ordinary content-only save.
                //
                // NULL for every existing row and for a freshly-created one
                // (nothing confirmed on the remote yet) — callers must treat
                // NULL as "unknown, do not assume a move is needed", which is
                // the safe default: a stale/missing value only means one
                // rare Apple-side-move-plus-Jodd-edit racing past unnoticed
                // once, and the very next successful pull
                // (`upsert_from_remote`) or push (`mark_pushed`) stamps a
                // real value from ground truth — bounded and self-healing
                // (gotcha #4), not a data-loss risk.
                Step::Sql("ALTER TABLE notes ADD COLUMN remote_label TEXT;"),
            ),
            (
                17,
                // Why a push the worker will never retry needs a column of its
                // own (2026-08-17).
                //
                // `TransportError::Permanent` means retrying THIS operation
                // can never succeed. The worker had no way to act on that: it
                // logged the error and left the row `dirty`, so the same
                // doomed push fired every 5 seconds forever (measured:
                // 5,816 attempts, zero successes, ~80 minutes) while the UI
                // showed "Saved" and the note itself dropped out of the list
                // 30 s later, because no list path returns a note that does
                // not exist remotely.
                //
                // The two obvious cheaper answers are both wrong:
                //
                //   - Clear to `clean`. That is the lie the fix exists to
                //     remove — a note marked synced that was never written
                //     anywhere.
                //   - Drop the row, the way `push_folder` handles its own
                //     Permanent (lib.rs). Acceptable for a folder; for a note
                //     it is silent data loss.
                //
                // So: keep `sync_state = 'dirty'` (the row genuinely does hold
                // unsent local content — that stays true) and hang the reason
                // off it. NULL means "not blocked", which is every existing
                // row and every new one. `list_dirty` skips non-NULL, which
                // stops the retry; `has_pending_pushes` skips it too, so a
                // blocked row cannot wedge a Draining account into never
                // reaching Inactive and never being removable (gotcha #2's
                // wedge, the same one the folder path had to close).
                //
                // Deliberately NOT a sixth `sync_state`: blockage is a
                // qualifier ON dirty, not a peer of it, and the state machine
                // in `apply_local_edit` has five values' worth of transitions
                // that would all have needed a new arm. Same reasoning that
                // put `pin_dirty` in its own column.
                Step::Sql("ALTER TABLE notes ADD COLUMN push_blocked_reason TEXT;"),
            ),
            (
                18,
                // A forwarding address for `rekey_note_uuid`.
                //
                // When a backend assigns the note's identity at CREATE time
                // (Exchange's `internetMessageId`; also Gmail whenever
                // `resolve_uuid_for_save` mints a fresh uuid), the row's
                // PRIMARY KEY changes underneath every holder of the old
                // one. The DB half of that has always been correct and is
                // asserted by `an_uncontested_create_ends_up_clean_with_the_
                // real_identity` ("the placeholder uuid must be gone after
                // rekey") — but nothing recorded where it went, so a caller
                // still holding the old uuid had no way to find the row
                // again. `save_note` read that miss as "brand-new note" and
                // inserted a SECOND row, which the worker then CREATEd
                // remotely: measured 2026-08-18 on alex.h@outlook.example, one
                // note the user edited three times became three notes in
                // Outlook and in Apple Notes (cache rows carried
                // local_version 3/2/1 — one per save that landed between
                // pushes). See gotcha #16.
                //
                // Kept as its own table rather than a `prev_uuid` column on
                // `notes`: a row can be rekeyed more than once, and the
                // lookup needs to work from a uuid whose row no longer
                // exists — which a column on that very row cannot answer.
                //
                // Chains are collapsed on write (see `rekey_note_uuid`), so
                // resolution is a single indexed lookup and never recursive.
                Step::Sql("CREATE TABLE IF NOT EXISTS note_uuid_aliases (
                    account_id TEXT NOT NULL,
                    old_uuid   TEXT NOT NULL,
                    new_uuid   TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    PRIMARY KEY (account_id, old_uuid)
                 );
                 CREATE INDEX IF NOT EXISTS idx_note_uuid_aliases_new
                    ON note_uuid_aliases (account_id, new_uuid);"),
            ),
            (
                19,
                // account_id becomes {backend}:{email}, so two accounts can
                // share an email address on different backends. The first
                // migration that cannot be a SQL string: the backend kind of
                // each account lives in accounts.json, outside the database
                // entirely. See `migrate_account_ids` for the write order and
                // `migrate_account_ids_with` for the fail-closed rule.
                //
                // NO ROLLBACK PATH. A downgrade past this point is unsupported.
                Step::Rust(migrate_account_ids),
            ),
            (
                20,
                // Which kind of permanent block `push_blocked_reason` is.
                // 1 = the remote's own document refuses the write (iCloud's
                // per-note writability, gotcha #24): no edit made here can get
                // past it, so the editor locks the note. 0 = an ordinary
                // refusal, where an edit or a move may be the fix and gotcha
                // #14's re-arm-on-edit path must stay open. Meaningless while
                // `push_blocked_reason` is NULL; every path that clears the
                // reason clears this too.
                Step::Sql("ALTER TABLE notes ADD COLUMN push_blocked_by_remote INTEGER NOT NULL DEFAULT 0;"),
            ),
            (
                21,
                // Curate (M2, docs/superpowers/specs/2026-10-06-curate-m2-design.md):
                // proposals to fix duplicates, misfiled notes and secrets, kept on
                // the device that scanned. Keyed by its own id, so it is not one
                // of ACCOUNT_ID_TABLES; a proposal for an account that no longer
                // exists is never listed and refuses to apply.
                Step::Sql("CREATE TABLE curate_proposals (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    account_id TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    note_key TEXT NOT NULL,
                    payload TEXT NOT NULL,
                    source TEXT NOT NULL,
                    status TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    decided_at INTEGER,
                    error TEXT
                );
                CREATE INDEX idx_curate_account_status ON curate_proposals (account_id, status);"),
            ),
            (
                22,
                // Note provenance (docs/superpowers/specs/2026-10-07-note-provenance-design.md).
                // Keyed by its own id, so — like curate_proposals — NOT in
                // ACCOUNT_ID_TABLES: migration 19 iterates that list and runs
                // before this table exists on a fresh install.
                Step::Sql(crate::provenance::NOTE_EVENTS_DDL),
            ),
            (
                23,
                // Gotcha #14 for the two queues that had no block: a pin
                // sidecar and a folder row the backend refused permanently
                // (a meta label or folder name Gmail answers with a 409 its
                // `labels.list` cannot explain). Same shape as #17: NULL means
                // "not blocked"; non-NULL leaves the row what it is
                // (`pin_dirty = 1`, `dirty_new`) but takes it out of the
                // worker's queue and out of `has_pending_pushes`, so it
                // neither retries every tick nor holds a Draining account.
                // A column on `notes` rather than reusing
                // `push_blocked_reason`: that one stops the CONTENT push and
                // puts a banner on the editor, and a refused pin is neither.
                Step::Sql("ALTER TABLE notes ADD COLUMN pin_push_blocked_reason TEXT;
                           ALTER TABLE folders ADD COLUMN push_blocked_reason TEXT;"),
            ),
            (
                24,
                // Search matches folder names
                // (docs/superpowers/specs/2026-10-09-search-folder-names-design.md):
                // `notes_fts` gains a `folder` column, kept fresh by triggers.
                // `fts_backfill_if_empty` (right after `migrate()` in open)
                // repopulates the recreated table.
                Step::Rust(migrate_fts_folder_column),
            ),
            (
                25,
                // Gotcha #38: on iCloud only hashtag spans are tags. Rows the
                // old `#word` rule derived there (a tag Apple deleted but
                // whose text it kept) are re-derived once — and BEFORE
                // `migrate_tags_to_body` runs in `open`, which would
                // otherwise read them as tags missing from the body.
                Step::Rust(migrate_rederive_inline_object_tags),
            ),
            (
                26,
                // What the remote's CONTENT was at `remote_version` — a
                // `provenance::content_fingerprint`, set by every write of
                // `remote_version` (`upsert_from_remote`, `mark_pushed`,
                // cleared by `mark_pin_pushed`). On a backend whose version
                // moves without its content (`backend::
                // version_moves_without_content`) it lets `reconcile_one`
                // tell Apple's tag bump from another device's edit after a
                // local edit has replaced the row's own copy. NULL — every
                // row until its next pull or push, and every row on the
                // other backends — means "unknown", which keeps the old
                // keep-both answer.
                Step::Sql("ALTER TABLE notes ADD COLUMN remote_base_fp TEXT;"),
            ),
            (
                27,
                // When a CREATE push for this row was last sent — written
                // BEFORE the request, so it survives the process dying
                // mid-flight. A Gmail insert has no idempotency key: if Gmail
                // committed it but the answer was lost, `id` is still empty
                // and the next push would insert a second message. A row
                // with this set looks in its label for that message first
                // (`sync_worker::prepare_gmail_create`). Cleared by
                // `set_remote_id`. NULL — every row that has an id, or whose
                // create was never sent — means "nothing can have landed".
                Step::Sql("ALTER TABLE notes ADD COLUMN create_attempted_at INTEGER;"),
            ),
            (
                28,
                // Ask Jodd conversation history (spec 2026-10-14). Keyed by
                // its own id — like curate_proposals — NOT in
                // ACCOUNT_ID_TABLES; account removal deletes the conversations
                // scoped to that account in `delete_account`.
                Step::Sql(crate::ask::history::ASK_HISTORY_DDL),
            ),
            (
                29,
                // Gmail duplicates already preserved as their own note
                // (`reconcile::preserve_divergent_db`). A listing reports such
                // a duplicate on every pass until somebody trashes it, so the
                // copy is made once per message — and stays made after the user
                // deletes it. Not in ACCOUNT_ID_TABLES: it is created after
                // migration 19 and holds no note rows; `delete_account` clears
                // it.
                Step::Sql("CREATE TABLE IF NOT EXISTS divergent_copies (
                    account_id TEXT NOT NULL,
                    remote_id TEXT NOT NULL,
                    copy_uuid TEXT NOT NULL,
                    preserved_at INTEGER NOT NULL,
                    PRIMARY KEY (account_id, remote_id)
                );"),
            ),
            (
                30,
                // The body the remote held at `remote_version`, as stored here
                // — written beside `remote_base_fp` by every write of
                // `remote_version` (`upsert_from_remote`, `mark_pushed`,
                // `adopt_pushed_base`, `apply_remote_append_merge`), on every
                // backend. The base a three-way merge needs
                // (`merge::merge_remote_appends`): without it a pending edit
                // and another device's append can only be kept both. NULL —
                // every row until its next pull or push — means "unknown",
                // which keeps the keep-both answer.
                Step::Sql("ALTER TABLE notes ADD COLUMN remote_base_body TEXT;"),
            ),
            (
                31,
                // Gmail messages a listing's dedupe dropped as duplicates of a
                // kept copy (`Db::record_duplicate_ids`). The sign-in index
                // lists message ids only, so without this it counted every
                // duplicate as a note: the Mac showed 422 then 421 at startup
                // against a true 415 (2026-10-22). `index_account` leaves these
                // out (`reconcile::drop_known_duplicates_db`). Not in
                // ACCOUNT_ID_TABLES (created after migration 19, no note rows);
                // `delete_account` clears it.
                Step::Sql("CREATE TABLE IF NOT EXISTS gmail_duplicate_ids (
                    account_id TEXT NOT NULL,
                    remote_id TEXT NOT NULL,
                    PRIMARY KEY (account_id, remote_id)
                );"),
            ),
            (
                32,
                // Link rules changed (docs/superpowers/specs, hypermedia
                // links): `link_text` / `scan_links` now scope a `[[wikilink]]`
                // to one line, see through inline tags, and keep a balanced
                // `)` inside a URL. Edges derived under the old rules hold
                // junk (`mentions` for `draft ... [[real`, `cites` cut at
                // `Foo_(bar`). Same precedent as migration 15: `edges` is
                // fully derived, so clearing it is always safe, and `Db::open`
                // runs `edges_backfill_if_empty` right after `migrate()`,
                // which repopulates every rel from `body_html`.
                Step::Sql("DELETE FROM edges;"),
            ),
            (
                33,
                // Backup & restore bookkeeping. `restore_map` remembers which
                // note a restore created from which backed-up note, so
                // restoring the same backup twice is idempotent.
                // `account_listing_marks` records when an account first
                // completed a full listing (backup and restore both refuse an
                // account that never has). `note_render_loss` flags notes whose
                // body cannot round-trip through a backup. Not in
                // ACCOUNT_ID_TABLES (created after migration 19, no note
                // rows); `delete_account` clears them.
                Step::Sql("CREATE TABLE IF NOT EXISTS restore_map (
                    source_account TEXT NOT NULL, source_uuid TEXT NOT NULL,
                    target_account TEXT NOT NULL, target_uuid TEXT NOT NULL,
                    content_sha256 TEXT NOT NULL, backup_id TEXT NOT NULL,
                    restored_at INTEGER NOT NULL,
                    PRIMARY KEY (source_account, source_uuid, target_account));
                CREATE TABLE IF NOT EXISTS account_listing_marks (
                    account_id TEXT PRIMARY KEY, first_full_listing_at INTEGER NOT NULL);
                CREATE TABLE IF NOT EXISTS note_render_loss (
                    account_id TEXT NOT NULL, uuid TEXT NOT NULL, reason TEXT NOT NULL,
                    PRIMARY KEY (account_id, uuid));"),
            ),
            (
                34,
                // When this device last changed a note's pin: the user's
                // toggle (`set_pin*`) or the worker's push landing
                // (`mark_pin_pushed`). A pull's sidecar listing older than
                // that must not overwrite it — `apply_remote_pin` and
                // `clear_pins_not_in` spare rows inside `PRUNE_GRACE_MS` of
                // it (gotcha #43's pin entry; the same window as #35). NULL
                // on every existing row: nothing to spare.
                Step::Sql("ALTER TABLE notes ADD COLUMN pin_changed_at INTEGER;"),
            ),
            (
                35,
                // The labels a note was moved BETWEEN (both ends). A Gmail move
                // re-inserts the note under the new label, and a copy of it can
                // still turn up in the old one afterwards — Apple appending an
                // edit from before it synced the move (T4, 2026-11-13) — while
                // Jodd's own moved copy sits in the new one. Deleting the note
                // must trash those copies too, and the delete sweep reads one
                // label at a time (a whole-mailbox scan measured
                // 34–37 s), so it needs to know WHICH labels. Written by
                // `move_notes_batch`, read by `label_trail`, dropped with the
                // note (`delete`), followed by `rekey_note_uuid`, cleared by
                // `delete_account`. Not in ACCOUNT_ID_TABLES (it has no note
                // rows of its own), like `gmail_duplicate_ids`.
                Step::Sql("CREATE TABLE IF NOT EXISTS note_label_trail (
                    account_id TEXT NOT NULL,
                    uuid TEXT NOT NULL,
                    label TEXT NOT NULL,
                    PRIMARY KEY (account_id, uuid, label)
                );"),
            ),
            (
                36,
                // A Move to another account whose original is waiting for its
                // copy to be safe on the target's server (spec
                // 2026-11-13-cross-account-move-copy §4). One row per source
                // note; the source is `deleted_pending` while it exists, and
                // `list_deleted_pending` skips it until `state` is
                // `target_safe`. `source_prev_state` is what a call-off puts
                // back. Dropped with the source note (`delete`) and by
                // `delete_account` on either side. Not in ACCOUNT_ID_TABLES:
                // it names two accounts and holds no note rows of its own.
                Step::Sql("CREATE TABLE IF NOT EXISTS note_transfers (
                    source_account TEXT NOT NULL,
                    source_uuid TEXT NOT NULL,
                    target_account TEXT NOT NULL,
                    target_uuid TEXT NOT NULL,
                    source_sha TEXT NOT NULL,
                    source_prev_state TEXT NOT NULL,
                    state TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    PRIMARY KEY (source_account, source_uuid)
                );"),
            ),
        ];
        for (v, step) in migrations {
            if !applied.contains(v) {
                match step {
                    Step::Sql(sql) => conn.execute_batch(sql)?,
                    Step::Rust(f) => f(&conn)?,
                }
                conn.execute(
                    "INSERT INTO migrations (version, applied_at) VALUES (?1, ?2)",
                    params![v, now_ms()],
                )?;
            }
        }
        Ok(())
    }
}

/// Migration #19 — `account_id` becomes `{backend}:{email}`.
///
/// These exercise `migrate_account_ids_with`, NEVER the `migrate_account_ids`
/// wrapper. The wrapper reaches `accounts::load_accounts()` /
/// `save_accounts()`, which resolve through `paths::config_base()` — a
/// process-global `OnceLock` that "cannot be reset between tests"
/// (`paths.rs`). A test calling the wrapper would therefore read, and
/// REWRITE, the developer's real `~/Library/Application Support/jodd/
/// accounts.json`. The split exists so the migration's whole database
/// behaviour is testable without that.
#[cfg(test)]
mod migration_19_tests {
    use super::*;
    use crate::accounts::{Account, AccountStatus, BackendKind, LlmConfig};
    use crate::test_support::note;

    fn acct(id: &str, email: &str, backend_kind: BackendKind) -> Account {
        Account {
            id: id.to_string(),
            email: email.to_string(),
            added_at: "2026-01-01T00:00:00Z".to_string(),
            notes_label: None,
            meta_label: None,
            llm: LlmConfig::default(),
            backend_kind,
            root_dir: None,
            ssh_target: None,
            ssh_port: None,
            ssh_key: None,
            icloud_session_established: false,
            blocked_reason: None,
            sync_cursor: None,
            icloud_replica_id: None,
            status: AccountStatus::Active,
            pending_removal: false,
        }
    }

    /// A database carrying one row in EVERY `account_id` table for each id.
    ///
    /// Seeded AFTER `open_unencrypted` has already run migration #19 against
    /// an empty database, which is exactly how a real upgrade differs: there
    /// the rows predate the migration. The tests then call the migration
    /// explicitly, so the ordering costs nothing.
    fn seeded(ids: &[&str]) -> (Db, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        for (i, id) in ids.iter().enumerate() {
            let uuid = format!("UUID-{i}");
            // notes + notes_fts + note_tags + edges, all through the real path.
            note(id, &uuid)
                .title("t")
                .body("body #alpha [[Other]]")
                .label("Notes/A")
                .insert(&db);
            let conn = db.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO folders (account_id, path, label_id, sync_state,
                                      last_local_modified_at, last_synced_at)
                 VALUES (?1, 'Notes/A', 'Label_1', 'clean', 0, 0)",
                params![id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO tag_tombstones (account_id, uuid, tag, deleted_at)
                 VALUES (?1, ?2, 'gone', 0)",
                params![id, uuid],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO attachments (account_id, note_uuid, content_id, mime_type, data)
                 VALUES (?1, ?2, 'cid1', 'image/png', x'00')",
                params![id, uuid],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO note_uuid_aliases (account_id, old_uuid, new_uuid, created_at)
                 VALUES (?1, 'OLD', ?2, 0)",
                params![id, uuid],
            )
            .unwrap();
        }
        (db, dir)
    }

    fn distinct_account_ids(db: &Db, table: &str) -> Vec<String> {
        let conn = db.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT DISTINCT account_id FROM {table} ORDER BY account_id"
            ))
            .unwrap();
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<SqlResult<Vec<_>>>()
            .unwrap();
        rows
    }

    const LOCALFS_ID: &str = "localfs:1E2D3C4B-0000-0000-0000-000000000000";

    #[test]
    fn migration_19_qualifies_bare_ids_and_leaves_localfs_alone() {
        let (db, _dir) = seeded(&["a@b.com", LOCALFS_ID]);
        let accounts = vec![
            acct("a@b.com", "a@b.com", BackendKind::Gmail),
            acct(LOCALFS_ID, "vault", BackendKind::LocalFs),
        ];
        {
            let conn = db.conn.lock().unwrap();
            migrate_account_ids_with(&conn, &accounts).unwrap();
        }
        // Every table, not just `notes` — a rewrite that stops short strands
        // the rest under an id nothing will ever query again.
        for table in ACCOUNT_ID_TABLES {
            assert_eq!(
                distinct_account_ids(&db, table),
                vec!["gmail:a@b.com".to_string(), LOCALFS_ID.to_string()],
                "table {table} was not rewritten"
            );
        }
    }

    /// Migration 19 runs while `notes_fts` still has its migration-10 shape
    /// (no `folder` column — that arrives at 24). Its FTS re-derive must
    /// therefore not name `folder`, or every pre-19 vault fails to open.
    #[test]
    fn migration_19_reindexes_a_pre_24_fts_table() {
        let (db, _dir) = seeded(&["a@b.com"]);
        let accounts = vec![acct("a@b.com", "a@b.com", BackendKind::Gmail)];
        let conn = db.conn.lock().unwrap();
        conn.execute_batch(
            "DROP TRIGGER notes_fts_folder_ai;
             DROP TRIGGER notes_fts_folder_au;
             DROP TABLE notes_fts;
             CREATE VIRTUAL TABLE notes_fts USING fts5(
                 uuid UNINDEXED, account_id UNINDEXED, title, body, tokenize = 'trigram');",
        )
        .unwrap();
        migrate_account_ids_with(&conn, &accounts).unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM notes_fts WHERE account_id = 'gmail:a@b.com'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(n > 0, "the re-derive must still index under the new id");
    }

    #[test]
    fn migration_19_is_idempotent() {
        let (db, _dir) = seeded(&["a@b.com"]);
        let accounts = vec![acct("a@b.com", "a@b.com", BackendKind::Gmail)];
        let conn = db.conn.lock().unwrap();
        migrate_account_ids_with(&conn, &accounts).unwrap();
        migrate_account_ids_with(&conn, &accounts).unwrap();
        let id: String = conn
            .query_row("SELECT account_id FROM notes LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(id, "gmail:a@b.com", "second run must not double-prefix");
    }

    #[test]
    fn migration_19_resolves_by_email_when_the_account_list_was_already_rewritten() {
        // The half-finished state: accounts.json holds gmail:a@b.com, the
        // database still holds the bare a@b.com. Resolution is by email, so
        // this completes rather than aborting.
        let (db, _dir) = seeded(&["a@b.com"]);
        let accounts = vec![acct("gmail:a@b.com", "a@b.com", BackendKind::Gmail)];
        let conn = db.conn.lock().unwrap();
        migrate_account_ids_with(&conn, &accounts).unwrap();
        let id: String = conn
            .query_row("SELECT account_id FROM notes LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(id, "gmail:a@b.com");
    }

    /// The orphan is seeded ALONGSIDE a resolvable id, and sorts after it, so
    /// `a@b.com` is rewritten in every table before the orphan trips the
    /// abort. A single-id fixture would not test this at all: the abort would
    /// fire before any `UPDATE` ran, and "untouched" would be satisfied by a
    /// transaction that never wrote anything — the assertion would hold even
    /// if the transaction's drop behaviour were `Commit`. The state that
    /// destroys data is a *partial* rewrite, which is what this pins.
    #[test]
    fn migration_19_aborts_and_rolls_back_a_partial_rewrite() {
        let (db, _dir) = seeded(&["a@b.com", "orphan@nowhere.com"]);
        let accounts = vec![acct("a@b.com", "a@b.com", BackendKind::Gmail)];
        let conn = db.conn.lock().unwrap();
        let err = migrate_account_ids_with(&conn, &accounts).unwrap_err();
        assert!(
            format!("{err}").contains("orphan@nowhere.com"),
            "error must name the row: {err}"
        );
        drop(conn);
        for table in ACCOUNT_ID_TABLES {
            assert_eq!(
                distinct_account_ids(&db, table),
                vec!["a@b.com".to_string(), "orphan@nowhere.com".to_string()],
                "abort must roll back the rewrite already applied to {table}"
            );
        }
    }

    #[test]
    fn migration_19_carries_fts_rows_to_the_new_id() {
        let (db, _dir) = seeded(&["a@b.com"]);
        let accounts = vec![acct("a@b.com", "a@b.com", BackendKind::Gmail)];
        {
            let conn = db.conn.lock().unwrap();
            migrate_account_ids_with(&conn, &accounts).unwrap();
        }
        // Content, not just the id column. Counting rows by account_id says
        // nothing about what was re-derived INTO them: transposing the title
        // and body_html arguments at the `fts_index_conn` call site would
        // leave the counts identical and every search silently indexing the
        // wrong column.
        let hits = db
            .search_notes(Some("gmail:a@b.com"), None, "alpha", &[])
            .unwrap();
        assert_eq!(
            hits.iter().map(|n| n.uuid.as_str()).collect::<Vec<_>>(),
            vec!["UUID-0"],
            "the re-derived index must still find the note by its body text"
        );

        let conn = db.conn.lock().unwrap();
        let stale: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notes_fts WHERE account_id = 'a@b.com'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stale, 0, "no FTS row may keep the old account_id");
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notes_fts WHERE account_id = 'gmail:a@b.com'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(n > 0, "FTS rows must carry the new account_id");

        // `search_notes` MATCHes every column at once, so it alone cannot tell
        // a transposition from a correct index. These two column-scoped
        // queries can: body text must be in `body`, the title in `title`.
        let in_body: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notes_fts WHERE notes_fts MATCH 'body:\"alpha\"'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(in_body, 1, "body text must be indexed in the body column");
        let (fts_title, fts_body): (String, String) = conn
            .query_row(
                "SELECT title, body FROM notes_fts WHERE account_id = 'gmail:a@b.com'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(fts_title, "t", "title column must hold the title");
        assert!(
            fts_body.contains("alpha"),
            "body column must hold the body text, got {fts_body:?}"
        );
    }

    /// The `false` branches of the guard that keeps `cargo test` — and every
    /// temp-dir database — away from the developer's real `accounts.json`.
    /// Untested, a refactor of `paths::data_base()` or of `Db::open`'s
    /// directory handling could flip it with the whole suite still green.
    ///
    /// `live_base` is the REAL `paths::data_base()` here, exactly as the
    /// production call site passes it: the point of this test is that a
    /// tempdir connection is rejected on the DIRECTORY comparison and not by
    /// an early `None`/empty return, so it cannot pass for the wrong reason.
    #[test]
    fn the_live_cache_guard_rejects_temp_and_in_memory_connections() {
        let (db, dir) = seeded(&["a@b.com"]);
        let conn = db.conn.lock().unwrap();
        let path = conn.path().expect("an on-disk connection must expose a path");
        assert!(!path.is_empty(), "an on-disk connection must expose a path");
        // Canonicalised: on macOS the tempdir is handed out as `/var/...`
        // while SQLite reports the resolved `/private/var/...`.
        assert!(
            std::path::Path::new(path)
                .canonicalize()
                .unwrap()
                .starts_with(dir.path().canonicalize().unwrap()),
            "fixture sanity: the connection must live in the tempdir"
        );
        assert!(
            !connection_is_the_live_user_cache(&conn, crate::paths::data_base()),
            "a tempdir database must never be treated as the live user cache"
        );

        let mem = rusqlite::Connection::open_in_memory().unwrap();
        assert!(
            !connection_is_the_live_user_cache(&mem, crate::paths::data_base()),
            "an in-memory database must never be treated as the live user cache"
        );
    }

    /// The **`true`** branch — the one that rewrites `accounts.json`.
    ///
    /// Reachable only because `live_base` is a parameter. With the lookup
    /// inlined, the sole way to make this function answer `true` was to make
    /// the connection the developer's real cache, and an earlier attempt to
    /// test it did exactly that and rewrote a real `accounts.json`. Here the
    /// "live" directory is a tempdir, so the assertion costs nothing.
    #[test]
    fn the_live_cache_guard_accepts_a_connection_in_the_live_directory() {
        let base = tempfile::tempdir().unwrap();
        // `<base>/jodd` is the shape `.setup()` builds: data_base() joined with
        // "jodd" (the guard appends the leaf itself, so `base` is passed bare).
        let db = Db::open_unencrypted(&base.path().join("jodd")).unwrap();
        let conn = db.conn.lock().unwrap();
        assert!(
            connection_is_the_live_user_cache(&conn, Some(base.path().to_path_buf())),
            "a connection inside <live_base>/jodd IS the live user cache"
        );
    }

    /// The false-negative direction the canonicalisation exists for.
    ///
    /// A false negative is the expensive one: the tables get qualified and
    /// `accounts.json` is left bare, which is the split state migration #19
    /// exists to close. macOS reaches the same shape for real — `/var` is a
    /// symlink to `/private/var`, and SQLite reports the resolved path while
    /// `paths::data_base()` does not — so this is a live case, not a
    /// contrived one.
    #[cfg(unix)]
    #[test]
    fn the_live_cache_guard_sees_through_a_symlinked_data_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir_all(real.join("jodd")).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        // Connection opened through the REAL path, `live_base` given as the
        // symlink. Plain path equality says "different"; canonicalising both
        // sides says "same", which is the answer that keeps the account list
        // and the tables together.
        let db = Db::open_unencrypted(&real.join("jodd")).unwrap();
        let conn = db.conn.lock().unwrap();
        assert!(
            connection_is_the_live_user_cache(&conn, Some(link.clone())),
            "a symlinked data directory must still resolve to the live user cache"
        );
    }

    /// Case 2 of the fail-closed rule: an id that survives ONLY in satellite
    /// tables belongs to an account that is provably gone (`remove_account`
    /// shortens `accounts.json` before wiping the database, and
    /// `delete_account`'s satellite deletes are best-effort). It has no note
    /// to misfile, so it must be deleted rather than abort the whole open.
    #[test]
    fn migration_19_deletes_a_satellite_only_orphan_instead_of_aborting() {
        let (db, _dir) = seeded(&["a@b.com"]);
        const GONE: &str = "gone@nowhere.com";
        {
            let conn = db.conn.lock().unwrap();
            // Exactly the leftovers `delete_account` used to miss — no row in
            // `notes` or `folders` anywhere.
            conn.execute(
                "INSERT INTO note_tags (account_id, uuid, tag) VALUES (?1, 'U-GONE', 'stale')",
                params![GONE],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO edges (account_id, src_uuid, dst_id, dst_title, rel) \
                 VALUES (?1, 'U-GONE', '', 'stale', 'tagged')",
                params![GONE],
            )
            .unwrap();
        }
        let accounts = vec![acct("a@b.com", "a@b.com", BackendKind::Gmail)];
        {
            let conn = db.conn.lock().unwrap();
            migrate_account_ids_with(&conn, &accounts)
                .expect("a satellite-only orphan must not abort the migration");
        }
        for table in ACCOUNT_ID_TABLES {
            assert!(
                !distinct_account_ids(&db, table).iter().any(|id| id == GONE),
                "table {table} still holds rows for the departed account"
            );
        }
        // …and the resolvable account was migrated as normal.
        assert_eq!(
            distinct_account_ids(&db, "notes"),
            vec!["gmail:a@b.com".to_string()]
        );
    }

    /// Case 1 — the one that still aborts — must be distinguishable at the
    /// caller. Without the marker it arrives as `DbOpenError::Sqlite`, whose
    /// `.setup()` handler opens an empty temp-dir cache: the user's intact
    /// database is simply not shown, every launch, with one log line.
    #[test]
    fn an_orphan_that_still_holds_notes_aborts_with_a_classifiable_error() {
        let (db, _dir) = seeded(&["a@b.com", "orphan@nowhere.com"]);
        let accounts = vec![acct("a@b.com", "a@b.com", BackendKind::Gmail)];
        let conn = db.conn.lock().unwrap();
        let err = migrate_account_ids_with(&conn, &accounts).unwrap_err();
        assert!(
            matches!(
                classify_migrate_error(err),
                crate::db_crypto::DbOpenError::AccountIdMigration(_)
            ),
            "the abort must reach the caller as AccountIdMigration, not as a \
             generic Sqlite error the temp-dir fallback swallows"
        );
    }

    /// An ordinary SQLite failure keeps its existing meaning — the classifier
    /// must not turn every migration error into the new variant, or the
    /// temp-dir fallback stops working for the case it was written for.
    #[test]
    fn an_ordinary_sqlite_error_is_not_reclassified() {
        let err = rusqlite::Error::InvalidParameterName("some other problem".to_string());
        assert!(matches!(
            classify_migrate_error(err),
            crate::db_crypto::DbOpenError::Sqlite(_)
        ));
    }

    /// Case 3: `load_accounts` swallows every error and returns an empty
    /// `Vec`, so an unreadable or truncated `accounts.json` is indistinguishable
    /// from "this user has no accounts". Treating it as orphans would either
    /// delete an intact cache or lock the user out of it on every launch.
    #[test]
    fn an_empty_account_list_leaves_a_non_empty_database_untouched() {
        let (db, _dir) = seeded(&["a@b.com", "c@d.com"]);
        {
            let conn = db.conn.lock().unwrap();
            migrate_account_ids_with(&conn, &[])
                .expect("an unreadable account list must not abort the open");
        }
        for table in ACCOUNT_ID_TABLES {
            assert_eq!(
                distinct_account_ids(&db, table),
                vec!["a@b.com".to_string(), "c@d.com".to_string()],
                "table {table} must be untouched when the account list could not be read"
            );
        }
    }
}

#[cfg(test)]
mod tests_migrate {
    use super::*;
    use tempfile::tempdir;

    // Characterization guard for the Step refactor (task 2 of the account-
    // identity plan): proves migrate() still applies every shipped migration
    // exactly once, both before AND after `migrations` moves from
    // `&[(i64, &str)]` to `&[(i64, Step)]`. It must stay green across that
    // refactor — a red run here would mean the refactor changed behaviour.
    #[test]
    fn a_rust_migration_step_runs_and_is_recorded() {
        let dir = tempdir().unwrap();
        let db = Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        let conn = db.conn.lock().unwrap();
        // Every shipped migration must be recorded exactly once after open().
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM migrations", [], |r| r.get(0))
            .unwrap();
        assert!(n >= 18, "expected the shipped migrations to be recorded, got {n}");
        // Named explicitly, because `n >= 18` pins no particular version: the
        // only `Step::Rust` in the slice is migration 19, so without this the
        // registration line is compile-verified and nothing more — deleting
        // the `(19, Step::Rust(..))` entry would leave this test green.
        let has_19: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM migrations WHERE version = 19",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_19, 1, "migration 19 must be registered and recorded");
        let dupes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM (SELECT version FROM migrations GROUP BY version HAVING COUNT(*) > 1)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dupes, 0, "a version was recorded twice");
    }
}
