//! Cross-account move/copy bookkeeping (`note_transfers`, roadmap #6).
//! Split out of `db.rs` 2026-11-16.

use super::*;

/// Put a moved original back as it was and forget the move. Only a row the
/// move hid is restored: one whose deletion already went is left alone.
pub(crate) fn call_off_transfer_conn(conn: &Connection, t: &NoteTransfer) -> SqlResult<()> {
    conn.execute(
        "UPDATE notes SET sync_state = ?3
         WHERE account_id = ?1 AND uuid = ?2 AND sync_state = 'deleted_pending'",
        params![t.source_account, t.source_uuid, t.source_prev_state],
    )?;
    conn.execute(
        "DELETE FROM note_transfers WHERE source_account = ?1 AND source_uuid = ?2",
        params![t.source_account, t.source_uuid],
    )?;
    Ok(())
}

impl Db {
    /// The open move of this original, if any (following a rekey alias).
    pub fn open_transfer(&self, source_account: &str, source_uuid: &str) -> SqlResult<Option<NoteTransfer>> {
        let conn = self.conn.lock().unwrap();
        let uuid = Self::resolve_note_uuid_conn(&conn, source_uuid, source_account)?;
        Ok(transfers_where_conn(&conn, "source_account = ?1 AND source_uuid = ?2", params![source_account, uuid])?
            .into_iter()
            .next())
    }

    /// Every open move, oldest first.
    pub fn list_transfers(&self) -> SqlResult<Vec<NoteTransfer>> {
        let conn = self.conn.lock().unwrap();
        transfers_where_conn(&conn, "1 = 1", [])
    }

    /// The worker's pass over the moves still waiting for their copy (spec
    /// §4.2, §4.3), in one transaction:
    ///
    /// - copy `clean` with a remote id → `target_safe`: the original's
    ///   deletion is let through;
    /// - copy gone or itself deleted (the user deleted the moved note) →
    ///   `target_safe` too: nothing to protect, and the user wanted it gone;
    /// - copy refused for good (`push_blocked_reason`) → called off: the
    ///   original comes back, and the copy stays to be seen (gotcha #14).
    pub fn settle_transfers(&self) -> SqlResult<TransferSettlement> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let waiting = transfers_where_conn(&tx, "state = ?1", params![TRANSFER_AWAITING_TARGET])?;
        let mut out = TransferSettlement::default();
        for t in waiting {
            let target_uuid = Self::resolve_note_uuid_conn(&tx, &t.target_uuid, &t.target_account)?;
            let target: Option<(String, String, Option<String>)> = tx
                .query_row(
                    "SELECT id, sync_state, push_blocked_reason FROM notes WHERE account_id = ?1 AND uuid = ?2",
                    params![t.target_account, target_uuid],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let safe = match &target {
                None => true,
                Some((_, state, _)) if state == "deleted_pending" => true,
                Some((id, state, _)) => state == "clean" && !id.is_empty(),
            };
            let refused = matches!(&target, Some((_, state, Some(_))) if state != "deleted_pending");
            if safe {
                tx.execute(
                    "UPDATE note_transfers SET state = ?3, target_uuid = ?4
                     WHERE source_account = ?1 AND source_uuid = ?2",
                    params![t.source_account, t.source_uuid, TRANSFER_TARGET_SAFE, target_uuid],
                )?;
                out.safe.push(NoteTransfer { state: TRANSFER_TARGET_SAFE.into(), target_uuid, ..t });
            } else if refused {
                call_off_transfer_conn(&tx, &t)?;
                out.called_off.push((t, TransferCallOff::TargetRefused));
            }
        }
        tx.commit()?;
        Ok(out)
    }

    /// Call off the open move of this original: it comes back as it was.
    /// Used when its own server changed it after the copy was made (spec
    /// §4.2's base check). Returns the move, or `None` when there was none.
    pub fn call_off_transfer(&self, source_account: &str, source_uuid: &str) -> SqlResult<Option<NoteTransfer>> {
        let conn = self.conn.lock().unwrap();
        let uuid = Self::resolve_note_uuid_conn(&conn, source_uuid, source_account)?;
        let Some(t) = transfers_where_conn(&conn, "source_account = ?1 AND source_uuid = ?2", params![source_account, uuid])?
            .into_iter()
            .next()
        else {
            return Ok(None);
        };
        call_off_transfer_conn(&conn, &t)?;
        Ok(Some(t))
    }
}
