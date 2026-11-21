//! Pin state: local set/batch, remote pins (`RemotePin` policy), the pin
//! sidecar dirty queue.
//! Split out of `db.rs` 2026-11-16.

use super::*;

impl Db {
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
        let uuid = Self::resolve_note_uuid_conn(&conn, uuid, account_id)?;
        conn.execute(
            "UPDATE notes SET pinned = ?1, pin_dirty = 1, pin_push_blocked_reason = NULL,
                    last_local_modified_at = ?2, pin_changed_at = ?2
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
        let resolved = uuids.iter()
            .map(|u| Self::resolve_note_uuid_conn(&tx, u, account_id))
            .collect::<SqlResult<std::collections::HashSet<String>>>()?;
        let now = now_ms();
        let mut touched = 0usize;
        {
            let mut stmt = tx.prepare(
                "UPDATE notes SET pinned = ?1, pin_dirty = 1, pin_push_blocked_reason = NULL,
                    last_local_modified_at = ?2, pin_changed_at = ?2
                 WHERE uuid = ?3 AND account_id = ?4",
            )?;
            for uuid in &resolved {
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
        horizon_ms: i64,
    ) -> SqlResult<usize> {
        let conn = self.conn.lock().unwrap();
        // Only apply when not locally pin-dirty. If the user has flipped
        // pin since the last sync, the local intent wins until the worker
        // pushes — applying remote here would clobber the user's recent
        // toggle. Worker push will overwrite the remote sidecar shortly.
        //
        // Nor when this device changed the pin after `horizon_ms`
        // (`prune_horizon_ms()` in production) to a DIFFERENT value: the
        // listing may predate the push that just cleared `pin_dirty` — an
        // unpin whose trash landed after the listing was read would come
        // straight back. A listing that agrees still records its
        // `meta_msg_id`.
        let touched = conn.execute(
            "UPDATE notes SET pinned = ?1, meta_msg_id = ?2
             WHERE uuid = ?3 AND account_id = ?4 AND pin_dirty = 0
               AND (pinned = ?1 OR pin_changed_at IS NULL OR pin_changed_at < ?5)",
            params![pinned as i64, meta_msg_id, uuid, account_id, horizon_ms],
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
        horizon_ms: i64,
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
               -- A pin changed here inside the horizon is newer than the
               -- listing may be: the push that cleared pin_dirty can land
               -- after the sidecars were read, and Gmail's list can lag a
               -- just-inserted message (gotcha #43's pin entry).
               AND (pin_changed_at IS NULL OR pin_changed_at < ?2)
               AND uuid NOT IN (SELECT uuid FROM _keep_pin_uuids)",
            params![account_id, horizon_ms],
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
                    pinned, meta_msg_id, pin_dirty, push_blocked_reason, push_blocked_by_remote
             FROM notes WHERE pin_dirty = 1 AND pin_push_blocked_reason IS NULL
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
    ///
    /// `new_remote_version` is `Some((version, date))` when the sidecar push
    /// also touched the note's own remote object (Microsoft's named MAPI
    /// property — see `backend::RemoteNoteVersion`'s doc comment) and
    /// `None` when it did not (Gmail/LocalFs's separate sidecar object).
    /// Only in the `Some` case are `remote_version`/`date` updated —
    /// otherwise this call must not touch them, or a genuinely unrelated
    /// remote change made in the same window would be silently forgotten.
    /// Stamping them here on the `Some` path is exactly why this exists:
    /// without it, `notes.remote_version` would silently desync from what
    /// the property PATCH actually left on the server, and the next poll's
    /// `reconcile_one` would misread a pin-only push as a remote content
    /// change — spinning up a spurious conflict copy for a Dirty row.
    pub fn mark_pin_pushed(
        &self,
        uuid: &str,
        account_id: &str,
        new_meta_msg_id: Option<&str>,
        pushed_pinned: bool,
        new_remote_version: Option<(&str, &str)>,
    ) -> SqlResult<usize> {
        let conn = self.conn.lock().unwrap();
        // Only clear pin_dirty if the SQLite-side `pinned` still equals
        // the value we just pushed. If it changed (user re-toggled
        // mid-push) leave pin_dirty=1 so the worker re-pushes.
        let touched = match new_remote_version {
            Some((version, date)) => conn.execute(
                "UPDATE notes
                 SET meta_msg_id = ?1,
                     pin_dirty = 0,
                     pin_push_blocked_reason = NULL,
                     remote_version = ?5,
                     -- A version this write minted describes content no
                     -- fingerprint was taken of (migration 26).
                     remote_base_fp = NULL,
                     date = ?6,
                     pin_changed_at = ?7
                 WHERE uuid = ?2 AND account_id = ?3 AND pinned = ?4",
                params![new_meta_msg_id, uuid, account_id, pushed_pinned as i64, version, date, now_ms()],
            )?,
            None => conn.execute(
                "UPDATE notes
                 SET meta_msg_id = ?1,
                     pin_dirty = 0,
                     pin_push_blocked_reason = NULL,
                     pin_changed_at = ?5
                 WHERE uuid = ?2 AND account_id = ?3 AND pinned = ?4",
                params![new_meta_msg_id, uuid, account_id, pushed_pinned as i64, now_ms()],
            )?,
        };
        Ok(touched)
    }
}
