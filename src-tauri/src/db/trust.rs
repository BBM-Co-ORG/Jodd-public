//! Provenance and trust: note events, creators, `verify_note`, the
//! unreviewed list.
//! Split out of `db.rs` 2026-11-16.

use super::*;

impl Db {
    pub(crate) fn trust_for_conn(conn: &Connection, account_id: &str, uuids: &[String]) -> SqlResult<HashMap<String, Trust>> {
        let mut events = crate::provenance::events_for_conn(conn, account_id, uuids)?;
        let mut lvs: HashMap<String, i64> = HashMap::new();
        for chunk in uuids.chunks(500) {
            let marks = vec!["?"; chunk.len()].join(",");
            let mut stmt = conn.prepare(&format!(
                "SELECT uuid, local_version FROM notes WHERE account_id = ?1 AND uuid IN ({marks})"))?;
            let mut args: Vec<&dyn rusqlite::ToSql> = vec![&account_id];
            for u in chunk { args.push(u); }
            for r in stmt.query_map(args.as_slice(), |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
                let (u, lv) = r?;
                lvs.insert(u, lv);
            }
        }
        Ok(uuids.iter().map(|u| {
            let ev = events.remove(u).unwrap_or_default();
            (u.clone(), crate::provenance::derive(&ev, *lvs.get(u).unwrap_or(&0)))
        }).collect())
    }

    /// Every provenance event recorded for the account, for tests that must
    /// prove a failed run wrote nothing at all (not even an event).
    #[cfg(test)]
    pub(crate) fn note_event_count(&self, account_id: &str) -> usize {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM note_events WHERE account_id = ?1", params![account_id], |r| r.get::<_, i64>(0))
            .unwrap() as usize
    }

    /// uuid → actor string of that note's `create` event, for the uuids that
    /// have one. Studio reads it to recognise its own earlier notes
    /// (`jodd-studio-*`, spec §6); a note synced from elsewhere has no local
    /// events, so it is simply absent here.
    pub fn creators_for(&self, account_id: &str, uuids: &[String]) -> SqlResult<HashMap<String, String>> {
        let conn = self.conn.lock().unwrap();
        let events = crate::provenance::events_for_conn(&conn, account_id, uuids)?;
        Ok(events
            .into_iter()
            .filter_map(|(uuid, evs)| {
                let create = crate::provenance::EventKind::Create.as_str();
                evs.into_iter().find(|e| e.kind == create).and_then(|e| e.actor).map(|a| (uuid, a))
            })
            .collect())
    }

    /// Trust for every requested uuid (no events -> `NotRecorded`).
    pub fn trust_for(&self, account_id: &str, uuids: &[String]) -> SqlResult<HashMap<String, Trust>> {
        let conn = self.conn.lock().unwrap();
        Self::trust_for_conn(&conn, account_id, uuids)
    }

