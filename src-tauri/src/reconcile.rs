//! Extracted behavior; transport and transaction contracts are unchanged.
use super::*;

// ─── Sync reconciliation ─────────────────────────────────────────────────────
//
// Called for each note that comes back from a Gmail fetch. Decides what to
// do based on the local sync_state AND whether the remote actually changed
// since we last saw it (by comparing remote_version vs the fetched
// backend::Note::version — NOT the fetched id; see that field's doc comment).
//
// See docs/DATA-HANDLING.md §8 (conflict handling) for the design.

/// Compute a short descriptor for the device that generated the local copy.
/// Used in conflict-copy titles so the user can tell which device the
/// remote version came from vs the one currently in front of them.
pub(super) fn device_label() -> String {
    let os = std::env::consts::OS;
    let pretty = match os {
        "macos" => "Mac",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    };
    pretty.to_string()
}

/// Has the remote copy moved since we last synced this row?
///
/// Split out of `reconcile_one` so it can be tested without a DB, an account
/// or a network. `None` (never synced) counts as changed.
///
/// An empty `fetched_version` ALSO counts as changed, unconditionally — even
/// against a cache that is itself `Some("")`. This is Microsoft-specific:
/// `RawMessage::last_modified_date_time` is `#[serde(default)]`, so a message
/// missing that field decodes to `""` and `to_note` stores it verbatim (see
/// that field's doc comment for why it is NOT run through the same fallback
/// `date` gets). Without this guard such a note would carry `version: ""`
/// forever, `remote_changed` would report `false` on every poll, and an
/// Apple-side edit would be silently overwritten with no conflict copy —
/// which is the exact failure this function exists to prevent, re-entering
/// through a side door. Failing toward "changed" here costs a conflict copy
/// (both sides preserved, the user reconciles); failing toward "unchanged"
/// costs a silently dropped edit. Only the first is acceptable.
pub(super) fn remote_changed(cached_version: Option<&str>, fetched_version: &str) -> bool {
    if fetched_version.is_empty() {
        return true;
    }
    cached_version != Some(fetched_version)
}

/// Whether a fetched note whose version moved holds the same content as the
/// one `remote_version` named — the content a pending local edit was based on.
///
/// Only answerable where `notes.remote_base_fp` is kept
/// (`backend::version_moves_without_content`), and only once a pull or a push
/// has stamped it: NULL is "unknown", which answers `false` and keeps the
/// keep-both copy. Failing toward a copy costs the user a duplicate to delete;
/// failing the other way would push an edit over another device's.
fn only_the_version_moved(db: &db::Db, account_id: &str, uuid: &str, fetched: &db::CachedNote) -> bool {
    let Some(fetched_fp) = db::remote_base_fp_for(account_id, &fetched.title, &fetched.body_html, &fetched.label)
    else {
        return false;
    };
    matches!(db.remote_base_fp(uuid, account_id), Ok(Some(base)) if base == fetched_fp)
}

/// Reconcile a single fetched note against the cache. Implements the full
/// Phase 4 decision table. Takes the AppState so it can check whether our
/// own sync worker is mid-push for this uuid (which would make a
/// "remote changed" observation a false alarm).
pub(super) fn reconcile_one(state: &State<'_, AppState>, account_id: &str, fetched: &gmail::Note) {
    // If our own worker is in the middle of pushing this uuid, the fetched
    // id likely reflects our own in-flight insert — not someone else's
    // edit. Skip reconcile entirely; we'll process this row again on the
    // next list_notes after mark_pushed has updated the cache.
    {
        let pushing = state.pushing.lock().unwrap();
        if pushing.contains(&(account_id.to_string(), fetched.uuid.clone())) {
            return;
        }
    }
    // Whether this backend's pin is authoritative is a per-backend fact, and
    // the account list is the only place that knows which backend this is.
    // Decided here so `reconcile_one_db` stays a pure DB-level function.
    let pin = account_backend_kind(state, account_id)
        .map(db::remote_pin_policy)
        // An account that vanished mid-reconcile: keep the local value. The
        // conservative half — a pin that fails to arrive is a missing feature,
        // a pin overwritten by a guess is lost user intent.
        .unwrap_or(db::RemotePin::LocalWins);
    // Minting is a per-backend policy for the same reason the pin's arrival is
    // (`backend::mint_uuid_for`), and a conflict copy is a brand-new note the
    // worker will CREATE remotely — on iCloud this string becomes the record's
    // own name. Decided here, where the backend is known, rather than inside a
    // function that has no business knowing about backends. `LocalFs` is the
    // conservative fallback for an account that vanished mid-reconcile: it is
    // Apple's own wire form, which is what every backend but iCloud wants, and
    // an iCloud account that has just disappeared has nothing to push to.
    let conflict_uuid = crate::backend::mint_uuid_for(
        account_backend_kind(state, account_id).unwrap_or(accounts::BackendKind::LocalFs),
    );
    reconcile_one_db(&state.db, account_id, fetched, pin, &conflict_uuid);
}

