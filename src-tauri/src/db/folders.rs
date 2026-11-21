//! Folder operations: the local-first folder rows, their sync states, and
//! the subtree rename.
//! Split out of `db.rs` 2026-11-16.

use super::*;

// ─── Folder operations ──────────────────────────────────────────────────────
//
// Folders mirror the same local-first + sync-worker pattern as notes, but
// with simpler shape: a folder is essentially (account, path, sync_state).
// No body content, no UUID — Gmail's label_id is the durable identifier
// once the label exists remotely.

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            FolderSyncState::Clean => "clean",
            FolderSyncState::DirtyNew => "dirty_new",
            FolderSyncState::DirtyRenamed => "dirty_renamed",
            FolderSyncState::DeletedPending => "deleted_pending",
        }
    }
    pub(crate) fn from_str(s: &str) -> Self {
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
    /// frontend doesn't see folders being trashed). A delete the backend
    /// REFUSED is listed again: the folder still exists remotely, and only
    /// listed can it show its block and Try again (gotcha #14).
    pub fn list_folders(&self, account_id: &str) -> SqlResult<Vec<CachedFolder>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT account_id, path, label_id, sync_state,
                    last_local_modified_at, last_synced_at, kind
             FROM folders
             WHERE account_id = ?1
               AND (sync_state != 'deleted_pending' OR push_blocked_reason IS NOT NULL)
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

    /// A folder row's sync state INCLUDING `deleted_pending`, which
    /// `list_folders` hides. `None` = no row. `llm::filing` needs the
    /// difference: a pending-deletion Inbox must not be reused or recreated.
    pub fn folder_sync_state(&self, account_id: &str, path: &str) -> SqlResult<Option<FolderSyncState>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT sync_state FROM folders WHERE account_id = ?1 AND path = ?2",
            params![account_id, path],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map(|s| s.map(|s| FolderSyncState::from_str(&s)))
    }

    /// Test-only: put a folder row into an arbitrary sync state. Reaching
    /// `deleted_pending` honestly needs a clean row first (a `dirty_new`
    /// delete drops the row), which needs a remote.
    #[cfg(test)]
    pub(crate) fn force_folder_sync_state(&self, account_id: &str, path: &str, state: FolderSyncState) {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE folders SET sync_state = ?1 WHERE account_id = ?2 AND path = ?3",
            params![state.as_str(), account_id, path],
        )
        .unwrap();
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
    ///
    /// **A blocked `dirty_new` row is adopted** when the remote lists its exact
    /// name: the refused create's blocker is gone (another client made the
    /// label, or the user fixed the clash in Gmail), and nothing else would
    /// ever retry it. The same adoption a create's 409 performs
    /// (`wire::adopt_on_conflict`). An unblocked `dirty_new` row is still
    /// left to its own push.
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
                last_synced_at = excluded.last_synced_at,
                push_blocked_reason = NULL
             WHERE folders.sync_state = 'clean'
                OR (folders.sync_state = 'dirty_new' AND folders.push_blocked_reason IS NOT NULL)",
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
    /// `kind` is re-derived from the new leaf (gotcha #4).
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
                 push_blocked_reason = NULL,
                 last_local_modified_at = ?2,
                 kind = ?5
             WHERE account_id = ?3 AND path = ?4",
            params![new_path, now_ms(), account_id, old_path, derive_workflow_kind(new_path)],
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
                        push_blocked_reason = NULL,
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
                last_synced_at = ?2, push_blocked_reason = NULL
             WHERE account_id = ?3 AND path = ?4",
            params![label_id, now_ms(), account_id, path],
        )?;
        Ok(())
    }

    /// After successful push of a rename, mark clean — and retire any clean
    /// mirror of it: the row a pull wrote at the remote's old name, with the
    /// same label id, while the rename was blocked (`rename_subtree_over_mirror`).
    /// Left alone it lingered until a full pull pruned it, an empty duplicate
    /// that was still this label, so deleting it deleted this folder. Its
    /// notes — the clean ones the pull relabelled to the old name — follow.
    pub fn mark_folder_renamed(&self, account_id: &str, path: &str) -> SqlResult<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE folders SET sync_state = 'clean', last_synced_at = ?1,
                push_blocked_reason = NULL
             WHERE account_id = ?2 AND path = ?3",
            params![now_ms(), account_id, path],
        )?;
        let mirrors: Vec<String> = tx
            .prepare(
                "SELECT m.path FROM folders m JOIN folders r
                   ON r.account_id = m.account_id AND r.label_id = m.label_id
                 WHERE r.account_id = ?1 AND r.path = ?2
                   AND r.label_id IS NOT NULL AND r.label_id != ''
                   AND m.path != r.path AND m.sync_state = 'clean'",
            )?
            .query_map(params![account_id, path], |r| r.get(0))?
            .collect::<SqlResult<_>>()?;
        for mirror in mirrors {
            tx.execute("DELETE FROM folders WHERE account_id = ?1 AND path = ?2", params![account_id, mirror])?;
            tx.execute(
                "UPDATE notes
                 SET label = CASE WHEN label = ?1 THEN ?2 ELSE ?2 || substr(label, length(?1) + 1) END
                 WHERE account_id = ?3
                   AND (label = ?1 OR substr(label, 1, length(?1) + 1) = ?1 || '/')",
                params![mirror, path, account_id],
            )?;
        }
        tx.commit()?;
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
             WHERE sync_state != 'clean' AND push_blocked_reason IS NULL
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
    /// `synced_before_ms`: as `prune_clean` — a folder pushed during the
    /// listing, or after the cached label map was fetched, is spared.
    pub fn prune_clean_folders(
        &self,
        account_id: &str,
        keep_paths: &[String],
        synced_before_ms: i64,
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
               AND COALESCE(last_synced_at, 0) < ?2
               AND path NOT IN (SELECT path FROM _keep_folders)",
            params![account_id, synced_before_ms],
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

    /// Why `delete_folder` must not delete this folder, if it must not.
    /// Checked from the cache (cheap, no network). The cache mirrors notes
    /// from the most recent fetch; in-flight local-only edits are counted
    /// too — both safer.
    pub fn folder_delete_refusal(&self, account_id: &str, path: &str) -> SqlResult<Option<String>> {
        let folders = self.list_folders(account_id)?;
        let Some(row) = folders.iter().find(|f| f.path == path) else {
            return Ok(Some(format!("Folder not found: {}", path)));
        };
        // Two rows that share a label id are ONE folder on the server, and the
        // worker deletes by label id. Emptiness below is counted by PATH, so
        // either row can look empty while the other holds the notes: a pull's
        // mirror of a blocked rename (its notes are dirty under the blocked
        // name), or — the post-merge review of #151 — the blocked row itself
        // once a pull has moved the clean notes into the mirror. Deleting the
        // "empty" one deleted the real folder and every note in it (LocalFs
        // `remove_dir_all`; a Gmail label). Refuse whichever side is chosen.
        if let Some(other) = self.other_row_with_label(account_id, row)? {
            return Ok(Some(self.shared_label_refusal(account_id, row, &other)?));
        }
        let prefix = format!("{}/", path);
        if folders.iter().any(|f| f.path.starts_with(&prefix)) {
            return Ok(Some(format!("Folder '{}' has sub-folders. Delete those first.", path)));
        }
        let note_count = self.count_notes_in_label(account_id, path)?;
        if note_count > 0 {
            return Ok(Some(format!(
                "Folder '{}' is not empty ({} notes). Move or delete them first.",
                path, note_count
            )));
        }
        Ok(None)
    }

    /// Why a folder sharing its label id with `other` cannot be deleted, and
    /// what the user can actually do — the remedy must name a row that can
    /// act: a refused DELETE is kept by renaming it to the other name; a
    /// pending or refused RENAME is settled on its outermost pending ancestor
    /// (a child blocked behind its parent cannot be renamed "back" by itself —
    /// a rename changes one segment — and its own Try again re-blocks it).
    pub(crate) fn shared_label_refusal(&self, account_id: &str, row: &CachedFolder, other: &CachedFolder) -> SqlResult<String> {
        use FolderSyncState::*;
        let path = &row.path;
        let deleted = if row.sync_state == DeletedPending { Some(row) } else if other.sync_state == DeletedPending { Some(other) } else { None };
        if let Some(d) = deleted {
            let keep = if d.path == *path { &other.path } else { path };
            return Ok(format!(
                "The delete of '{}' was refused, and it is now the same folder on the server as \
                 '{}' — deleting either would delete that folder and its notes. To keep it, \
                 rename '{}' to '{}'.",
                d.path, keep, d.path, keep
            ));
        }
        let pending = if row.sync_state != Clean { row } else if other.sync_state != Clean { other } else {
            return Ok(format!(
                "'{}' and '{}' are the same folder on the server — one name is left over from a \
                 rename. It clears within a few minutes of syncing; then delete the one that remains.",
                path, other.path
            ));
        };
        let mut actor = pending.path.clone();
        while let Some((up, _)) = self.pending_ancestor_push(account_id, &actor)? {
            actor = up;
        }
        Ok(format!(
            "'{}' and '{}' are the same folder on the server — a rename of '{}' has not been saved \
             yet, and deleting either would delete that folder and its notes. Rename '{}' back to \
             its old name, or press Try again on it.",
            path, other.path, actor, actor
        ))
    }

    /// The other row sharing this folder's label id, if any — what the worker
    /// checks before it deletes a label, so a delete queued before that row
    /// appeared (a pull landed between the click and the push) cannot take
    /// the shared folder with it.
    pub fn shared_label_partner(&self, account_id: &str, path: &str) -> SqlResult<Option<String>> {
        let Some(row) = self.get_folder(account_id, path)? else { return Ok(None) };
        Ok(self.other_row_with_label(account_id, &row)?.map(|o| o.path))
    }

    /// Another row holding `row`'s (non-empty) label id, in any state — the
    /// other half of one server folder (a rename's mirror, a blocked rename,
    /// or a name left over from another device's rename until the next pull).
    /// A non-clean partner is preferred, since that is the one the user can
    /// act on (Try again / rename back).
    pub(crate) fn other_row_with_label(&self, account_id: &str, row: &CachedFolder) -> SqlResult<Option<CachedFolder>> {
        let Some(label_id) = row.label_id.as_deref().filter(|id| !id.is_empty()) else {
            return Ok(None);
        };
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT account_id, path, label_id, sync_state,
                    last_local_modified_at, last_synced_at, kind
             FROM folders
             WHERE account_id = ?1 AND label_id = ?2 AND path != ?3
             ORDER BY sync_state = 'clean', path LIMIT 1",
        )?;
        stmt.query_row(params![account_id, label_id, row.path], row_to_folder).optional()
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

    /// The nearest folder at or above `label` whose rename or delete has not
    /// pushed yet, if any, with its state and its block reason when the backend refused it
    /// (`mark_folder_push_blocked`). A note push waits on this: `rename_subtree`
    /// relabels notes without dirtying them, so the note carries the NEW name,
    /// which the remote does not hold yet — pushed by name it would create that
    /// label (and the folder's own rename would then collide with it) or be
    /// filed into whichever other label already holds it. Compared by `substr`,
    /// not `LIKE`, so `_` in `__Agent__` stays literal.
    ///
    /// A new folder counts only once its create is BLOCKED: pending, the
    /// note's own push creates the label and the folder's push adopts it;
    /// refused, the note's push meets the same refusal and is blocked with
    /// text no re-arm recognises, so a pull adopting the folder left it
    /// blocked. Held by the folder, it is re-armed with it.
    ///
    /// A delete counts only while it is PENDING. Once refused, the folder
    /// still exists remotely under this name and is listed again, so a note
    /// filed there pushes into it; held, it could never push, since Try
    /// again on the delete is refused while the note is in it.
    pub fn pending_folder_change_over(
        &self,
        account_id: &str,
        label: &str,
    ) -> SqlResult<Option<(String, FolderSyncState, Option<String>)>> {
        self.conn.lock().unwrap().query_row(
            &format!(
                "SELECT path, sync_state, push_blocked_reason FROM folders
                 WHERE account_id = ?1 AND {FOLDER_HOLDS_NOTES}
                   AND (path = ?2 OR substr(?2, 1, length(path) + 1) = path || '/')
                 ORDER BY length(path) DESC LIMIT 1"
            ),
            params![account_id, label],
            |r| Ok((r.get(0)?, FolderSyncState::from_str(&r.get::<_, String>(1)?), r.get(2)?)),
        ).optional()
    }

    /// Folders whose refused push blocks every note at or under them: exactly
    /// the blocked rows `pending_folder_change_over` holds a note for, so
    /// filing a new note agrees with what its push will do. A refused delete
    /// is not one — the folder is still on the remote (#151).
    pub fn folders_blocking_notes(&self, account_id: &str) -> SqlResult<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT path FROM folders
             WHERE account_id = ?1 AND {FOLDER_HOLDS_NOTES} AND push_blocked_reason IS NOT NULL"
        ))?;
        let rows = stmt.query_map(params![account_id], |r| r.get(0))?;
        rows.collect()
    }

    /// The nearest strict ancestor of `path` whose create or rename has not
    /// pushed yet, with its block reason when the backend refused it. A child
    /// folder's push depends on it either way: pending, the child waits (on
    /// iCloud a parent is looked up on the remote by path, so the child pushed
    /// first is refused although the parent lands a tick later); blocked, the
    /// child is blocked too — pushed by name it would be renamed or created
    /// INSIDE the other folder that holds the parent's new name (PR #147
    /// review: `Notes/B/x` landing under someone else's `Notes/B`).
    pub fn pending_ancestor_push(&self, account_id: &str, path: &str) -> SqlResult<Option<(String, Option<String>)>> {
        self.conn.lock().unwrap().query_row(
            "SELECT path, push_blocked_reason FROM folders
             WHERE account_id = ?1
               AND sync_state IN ('dirty_new', 'dirty_renamed')
               AND path != ?2 AND substr(?2, 1, length(path) + 1) = path || '/'
             ORDER BY length(path) DESC LIMIT 1",
            params![account_id, path],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional()
    }

    /// The folder twin of `clear_folder_wait_blocks`: re-arm every folder row
    /// blocked only because an ancestor was (`reason` begins with `prefix`)
    /// once no blocked create or rename sits strictly above it. Returns how
    /// many rows were re-armed.
    pub fn clear_folder_wait_folder_blocks(&self, account_id: &str, prefix: &str) -> SqlResult<usize> {
        self.conn.lock().unwrap().execute(
            "UPDATE folders SET push_blocked_reason = NULL
             WHERE account_id = ?1
               AND substr(push_blocked_reason, 1, length(?2)) = ?2
               AND NOT EXISTS (
                 SELECT 1 FROM folders a
                 WHERE a.account_id = folders.account_id
                   AND a.sync_state IN ('dirty_new', 'dirty_renamed')
                   AND a.push_blocked_reason IS NOT NULL
                   AND a.path != folders.path
                   AND substr(folders.path, 1, length(a.path) + 1) = a.path || '/')",
            params![account_id, prefix],
        )
    }

    /// `rename_subtree`, but `to` may already hold a clean row that is the
    /// SAME folder (same label id as the row at `from`) — the mirror a pull
    /// writes of the remote's name while a rename is refused (it relabels the
    /// clean notes back to that name too). Renaming back to it is how the
    /// user settles a refused rename, so the mirror rows are dropped in the
    /// same transaction instead of failing as "already exists". Any other row
    /// under `to` still fails, and nothing is changed.
    pub fn rename_subtree_over_mirror(&self, account_id: &str, from: &str, to: &str) -> SqlResult<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM folders
             WHERE account_id = ?1 AND sync_state = 'clean'
               AND (path = ?3 OR substr(path, 1, length(?3) + 1) = ?3 || '/')
               AND label_id IS NOT NULL AND label_id != ''
               AND label_id IN (SELECT label_id FROM folders
                                WHERE account_id = ?1 AND label_id IS NOT NULL
                                  AND (path = ?2 OR substr(path, 1, length(?2) + 1) = ?2 || '/'))",
            params![account_id, from, to],
        )?;
        let n = rename_subtree_tx(&tx, account_id, from, to)?;
        tx.commit()?;
        Ok(n)
    }

    /// Whether the row at `to` is only a clean mirror of the folder at `from`
    /// (same non-empty label id) — see `rename_subtree_over_mirror`.
    pub fn is_mirror_of(&self, account_id: &str, to: &str, from: &str) -> SqlResult<bool> {
        let (Some(t), Some(f)) = (self.get_folder(account_id, to)?, self.get_folder(account_id, from)?) else {
            return Ok(false);
        };
        Ok(t.sync_state == FolderSyncState::Clean
            && t.label_id.as_deref().is_some_and(|id| !id.is_empty())
            && t.label_id == f.label_id)
    }

    /// Re-arm every note blocked only because its folder was (`reason`
    /// begins with `prefix`) once no blocked create or rename sits at or
    /// above its label any more — however the folder's block was cleared:
    /// "Try again", renaming it (or deleting it, when no other row shares its
    /// label id), or a pull that adopted it. Local
    /// and condition-based, not a timer: a note whose folder is still blocked
    /// keeps its block. Uses `pending_folder_change_over`'s predicate, so it
    /// cannot re-arm a note the next push would block again. Returns how many
    /// notes were re-armed.
    pub fn clear_folder_wait_blocks(&self, account_id: &str, prefix: &str) -> SqlResult<usize> {
        self.conn.lock().unwrap().execute(
            CLEAR_FOLDER_WAIT_BLOCKS_SQL,
            params![account_id, prefix],
        )
    }

    /// Rename a folder AND all its descendants in one transaction, AND
    /// update all notes' label field to match; `kind` of the renamed folder
    /// is re-derived from its new leaf (gotcha #4). Used for move/rename ops
    /// that affect a subtree. Returns the count of folder rows touched.
    pub fn rename_subtree(
        &self,
        account_id: &str,
        old_path: &str,
        new_path: &str,
    ) -> SqlResult<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let n = rename_subtree_tx(&tx, account_id, old_path, new_path)?;
        tx.commit()?;
        Ok(n)
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
             WHERE account_id = ?1 AND substr(id, 1, length(?2) + 1) = ?2 || '/'",
            params![account_id, old_path, new_path],
        )
    }
}