    /// Record a human review, unless agent/remote content arrived that the
    /// caller did not see. Spec 5.2 (as corrected by the plan's Task 1).
    pub fn verify_note(&self, account_id: &str, uuid: &str, seen_event_id: Option<i64>, seen_local_version: i64)
        -> SqlResult<VerifyOutcome> {
        let mut conn = self.conn.lock().unwrap();
        // IMMEDIATE: this reads before it writes, and a deferred tx cannot wait out busy_timeout on upgrade.
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // The chip may hold a pre-rekey uuid (gotcha #16): follow the alias first.
        let uuid = Self::resolve_note_uuid_conn(&tx, uuid, account_id)?;
        let uuid = uuid.as_str();
        let Some(lv) = local_version_conn(&tx, uuid, account_id)? else {
            // Displays as exactly this text (rusqlite formats the boxed error
            // verbatim), which the frontend shows; not "Query returned no rows".
            return Err(rusqlite::Error::ToSqlConversionFailure("Note not found".into()));
        };
        let events = crate::provenance::events_for_conn(&tx, account_id, &[uuid.to_string()])?
            .remove(uuid).unwrap_or_default();
        let now = crate::provenance::derive(&events, lv);
        // "Unseen" = non-human content after the last verify (what is pending)
        // that is newer than the event the chip showed, or that same event
        // coalesced forward since (coalescing keeps the id, advances local_version).
        // Before the last verify everything was already certified.
        let unseen_non_human = crate::provenance::after_last_verify(&events).iter().any(|e| {
            let unseen = match seen_event_id {
                None => true,
                Some(s) => e.id > s || (e.id == s && e.local_version.is_some_and(|v| v > seen_local_version)),
            };
            unseen && crate::provenance::non_human_content(e)
        });
        // Rule 2b proper, not `unrecorded_change`: a gap already pinned by an
        // `unrecorded` marker is covered by the id check above, and the user's
        // own later edits (which move `local_version`) must never make it stale.
        let gap = crate::provenance::unrecorded_gap(&events, lv);
        if unseen_non_human || (gap && lv != seen_local_version) {
            return Ok(VerifyOutcome::Stale(now));
        }
        record_event_conn(&tx, account_id, uuid, EventKind::Verify, Some(&Actor::Human), Some(lv), None)?;
        let after = Self::trust_for_conn(&tx, account_id, &[uuid.to_string()])?.remove(uuid)
            .expect("trust_for_conn answers every requested uuid");
        tx.commit()?;
        Ok(VerifyOutcome::Verified(after))
    }

    /// Notes whose content was changed by something other than the user and
    /// not since reviewed, outside the agent workspace. Newest unreviewed event first.
    pub fn list_unreviewed(&self, account_id: &str) -> SqlResult<Vec<(CachedNote, Trust)>> {
        let conn = self.conn.lock().unwrap();
        let notes: Vec<CachedNote> = Self::unreviewed_candidates_conn(&conn, account_id, Self::NOTE_COLS, row_to_note)?;
        let uuids: Vec<String> = notes.iter().map(|n| n.uuid.clone()).collect();
        let mut trust = Self::trust_for_conn(&conn, account_id, &uuids)?;
        let mut out: Vec<(CachedNote, Trust)> = notes.into_iter()
            .filter_map(|n| { let t = trust.remove(&n.uuid)?; (t.tier == Tier::Unreviewed).then_some((n, t)) })
            .collect();
        Self::sort_unreviewed(&mut out, |x| &x.1);
        Ok(out)
    }

    /// The same listing as `(uuid, trust)` without loading any note body (M8):
    /// `trust_for_conn` reads the `local_version` it needs itself.
    pub fn list_unreviewed_trust(&self, account_id: &str) -> SqlResult<Vec<(String, Trust)>> {
        let conn = self.conn.lock().unwrap();
        let uuids: Vec<String> = Self::unreviewed_candidates_conn(&conn, account_id, "n.uuid", |r| r.get(0))?;
        let mut trust = Self::trust_for_conn(&conn, account_id, &uuids)?;
        let mut out: Vec<(String, Trust)> = uuids.into_iter()
            .filter_map(|u| { let t = trust.remove(&u)?; (t.tier == Tier::Unreviewed).then_some((u, t)) })
            .collect();
        Self::sort_unreviewed(&mut out, |x| &x.1);
        Ok(out)
    }

