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
                let dup = db::CachedNote {
                    uuid: new_uuid,
                    account_id: account_id.to_string(),
                    id: String::new(),
                    title: dup_title,
                    body_html: existing.body_html.clone(),     // LOCAL preserved
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
                };
                if let Err(e) = db.insert_local_new(&dup) {
                    log!("reconcile_one: insert conflict-copy failed for {}: {}",
                         existing.uuid, e);
                    return;
                }
                // Conflict-copy inherits the primary's tags. Without this the
                // copy starts untagged and whichever side the user picks (by
                // deleting the other) costs them their tag state on that note.
                // Best-effort: a copy failure isn't worth aborting the whole
                // reconcile, but log so we notice if it's a recurring problem.
                if let Err(e) = db.copy_tags(account_id, &existing.uuid, &dup.uuid) {
                    log!("reconcile_one: copy_tags to conflict-copy failed for {} → {}: {}",
                         existing.uuid, dup.uuid, e);
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