/// The DB-only body of `reconcile_one` — everything after the in-flight-push
/// check above. Split out so it can be exercised in tests against a plain
/// `db::Db`: `tauri::State` has no public constructor outside a running
/// Tauri app, so nothing that needs it can be unit-tested directly (which is
/// exactly why the regression coverage for the `mark_pushed`/`SavedNote`
/// version-mismatch bug — `localfs_push_reconcile_tests` below — calls this,
/// not `reconcile_one`).
pub(super) fn reconcile_one_db(
    db: &db::Db,
    account_id: &str,
    fetched: &gmail::Note,
    pin: db::RemotePin,
    // The uuid a conflict copy would take, decided by the caller because it
    // is a per-backend policy — see `backend::mint_uuid_for`. Passed in rather
    // than minted here for the same reason `pin` is, with the useful side
    // effect that a test can pin the conflict copy's identity.
    conflict_uuid: &str,
) {
    let cached = db::CachedNote::from_remote(account_id, fetched);

    // Persist any inline attachments (images) so the save path can re-emit them
    // instead of stripping them on re-save. Upsert-only (never deletes on read);
    // runs regardless of the reconcile decision below, since the bytes are the
    // canonical Gmail-side content keyed by stable cid. Errors are logged, not
    // fatal — attachment capture must never break note sync.
    for att in &fetched.attachments {
        if let Err(e) = db.upsert_attachment(account_id, &cached.uuid, att) {
            log!(
                "reconcile_one: upsert_attachment failed uuid={} cid={}: {}",
                cached.uuid, att.content_id, e
            );
        }
    }

    let existing = match db.get(&cached.uuid, account_id) {
        Ok(x) => x,
        Err(e) => {
            log!("reconcile_one: db.get failed for {}: {}", cached.uuid, e);
            return;
        }
    };

    let Some(existing) = existing else {
        // No row → insert fresh. The note is new to us.
        if let Err(e) = db.upsert_from_remote(&cached, pin) {
            log!("reconcile_one: insert failed for {}: {}", cached.uuid, e);
        }
        return;
    };

    use db::SyncState::*;
    let remote_changed = remote_changed(existing.remote_version.as_deref(), &fetched.version);

    match existing.sync_state {
        // User wants this gone — don't resurrect by pulling.
        DeletedPending => {}
        // Already flagged — don't keep re-creating duplicate "conflict copy"
        // rows on every poll. The user has to resolve manually.
        Conflict => {}
        // Jodd's own CREATE, whose answer was lost (gotcha #39): the row was
        // sent but never learned its id, so this message is the copy that
        // landed, not another device's edit. Adopt it; the pending edit then
        // pushes over it. Read as a conflict, it forked a "(conflict from …)"
        // note the worker CREATEd — the duplicate in another form.
        Dirty if existing.id.is_empty() && db.create_attempted(&existing.uuid, account_id).unwrap_or(false) => {
            match db.adopt_landed_create(
                &existing.uuid, account_id, &fetched.id, &fetched.version,
                fetched.x_mail_created_date.as_deref(),
            ) {
                Ok(_) => log!(
                    "reconcile_one: uuid={} {} is the copy an earlier create left — adopted, \
                     keeping the local edit dirty",
                    existing.uuid, fetched.id
                ),
                Err(e) => log!("reconcile_one: adopt_landed_create failed for {}: {}", existing.uuid, e),
            }
        }
        // Local has unpushed edits and the remote's version moved — but the
        // content those edits were based on did not: Apple's own tag bump
        // (gotcha #29), not another device's edit. Adopt the version and let
        // the worker push the edit; the push gate (`icloud_content_differs`)
        // makes the same call on its side.
        Dirty if remote_changed && only_the_version_moved(db, account_id, &existing.uuid, &cached) => {
            match db.adopt_remote_version(&existing.uuid, account_id, &fetched.version) {
                Ok(_) => log!(
                    "reconcile_one: uuid={} version {} → {} with its content unchanged — not a \
                     conflict, keeping the local edit dirty",
                    existing.uuid,
                    existing.remote_version.as_deref().unwrap_or("<none>"),
                    fetched.version
                ),
                Err(e) => log!("reconcile_one: adopt_remote_version failed for {}: {}", existing.uuid, e),
            }
        }
        // Local has unpushed edits. The interesting case.
        Dirty => {
            if remote_changed {
                // CONFLICT detected. The "keep-both" rule, refined per design:
                //
                // The PRIMARY note (uuid=X) converges to the REMOTE state —
                // so all replicas agree on uuid=X's content. The LOCAL
                // content (the one that was about to be overwritten) is
                // preserved as a new conflict-copy note with a fresh uuid.
                //
                // Earlier version of this code did the opposite (kept local
                // on the primary, remote in the copy) but that produced an
                // asymmetry: Apple Notes/Gmail had remote content under
                // uuid=X, Jodd had local content under uuid=X — same
                // identity, different content across replicas. Confusing.
                //
                // Now both replicas show the same picture: primary has
                // remote, conflict-copy has the "device's earlier version".
                // A conflict copy is a brand-new note that the worker will
                // CREATE remotely, so it is minted in the backend's own shape
                // — not Apple's email form on every backend.
                let new_uuid = conflict_uuid.to_string();
                let date_str = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
                let suffix = format!(" (conflict from {} {})", device_label(), date_str);
                let dup_title = format!("{}{}", existing.title, suffix);
                let now = db::now_ms();
                // A copy is a CREATE, and a CREATE cannot carry Apple's tag
                // objects on iCloud — they go up as plain `#tag` text. Hold
                // that shape locally from the start, or the copy shows tags
                // that vanish on its next pull (gotcha #38). No wildcard arm.
                let tag_model = db::tag_model_for_account(account_id);
                let dup_body = match tag_model {
                    crate::backend::TagModel::BodyText => existing.body_html.clone(),
                    crate::backend::TagModel::InlineObjects => {
                        crate::backend::icloud::format_html::flatten_hashtag_spans(&existing.body_html)
                    }
                };
                let dup = db::CachedNote {
                    uuid: new_uuid,
                    account_id: account_id.to_string(),
                    id: String::new(),
                    title: dup_title,
                    body_html: dup_body,                       // LOCAL preserved
                    date: existing.date.clone(),
                    x_mail_created_date: existing.x_mail_created_date.clone(),
                    label: existing.label.clone(),
                    local_version: 1,
                    remote_version: None,
                    sync_state: Dirty,
                    last_synced_at: None,
                    last_local_modified_at: now,
                    last_remote_modified_at: None,
                    // Conflict copies start unpinned regardless of the
                    // primary's pin state — the user is being asked to
                    // pick between two versions, and surfacing the copy
                    // at the top of the list would be misleading. They
                    // can pin the survivor after resolving. No sidecar
                    // until the user explicitly pins the copy.
                    pinned: false,
                    meta_msg_id: None,
                    pin_dirty: false,
                    push_blocked_reason: None,
                    push_blocked_by_remote: false,
                };
                if let Err(e) = db.insert_local_new(&dup, &Actor::Process("conflict-copy")) {
                    log!("reconcile_one: insert conflict-copy failed for {}: {}",
                         existing.uuid, e);
                    return;
                }
                // Conflict-copy inherits the primary's tags. Without this the
                // copy starts untagged and whichever side the user picks (by
                // deleting the other) costs them their tag state on that note.
                // Best-effort: a copy failure isn't worth aborting the whole
                // reconcile, but log so we notice if it's a recurring problem.
                // Not on iCloud: the copy's tags are text there, so copying
                // the primary's rows would put back the tags flattened above.
                let inherit_tags = match tag_model {
                    crate::backend::TagModel::BodyText => true,
                    crate::backend::TagModel::InlineObjects => false,
                };
                if inherit_tags {
                    if let Err(e) = db.copy_tags(account_id, &existing.uuid, &dup.uuid) {
                        log!("reconcile_one: copy_tags to conflict-copy failed for {} → {}: {}",
                             existing.uuid, dup.uuid, e);
                    }
                }
                // Now accept remote into the primary. upsert_from_remote
                // sets sync_state = clean, so the worker won't push the
                // (now-irrelevant) local content under uuid=X. The local
                // content survives in `dup` which the worker WILL push.
                if let Err(e) = db.upsert_from_remote(&cached, pin) {
                    log!("reconcile_one: apply remote on conflict failed for {}: {}",
                         cached.uuid, e);
                } else {
                    log!(
                        "reconcile_one: CONFLICT on uuid={} — saved local content as duplicate uuid={} (\"{}\"), accepted remote into primary",
                        existing.uuid, dup.uuid, dup.title
                    );
                    // The primary's CONTENT was just replaced, but
                    // `upsert_from_remote` leaves `local_version` alone — it
                    // counts local writes. An editor still open on this note
                    // holds text based on what was replaced, and its next save
                    // carries the version it rendered from; without this bump
                    // that version still matches, the save lands on the
                    // primary, and the remote edit is overwritten remotely.
                    // Only this branch: an ordinary pull replaces nothing a
                    // local write was pending on.
                    if let Err(e) = db.bump_local_version(&cached.uuid, account_id) {
                        log!("reconcile_one: bump_local_version after conflict failed for {}: {}",
                             cached.uuid, e);
                    }
                }
            }
            // remote unchanged → keep dirty, worker will push our edits.
        }
        // No pending local intent. Apply remote.
        Clean | PullNeeded => {
            if let Err(e) = db.upsert_from_remote(&cached, pin) {
                log!("reconcile_one: upsert failed for {}: {}", cached.uuid, e);
            }
        }
    }
}