    /// Notes that could be unreviewed: live, outside the agent workspace, with
    /// at least one event. `cols` is the SELECT list, `map` its row mapper.
    pub(crate) fn unreviewed_candidates_conn<T>(conn: &Connection, account_id: &str, cols: &str,
        map: impl FnMut(&rusqlite::Row<'_>) -> SqlResult<T>) -> SqlResult<Vec<T>> {
        let root = crate::mcp_scope::AGENT_WORKSPACE;
        let sql = format!(
            "SELECT {cols} FROM notes n
             WHERE n.account_id = ?1 AND n.sync_state != 'deleted_pending'
               AND n.label != ?2 AND substr(n.label, 1, length(?2) + 1) != ?2 || '/'
               AND EXISTS (SELECT 1 FROM note_events e WHERE e.account_id = n.account_id AND e.uuid = n.uuid)");
        // A literal prefix, as folder_scope::matches: LIKE would read the `_`
        // in `Notes/__Agent__` as a wildcard and ignore ASCII case.
        let rows = conn.prepare(&sql)?
            .query_map(params![account_id, root], map)?
            .collect::<SqlResult<_>>();
        rows
    }

    /// ids are monotonic: newest unreviewed event first; a bare rule-2b gap (no
    /// event to point at) sorts last.
    pub(crate) fn sort_unreviewed<T>(rows: &mut [T], trust: impl Fn(&T) -> &Trust) {
        rows.sort_by(|a, b| trust(b).event_id.cmp(&trust(a).event_id));
    }

    /// Test-only: one note's recorded events, sorted by `id`.
    #[cfg(test)]
    pub(crate) fn events_for_test(&self, account_id: &str, uuid: &str) -> Vec<crate::provenance::Event> {
        let conn = self.conn.lock().unwrap();
        crate::provenance::events_for_conn(&conn, account_id, &[uuid.to_string()]).unwrap().remove(uuid).unwrap_or_default()
    }
}



#[cfg(test)]
mod trust_api_tests {
    use super::*;
    use crate::provenance::{Actor, Tier};
    use crate::test_support::temp_db;

    fn n(uuid: &str, label: &str) -> CachedNote {
        crate::test_support::note("acct", uuid).title("T").body("<div>a</div>").label(label).build()
    }

    #[test]
    fn trust_for_answers_every_uuid() {
        let db = temp_db();
        db.insert_local_new(&n("A", "Notes"), &Actor::agent("cc", "1")).unwrap();
        let m = db.trust_for("acct", &["A".into(), "NONE".into()]).unwrap();
        assert_eq!(m["A"].tier, Tier::Unreviewed);
        assert_eq!(m["NONE"].tier, Tier::NotRecorded);
    }

    #[test]
    fn verify_accepts_a_seen_state_and_refuses_an_unseen_agent_edit() {
        let db = temp_db();
        let agent = Actor::agent("cc", "1");
        db.insert_local_new(&n("A", "Notes"), &agent).unwrap();
        let t = db.trust_for("acct", &["A".into()]).unwrap().remove("A").unwrap();
        // the agent edits after the chip rendered
        db.apply_local_edit("A", "acct", "T", "<div>b</div>", "Notes", &Actor::agent("other", "2")).unwrap();
        assert!(matches!(db.verify_note("acct", "A", t.event_id, t.note_local_version).unwrap(), VerifyOutcome::Stale(_)));
        let t2 = db.trust_for("acct", &["A".into()]).unwrap().remove("A").unwrap();
        assert!(matches!(db.verify_note("acct", "A", t2.event_id, t2.note_local_version).unwrap(), VerifyOutcome::Verified(ref v) if v.tier == Tier::HumanReviewed));
    }

    // The 2026-10-08 live pass: a stale refusal put the agent's event on the
    // chip while the editor still showed the older body. The editor now sends
    // the version it RENDERED with the chip's event id, and this is the refusal
    // that makes the click certify nothing it has not shown.
    #[test]
    fn a_verify_of_the_shown_event_from_an_older_rendered_version_is_stale() {
        let db = temp_db();
        db.insert_local_new(&n("A", "Notes"), &Actor::agent("cc", "1")).unwrap();
        let rendered = db.trust_for("acct", &["A".into()]).unwrap().remove("A").unwrap().note_local_version;
        db.apply_local_edit("A", "acct", "T", "<div>an appended line</div>", "Notes", &Actor::agent("other", "2")).unwrap();
        let chip = db.trust_for("acct", &["A".into()]).unwrap().remove("A").unwrap();
        assert!(chip.note_local_version > rendered, "precondition: the agent's write moved the version");
        assert!(matches!(db.verify_note("acct", "A", chip.event_id, rendered).unwrap(), VerifyOutcome::Stale(_)));
        assert!(matches!(db.verify_note("acct", "A", chip.event_id, chip.note_local_version).unwrap(), VerifyOutcome::Verified(_)),
            "once the editor renders that version the same event verifies");
    }

    #[test]
    fn a_coalesced_agent_edit_after_the_chip_rendered_is_stale() {
        let db = temp_db();
        let agent = Actor::agent("cc", "1");
        db.insert_local_new(&n("A", "Notes"), &agent).unwrap();
        db.apply_local_edit("A", "acct", "T", "<div>b</div>", "Notes", &agent).unwrap();
        let t = db.trust_for("acct", &["A".into()]).unwrap().remove("A").unwrap();
        // same agent, within the window: coalesced into the event the chip shows
        db.apply_local_edit("A", "acct", "T", "<div>c</div>", "Notes", &agent).unwrap();
        assert_eq!(db.trust_for("acct", &["A".into()]).unwrap()["A"].event_id, t.event_id, "precondition: coalesced, same id");
        assert!(matches!(db.verify_note("acct", "A", t.event_id, t.note_local_version).unwrap(), VerifyOutcome::Stale(_)));
    }

    #[test]
    fn the_users_own_edit_never_makes_a_verify_stale() {
        let db = temp_db();
        db.insert_local_new(&n("A", "Notes"), &Actor::agent("cc", "1")).unwrap();
        let t = db.trust_for("acct", &["A".into()]).unwrap().remove("A").unwrap();
        db.apply_local_edit("A", "acct", "T", "<div>fixed a typo</div>", "Notes", &Actor::Human).unwrap();
        assert!(matches!(db.verify_note("acct", "A", t.event_id, t.note_local_version).unwrap(), VerifyOutcome::Verified(_)));
    }

    #[test]
    fn a_remote_change_after_the_chip_rendered_is_stale() {
        let db = temp_db();
        db.insert_local_new(&n("A", "Notes"), &Actor::agent("cc", "1")).unwrap();
        let t = db.trust_for("acct", &["A".into()]).unwrap().remove("A").unwrap();
        let mut changed = n("A", "Notes");
        changed.body_html = "<div>from the iPhone</div>".into();
        db.upsert_from_remote(&changed, RemotePin::LocalWins).unwrap();
        assert!(matches!(db.verify_note("acct", "A", t.event_id, t.note_local_version).unwrap(), VerifyOutcome::Stale(_)));
    }

    #[test]
    fn a_second_remote_change_after_the_chip_rendered_is_stale() {
        let db = temp_db();
        db.insert_local_new(&n("A", "Notes"), &Actor::agent("cc", "1")).unwrap();
        let mut first = n("A", "Notes");
        first.body_html = "<div>from the iPhone</div>".into();
        db.upsert_from_remote(&first, RemotePin::LocalWins).unwrap();
        let t = db.trust_for("acct", &["A".into()]).unwrap().remove("A").unwrap();
        // a second iPhone edit inside the coalescing window: must not fold into the event the chip shows
        let mut second = n("A", "Notes");
        second.body_html = "<div>from the iPhone, again</div>".into();
        db.upsert_from_remote(&second, RemotePin::LocalWins).unwrap();
        assert!(matches!(db.verify_note("acct", "A", t.event_id, t.note_local_version).unwrap(), VerifyOutcome::Stale(_)));
    }

    #[test]
    fn an_unrecorded_change_can_be_verified_once_seen() {
        let db = temp_db();
        db.insert_local_new(&n("A", "Notes"), &Actor::Human).unwrap();
        db.bump_local_version("A", "acct").unwrap(); // a write with no event (rule 2b)
        let t = db.trust_for("acct", &["A".into()]).unwrap().remove("A").unwrap();
        assert!(t.unrecorded_change);
        db.bump_local_version("A", "acct").unwrap(); // another one the chip did not show
        assert!(matches!(db.verify_note("acct", "A", t.event_id, t.note_local_version).unwrap(), VerifyOutcome::Stale(_)));
        let t2 = db.trust_for("acct", &["A".into()]).unwrap().remove("A").unwrap();
        assert!(matches!(db.verify_note("acct", "A", t2.event_id, t2.note_local_version).unwrap(), VerifyOutcome::Verified(ref v) if v.tier == Tier::HumanReviewed),
            "verify records the current local_version, which clears rule 2b");
    }

    // I1 / R11: a stale jodd-mcp binary writes with no event, then the user moves the note.
    #[test]
    fn a_stale_binary_write_survives_a_later_human_move_until_verified() {
        let db = temp_db();
        db.insert_local_new(&n("A", "Notes"), &Actor::Human).unwrap();
        db.conn.lock().unwrap().execute(
            "UPDATE notes SET body_html = '<div>agent text</div>', local_version = local_version + 1 WHERE uuid = 'A'", []).unwrap();
        db.move_notes_batch("acct", &["A".into()], "Notes/Elsewhere", &Actor::Human).unwrap();
        let t = db.trust_for("acct", &["A".into()]).unwrap().remove("A").unwrap();
        assert_eq!(t.tier, Tier::Unreviewed, "a later recorded event must not erase the unrecorded write");
        assert!(t.unrecorded_change);
        // and the user's next edit does not erase it either
        db.apply_local_edit("A", "acct", "T", "<div>agent text, typo fixed</div>", "Notes/Elsewhere", &Actor::Human).unwrap();
        let t = db.trust_for("acct", &["A".into()]).unwrap().remove("A").unwrap();
        assert_eq!(t.tier, Tier::Unreviewed);
        assert!(matches!(db.verify_note("acct", "A", t.event_id, t.note_local_version).unwrap(),
            VerifyOutcome::Verified(ref v) if v.tier == Tier::HumanReviewed));
    }

    // A source's UNPINNED rule-2b gap: copied rows carry NULL versions and the
    // copy's own event records the copy's version, so without a marker the copy
    // would read as human_written over text nobody attributed.
    #[test]
    fn duplicating_a_note_with_an_unrecorded_gap_keeps_the_copy_unreviewed() {
        let db = temp_db();
        let save = |title: &str, dup: Option<&str>| crate::note_mutations::save_note_db(&db, "acct",
            crate::accounts::BackendKind::Gmail, None, title, "<div>a</div>", "Notes", None, None, &Actor::Human, dup).unwrap();
        let src = save("T", None);
        db.conn.lock().unwrap().execute(
            "UPDATE notes SET local_version = local_version + 1 WHERE uuid = ?1", params![src.uuid]).unwrap();
        let s = db.trust_for("acct", &[src.uuid.clone()]).unwrap().remove(&src.uuid).unwrap();
        assert_eq!(s.tier, Tier::Unreviewed, "precondition: the source has an unpinned gap");
        assert!(db.events_for_test("acct", &src.uuid).iter().all(|e| e.kind != "unrecorded"), "precondition: unpinned");

        let dup = save("T copy", Some(&src.uuid));
        let t = db.trust_for("acct", &[dup.uuid.clone()]).unwrap().remove(&dup.uuid).unwrap();
        assert_eq!(t.tier, Tier::Unreviewed, "Duplicate must not launder an unrecorded write into human_written");
        assert!(t.unrecorded_change, "the chip says 'changed without a record'");
        assert_eq!(t.unreviewed_by, None, "nobody is named for an unrecorded write");
        assert!(t.event_id.is_some(), "Mark reviewed has something to certify");
        let marker = db.events_for_test("acct", &dup.uuid).into_iter().find(|e| e.kind == "unrecorded")
            .expect("the copy carries a marker");
        assert_eq!((marker.actor, marker.local_version), (None, None), "the source's numbers do not belong to the copy");
        // the source's own history is untouched
        assert!(db.events_for_test("acct", &src.uuid).iter().all(|e| e.kind != "unrecorded"));
        assert!(matches!(db.verify_note("acct", &dup.uuid, t.event_id, t.note_local_version).unwrap(),
            VerifyOutcome::Verified(ref v) if v.tier == Tier::HumanReviewed));
    }

    #[test]
    fn unreviewed_list_skips_the_agent_workspace_and_human_notes() {
        let db = temp_db();
        let agent = Actor::agent("cc", "1");
        db.insert_local_new(&n("A", "Notes"), &agent).unwrap();
        db.insert_local_new(&n("W", &format!("{}/Projects", crate::mcp_scope::AGENT_WORKSPACE)), &agent).unwrap();
        db.insert_local_new(&n("R", crate::mcp_scope::AGENT_WORKSPACE), &agent).unwrap();
        // `_` is a LIKE wildcard: an unescaped `__Agent__` pattern would swallow this sibling.
        db.insert_local_new(&n("X", "Notes/xxAgentxx/Projects"), &agent).unwrap();
        // LIKE also ignores ASCII case; folder paths are case-sensitive (gotcha #33).
        db.insert_local_new(&n("L", "Notes/__agent__/Projects"), &agent).unwrap();
        db.insert_local_new(&n("H", "Notes"), &Actor::Human).unwrap();
        let mut uuids: Vec<String> = db.list_unreviewed("acct").unwrap().into_iter().map(|(n, _)| n.uuid).collect();
        // newest latest-content-event first
        assert_eq!(uuids, vec!["L".to_string(), "X".to_string(), "A".to_string()]);
        uuids.sort();
        assert!(!uuids.contains(&"W".to_string()) && !uuids.contains(&"R".to_string()));
    }

    // M7: the editor may still hold a pre-rekey uuid (gotcha #16).
    #[test]
    fn verify_follows_a_rekey_alias_and_names_a_missing_note() {
        let db = temp_db();
        db.insert_local_new(&n("OLD", "Notes"), &Actor::agent("cc", "1")).unwrap();
        db.rekey_note_uuid("OLD", "NEW", "acct").unwrap();
        let t = db.trust_for("acct", &["NEW".into()]).unwrap().remove("NEW").unwrap();
        assert!(matches!(db.verify_note("acct", "OLD", t.event_id, t.note_local_version).unwrap(),
            VerifyOutcome::Verified(ref v) if v.tier == Tier::HumanReviewed));
        assert_eq!(db.trust_for("acct", &["NEW".into()]).unwrap()["NEW"].tier, Tier::HumanReviewed);
        let err = db.verify_note("acct", "NOPE", None, 1).unwrap_err();
        assert_eq!(err.to_string(), "Note not found");
    }

    // M8: the trust-only listing answers the same notes without loading bodies.
    #[test]
    fn unreviewed_trust_listing_matches_the_note_listing() {
        let db = temp_db();
        db.insert_local_new(&n("A", "Notes"), &Actor::agent("cc", "1")).unwrap();
        db.insert_local_new(&n("B", "Notes"), &Actor::agent("cc", "1")).unwrap();
        db.insert_local_new(&n("H", "Notes"), &Actor::Human).unwrap();
        let full: Vec<(String, Trust)> = db.list_unreviewed("acct").unwrap().into_iter().map(|(n, t)| (n.uuid, t)).collect();
        assert_eq!(db.list_unreviewed_trust("acct").unwrap(), full);
        assert_eq!(full.iter().map(|(u, _)| u.as_str()).collect::<Vec<_>>(), vec!["B", "A"]);
    }

    #[test]
    fn unreviewed_list_skips_deleted_pending() {
        let db = temp_db();
        db.insert_local_new(&n("A", "Notes"), &Actor::agent("cc", "1")).unwrap();
        db.insert_local_new(&n("D", "Notes"), &Actor::agent("cc", "1")).unwrap();
        db.mark_deleted("D", "acct").unwrap();
        let uuids: Vec<String> = db.list_unreviewed("acct").unwrap().into_iter().map(|(n, _)| n.uuid).collect();
        assert_eq!(uuids, vec!["A".to_string()]);
    }
}
