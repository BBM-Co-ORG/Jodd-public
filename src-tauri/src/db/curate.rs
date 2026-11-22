//! Curate candidates and the URLs already cited.
//! Split out of `db.rs` 2026-11-16.

use super::*;

impl Db {
    /// `(uuid, url)` for every citation edge in the account (ingest Sources).
    pub fn cited_urls(&self, account_id: &str) -> SqlResult<Vec<(String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT src_uuid, dst_id FROM edges WHERE account_id = ?1 AND rel = 'cites' AND dst_id <> ''")?;
        let rows = stmt.query_map(params![account_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    }

    /// Insert a pending Curate proposal unless one with the same
    /// `(account, kind, note_key)` is already pending or was dismissed —
    /// a scan never repeats itself or re-proposes what the user refused.
    /// Returns the new id, or `None` when it was skipped.
    pub fn curate_insert(&self, account_id: &str, kind: &str, note_key: &str, payload: &str, source: &str) -> SqlResult<Option<i64>> {
        let conn = self.conn.lock().unwrap();
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM curate_proposals WHERE account_id = ?1 AND kind = ?2 AND note_key = ?3 AND status IN ('pending','dismissed'))",
            params![account_id, kind, note_key],
            |r| r.get(0),
        )?;
        if exists {
            return Ok(None);
        }
        conn.execute(
            "INSERT INTO curate_proposals (account_id, kind, note_key, payload, source, status, created_at) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6)",
            params![account_id, kind, note_key, payload, source, now_ms()],
        )?;
        Ok(Some(conn.last_insert_rowid()))
    }

    /// `(id, kind, payload, status, created_at, error)` for one account's
    /// proposals in `statuses`, oldest first.
    pub fn curate_list(&self, account_id: &str, statuses: &[&str]) -> SqlResult<Vec<(i64, String, String, String, i64, Option<String>)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, kind, payload, status, created_at, error, account_id FROM curate_proposals WHERE account_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![account_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, String>(3)?, r.get(4)?, r.get(5)?))
        })?;
        Ok(rows.collect::<SqlResult<Vec<_>>>()?.into_iter().filter(|r| statuses.contains(&r.3.as_str())).collect())
    }

    /// `(account_id, kind, payload, status)` of one proposal.
    pub fn curate_get(&self, id: i64) -> SqlResult<Option<(String, String, String, String)>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT account_id, kind, payload, status FROM curate_proposals WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
    }

    /// Compare-and-swap a proposal's status: `true` only if it was `from`.
    /// Two Approve clicks, or Approve racing Dismiss, decide once.
    pub fn curate_decide(&self, id: i64, from: &str, to: &str, error: Option<&str>) -> SqlResult<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "UPDATE curate_proposals SET status = ?1, decided_at = ?2, error = ?3 WHERE id = ?4 AND status = ?5",
            params![to, now_ms(), error, id, from],
        )? == 1)
    }
}
