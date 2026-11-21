//! Ask Jodd conversation history.
//! Split out of `db.rs` 2026-11-16.

use super::*;

impl Db {
    /// Append one answered turn; `None` creates the conversation (title from
    /// this question). Returns the conversation id. `Some(id)` for a
    /// conversation that no longer exists is an error, never a re-create —
    /// a turn landing after the owner deleted it must not resurrect it.
    pub fn ask_append_turn(
        &self,
        conversation_id: Option<&str>,
        scope: &crate::ask::AskScope,
        destination: &str,
        turn: &crate::ask::history::NewTurn,
        now: i64,
    ) -> SqlResult<String> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let id = match conversation_id {
            Some(id) => {
                let updated = tx.execute(
                    "UPDATE ask_conversations SET updated_at = ?2, destination = ?3 WHERE id = ?1",
                    params![id, now, destination],
                )?;
                if updated == 0 {
                    return Err(rusqlite::Error::QueryReturnedNoRows);
                }
                id.to_string()
            }
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                tx.execute(
                    "INSERT INTO ask_conversations (id, title, scope_json, destination, created_at, updated_at, archived_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?5, NULL)",
                    params![
                        id,
                        crate::ask::history::conversation_title(turn.question),
                        serde_json::to_string(scope).unwrap_or_default(),
                        destination,
                        now
                    ],
                )?;
                id
            }
        };
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM ask_turns WHERE conversation_id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO ask_turns (conversation_id, seq, question, answer_markdown, cited_json, read_json, stats_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                seq,
                turn.question,
                turn.answer_markdown,
                serde_json::to_string(turn.cited).unwrap_or_else(|_| "[]".into()),
                serde_json::to_string(turn.read).unwrap_or_else(|_| "[]".into()),
                serde_json::to_string(turn.stats).unwrap_or_else(|_| "{}".into()),
                now
            ],
        )?;
        tx.commit()?;
        Ok(id)
    }

    pub fn ask_list_conversations(&self, archived: bool) -> SqlResult<Vec<crate::ask::history::ConversationSummary>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT c.id, c.title, c.scope_json, c.updated_at, c.archived_at,
                    (SELECT COUNT(*) FROM ask_turns t WHERE t.conversation_id = c.id)
             FROM ask_conversations c
             WHERE (c.archived_at IS NOT NULL) = ?1
             ORDER BY c.updated_at DESC, c.id",
        )?;
        let rows = stmt.query_map(params![archived], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?, r.get::<_, Option<i64>>(4)?, r.get::<_, i64>(5)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, title, scope_json, updated_at, archived_at, turn_count) = row?;
            // A row whose scope no longer parses is skipped, not fatal: one
            // bad row must not hide the rest of the history.
            let Ok(scope) = serde_json::from_str(&scope_json) else { continue };
            out.push(crate::ask::history::ConversationSummary {
                id, title, scope, updated_at, archived: archived_at.is_some(), turn_count,
            });
        }
        Ok(out)
    }

    pub fn ask_load_conversation(&self, id: &str) -> SqlResult<Option<crate::ask::history::StoredConversation>> {
        let conn = self.conn.lock().unwrap();
        let head = conn
            .query_row(
                "SELECT title, scope_json, destination, created_at, updated_at, archived_at
                 FROM ask_conversations WHERE id = ?1",
                params![id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?, r.get::<_, i64>(4)?, r.get::<_, Option<i64>>(5)?)),
            )
            .optional()?;
        let Some((title, scope_json, destination, created_at, updated_at, archived_at)) = head else {
            return Ok(None);
        };
        let scope = serde_json::from_str(&scope_json)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e)))?;
        let mut stmt = conn.prepare(
            "SELECT seq, question, answer_markdown, cited_json, read_json, stats_json, created_at
             FROM ask_turns WHERE conversation_id = ?1 ORDER BY seq",
        )?;
        let turns = stmt
            .query_map(params![id], |r| {
                let json = |i: usize| -> rusqlite::Result<String> { r.get(i) };
                let parse_err = |i: usize, e: serde_json::Error| {
                    rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e))
                };
                Ok(crate::ask::history::StoredTurn {
                    seq: r.get(0)?,
                    question: r.get(1)?,
                    answer_markdown: r.get(2)?,
                    cited: serde_json::from_str(&json(3)?).map_err(|e| parse_err(3, e))?,
                    read: serde_json::from_str(&json(4)?).map_err(|e| parse_err(4, e))?,
                    stats: serde_json::from_str(&json(5)?).map_err(|e| parse_err(5, e))?,
                    created_at: r.get(6)?,
                })
            })?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(Some(crate::ask::history::StoredConversation {
            id: id.to_string(), title, scope, destination, created_at, updated_at,
            archived: archived_at.is_some(), turns,
        }))
    }

    pub fn ask_rename_conversation(&self, id: &str, title: &str) -> SqlResult<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute("UPDATE ask_conversations SET title = ?2 WHERE id = ?1", params![id, title])? > 0)
    }

    pub fn ask_set_archived(&self, id: &str, archived_at: Option<i64>) -> SqlResult<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute("UPDATE ask_conversations SET archived_at = ?2 WHERE id = ?1", params![id, archived_at])? > 0)
    }

    pub fn ask_delete_conversation(&self, id: &str) -> SqlResult<bool> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM ask_turns WHERE conversation_id = ?1", params![id])?;
        let n = tx.execute("DELETE FROM ask_conversations WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(n > 0)
    }

    /// Returns how many conversations were removed.
    pub fn ask_clear_history(&self) -> SqlResult<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM ask_turns", [])?;
        let n = tx.execute("DELETE FROM ask_conversations", [])?;
        tx.commit()?;
        Ok(n)
    }

    /// Conversations scoped to `account_id` (Account or Folder). Parsed in
    /// Rust rather than with `json_extract`, so it does not depend on the
    /// SQLCipher build's JSON1 support. Best-effort, like the rest of
    /// `delete_account`.
    pub(crate) fn ask_delete_for_account_conn(conn: &Connection, account_id: &str) {
        let ids: Vec<String> = match conn.prepare("SELECT id, scope_json FROM ask_conversations") {
            Ok(mut stmt) => stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map(|rows| {
                    rows.filter_map(Result::ok)
                        .filter(|(_, json)| {
                            serde_json::from_str::<crate::ask::AskScope>(json)
                                .is_ok_and(|s| s.account_id() == Some(account_id))
                        })
                        .map(|(id, _)| id)
                        .collect()
                })
                .unwrap_or_default(),
            Err(_) => return,
        };
        for id in ids {
            // Turns and conversation go together or not at all — a failure
            // here must never leave a listed conversation with no turns.
            let result = conn.unchecked_transaction().and_then(|tx| {
                tx.execute("DELETE FROM ask_turns WHERE conversation_id = ?1", params![id])?;
                tx.execute("DELETE FROM ask_conversations WHERE id = ?1", params![id])?;
                tx.commit()
            });
            if let Err(e) = result {
                crate::log!("delete_account: ask conversation {id} not removed: {e}");
            }
        }
    }

    #[cfg(test)]
    pub fn ask_turn_count_for_tests(&self, conversation_id: &str) -> i64 {
        self.conn.lock().unwrap()
            .query_row("SELECT COUNT(*) FROM ask_turns WHERE conversation_id = ?1", params![conversation_id], |r| r.get(0))
            .unwrap()
    }

    #[cfg(test)]
    pub fn ask_drop_tables_for_tests(&self) {
        self.conn.lock().unwrap()
            .execute_batch("DROP TABLE ask_turns; DROP TABLE ask_conversations; DELETE FROM migrations WHERE version = 28;")
            .unwrap();
    }
}
