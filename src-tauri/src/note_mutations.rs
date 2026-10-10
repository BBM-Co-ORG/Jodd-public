//! Extracted behavior; transport and transaction contracts are unchanged.
use super::*;

/// The SQLite-only core of `save_note`: resolve which row this edit belongs
/// to, apply it (or insert a brand-new note), and read the row back.
///
/// Split out of the command for the same reason `push_one_dirty_db` and
/// `reconcile_one_db` are (see their doc comments): `tauri::State` has no
/// public constructor outside a running Tauri app, so this is what makes the
/// save→push→save sequence testable at all — and that sequence, not the
/// wire, is where the duplicate-on-edit defect lived (gotcha #16).
///
/// Returns the row as it stands after the write, so the caller echoes the
/// note's CURRENT uuid back to the frontend rather than the one it sent.
/// That echo is the route back the UI needs after a rekey — see the
/// forwarding step below.
#[allow(clippy::too_many_arguments)]
pub(super) fn save_note_db(
    db: &db::Db,
    account_id: &str,
    // Which identity spelling this account's backend requires. NOT derivable
    // from `account_id` here: this function deliberately takes no `AppState`
    // (that is what makes the save -> push -> save sequence testable at all,
    // gotcha #16), so the caller — which already holds the account — passes
    // the policy in. See `backend::canonical_uuid_for`.
    backend_kind: accounts::BackendKind,
    existing_uuid: Option<&str>,
    title: &str,
    body_html: &str,
    label: &str,
    existing_x_mail_created_date: Option<&str>,
    expected_local_version: Option<i64>,
    actor: &Actor,
    // The note this save duplicates, if any (alias-resolved): its history is
    // copied onto the new row BEFORE the copy's own event, in the insert's own
    // transaction (`Db::insert_duplicate`, ruling R12).
    duplicate_of: Option<&str>,
) -> Result<db::CachedNote, String> {
    // Resolve the canonical UUID. tmp: prefixes from a fresh + click and
    // empty strings both mean "this is a brand-new note — generate one".
    let requested_uuid = match existing_uuid {
        // Was an unconditional uppercase-if-it-parses. That is right for
        // Apple's email backends and wrong for every backend that assigns its
        // own identity — a lowercase CloudKit `recordName` parses as a UUID
        // and would have been rewritten into an id naming nothing, on the
        // user's first edit. `canonical_uuid_for` makes the per-backend
        // policy explicit; Microsoft's pass-through stops being an accident
        // of `<…@…>` failing to parse.
        Some(u) if !u.is_empty() && !u.starts_with("tmp:") => {
            crate::backend::canonical_uuid_for(backend_kind, u)
        }
        // Per backend, for the same reason the arm above is: on iCloud this
        // uuid IS the `recordName` the create sends, and Apple's own clients
        // write those lowercase (`backend::mint_uuid_for`).
        _ => crate::backend::mint_uuid_for(backend_kind),
    };

    // Follow the rekey forwarding address BEFORE deciding "edit or insert".
    //
    // A backend that assigns identity at CREATE time (Exchange's
    // `internetMessageId`; Gmail when `resolve_uuid_for_save` mints a fresh
    // uuid) makes the worker change this row's PRIMARY KEY with no user
    // command behind it — gotcha #6's shape exactly. The editor keeps the
    // uuid it was last handed, so the very next autosave arrives addressed
    // to a uuid that no longer exists.
    //
    // Without this line that miss falls through to the `else` branch below
    // and inserts a SECOND row with an empty `id`, which the worker then
    // CREATEs remotely. Measured 2026-08-18 on kaiwan.h@live.com: one note
    // edited three times became three notes in Outlook and in Apple Notes,
    // one per push, each frozen at a different keystroke.
    //
    // A no-op (returns its input) whenever a live row exists under the uuid
    // as sent — which is every save on Gmail's steady state and every save
    // after the frontend has picked up the new uuid from this function's
    // return value. So the cost is one indexed lookup that the `db.get`
    // below would have done anyway.
    let real_uuid = db
        .resolve_note_uuid(&requested_uuid, account_id)
        .map_err(|e| e.to_string())?;
    if real_uuid != requested_uuid {
        log!(
            "save_note: uuid {} was rekeyed to {} by an earlier push — applying this edit to \
             the existing row rather than creating a duplicate",
            requested_uuid, real_uuid
        );
    }

    // Apply edit if the row already exists, otherwise insert new.
    let existing = db.get(&real_uuid, account_id).map_err(|e| e.to_string())?;
    if let Some(row) = existing {
        // An edit never moves the note: the row's own folder wins over the
        // caller's. Every caller here means "where it already is" — moves go
        // through `move_notes_batch` — so a different label is a stale copy:
        // the editor still holding the path from before a folder rename
        // (measured 2026-10-08), which wrote the old path back over
        // `rename_subtree`'s cascade and pushed the note out of its folder.
        if row.label != label {
            log!(
                "save_note: uuid={} sent folder {:?} but is in {:?} — keeping its folder",
                real_uuid, label, row.label
            );
        }
        let label = row.label.as_str();
        let applied = match expected_local_version {
            Some(expected) => db
                .apply_local_edit_versioned(&real_uuid, account_id, title, body_html, label, expected, actor)
                .map_err(|e| e.to_string())?,
            None => {
                db.apply_local_edit(&real_uuid, account_id, title, body_html, label, actor)
                    .map_err(|e| e.to_string())?;
                true
            }
        };
        if !applied {
            // A second local writer (another device's Jodd, or jodd-mcp) landed
            // an edit on this note after `expected_local_version` was captured
            // — a full-body replace here would silently discard that edit.
            // Surface it instead of retrying: unlike an append, there's
            // nothing safe to recompute automatically.
            //
            // The editor recognises this refusal by "changed elsewhere"
            // (`isVersionRefusal` in src/lib/editorRenderDecision.ts) and saves
            // its text as a conflict copy — keep that phrase if you reword it.
            return Err(
                "This note changed elsewhere while you were editing (another device or an MCP agent). \
                 Reload the note and re-apply your changes before saving again.".to_string()
            );
        }
    } else {
        let now = db::now_ms();
        let new_note = db::CachedNote {
            uuid: real_uuid.clone(),
            account_id: account_id.to_string(),
            id: String::new(), // no Gmail id yet — worker will fill it in
            title: title.to_string(),
            body_html: body_html.to_string(),
            // Frontend treats this date as "last modified" — set to now for a
            // new local note. The worker will overwrite with the real Date
            // header when Gmail confirms.
            date: chrono::Local::now().to_rfc2822(),
            x_mail_created_date: existing_x_mail_created_date.map(str::to_string),
            label: label.to_string(),
            local_version: 1,
            remote_version: None,
            sync_state: db::SyncState::Dirty,
            last_synced_at: None,
            last_local_modified_at: now,
            last_remote_modified_at: None,
            // New notes start unpinned with no sidecar yet. User toggles
            // via set_pin from the menu; the worker creates the sidecar.
            pinned: false,
            meta_msg_id: None,
            pin_dirty: false,
            // A brand-new note has never been pushed, so nothing can have
            // refused it yet (gotcha #14).
            push_blocked_reason: None,
            push_blocked_by_remote: false,
        };
        match duplicate_of {
            Some(src) => db.insert_duplicate(&new_note, actor, src),
            None => db.insert_local_new(&new_note, actor),
        }
        .map_err(|e| e.to_string())?;
    }

    // Read back the row so the response reflects current state (most
    // importantly: the cached `id` if any prior push has succeeded).
    db.get(&real_uuid, account_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "save_note: row vanished after write".to_string())
}