/// The folder rows a note push holds for (`pending_folder_change_over`): a
/// rename not yet pushed, a delete not yet pushed, a create the backend
/// refused. One clause, so filing (`folders_blocking_notes`) and the push agree.
pub(crate) const FOLDER_HOLDS_NOTES: &str = "(sync_state = 'dirty_renamed'
    OR (sync_state = 'deleted_pending' AND push_blocked_reason IS NULL)
    OR (sync_state = 'dirty_new' AND push_blocked_reason IS NOT NULL))";

/// The body of `Db::rename_subtree`, inside the caller's transaction.
pub(crate) fn rename_subtree_tx(
    tx: &rusqlite::Transaction<'_>,
    account_id: &str,
    old_path: &str,
    new_path: &str,
) -> SqlResult<usize> {
    // Folders: the exact match + everything under it. We rewrite by
    // splicing the prefix. SQLite's substr() is 1-indexed. "Under it" is a
    // literal prefix compare, never LIKE: `_` and `%` are wildcards there and
    // ASCII case is ignored, so `Notes/A_b/%` also matches `Notes/aXb/…`.
    let folder_count = tx.execute(
        "UPDATE folders
         SET path = CASE
                 WHEN path = ?1 THEN ?2
                 ELSE ?2 || substr(path, length(?1) + 1)
             END,
             sync_state = CASE
                 WHEN sync_state = 'clean' THEN 'dirty_renamed'
                 WHEN sync_state = 'dirty_new' THEN 'dirty_new'
                 WHEN sync_state = 'dirty_renamed' THEN 'dirty_renamed'
                 -- A refused delete is listed again (`list_folders`), and
                 -- renaming it means keeping it, not the delete under a new name.
                 WHEN sync_state = 'deleted_pending' AND push_blocked_reason IS NOT NULL
                     THEN 'dirty_renamed'
                 ELSE sync_state
             END,
             push_blocked_reason = NULL,
             last_local_modified_at = ?3
         WHERE account_id = ?4
           AND (path = ?1 OR substr(path, 1, length(?1) + 1) = ?1 || '/')",
        params![old_path, new_path, now_ms(), account_id],
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
           AND (label = ?1 OR substr(label, 1, length(?1) + 1) = ?1 || '/')",
        params![old_path, new_path, account_id],
    )?;

    // Re-derive kind for the renamed folder itself: only the top row's leaf
    // changes. Descendants keep their own leaf names, so their derived kind
    // is unchanged.
    tx.execute(
        "UPDATE folders SET kind = ?1 WHERE account_id = ?2 AND path = ?3",
        params![derive_workflow_kind(new_path), account_id, new_path],
    )?;

    Ok(folder_count)
}