/// Gotcha #29 reaching the POLL, not the push (measured live 2026-10-12,
/// `icloud:kaiwan@me.com`). Apple bumps a freshly created note's
/// `recordChangeTag` with no client write in between (`eyv` → `eyw`); a zone
/// walk caches the bumped tag while SQLite still holds the create's; the user
/// edits; the folder listing reconciles from the cached walk and finds a Dirty
/// row whose `remote_version` differs from the fetched tag — which used to be
/// read as a concurrent edit and mint a "(conflict from …)" copy of a note
/// nobody else had touched.
#[cfg(test)]
mod icloud_tag_only_bump_tests {
    use super::*;

    const ICLOUD: &str = "icloud:someone@me.com";
    const UUID: &str = "abb404cf-136c-4e80-b430-9f558e76d87b";
    const DATE: &str = "Mon, 12 Oct 2026 00:44:11 +0700";

    fn temp_db() -> db::Db {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::mem::forget(dir);
        db::Db::open_unencrypted(&path).expect("open temp db")
    }

    /// What the zone walk hands `reconcile_one`: a note at `tag`.
    fn walked(title: &str, body_html: &str, tag: &str, label: &str) -> gmail::Note {
        serde_json::from_value(serde_json::json!({
            "id": UUID, "uuid": UUID, "title": title, "body_html": body_html,
            "date": DATE, "version": tag, "label": label,
        }))
        .unwrap()
    }