#[cfg(test)]
mod provenance_tests {
    use super::*;
    use crate::provenance::{Actor, Tier};
    use crate::test_support::temp_db;

    fn tier(db: &db::Db, uuid: &str) -> Tier {
        db.trust_for("acct", &[uuid.to_string()]).unwrap()[uuid].tier
    }

    #[test]
    fn duplicating_an_unreviewed_note_keeps_it_unreviewed() {
        let db = temp_db();
        let agent = Actor::agent("cc", "1");
        let src = save_note_db(&db, "acct", accounts::BackendKind::Gmail, None, "T", "<div>a</div>", "Notes", None, None, &agent, None).unwrap();
        let dup = save_note_db(&db, "acct", accounts::BackendKind::Gmail, None, "T copy", "<div>a</div>", "Notes", None, None, &Actor::Human, Some(&src.uuid)).unwrap();
        assert_eq!(tier(&db, &src.uuid), Tier::Unreviewed);
        assert_eq!(tier(&db, &dup.uuid), Tier::Unreviewed, "Duplicate must not launder agent content into human_written");
        // No gap on the source, so no marker on the copy: a spurious one would
        // keep the tier but turn the chip into "changed without a record" and
        // stop naming the agent.
        let t = db.trust_for("acct", &[dup.uuid.clone()]).unwrap().remove(&dup.uuid).unwrap();
        assert!(!t.unrecorded_change);
        assert_eq!(t.unreviewed_by.as_deref(), Some("cc/1"));
        assert!(db.events_for_test("acct", &dup.uuid).iter().all(|e| e.kind != "unrecorded"));
    }

