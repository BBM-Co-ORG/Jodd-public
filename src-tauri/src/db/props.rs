//! Reads over the derived `note_props` table (spec
//! 2026-11-19-properties-tasks-views §5.2). The rows are written by
//! `reconcile_props_and_tasks_conn` (db.rs) on every note write, never here.

use super::*;
use crate::props::Prop;

/// How many of a key's values `property_vocabulary` offers.
const VOCABULARY_TOP_VALUES: usize = 10;

impl Db {
    /// One note's property block, in block order.
    pub fn props_for_note(&self, account_id: &str, uuid: &str) -> SqlResult<Vec<Prop>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT pos, key, key_display, value, value_date FROM note_props
             WHERE account_id = ?1 AND uuid = ?2 ORDER BY pos",
        )?;
        let rows = stmt.query_map(params![account_id, uuid], |r| {
            Ok(Prop {
                pos: r.get::<_, i64>(0)? as usize,
                key: r.get(1)?,
                key_display: r.get(2)?,
                value: r.get(3)?,
                value_date: r
                    .get::<_, Option<String>>(4)?
                    .and_then(|d| crate::props::parse_iso_date(&d)),
            })
        })?;
        rows.collect()
    }

    /// The account's property keys for the pickers: per key, its display
    /// spelling (the most used; ties go to the alphabetically first), how
    /// many notes carry it, and its most used values (by notes, then
    /// alphabetically, at most [`VOCABULARY_TOP_VALUES`]). Keys are ordered by
    /// notes, then display spelling. Views and notes waiting to be deleted
    /// are left out.
    pub fn property_vocabulary(&self, account_id: &str) -> SqlResult<Vec<(String, u32, Vec<String>)>> {
        let conn = self.conn.lock().unwrap();
        let live = format!(
            "SELECT p.uuid, p.key, p.key_display, p.value FROM note_props p
             JOIN notes n ON n.account_id = p.account_id AND n.uuid = p.uuid
             WHERE p.account_id = ?1 AND n.sync_state != 'deleted_pending' AND {NOT_A_VIEW_SQL}"
        );
        let display: HashMap<String, String> = {
            let mut stmt = conn.prepare(&format!(
                "SELECT key, key_display FROM ({live}) GROUP BY key, key_display
                 ORDER BY key, count(*) DESC, key_display"
            ))?;
            let rows = stmt.query_map(params![account_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut out = HashMap::new();
            for row in rows {
                let (key, shown) = row?;
                out.entry(key).or_insert(shown);
            }
            out
        };
        let mut values: HashMap<String, Vec<String>> = HashMap::new();
        {
            let mut stmt = conn.prepare(&format!(
                "SELECT key, value FROM ({live}) GROUP BY key, value
                 ORDER BY key, count(DISTINCT uuid) DESC, value"
            ))?;
            let rows = stmt.query_map(params![account_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            for row in rows {
                let (key, value) = row?;
                let top = values.entry(key).or_default();
                if top.len() < VOCABULARY_TOP_VALUES {
                    top.push(value);
                }
            }
        }
        let mut stmt = conn.prepare(&format!(
            "SELECT key, count(DISTINCT uuid) FROM ({live}) GROUP BY key"
        ))?;
        let rows = stmt.query_map(params![account_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (key, notes) = row?;
            let shown = display.get(&key).cloned().unwrap_or_else(|| key.clone());
            out.push((shown, notes as u32, values.remove(&key).unwrap_or_default()));
        }
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{backend_prefix, BackendKind, ALL_BACKENDS};
    use crate::provenance::Actor;
    use crate::test_support::{note, temp_db};

    fn props(db: &Db, account_id: &str, uuid: &str) -> Vec<(String, String, String, Option<String>)> {
        db.props_for_note(account_id, uuid)
            .unwrap()
            .into_iter()
            .map(|p| (p.key, p.key_display, p.value, p.value_date.map(|d| d.to_string())))
            .collect()
    }

    /// `(idx, text, done, due, depth)` in index order.
    fn tasks(db: &Db, account_id: &str, uuid: &str) -> Vec<(i64, String, i64, Option<String>, i64)> {
        let conn = db.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT idx, text, done, due, depth FROM task_items
                 WHERE account_id = ?1 AND uuid = ?2 ORDER BY idx",
            )
            .unwrap();
        let rows = stmt
            .query_map(params![account_id, uuid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            .unwrap()
            .collect::<SqlResult<Vec<_>>>()
            .unwrap();
        rows
    }

    fn cached(account_id: &str, uuid: &str, title: &str, body_html: &str) -> CachedNote {
        let mut n = note(account_id, uuid).build();
        n.title = title.to_string();
        n.body_html = body_html.to_string();
        n
    }

    const TITLE: &str = "Status: draft";
    /// The editor-view body of a note titled [`TITLE`]: a two-line block, a
    /// blank line, then a line that looks like a property but is not in the
    /// block.
    const EDITOR_BODY: &str =
        "<div>Status: active</div><div>Type: project</div><div><br></div><div>Later: not a property</div>";
    const TEXT: &str = "Status: draft\nStatus: active\nType: project\n\nLater: not a property";

    /// What each backend actually caches for a note whose wire form carries
    /// the title as its first line — every body below comes out of that
    /// backend's own decoder, fed the wire shape it really receives.
    fn cached_bodies() -> Vec<(BackendKind, &'static str, String)> {
        use crate::backend;
        let mut out = Vec::new();

        // Gmail — Jodd's own push (`inject_title_into_body`) and Apple's
        // `<div>{title}</div>` shape, both through the pull's
        // `strip_leading_title` (gmail/wire.rs).
        let jodd_wire = crate::mime822::inject_title_into_body(EDITOR_BODY, TITLE);
        out.push((BackendKind::Gmail, "jodd wire", crate::mime822::strip_leading_title(&jodd_wire, TITLE)));
        let apple_wire = format!(
            "<html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=utf-8\"></head>\
             <body style=\"word-wrap: break-word;\"><div>{TITLE}</div>{EDITOR_BODY}</body></html>"
        );
        out.push((BackendKind::Gmail, "apple wire", crate::mime822::strip_leading_title(&apple_wire, TITLE)));

        // LocalFs — the `.eml` Jodd writes, through `decode_eml`.
        let raw = crate::mime822::build_note_mime(
            TITLE,
            EDITOR_BODY,
            "AAAA-BBBB",
            "Thu, 4 Jun 2026 01:19:50 +0700",
            "Mon, 1 Jan 2024 09:00:00 +0700",
            "u@x.com",
            &[],
        );
        let d = backend::localfs::decode::decode_eml(raw.as_bytes(), "Notes").unwrap();
        out.push((BackendKind::LocalFs, "jodd eml", d.body_html));

        // Microsoft — Jodd's push (`inject_title_into_body_ms`) and Outlook's
        // plain-text-run title (gotcha #11), through `decode_messages`.
        let graph = |content: &str| {
            serde_json::json!({ "value": [{
                "id": "AQMk", "subject": TITLE,
                "internetMessageId": "<m1@example.com>", "parentFolderId": "AQMk",
                "createdDateTime": "2026-08-13T17:08:23Z",
                "lastModifiedDateTime": "2026-08-13T19:48:36Z",
                "body": { "contentType": "html", "content": content },
                "singleValueExtendedProperties": [{ "id": "String 0x0E05", "value": "Notes" }]
            }]})
            .to_string()
        };
        let ms = backend::microsoft::wire::inject_title_into_body_ms(TITLE, EDITOR_BODY);
        let m = backend::microsoft::wire::decode_messages(&graph(&ms)).unwrap();
        out.push((BackendKind::Microsoft, "jodd graph", m[0].body_html.clone()));
        let outlook = format!("<html><body>{TITLE}{EDITOR_BODY}</body></html>");
        let m = backend::microsoft::wire::decode_messages(&graph(&outlook)).unwrap();
        out.push((BackendKind::Microsoft, "outlook graph", m[0].body_html.clone()));

        // iCloud — the note's text, whose first line IS the title (gotcha
        // #21), through the renderer the pull uses (icloud/wire.rs).
        let html = backend::icloud::doc::note_body_html_formatted(TEXT, TITLE, &[], &HashMap::new());
        out.push((BackendKind::ICloud, "cloudkit text", html));

        // Ssh — the Markdown file Jodd writes (`encode`) and one typed by
        // hand, through `decode`.
        let (_, md) = backend::ssh::file::encode(&backend::ssh::frontmatter::Frontmatter::default(), TITLE, EDITOR_BODY);
        let d = backend::ssh::file::decode("Notes/n.md", md.as_bytes()).unwrap();
        out.push((BackendKind::Ssh, "jodd md", d.body_html));
        let hand = "# Status: draft\n\nStatus: active\nType: project\n\nLater: not a property\n";
        let d = backend::ssh::file::decode("Notes/h.md", hand.as_bytes()).unwrap();
        out.push((BackendKind::Ssh, "hand md", d.body_html));

        out
    }

    /// Spec §5.1: where the block starts depends on whether a backend's
    /// CACHED body still carries the title line. Measured here against each
    /// decoder: none does — every cache holds editor-view bodies (gotcha #27,
    /// `SavedNote.body_html`), so the block starts on the body's line 0
    /// everywhere and the title `Status: draft` is never a property.
    #[test]
    fn property_block_starts_after_title_per_backend() {
        let db = temp_db();
        let bodies = cached_bodies();
        for kind in ALL_BACKENDS {
            assert!(
                bodies.iter().any(|(k, ..)| *k == kind),
                "{kind:?} has no cached-body fixture: decide where its block starts"
            );
        }
        for (i, (kind, shape, body)) in bodies.iter().enumerate() {
            let account_id = format!("{}:a@b.com", backend_prefix(*kind));
            let uuid = format!("U{i}");
            db.upsert_from_remote(&cached(&account_id, &uuid, TITLE, body), RemotePin::LocalWins)
                .unwrap();
            assert_eq!(
                props(&db, &account_id, &uuid),
                vec![
                    ("status".into(), "Status".into(), "active".into(), None),
                    ("type".into(), "Type".into(), "project".into(), None),
                ],
                "{kind:?} ({shape}) cached body: {body}"
            );
        }
    }

    const ACCT: &str = "gmail:a@b.com";
    const WITH_BLOCK: &str = concat!(
        "<div>Status:&nbsp;active</div><div>Due: 2026-11-25</div><div><br></div>",
        r#"<div><input type="checkbox" contenteditable="false">&nbsp;ship 📅 2026-11-30</div>"#,
        r#"<div style="margin-left: 28px"><input type="checkbox" checked="" contenteditable="false">&nbsp;done thing</div>"#,
    );
    const WITHOUT: &str = "<div>just text</div><div>Status: below the first line</div>";

    fn assert_with_block(db: &Db, uuid: &str) {
        assert_eq!(
            props(db, ACCT, uuid),
            vec![
                ("status".into(), "Status".into(), "active".into(), None),
                ("due".into(), "Due".into(), "2026-11-25".into(), Some("2026-11-25".into())),
            ],
            "{uuid}"
        );
        assert_eq!(
            tasks(db, ACCT, uuid),
            vec![
                (0, "ship 📅 2026-11-30".into(), 0, Some("2026-11-30".into()), 0),
                (1, "done thing".into(), 1, None, 1),
            ],
            "{uuid}"
        );
    }

    fn assert_empty(db: &Db, uuid: &str) {
        assert!(props(db, ACCT, uuid).is_empty(), "{uuid}: props left behind");
        assert!(tasks(db, ACCT, uuid).is_empty(), "{uuid}: task rows left behind");
    }

    #[test]
    fn props_and_tasks_follow_every_write_path() {
        let db = temp_db();
        let actor = Actor::test();

        // insert_local_new
        db.insert_local_new(&cached(ACCT, "L", "t", WITH_BLOCK), &actor).unwrap();
        assert_with_block(&db, "L");

        // apply_local_edit: removing the block and the tasks empties both.
        db.apply_local_edit("L", ACCT, "t", WITHOUT, "Notes", &actor).unwrap();
        assert_empty(&db, "L");
        db.apply_local_edit("L", ACCT, "t", WITH_BLOCK, "Notes", &actor).unwrap();
        assert_with_block(&db, "L");

        // upsert_from_remote: a new row, then a pull that drops both.
        db.upsert_from_remote(&cached(ACCT, "R", "t", WITH_BLOCK), RemotePin::LocalWins).unwrap();
        assert_with_block(&db, "R");
        let mut gone = cached(ACCT, "R", "t", WITHOUT);
        gone.id = "msg-R2".into();
        db.upsert_from_remote(&gone, RemotePin::LocalWins).unwrap();
        assert_empty(&db, "R");

        // write_ingest: a new source note, an update to an existing one, the
        // derived note — all in one transaction.
        db.insert_local_new(&cached(ACCT, "S", "t", WITH_BLOCK), &actor).unwrap();
        let lv = db.get("S", ACCT).unwrap().unwrap().local_version;
        let upd = SourceUpdate {
            uuid: "S".into(),
            account_id: ACCT.into(),
            title: "t".into(),
            body_html: WITHOUT.into(),
            label: "Notes".into(),
            expected_local_version: lv,
        };
        let skipped = db
            .write_ingest(&[cached(ACCT, "N", "t", WITH_BLOCK)], &[upd], &cached(ACCT, "D", "t", WITH_BLOCK), &actor)
            .unwrap();
        assert!(skipped.is_empty());
        assert_with_block(&db, "N");
        assert_empty(&db, "S");
        assert_with_block(&db, "D");
    }

    /// Gotcha #16: a backend that assigns identity at CREATE rekeys the row,
    /// and every holder of the old uuid moves with it — props and tasks
    /// included, replacing anything already sitting at the new uuid.
    #[test]
    fn rekey_moves_props_and_tasks() {
        let db = temp_db();
        db.insert_local_new(&cached(ACCT, "U-OLD", "t", WITH_BLOCK), &Actor::test()).unwrap();
        assert_with_block(&db, "U-OLD");
        // Stale rows at the destination (the poll-vs-push race) must be replaced.
        {
            let conn = db.conn.lock().unwrap();
            conn.execute_batch(&format!(
                "INSERT INTO note_props (account_id, uuid, pos, key, key_display, value, value_date)
                     VALUES ('{ACCT}', 'U-NEW', 0, 'stale', 'Stale', 'x', NULL);
                 INSERT INTO task_items (account_id, uuid, idx, text, done, due, depth)
                     VALUES ('{ACCT}', 'U-NEW', 0, 'stale', 0, NULL, 0);"
            ))
            .unwrap();
        }
        db.rekey_note_uuid("U-OLD", "U-NEW", ACCT).unwrap();
        assert_with_block(&db, "U-NEW");
        assert_empty(&db, "U-OLD");
    }

    /// Deleting a note — locally, as a batch, or by pruning — drops its props
    /// and tasks with its tags.
    #[test]
    fn deleting_a_note_drops_its_props_and_tasks() {
        let db = temp_db();
        let actor = Actor::test();
        for u in ["D1", "D2", "D3", "D4"] {
            db.insert_local_new(&cached(ACCT, u, "t", WITH_BLOCK), &actor).unwrap();
            assert_with_block(&db, u);
        }
        db.delete("D1", ACCT).unwrap();
        assert_empty(&db, "D1");
        db.mark_deleted("D2", ACCT).unwrap();
        assert_empty(&db, "D2");
        db.delete_notes_batch(ACCT, &["D3".to_string()]).unwrap();
        assert_empty(&db, "D3");
        assert_with_block(&db, "D4");
        db.delete_account(ACCT).unwrap();
        assert_empty(&db, "D4");
    }

    /// Pin ruling R5, deliberate: a legacy Gmail row that still carries its
    /// title line as the first body line derives BOTH `Status:` lines. Such a
    /// line shows as ordinary body text in the editor too (gotcha #27), and
    /// the derivation has no title to compare the line against.
    #[test]
    fn a_gmail_body_that_still_carries_its_title_line_derives_both_status_lines() {
        let db = temp_db();
        let body = "<div>Status: draft</div><div>Status: active</div>";
        db.upsert_from_remote(&cached(ACCT, "LEG", "Status: draft", body), RemotePin::LocalWins).unwrap();
        assert_eq!(
            props(&db, ACCT, "LEG"),
            vec![
                ("status".into(), "Status".into(), "draft".into(), None),
                ("status".into(), "Status".into(), "active".into(), None),
            ]
        );
        let pos: Vec<i64> = db.props_for_note(ACCT, "LEG").unwrap().iter().map(|p| p.pos as i64).collect();
        assert_eq!(pos, vec![0, 1]);
    }

    /// Spec §5.1: the block starts on the line directly after the title, so
    /// a blank first body line means the note has no block.
    #[test]
    fn a_blank_first_line_means_no_block() {
        let db = temp_db();
        let body = "<div><br></div><div>Status: active</div>";
        db.insert_local_new(&cached(ACCT, "B", "t", body), &Actor::test()).unwrap();
        assert!(props(&db, ACCT, "B").is_empty());
    }

    #[test]
    fn property_vocabulary_counts_notes_and_skips_views() {
        let db = temp_db();
        let actor = Actor::test();
        let put = |uuid: &str, label: &str, body: &str| {
            let mut n = cached(ACCT, uuid, "t", body);
            n.label = label.into();
            db.insert_local_new(&n, &actor).unwrap();
        };
        put("a", "Notes", "<div>Status: active</div><div>Type: project</div>");
        put("b", "Notes/Work", "<div>status: active</div>");
        put("c", "Notes", "<div>Status: done</div><div>Status: active</div>");
        put("v", "Notes/__Views__", "<div>Show: notes</div><div>Status: active</div>");
        put("w", "Notes/Work/__Views__", "<div>Show: tasks</div>");
        // A folder merely NAMED like the views folder is not one — and `_`
        // is a LIKE wildcard, so `xxViewsyy` would match an unescaped pattern.
        put("x", "Notes/xxViewsyy", "<div>Kind: real</div>");
        put("other", "Notes", "<div>Status: active</div>");
        {
            // Another account's rows never leak in.
            let mut n = cached("gmail:z@b.com", "z", "t", "<div>Owner: z</div>");
            n.label = "Notes".into();
            db.insert_local_new(&n, &actor).unwrap();
        }

        let vocab = db.property_vocabulary(ACCT).unwrap();
        assert_eq!(
            vocab,
            vec![
                ("Status".to_string(), 4, vec!["active".to_string(), "done".to_string()]),
                ("Kind".to_string(), 1, vec!["real".to_string()]),
                ("Type".to_string(), 1, vec!["project".to_string()]),
            ]
        );
    }
}