    /// `save_note` of a new note, then `push_one_dirty`'s create landing at
    /// `tag` — the 00:44:05 → 00:44:12 half of the live log.
    fn created(db: &db::Db, title: &str, body_html: &str, tag: &str) {
        let mut row = db::CachedNote::from_remote(ICLOUD, &walked(title, body_html, "", "Notes/Jodd.M3.test"));
        row.id = String::new();
        row.remote_version = None;
        row.sync_state = db::SyncState::Dirty;
        db.insert_local_new(&row, &Actor::test()).unwrap();
        let lv = db.get(UUID, ICLOUD).unwrap().unwrap().local_version;
        db.mark_pushed(UUID, ICLOUD, UUID, tag, DATE, body_html, lv).unwrap();
    }

    fn rows(db: &db::Db) -> Vec<String> {
        db.list_notes(ICLOUD).unwrap().into_iter().map(|r| r.title).collect()
    }

    /// The live sequence, step for step: create at `eyv`, a clean walk at
    /// `eyv`, Apple's bump to `eyw` with the content untouched, a local edit,
    /// then the folder listing reconciling the bumped walk.
    #[test]
    fn a_tag_only_bump_under_a_local_edit_is_not_a_conflict() {
        let db = temp_db();
        let pin = db::RemotePin::RemoteWins;
        let base = "<div>Existing body</div>";
        created(&db, "New Note", base, "eyv");
        reconcile_one_db(&db, ICLOUD, &walked("New Note", base, "eyv", "Notes/Jodd.M3.test"), pin, "unused");

        db.apply_local_edit(UUID, ICLOUD, "Append format lab", "<div>Existing body, edited</div>",
            "Notes/Jodd.M3.test", &Actor::test()).unwrap();

        reconcile_one_db(&db, ICLOUD, &walked("New Note", base, "eyw", "Notes/Jodd.M3.test"), pin, "copy-1");

        assert!(db.get("copy-1", ICLOUD).unwrap().is_none(), "no conflict copy: {:?}", rows(&db));
        let row = db.get(UUID, ICLOUD).unwrap().unwrap();
        assert_eq!(row.sync_state, db::SyncState::Dirty, "the local edit is still to be pushed");
        assert_eq!(row.title, "Append format lab");
        assert!(row.body_html.contains("edited"), "the local edit survives: {}", row.body_html);
        assert_eq!(row.remote_version.as_deref(), Some("eyw"), "the bumped tag is adopted");
    }