    // The `unrecorded` marker on a copy is written only for a real rule-2b gap
    // on the source (`unrecorded_gap`), never for any unreviewed content: a
    // marker here would demote both of these copies to unreviewed.
    #[test]
    fn duplicating_a_human_written_note_stays_human_written() {
        let db = temp_db();
        let src = save_note_db(&db, "acct", accounts::BackendKind::Gmail, None, "T", "<div>a</div>", "Notes", None, None, &Actor::Human, None).unwrap();
        assert_eq!(tier(&db, &src.uuid), Tier::HumanWritten, "precondition");
        let dup = dup_of(&db, &src.uuid);
        assert_eq!(tier(&db, &dup.uuid), Tier::HumanWritten);
        assert!(db.events_for_test("acct", &dup.uuid).iter().all(|e| e.kind != "unrecorded"), "no gap, no marker");
    }

    #[test]
    fn duplicating_a_human_reviewed_note_stays_human_reviewed() {
        let db = temp_db();
        let src = save_note_db(&db, "acct", accounts::BackendKind::Gmail, None, "T", "<div>a</div>", "Notes", None, None, &Actor::agent("cc", "1"), None).unwrap();
        let t = db.trust_for("acct", &[src.uuid.clone()]).unwrap()[&src.uuid].clone();
        assert!(matches!(db.verify_note("acct", &src.uuid, t.event_id, t.note_local_version).unwrap(), db::VerifyOutcome::Verified(_)));
        assert_eq!(tier(&db, &src.uuid), Tier::HumanReviewed, "precondition");
        let dup = dup_of(&db, &src.uuid);
        assert_eq!(tier(&db, &dup.uuid), Tier::HumanReviewed);
        assert!(db.events_for_test("acct", &dup.uuid).iter().all(|e| e.kind != "unrecorded"), "no gap, no marker");
    }

    fn dup_of(db: &db::Db, src: &str) -> db::CachedNote {
        save_note_db(db, "acct", accounts::BackendKind::Gmail, None, "T copy", "<div>a</div>", "Notes", None, None, &Actor::Human, Some(src)).unwrap()
    }

    // I2 / R12: a source whose origin is unknown must not duplicate into human_written.
    #[test]
    fn duplicating_a_note_with_no_events_stays_not_recorded() {
        let db = temp_db();
        db.upsert_from_remote(&crate::test_support::note("acct", "PRE").build(), db::RemotePin::LocalWins).unwrap(); // pre-existing: no events
        assert_eq!(tier(&db, "PRE"), Tier::NotRecorded);
        let dup = dup_of(&db, "PRE");
        assert_eq!(tier(&db, &dup.uuid), Tier::NotRecorded, "Duplicate must not mint a human origin");
    }

    #[test]
    fn duplicating_a_rule_5_note_stays_not_recorded() {
        let db = temp_db();
        db.upsert_from_remote(&crate::test_support::note("acct", "PRE").build(), db::RemotePin::LocalWins).unwrap();
        db.apply_local_edit("PRE", "acct", "T", "<div>edited by me</div>", "Notes", &Actor::Human).unwrap();
        assert_eq!(tier(&db, "PRE"), Tier::NotRecorded, "precondition: rule 5");
        let dup = dup_of(&db, "PRE");
        assert_eq!(tier(&db, &dup.uuid), Tier::NotRecorded);
    }

    #[test]
    fn duplicating_through_a_pre_rekey_alias_copies_the_history() {
        let db = temp_db();
        let src = save_note_db(&db, "acct", accounts::BackendKind::Gmail, None, "T", "<div>a</div>", "Notes", None, None, &Actor::agent("cc", "1"), None).unwrap();
        db.rekey_note_uuid(&src.uuid, "REKEYED-UUID", "acct").unwrap();
        // the frontend still holds the old uuid (gotcha #16)
        let dup = dup_of(&db, &src.uuid);
        assert_eq!(tier(&db, &dup.uuid), Tier::Unreviewed, "the alias must resolve before the copy");
    }