/// Named so `db::query_plans` can EXPLAIN it; runs every sync tick.
/// `push_blocked_reason IS NOT NULL` changes no result (`substr` of NULL
/// matches nothing); it is what lets `idx_notes_push_blocked` serve it.
pub(crate) const CLEAR_FOLDER_WAIT_BLOCKS_SQL: &str = "UPDATE notes SET push_blocked_reason = NULL, push_blocked_by_remote = 0
             WHERE account_id = ?1
               AND push_blocked_reason IS NOT NULL
               AND substr(push_blocked_reason, 1, length(?2)) = ?2
               AND NOT EXISTS (
                 SELECT 1 FROM folders f
                 WHERE f.account_id = notes.account_id
                   AND f.sync_state IN ('dirty_new', 'dirty_renamed')
                   AND f.push_blocked_reason IS NOT NULL
                   AND (f.path = notes.label
                        OR substr(notes.label, 1, length(f.path) + 1) = f.path || '/'))";

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
        Db::open_unencrypted(&path).expect("open temp db")
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

    /// Gotcha #4: kind is DERIVED from the leaf. A device that pulls the
    /// renamed label derives `user`; if the renaming device kept
    /// `system_workflow`, the same folder would sit in different Sidebar
    /// groups on different devices. Reachable since extract filing made
    /// `__Extracts__` renamable. This test exercises the production path
    /// (rename_subtree), not the direct rename_folder API.
    #[test]
    fn renaming_a_folder_re_derives_its_kind_from_the_new_leaf() {
        let db = temp_db();
        let acct = "test@example.com";
        db.ensure_workflow_folder(acct, "__Extracts__").unwrap();
        db.rename_subtree(acct, "Notes/__Extracts__", "Notes/Extracts").unwrap();
        db.create_folder_local_new(acct, "Notes/Plain").unwrap();
        db.rename_subtree(acct, "Notes/Plain", "Notes/Still Plain").unwrap();

        let kinds: std::collections::HashMap<String, String> =
            db.list_folders(acct).unwrap().into_iter().map(|f| (f.path, f.kind)).collect();
        assert_eq!(kinds.get("Notes/Extracts").map(String::as_str), Some("user"));
        assert_eq!(kinds.get("Notes/Still Plain").map(String::as_str), Some("user"));
    }

    /// A note push that creates its label must wait while that folder, or an
    /// ancestor, has a rename or delete still to push: creating `Notes/B`
    /// under a pending `Notes/A` → `Notes/B` rename makes the rename collide
    /// with the label the note just made, and it retries forever.
    #[test]
    fn a_pending_rename_or_delete_over_a_label_is_found_on_it_and_its_ancestors() {
        let db = temp_db();
        let acct = "test@example.com";
        db.upsert_folder_from_remote(acct, "Notes/A", "Label_A").unwrap();
        db.upsert_folder_from_remote(acct, "Notes/A/C", "Label_C").unwrap();
        db.create_folder_local_new(acct, "Notes/A/New").unwrap();
        db.upsert_folder_from_remote(acct, "Notes/Gone", "Label_G").unwrap();
        db.upsert_folder_from_remote(acct, "Notes/__Agent__", "Label_Ag").unwrap();
        db.upsert_folder_from_remote(acct, "Notes/_x_Agent__", "Label_X").unwrap();
        db.rename_subtree(acct, "Notes/A", "Notes/B").unwrap();
        db.mark_folder_deleted(acct, "Notes/Gone").unwrap();
        db.rename_subtree(acct, "Notes/_x_Agent__", "Notes/_y_Agent__").unwrap();

        let pending = |label: &str| db.pending_folder_change_over(acct, label).unwrap().map(|(p, _, _)| p);
        assert_eq!(pending("Notes/B").as_deref(), Some("Notes/B"));
        assert_eq!(pending("Notes/B/C").as_deref(), Some("Notes/B/C"), "rename_subtree marks C too");
        // rename_subtree leaves a dirty_new child dirty_new — only its
        // ancestor says the label is not ready.
        assert_eq!(pending("Notes/B/New").as_deref(), Some("Notes/B"));
        assert_eq!(pending("Notes/Gone").as_deref(), Some("Notes/Gone"));
        // A sibling sharing a prefix is not under the folder, and `_` is a
        // literal, not a LIKE wildcard.
        assert_eq!(pending("Notes/Bx"), None);
        assert_eq!(pending("Notes/__Agent__"), None);
        assert_eq!(pending("Notes"), None);
    }

    /// A subtree is a literal path prefix. Under `LIKE 'Notes/A_b/%'`, `_`
    /// matches any character and ASCII case is ignored, so renaming
    /// `Notes/A_b` also rewrote `Notes/AXb/…` and `Notes/a_b/…` — siblings,
    /// not descendants. Folder names like `__Agent__` make this reachable.
    #[test]
    fn renaming_a_subtree_leaves_wildcard_and_case_siblings_alone() {
        let db = temp_db();
        let acct = "test@example.com";
        for top in ["Notes/A_b", "Notes/AXb", "Notes/a_b", "Notes/%"] {
            db.upsert_folder_from_remote(acct, top, &format!("L-{top}")).unwrap();
            db.upsert_folder_from_remote(acct, &format!("{top}/child"), &format!("L-{top}/c")).unwrap();
            crate::test_support::note(acct, &format!("n-{top}")).label(&format!("{top}/child")).insert(&db);
        }

        db.rename_subtree(acct, "Notes/A_b", "Notes/C").unwrap();
        db.rename_subtree(acct, "Notes/%", "Notes/P").unwrap();

        let mut paths: Vec<String> = db.list_folders(acct).unwrap().into_iter().map(|f| f.path).collect();
        paths.sort();
        assert_eq!(
            paths,
            ["Notes/AXb", "Notes/AXb/child", "Notes/C", "Notes/C/child",
             "Notes/P", "Notes/P/child", "Notes/a_b", "Notes/a_b/child"],
        );
        for (uuid, label) in [
            ("n-Notes/A_b", "Notes/C/child"),
            ("n-Notes/AXb", "Notes/AXb/child"),
            ("n-Notes/a_b", "Notes/a_b/child"),
            ("n-Notes/%", "Notes/P/child"),
        ] {
            let n = db.note_by_uuid(acct, uuid).unwrap().unwrap();
            assert_eq!(n.label, label, "{uuid}");
        }
        for sibling in ["Notes/AXb", "Notes/AXb/child", "Notes/a_b", "Notes/a_b/child"] {
            assert_eq!(db.folder_sync_state(acct, sibling).unwrap(), Some(FolderSyncState::Clean), "{sibling}");
        }
    }

    /// LocalFs note ids are file paths under the folder: the same literal
    /// prefix rule as `rename_subtree`.
    #[test]
    fn renaming_note_ids_for_a_folder_leaves_wildcard_and_case_siblings_alone() {
        let db = temp_db();
        let acct = "localfs:x";
        for (uuid, id) in [
            ("u1", "Notes/A_b/n1.eml"),
            ("u2", "Notes/AXb/n2.eml"),
            ("u3", "Notes/a_b/n3.eml"),
        ] {
            crate::test_support::note(acct, uuid).id(id).insert(&db);
        }

        db.rename_note_ids_for_folder(acct, "Notes/A_b", "Notes/C").unwrap();

        let id = |u: &str| db.note_by_uuid(acct, u).unwrap().unwrap().id;
        assert_eq!(id("u1"), "Notes/C/n1.eml");
        assert_eq!(id("u2"), "Notes/AXb/n2.eml");
        assert_eq!(id("u3"), "Notes/a_b/n3.eml");
    }
}