    /// The other half of the live pair, and gotcha #29's own case: the edit
    /// comes before any walk has stamped the base, so all the poll has is what
    /// the PUSH recorded — editor HTML — against what a walk renders of the
    /// document that push wrote. Rendered here through the real compose and
    /// render functions, not a second hand-written string that agrees by
    /// construction.
    #[test]
    fn a_tag_only_bump_is_recognised_against_the_base_the_create_push_recorded() {
        use crate::backend::icloud::{compose, doc, format_html, wire};
        let db = temp_db();
        let pin = db::RemotePin::RemoteWins;
        let (title, editor_body) = ("Append format lab 2026-10-12", "<div>First line</div><div><br></div><div>Second line</div>");
        created(&db, title, editor_body, "eyx");

        // What the create put in the document, and what the walk shows of it.
        let text = compose::compose_new(title, &format_html::parse_editor_html_objects_as_text(editor_body).text);
        let title_field = wire::derive_note_title(&text);
        let rendered_title = doc::note_title(&text, title_field);
        let rendered_body = doc::note_body_html_formatted(&text, title_field, &[], &Default::default());

        db.apply_local_edit(UUID, ICLOUD, title, "<div>First line</div><div><br></div><div>Second line, appended</div>",
            "Notes/Jodd.M3.test", &Actor::test()).unwrap();
        reconcile_one_db(&db, ICLOUD, &walked(&rendered_title, &rendered_body, "eyy", "Notes/Jodd.M3.test"), pin, "copy-1");

        assert!(db.get("copy-1", ICLOUD).unwrap().is_none(),
            "no conflict copy — rendered {rendered_title:?} / {rendered_body:?}: {:?}", rows(&db));
        let row = db.get(UUID, ICLOUD).unwrap().unwrap();
        assert_eq!(row.sync_state, db::SyncState::Dirty);
        assert!(row.body_html.contains("appended"), "{}", row.body_html);
    }