    // M11: `save_note` with an `ai_result_id` records the workflow actor. The
    // command needs AppState, so this drives the two halves it composes: the
    // policy-registry lookup (`actor_for_result`) and `save_note_db`.
    #[test]
    fn a_save_carrying_a_live_ai_result_records_the_workflow_actor() {
        let rt = crate::llm::policy::Runtime::default();
        let wf = Actor::Workflow { name: "autolink".into(), model: "m".into() };
        let id = rt.issue_result("acct", serde_json::json!(1), wf);
        let db = temp_db();
        let save = |actor: &Actor| save_note_db(&db, "acct", accounts::BackendKind::Gmail, None, "T", "<div>Related: [[X]]</div>", "Notes", None, None, actor, None).unwrap();
        let last_actor = |uuid: &str| db.events_for_test("acct", uuid).last().unwrap().actor.clone();
        let n = save(&crate::actor_for_result(&rt, Some(&id)));
        assert_eq!(last_actor(&n.uuid).as_deref(), Some("jodd-autolink/m"));
        for none in [None, Some(""), Some("expired-or-forged")] {
            let n = save(&crate::actor_for_result(&rt, none));
            assert_eq!(last_actor(&n.uuid).as_deref(), Some("human:owner"), "{none:?} is the user");
        }
    }

    #[test]
    fn a_plain_human_save_is_human_written() {
        let db = temp_db();
        let n = save_note_db(&db, "acct", accounts::BackendKind::Gmail, None, "T", "<div>a</div>", "Notes", None, None, &Actor::Human, None).unwrap();
        assert_eq!(tier(&db, &n.uuid), Tier::HumanWritten);
    }
}

#[cfg(test)]
mod folder_tests {
    use super::*;
    use crate::provenance::Actor;
    use crate::test_support::temp_db;

    const OLD: &str = "Notes/BBMedia/Influencers/ลุงจืด";
    const NEW: &str = "Notes/BBMedia/Influencers/ลุงจืด (พี่จืด)";

    /// Measured live 2026-10-08: the folder holding the open note was renamed,
    /// and the editor's next autosave still carried the old path. The save
    /// wrote it back over the cascaded label, so the note reappeared in a
    /// folder of the old name and was pushed to a label that no longer
    /// existed. Save → rename → save, through `save_note_db` (gotcha #16).
    #[test]
    fn a_save_carrying_the_pre_rename_folder_keeps_the_renamed_one() {
        let db = temp_db();
        let n = save_note_db(&db, "acct", accounts::BackendKind::Gmail, None, "T", "<div>a</div>", OLD, None, None, &Actor::Human, None).unwrap();
        db.rename_subtree("acct", OLD, NEW).unwrap();

        let saved = save_note_db(&db, "acct", accounts::BackendKind::Gmail, Some(&n.uuid), "T", "<div>b</div>", OLD, None, Some(n.local_version), &Actor::Human, None).unwrap();
        assert_eq!(saved.label, NEW);
        assert_eq!(saved.body_html, "<div>b</div>", "the edit itself still lands");
    }

    /// Same, without a version guard — LessonExtract's auto-link save.
    #[test]
    fn an_unversioned_save_never_moves_the_note_either() {
        let db = temp_db();
        let n = save_note_db(&db, "acct", accounts::BackendKind::Gmail, None, "T", "<div>a</div>", NEW, None, None, &Actor::Human, None).unwrap();
        let saved = save_note_db(&db, "acct", accounts::BackendKind::Gmail, Some(&n.uuid), "T", "<div>b</div>", "Notes", None, None, &Actor::Human, None).unwrap();
        assert_eq!(saved.label, NEW);
    }

    /// A new note has no folder yet — the one the caller names is the folder.
    #[test]
    fn a_new_note_is_filed_where_the_caller_says() {
        let db = temp_db();
        let n = save_note_db(&db, "acct", accounts::BackendKind::Gmail, None, "T", "<div>a</div>", NEW, None, None, &Actor::Human, None).unwrap();
        assert_eq!(n.label, NEW);
    }
}
