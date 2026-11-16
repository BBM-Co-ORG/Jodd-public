//! Tag queries and edits over the derived `note_tags` table.
//! Split out of `db.rs` 2026-11-16.

use super::*;

impl Db {

    // ─── Tags ──────────────────────────────────────────────────────────────
    //
    // Tags have no sidecar of their own: they round-trip as inline
    // #hashtags in the note body (see `tags_from_body` /
    // `reconcile_tags_from_body_conn`), which re-derives `note_tags` on
    // every local edit and every remote pull. This is just a read helper
    // over that derived table.

    /// Every tag on one note, sorted alphabetically.
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
}

impl Db {
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

    /// (tag, label) for every distinct pair over the same rows as
    /// `list_all_tags` (non-deleted notes). The AI vocabulary filters these
    /// by folder (`AiScope::tags_visible_to_ai`); the sidebar cloud keeps
    /// `list_all_tags`.
    pub fn list_tags_with_labels(&self, account_id: &str) -> SqlResult<Vec<(String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT t.tag, n.label
             FROM note_tags t
             JOIN notes n ON n.account_id = t.account_id AND n.uuid = t.uuid
             WHERE t.account_id = ?1 AND n.sync_state != 'deleted_pending'
             ORDER BY t.tag, n.label",
        )?;
        let rows = stmt.query_map(params![account_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        rows.collect()
    }

    /// uuid → tags for every tagged note in an account, each list in the order
    /// its body carries them. `reconcile_tags_from_body_conn` replaces a note's
    /// rows in body order on every write, so rowid order IS body order — which
    /// is what lets Ask read here instead of re-deriving every body.
    pub fn note_tags_in_body_order(&self, account_id: &str) -> SqlResult<HashMap<String, Vec<String>>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT uuid, tag FROM note_tags WHERE account_id = ?1 ORDER BY rowid",
        )?;
        let rows = stmt.query_map(params![account_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut out: HashMap<String, Vec<String>> = HashMap::new();
        for row in rows {
            let (uuid, tag) = row?;
            out.entry(uuid).or_default().push(tag);
        }
        Ok(out)
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
    /// Notes in `account_id` carrying any of `tags`, newest first — the
    /// same-tag step of Ask Jodd's note-scope walk (ask/related.rs). Reads
    /// `note_tags`, which is derived per backend (gotcha #38), never the body.
    pub fn notes_sharing_tags(&self, account_id: &str, tags: &[String], exclude_uuid: &str) -> SqlResult<Vec<CachedNote>> {
        if tags.is_empty() {
            return Ok(vec![]);
        }
        let conn = self.conn.lock().unwrap();
        let placeholders = (0..tags.len()).map(|i| format!("?{}", i + 3)).collect::<Vec<_>>().join(", ");
        let sql = format!(
            "SELECT {} FROM notes n
             WHERE n.account_id = ?1 AND n.uuid != ?2 AND n.sync_state != 'deleted_pending'
               AND EXISTS (SELECT 1 FROM note_tags t
                           WHERE t.account_id = n.account_id AND t.uuid = n.uuid AND t.tag IN ({placeholders}))
             ORDER BY {}",
            Self::NOTE_COLS,
            Self::NEWEST_FIRST
        );
        let mut binds: Vec<&dyn rusqlite::ToSql> = vec![&account_id, &exclude_uuid];
        for t in tags {
            binds.push(t);
        }
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(binds), row_to_note)?;
        rows.collect()
    }

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
                    n.push_blocked_reason, n.push_blocked_by_remote
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
    pub fn rename_tag(&self, account_id: &str, old_tag: &str, new_tag: &str, actor: &Actor) -> SqlResult<usize> {
        self.rewrite_tag_in_bodies(account_id, old_tag, Some(new_tag), actor)
    }

    /// Delete a tag across an account by stripping the `#` of `#tag` from
    /// every note's body, keeping the word (same body-as-truth reasoning as
    /// rename). Returns the count rewritten.
    pub fn delete_tag(&self, account_id: &str, tag: &str, actor: &Actor) -> SqlResult<usize> {
        self.rewrite_tag_in_bodies(account_id, tag, None, actor)
    }

    pub(crate) fn rewrite_tag_in_bodies(
        &self,
        account_id: &str,
        old_tag: &str,
        new_tag: Option<&str>,
        actor: &Actor,
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
            let prior_lv = local_version_conn(&tx, uuid, account_id)?;
            tx.execute(
                "UPDATE notes SET body_html = ?1,
                     local_version = local_version + 1,
                     sync_state = CASE sync_state WHEN 'clean' THEN 'dirty' ELSE sync_state END,
                     last_local_modified_at = ?2
                 WHERE uuid = ?3 AND account_id = ?4",
                params![new_body, now_ms(), uuid, account_id],
            )?;
            reconcile_tags_and_edges_from_body_conn(&tx, account_id, uuid, label, &new_body)?;
            reconcile_citations_from_body_conn(&tx, account_id, uuid, &new_body)?;
            fts_index_conn(&tx, uuid, account_id, title, &new_body)?;
            let lv: i64 = tx.query_row(
                "SELECT local_version FROM notes WHERE uuid = ?1 AND account_id = ?2",
                params![uuid, account_id],
                |r| r.get(0),
            )?;
            record_event_conn(&tx, account_id, uuid, EventKind::Edit, Some(actor), Some(lv), prior_lv)?;
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

/// Gotcha #38: on iCloud a tag is a hashtag span, never `#word` text.
#[cfg(test)]
mod tests_tag_model {
    use super::*;
    use crate::backend::TagModel;
    use tempfile::tempdir;

    const ICLOUD: &str = "icloud:someone@me.com";
    const GMAIL: &str = "gmail:someone@gmail.com";

    /// One real tag (the span Jodd renders for an `InlineAttachment`) and one
    /// `#word` in the text — exactly what Apple's Delete Tag / Convert to Text
    /// leaves behind (note "Tag lab 2026-10-11").
    const SPAN_AND_TEXT: &str = r#"<div>Tag lab</div><div><span data-jodd-inline="hashtag" data-ref="ia-1" contenteditable="false">#LabLower</span> #LabUpper</div>"#;

    fn span(record: &str, text: &str) -> String {
        format!(r#"<span data-jodd-inline="hashtag" data-ref="{record}" contenteditable="false">{text}</span>"#)
    }

    fn mk_note(account_id: &str, uuid: &str, body_html: &str) -> CachedNote {
        CachedNote {
            uuid: uuid.to_string(),
            account_id: account_id.to_string(),
            id: String::new(),
            title: "Tag lab".to_string(),
            body_html: body_html.to_string(),
            date: "Sun, 11 Oct 2026 10:00:00 +0700".to_string(),
            x_mail_created_date: None,
            label: "Notes".to_string(),
            local_version: 0,
            remote_version: Some("v1".to_string()),
            sync_state: SyncState::Clean,
            last_synced_at: None,
            last_local_modified_at: 0,
            last_remote_modified_at: None,
            pinned: false,
            meta_msg_id: None,
            pin_dirty: false,
            push_blocked_reason: None,
            push_blocked_by_remote: false,
        }
    }

    fn tagged_edges(db: &Db, account_id: &str, uuid: &str) -> Vec<String> {
        let conn = db.conn.lock().unwrap();
        let mut s = conn
            .prepare("SELECT dst_title FROM edges WHERE account_id=?1 AND src_uuid=?2 AND rel='tagged' ORDER BY dst_title")
            .unwrap();
        let it = s.query_map(params![account_id, uuid], |r| r.get::<_, String>(0)).unwrap();
        it.map(|x| x.unwrap()).collect()
    }

    fn sorted(mut v: Vec<String>) -> Vec<String> {
        v.sort();
        v
    }

    #[test]
    fn on_icloud_only_the_hashtag_span_is_a_tag() {
        assert_eq!(tags_from_body_for(TagModel::InlineObjects, SPAN_AND_TEXT), vec!["lablower"]);
        // The text rule, everywhere else, is unchanged: both read as tags.
        assert_eq!(tags_from_body_for(TagModel::BodyText, SPAN_AND_TEXT), vec!["lablower", "labupper"]);
    }

    /// A span IS one tag — Apple's `DisplayText` — so it is not re-tokenized
    /// with the `#word` rule, which would cut `#lab😀` to `lab` and drop
    /// `#123` for having no letter. Adjacent spans with no space between them
    /// stay two tags; the same tag twice is one.
    #[test]
    fn a_span_is_one_tag_whatever_its_characters() {
        let body = format!(
            "<div>{}{}{} {} {}</div>",
            span("a", "#lab😀"),
            span("b", "#123"),
            span("c", "#แท็กทดลอง"),
            span("d", "#LAB😀"),
            span("e", "#"),
        );
        assert_eq!(tags_from_body_for(TagModel::InlineObjects, &body), vec!["lab😀", "123", "แท็กทดลอง"]);
    }

    #[test]
    fn the_tag_model_comes_from_the_account_id() {
        assert_eq!(tags_for_account(ICLOUD, SPAN_AND_TEXT), vec!["lablower"]);
        assert_eq!(tags_for_account(GMAIL, SPAN_AND_TEXT), vec!["lablower", "labupper"]);
        assert_eq!(tags_for_account("microsoft:a@live.com", SPAN_AND_TEXT), vec!["lablower", "labupper"]);
        // A legacy bare id predates every backend but Gmail.
        assert_eq!(tags_for_account("someone@gmail.com", SPAN_AND_TEXT), vec!["lablower", "labupper"]);
    }

    #[test]
    fn a_pulled_icloud_note_gets_only_its_span_tags() {
        let db = Db::open_unencrypted(&tempdir().unwrap().keep()).unwrap();
        let uuid = "c7793f2b-c4c3-4b28-82e3-d2248bd4899b";
        db.upsert_from_remote(&mk_note(ICLOUD, uuid, SPAN_AND_TEXT), RemotePin::RemoteWins).unwrap();
        assert_eq!(db.list_tags_for(ICLOUD, uuid).unwrap(), vec!["lablower"]);
        assert_eq!(tagged_edges(&db, ICLOUD, uuid), vec!["lablower"]);

        db.upsert_from_remote(&mk_note(GMAIL, "G-1", SPAN_AND_TEXT), RemotePin::LocalWins).unwrap();
        assert_eq!(sorted(db.list_tags_for(GMAIL, "G-1").unwrap()), vec!["lablower", "labupper"]);
        assert_eq!(tagged_edges(&db, GMAIL, "G-1"), vec!["lablower", "labupper"]);
    }

    /// Migration 25 clears the rows the old text rule wrote on iCloud — the
    /// `#labupper` chip — and touches neither the body nor the sync state.
    #[test]
    fn migration_25_rederives_icloud_tags_without_touching_the_note() {
        let path = tempdir().unwrap().keep();
        let uuid = "c7793f2b-c4c3-4b28-82e3-d2248bd4899b";
        {
            let db = Db::open_unencrypted(&path).unwrap();
            db.upsert_from_remote(&mk_note(ICLOUD, uuid, SPAN_AND_TEXT), RemotePin::RemoteWins).unwrap();
            db.upsert_from_remote(&mk_note(GMAIL, "G-1", "<div>#work</div>"), RemotePin::LocalWins).unwrap();
            let conn = db.conn.lock().unwrap();
            conn.execute_batch(&format!(
                "INSERT OR IGNORE INTO note_tags (account_id, uuid, tag) VALUES ('{ICLOUD}', '{uuid}', 'labupper');
                 INSERT OR IGNORE INTO edges (account_id, src_uuid, dst_id, dst_title, rel)
                     VALUES ('{ICLOUD}', '{uuid}', '', 'labupper', 'tagged');
                 DELETE FROM migrations WHERE version = 25;"
            ))
            .unwrap();
        }
        let db = Db::open_unencrypted(&path).unwrap();
        assert_eq!(db.list_tags_for(ICLOUD, uuid).unwrap(), vec!["lablower"]);
        assert_eq!(tagged_edges(&db, ICLOUD, uuid), vec!["lablower"]);
        let n = db.get(uuid, ICLOUD).unwrap().unwrap();
        assert_eq!(n.body_html, SPAN_AND_TEXT, "the body is Apple's; a migration must not edit it");
        assert_eq!(n.sync_state, SyncState::Clean);
        assert_eq!(n.local_version, 0);
        assert_eq!(db.list_tags_for(GMAIL, "G-1").unwrap(), vec!["work"], "other backends are untouched");
    }

    /// One write derives a body's tags ONCE: `note_tags` and the `tagged`
    /// edges share the list. On iCloud each derivation is a full
    /// `parse_editor_html`, and it used to run twice per write — and twice per
    /// note in migration 25.
    #[test]
    fn a_write_derives_the_body_tags_once() {
        let db = Db::open_unencrypted(&tempdir().unwrap().keep()).unwrap();
        let uuid = "c7793f2b-c4c3-4b28-82e3-d2248bd4899b";

        let before = tag_derivations();
        db.upsert_from_remote(&mk_note(ICLOUD, uuid, SPAN_AND_TEXT), RemotePin::RemoteWins).unwrap();
        assert_eq!(tag_derivations() - before, 1, "upsert_from_remote");

        let before = tag_derivations();
        db.apply_local_edit(uuid, ICLOUD, "Tag lab", SPAN_AND_TEXT, "Notes", &crate::provenance::Actor::test())
            .unwrap();
        assert_eq!(tag_derivations() - before, 1, "apply_local_edit");

        db.upsert_from_remote(&mk_note(ICLOUD, "u-2", SPAN_AND_TEXT), RemotePin::RemoteWins).unwrap();
        let before = tag_derivations();
        migrate_rederive_inline_object_tags(&db.conn.lock().unwrap()).unwrap();
        assert_eq!(tag_derivations() - before, 2, "migration 25, two notes");

        // Same rows as before the split.
        assert_eq!(db.list_tags_for(ICLOUD, uuid).unwrap(), vec!["lablower"]);
        assert_eq!(tagged_edges(&db, ICLOUD, uuid), vec!["lablower"]);
    }

    /// `migrate_tags_to_body` "repairs" a tag missing from the body by
    /// APPENDING `#tag` text and marking the note dirty. On iCloud that text
    /// is not a tag, so the repair could never converge — it would write into
    /// Apple's note on every open. It must skip iCloud outright.
    #[test]
    fn migrate_tags_to_body_never_writes_into_an_icloud_note() {
        let path = tempdir().unwrap().keep();
        let uuid = "c7793f2b-c4c3-4b28-82e3-d2248bd4899b";
        {
            let db = Db::open_unencrypted(&path).unwrap();
            db.upsert_from_remote(&mk_note(ICLOUD, uuid, SPAN_AND_TEXT), RemotePin::RemoteWins).unwrap();
            db.conn.lock().unwrap()
                .execute(
                    "INSERT INTO note_tags (account_id, uuid, tag) VALUES (?1, ?2, 'ghost')",
                    params![ICLOUD, uuid],
                )
                .unwrap();
        }
        let db = Db::open_unencrypted(&path).unwrap();
        let n = db.get(uuid, ICLOUD).unwrap().unwrap();
        assert_eq!(n.body_html, SPAN_AND_TEXT);
        assert_eq!(n.sync_state, SyncState::Clean);
    }
}