    /// The guard this must not weaken: another device really did edit the
    /// note. Keep-both, exactly as before.
    #[test]
    fn a_moved_tag_with_different_content_is_still_a_conflict() {
        let db = temp_db();
        let pin = db::RemotePin::RemoteWins;
        created(&db, "T", "<div>base</div>", "eyv");
        db.apply_local_edit(UUID, ICLOUD, "T", "<div>base, mine</div>", "Notes/Jodd.M3.test", &Actor::test()).unwrap();

        reconcile_one_db(&db, ICLOUD, &walked("T", "<div>base, theirs</div>", "eyw", "Notes/Jodd.M3.test"), pin, "copy-1");

        let copy = db.get("copy-1", ICLOUD).unwrap().expect("keep-both copy");
        assert!(copy.body_html.contains("mine"), "{}", copy.body_html);
        let primary = db.get(UUID, ICLOUD).unwrap().unwrap();
        assert!(primary.body_html.contains("theirs"), "{}", primary.body_html);
        assert_eq!(primary.sync_state, db::SyncState::Clean);
    }

    /// A move on another device leaves the text alone and moves the tag. The
    /// push would build against the new folder and file the note back under
    /// the local label, undoing the move unseen — so a move is a change.
    #[test]
    fn a_moved_tag_with_the_note_in_another_folder_is_still_a_conflict() {
        let db = temp_db();
        let pin = db::RemotePin::RemoteWins;
        created(&db, "T", "<div>base</div>", "eyv");
        db.apply_local_edit(UUID, ICLOUD, "T", "<div>base, mine</div>", "Notes/Jodd.M3.test", &Actor::test()).unwrap();

        reconcile_one_db(&db, ICLOUD, &walked("T", "<div>base</div>", "eyw", "Notes/Elsewhere"), pin, "copy-1");

        assert!(db.get("copy-1", ICLOUD).unwrap().is_some(), "keep-both: {:?}", rows(&db));
    }

    /// A row stamped before migration 26 has no fingerprint, and "unknown" is
    /// not "unchanged".
    #[test]
    fn a_base_with_no_fingerprint_keeps_both() {
        let db = temp_db();
        let pin = db::RemotePin::RemoteWins;
        // A synced row carrying a version but no fingerprint — the shape every
        // row has between migration 26 and its next pull or push.
        let mut row = db::CachedNote::from_remote(ICLOUD, &walked("T", "<div>base</div>", "eyv", "Notes/Jodd.M3.test"));
        row.sync_state = db::SyncState::Clean;
        db.insert_local_new(&row, &Actor::test()).unwrap();
        assert_eq!(db.remote_base_fp(UUID, ICLOUD).unwrap(), None, "precondition");
        db.apply_local_edit(UUID, ICLOUD, "T", "<div>base, mine</div>", "Notes/Jodd.M3.test", &Actor::test()).unwrap();

        reconcile_one_db(&db, ICLOUD, &walked("T", "<div>base</div>", "eyw", "Notes/Jodd.M3.test"), pin, "copy-1");

        assert!(db.get("copy-1", ICLOUD).unwrap().is_some(), "keep-both: {:?}", rows(&db));
    }

    /// Gmail's version is the message id the next push trashes; adopting a new
    /// one without re-addressing the row would strand the remote message as a
    /// duplicate. Unchanged there: keep-both.
    #[test]
    fn gmail_keeps_both_on_a_moved_version_even_with_the_content_unchanged() {
        let db = temp_db();
        let acct = "gmail:someone@gmail.com";
        let uuid = "AAAAAAAA-1111-2222-3333-444444444444";
        let note = |body: &str, version: &str| {
            let mut n = walked("T", body, version, "Notes");
            n.uuid = uuid.into();
            n.id = version.into();
            n
        };
        let pin = db::RemotePin::LocalWins;
        reconcile_one_db(&db, acct, &note("<div>base</div>", "m1"), pin, "unused");
        db.apply_local_edit(uuid, acct, "T", "<div>base, mine</div>", "Notes", &Actor::test()).unwrap();

        reconcile_one_db(&db, acct, &note("<div>base</div>", "m2"), pin, "COPY-1");

        assert!(db.get("COPY-1", acct).unwrap().is_some());
    }
}

#[cfg(test)]
mod lost_create_tests {
    use super::*;
    use crate::provenance::Actor;

    // The 2026-10-13 capture again (gotcha #39), met from the other side: the
    // pull reaches the message the lost CREATE left before the retry push does.
    const ACC: &str = "gmail:a@b.c";
    const UUID: &str = "526F5203-98E6-4AFB-99C7-C03D7CA06A07";
    const LANDED: &str = "1a134095a6f1793c";
    const CREATED: &str = "Tue, 13 Oct 2026 13:53:11 +0700";

    fn landed() -> gmail::Note {
        serde_json::from_value(serde_json::json!({
            "id": LANDED, "uuid": UUID, "title": "DONT PUT API KEYS IN AI CHATS! DO THIS INSTEAD!",
            "body_html": "<div>https://youtube.com/shorts/X5Ig6AIzVa4</div>",
            "date": CREATED, "version": LANDED, "label": "Notes/Inbox",
            "x_mail_created_date": CREATED,
        }))
        .unwrap()
    }

    /// A capture whose CREATE was sent, then edited before Jodd heard back.
    fn captured(db: &db::Db, marked: bool) {
        let n = crate::test_support::note(ACC, UUID)
            .id("")
            .label("Notes/Inbox")
            .title("DONT PUT API KEYS IN AI CHATS! DO THIS INSTEAD!")
            .body("<div>https://youtube.com/shorts/X5Ig6AIzVa4</div><div>my note on it</div>")
            .build();
        let n = db::CachedNote { sync_state: db::SyncState::Dirty, last_synced_at: None, last_remote_modified_at: None, ..n };
        db.insert_local_new(&n, &Actor::Human).unwrap();
        if marked {
            db.mark_create_attempted(UUID, ACC).unwrap();
        }
    }

    /// Before: `remote_version` None against the landed id reads as "the
    /// remote moved", there is no base fingerprint to say otherwise, and the
    /// user got a "(conflict from …)" note, which the worker then CREATEd.
    #[test]
    fn a_pull_that_meets_a_lost_create_adopts_it_instead_of_forking_a_conflict() {
        let db = crate::test_support::temp_db();
        captured(&db, true);

        reconcile_one_db(&db, ACC, &landed(), db::RemotePin::LocalWins, "copy-1");

        assert!(db.get("copy-1", ACC).unwrap().is_none(), "no conflict copy for Jodd's own create");
        let row = db.get(UUID, ACC).unwrap().unwrap();
        assert_eq!(row.sync_state, db::SyncState::Dirty, "the local edit is still to be pushed");
        assert!(row.body_html.contains("my note on it"), "the local edit survives: {}", row.body_html);
        assert_eq!(row.id, LANDED, "the next push replaces the landed message instead of inserting");
        assert_eq!(row.remote_version.as_deref(), Some(LANDED));
        assert_eq!(row.x_mail_created_date.as_deref(), Some(CREATED), "the note keeps its first creation time");
        assert!(!db.create_attempted(UUID, ACC).unwrap(), "no longer a create");
    }

    /// The marker is the only evidence the remote copy is Jodd's own. Without
    /// it, keep-both stands exactly as before.
    #[test]
    fn without_the_marker_the_same_meeting_is_still_a_conflict() {
        let db = crate::test_support::temp_db();
        captured(&db, false);

        reconcile_one_db(&db, ACC, &landed(), db::RemotePin::LocalWins, "copy-1");

        assert!(db.get("copy-1", ACC).unwrap().is_some(), "keep-both copy");
    }
}
