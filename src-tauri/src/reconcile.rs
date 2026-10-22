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

/// Merge another device's append into `existing`'s pending edit, if that is
/// all the remote did: same title and folder (the title is the first line,
/// gotcha #11 — a rename is not an append), a recorded base
/// (`notes.remote_base_body`, migration 30), and `fetched`'s body that base
/// plus an ending (`merge::merge_remote_appends`). Applied in one transaction
/// (`Db::apply_remote_append_merge`); `false` leaves keep-both to decide.
fn merged_remote_append(db: &db::Db, account_id: &str, existing: &db::CachedNote, fetched: &db::CachedNote) -> bool {
    if existing.title != fetched.title || existing.label != fetched.label {
        return false;
    }
    let Ok(Some(base)) = db.remote_base_body(&existing.uuid, account_id) else {
        return false;
    };
    let Some(merged) = crate::merge::merge_remote_appends(&base, &existing.body_html, &fetched.body_html) else {
        return false;
    };
    match db.apply_remote_append_merge(fetched, &merged, existing.local_version) {
        Ok(true) => {
            log!(
                "reconcile_one: uuid={} remote {} only appended to the base of the pending edit — merged ({}b → {}b), not a conflict",
                existing.uuid,
                fetched.remote_version.as_deref().unwrap_or("?"),
                existing.body_html.len(),
                merged.len()
            );
            true
        }
        Ok(false) => false,
        Err(e) => {
            log!("reconcile_one: append merge failed for {}: {}", existing.uuid, e);
            false
        }
    }
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
        // Local has unpushed edits, and another device only APPENDED to the
        // revision they were based on (roadmap P1): merge instead of keeping
        // both. Measured 2026-10-21: Share-to-Jodd appends from the phone
        // against an edit made in the middle of the same note on the Mac.
        Dirty if remote_changed && merged_remote_append(db, account_id, &existing, &cached) => {}
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

/// The divergent duplicates that are another device's work rather than
/// history: those NEWER than the revision this device last synced for the
/// note (its cached `date` — a local edit never moves it). One that is not
/// newer is a revision this device already had or had moved past: the old
/// message during Jodd's own insert-then-trash or Apple's APPEND-then-EXPUNGE,
/// or an orphan a failed trash-old left. Must run BEFORE the listing's
/// reconcile, which moves the row to the kept copy. A date that does not parse
/// keeps the duplicate: a stray copy is cheaper than lost text.
///
/// A note this device has NO row for is not kept: it never held a revision,
/// so it cannot have missed one. That is every note on a fresh sign-in —
/// `list_account_index` stores ids, not rows — and keeping them minted a copy
/// of every old leftover (gotcha #40, measured 2026-10-22). The devices that
/// did hold the note still keep a duplicate newer than their own revision.
pub(super) fn divergent_worth_keeping(db: &db::Db, account_id: &str, divergent: Vec<gmail::Note>) -> Vec<gmail::Note> {
    let parse = |d: &str| chrono::DateTime::parse_from_rfc2822(d).ok();
    divergent
        .into_iter()
        .filter(|d| {
            let cached = match db.get(&d.uuid, account_id) {
                Ok(Some(cached)) => cached,
                Ok(None) => return false,
                Err(_) => return true,
            };
            match (parse(&d.date), parse(&cached.date)) {
                (Some(dropped), Some(held)) => dropped > held,
                _ => true,
            }
        })
        .collect()
}

/// The full listing's prune — skipped when the listing was incomplete
/// (`DedupSummary::incomplete`). `keep` holds only the notes the pass could
/// READ, so on an incomplete pass every note it could not read looks gone and
/// `prune_clean` would drop it from the cache until a later pull brought it
/// back. Measured 2026-10-22: a throttled first sync read 162 of 422 notes.
/// Returns how many rows were pruned.
pub(super) fn prune_after_listing_db(db: &db::Db, account_id: &str, keep: &[String], incomplete: bool) -> usize {
    if incomplete {
        log!("list_notes: listing incomplete — skipping the prune this pass");
        return 0;
    }
    match db.prune_clean(account_id, keep, db::prune_horizon_ms()) {
        Ok(n) => {
            if n > 0 {
                log!("list_notes: pruned {} clean cache row(s) no longer on remote", n);
            }
            n
        }
        Err(e) => {
            log!("list_notes: prune failed: {}", e);
            0
        }
    }
}

/// On an INCOMPLETE listing, add to `result` the cached notes it could not
/// read (all of the account's, or `folder`'s when given), so the frontend —
/// which shows what a listing returns — keeps showing what SQLite holds. A
/// skipped prune alone left them in the cache but off the screen: measured
/// 2026-10-22, one failed label listing turned 415 into 414 on the phone.
/// Returns how many were added. Never call it on a complete listing: there a
/// cached note the listing lacks is one the remote deleted.
pub(super) fn carry_unread_cached_notes_db(db: &db::Db, account_id: &str, folder: Option<&str>, result: &mut Vec<gmail::Note>) -> usize {
    let rows = match folder {
        Some(path) => db.list_notes_by_label(account_id, path),
        None => db.list_notes(account_id),
    };
    let rows = match rows {
        Ok(r) => r,
        Err(e) => {
            log!("carry_unread_cached_notes: cache read failed for {}: {}", account_id, e);
            return 0;
        }
    };
    let read: std::collections::HashSet<String> = result.iter().map(|n| n.uuid.clone()).collect();
    let before = result.len();
    result.extend(rows.into_iter().filter(|r| !read.contains(&r.uuid)).map(|r| r.to_frontend_note()));
    let added = result.len() - before;
    if added > 0 {
        log!("listing incomplete — kept {} cached note(s) it could not read in the result", added);
    }
    added
}

/// The sign-in index with the messages a listing already found to be
/// duplicates (`Db::record_duplicate_ids`) taken out, so the startup folder
/// counts count notes, not Gmail messages. A cache-read failure keeps the
/// index whole: a high count is better than a missing folder.
pub(super) fn drop_known_duplicates_db(db: &db::Db, account_id: &str, index: Vec<gmail::MessageIndex>) -> Vec<gmail::MessageIndex> {
    let dups = match db.known_duplicate_ids(account_id) {
        Ok(d) if !d.is_empty() => d,
        Ok(_) => return index,
        Err(e) => {
            log!("index_account: known duplicates unreadable for {}: {}", account_id, e);
            return index;
        }
    };
    let before = index.len();
    let out: Vec<_> = index.into_iter().filter(|m| !dups.contains(&m.id)).collect();
    if out.len() < before {
        log!("index_account[{}]: left out {} known duplicate message(s)", account_id, before - out.len());
    }
    out
}

/// The folder listing's scoped prune, under the same rule as
/// `prune_after_listing_db`: an incomplete listing prunes nothing.
pub(super) fn prune_folder_after_listing_db(db: &db::Db, account_id: &str, path: &str, keep: &[String], incomplete: bool) -> usize {
    if incomplete {
        log!("list_notes_in_folder: listing of '{}' incomplete — skipping the prune this pass", path);
        return 0;
    }
    match db.prune_clean_in_label(account_id, path, keep) {
        Ok(n) => {
            if n > 0 {
                log!("list_notes_in_folder: pruned {} clean row(s) no longer in folder '{}'", n, path);
            }
            n
        }
        Err(e) => {
            log!("list_notes_in_folder: prune failed: {}", e);
            0
        }
    }
}

/// `preserve_divergent_db` for each duplicate a listing reported, with the
/// copy's uuid minted the backend's way (`backend::mint_uuid_for`). Each copy
/// made is registered with the worker so the frontend refreshes when its
/// push lands — like a keep-both copy, no listing shows it before then.
pub(super) fn preserve_divergent(state: &State<'_, AppState>, account_id: &str, divergent: &[gmail::Note]) {
    if divergent.is_empty() {
        return;
    }
    let kind = account_backend_kind(state, account_id).unwrap_or(accounts::BackendKind::LocalFs);
    for dropped in divergent {
        let copy_uuid = crate::backend::mint_uuid_for(kind);
        if preserve_divergent_db(&state.db, account_id, dropped, &copy_uuid) {
            crate::sync_worker::await_conflict_copy_push(account_id, &copy_uuid);
        }
    }
}

/// Keep a duplicate the listing dropped although it held text its kept copy
/// lacks (`gmail::dedupe_by_uuid`'s `divergent`): it becomes a note of its
/// own under `copy_uuid`, dirty, so the worker CREATEs it. Returns whether a
/// copy was made — never twice for one message (`Db::insert_divergent_copy`).
///
/// The duplicate itself is left where it is. Trashing it is the user's call
/// (`cleanup_orphans`), and once its text lives in the copy that is safe.
pub(super) fn preserve_divergent_db(db: &db::Db, account_id: &str, dropped: &gmail::Note, copy_uuid: &str) -> bool {
    // In the revision's own time, as its Date header states it: the point is
    // telling the user WHICH version this is, not when Jodd found it.
    let stamp = chrono::DateTime::parse_from_rfc2822(&dropped.date)
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| dropped.date.clone());
    // A copy is a CREATE: on a backend whose tags are inline objects they go
    // up as text (gotcha #38) — same rule as the keep-both copy above.
    let body_html = match db::tag_model_for_account(account_id) {
        crate::backend::TagModel::BodyText => dropped.body_html.clone(),
        crate::backend::TagModel::InlineObjects => {
            crate::backend::icloud::format_html::flatten_hashtag_spans(&dropped.body_html)
        }
    };
    let now = db::now_ms();
    let copy = db::CachedNote {
        uuid: copy_uuid.to_string(),
        account_id: account_id.to_string(),
        id: String::new(),
        title: format!("{} (conflict copy {})", dropped.title, stamp),
        body_html,
        date: dropped.date.clone(),
        x_mail_created_date: dropped.x_mail_created_date.clone(),
        label: dropped.label.clone(),
        local_version: 1,
        remote_version: None,
        sync_state: db::SyncState::Dirty,
        last_synced_at: None,
        last_local_modified_at: now,
        last_remote_modified_at: None,
        pinned: false,
        meta_msg_id: None,
        pin_dirty: false,
        push_blocked_reason: None,
        push_blocked_by_remote: false,
    };
    match db.insert_divergent_copy(&copy, &dropped.id, &Actor::Process("divergent-copy")) {
        Ok(made) => {
            if made {
                log!(
                    "reconcile: uuid={} duplicate {} holds text the kept copy lacks — preserved as uuid={} (\"{}\")",
                    dropped.uuid, dropped.id, copy.uuid, copy.title
                );
            }
            made
        }
        Err(e) => {
            log!("reconcile: preserving duplicate {} of uuid={} failed: {}", dropped.id, dropped.uuid, e);
            false
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
    /// duplicate. Before P1 that kept both. With the base body recorded
    /// (migration 30), "content unchanged" is an empty append: the merge
    /// adopts the new message AND re-addresses the row to it, so the pending
    /// edit is pushed over it — no copy, no stranded duplicate.
    #[test]
    fn gmail_readdresses_the_row_on_a_moved_version_with_the_content_unchanged() {
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

        assert!(db.get("COPY-1", acct).unwrap().is_none(), "no conflict to keep");
        let row = db.get(uuid, acct).unwrap().unwrap();
        assert_eq!(row.id, "m2", "the next push replaces the live message, stranding nothing");
        assert_eq!(row.sync_state, db::SyncState::Dirty);
        assert!(row.body_html.contains("base, mine"), "the pending edit stands");
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

/// A Gmail duplicate the listing dropped although it held text the kept copy
/// lacks (`gmail::dedupe_by_uuid`'s `divergent`) becomes its own note, once.
/// Measured 2026-10-21: the 18:05 revision of note 80D6C8E1 carried a link no
/// later revision had, and the listing dropped it with a log line and nothing
/// else.
#[cfg(test)]
mod divergent_duplicate_tests {
    use super::*;

    const ACC: &str = "gmail:kaiwan@bbmedia.co.th";
    const UUID: &str = "80D6C8E1-C49F-42F6-9859-540845D6359C";
    const LOSER: &str = "1a158fccdb4a2561";
    const LABEL: &str = "Notes/Personal/Creator/Charactor ref";

    fn the_1805_revision() -> gmail::Note {
        serde_json::from_value(serde_json::json!({
            "id": LOSER, "uuid": UUID, "title": "จีนสั้น character references",
            "body_html": "<div>— 2026-10-20 18:05 —</div><div>https://youtube.com/watch?v=a0uhMvKEgYI</div>",
            "date": "Tue, 20 Oct 2026 18:05:25 +0700", "version": LOSER, "label": LABEL,
            "x_mail_created_date": "Tue, 20 Oct 2026 03:40:08 +0700",
        }))
        .unwrap()
    }

    fn copies(db: &db::Db) -> Vec<db::CachedNote> {
        db.list_notes(ACC).unwrap().into_iter().filter(|n| n.uuid != UUID).collect()
    }

    #[test]
    fn a_divergent_duplicate_becomes_a_conflict_copy_to_push() {
        let db = crate::test_support::temp_db();

        assert!(preserve_divergent_db(&db, ACC, &the_1805_revision(), "COPY-1"));

        let copy = db.get("COPY-1", ACC).unwrap().expect("a copy");
        assert_eq!(copy.title, "จีนสั้น character references (conflict copy 2026-10-20 18:05)",
            "named for the revision it keeps, in that revision's own time");
        assert!(copy.body_html.contains("a0uhMvKEgYI"), "the lost link is back");
        assert_eq!(copy.label, LABEL);
        assert_eq!(copy.sync_state, db::SyncState::Dirty, "pushed as its own note");
        assert!(copy.id.is_empty(), "a CREATE, never a write over the duplicate");
    }

    /// Every listing reports the duplicate again until somebody trashes it,
    /// so preserving must happen once per message — and stay done after the
    /// user deletes the copy, or the copy would come back on the next poll.
    #[test]
    fn a_duplicate_is_preserved_once_even_after_its_copy_is_deleted() {
        let db = crate::test_support::temp_db();
        assert!(preserve_divergent_db(&db, ACC, &the_1805_revision(), "COPY-1"));

        assert!(!preserve_divergent_db(&db, ACC, &the_1805_revision(), "COPY-2"));
        assert_eq!(copies(&db).len(), 1);

        db.delete("COPY-1", ACC).unwrap();
        assert!(!preserve_divergent_db(&db, ACC, &the_1805_revision(), "COPY-3"));
        assert!(copies(&db).is_empty(), "{:?}", copies(&db));
    }

    /// Review findings (2026-10-21): a dropped copy that is not newer than the
    /// revision this device last synced is history, not another device's
    /// work — the old revision during Jodd's own insert-then-trash, Apple's
    /// APPEND-then-EXPUNGE, or an orphan a failed trash-old left behind.
    /// Copying those would mint notes nobody lost. The measured case passes:
    /// the Mac held the 20 Oct 03:51 revision when the 18:05 one turned up.
    #[test]
    fn only_a_duplicate_newer_than_this_devices_revision_is_worth_keeping() {
        let db = crate::test_support::temp_db();
        let at = |date: &str| {
            let mut n = crate::test_support::note(ACC, UUID).id("1a155eec0c820bca").label(LABEL).date(date).build();
            n.sync_state = db::SyncState::Clean;
            n
        };

        // The Mac at 01:07 on 21 Oct: synced at 03:51, never saw 18:05.
        db.insert_local_new(&at("Tue, 20 Oct 2026 03:51:13 +0700"), &Actor::test()).unwrap();
        assert_eq!(divergent_worth_keeping(&db, ACC, vec![the_1805_revision()]).len(), 1);

        // A device already holding the 18:05 revision, or anything newer.
        db.delete(UUID, ACC).unwrap();
        db.insert_local_new(&at("Tue, 20 Oct 2026 18:05:25 +0700"), &Actor::test()).unwrap();
        assert!(divergent_worth_keeping(&db, ACC, vec![the_1805_revision()]).is_empty());
        db.delete(UUID, ACC).unwrap();
        db.insert_local_new(&at("Wed, 21 Oct 2026 07:32:11 +0700"), &Actor::test()).unwrap();
        assert!(divergent_worth_keeping(&db, ACC, vec![the_1805_revision()]).is_empty());
    }

    /// A device that never held the note cannot have missed anything in it.
    /// Measured 2026-10-22: the Infinix's first sync after sign-in listed
    /// folders before any row existed (`list_account_index` keeps ids only),
    /// and minted three "(conflict copy …)" notes from leftovers every synced
    /// device had already passed over — one of them a 26 Sep orphan.
    #[test]
    fn a_duplicate_of_a_note_this_device_never_synced_is_not_kept() {
        let db = crate::test_support::temp_db();
        assert!(divergent_worth_keeping(&db, ACC, vec![the_1805_revision()]).is_empty());
    }

    /// The record is per account: removing the account forgets it.
    #[test]
    fn removing_the_account_forgets_what_was_preserved() {
        let db = crate::test_support::temp_db();
        assert!(preserve_divergent_db(&db, ACC, &the_1805_revision(), "COPY-1"));

        db.delete_account(ACC).unwrap();

        assert!(preserve_divergent_db(&db, ACC, &the_1805_revision(), "COPY-2"));
    }
}

#[cfg(test)]
mod divergent_wiring_tests {
    /// Both listings dedupe, so both must hand what they dropped to
    /// `preserve_divergent_db` — a folder view that skipped it would still
    /// lose the text the full listing now keeps.
    #[test]
    fn both_listings_preserve_the_duplicates_their_dedupe_reports() {
        let src = include_str!("lib.rs");
        for sig in ["async fn list_notes(\n", "async fn list_notes_in_folder(\n"] {
            let body = crate::test_support::extract_fn_body(src, sig);
            assert!(body.contains("preserve_divergent("), "{sig:?} must preserve divergent duplicates");
            // Filtered against the device's revision BEFORE reconcile moves
            // the row to the kept copy — afterwards nothing is ever newer.
            let filter = body.find("divergent_worth_keeping(").unwrap_or_else(|| panic!("{sig:?} must filter"));
            let reconcile = body.find("reconcile_one(").expect("reconciles");
            assert!(filter < reconcile, "{sig:?}: filter before reconcile");
        }
    }
}

/// P1: a pull that finds another device only APPENDED to the revision this
/// device's pending edit was based on merges instead of keeping both. The
/// fixture is the 2026-10-21 shape: the Mac moved a link inside the list
/// while the phone appended dated blocks at the end.
#[cfg(test)]
mod append_merge_tests {
    use super::*;
    use crate::provenance::Actor;

    const ACC: &str = "gmail:kaiwan@bbmedia.co.th";
    const UUID: &str = "80D6C8E1-C49F-42F6-9859-540845D6359C";
    const TITLE: &str = "จีนสั้น character references";
    const LABEL: &str = "Notes/Personal/Creator/Charactor ref";
    const BASE: &str = "<div>https://www.youtube.com/watch?v=jXZvDtXg5co</div><div>https://www.facebook.com/watch/?v=965106449453157</div>";
    const MAC: &str = "<div>https://www.youtube.com/watch?v=jXZvDtXg5co</div><div>https://www.youtube.com/watch?v=cvN7svEoIxc</div><div>https://www.facebook.com/watch/?v=965106449453157</div>";
    const APPENDED: &str = "<div><br></div><div>— 2026-10-21 07:32 —</div>";

    fn doc(inner: &str) -> String {
        format!("<html><head></head><body>{inner}</body></html>")
    }

    fn remote(id: &str, title: &str, inner: &str) -> gmail::Note {
        serde_json::from_value(serde_json::json!({
            "id": id, "uuid": UUID, "title": title, "body_html": doc(inner),
            "date": "Wed, 21 Oct 2026 07:32:11 +0700", "version": id, "label": LABEL,
        }))
        .unwrap()
    }

    /// Synced at `m1` holding BASE (a pull records the base body), then edited.
    fn synced_then_edited(db: &db::Db) -> db::CachedNote {
        reconcile_one_db(db, ACC, &remote("m1", TITLE, BASE), db::RemotePin::LocalWins, "unused");
        db.apply_local_edit(UUID, ACC, TITLE, &doc(MAC), LABEL, &Actor::Human).unwrap();
        db.get(UUID, ACC).unwrap().unwrap()
    }

    fn copies(db: &db::Db) -> Vec<db::CachedNote> {
        db.list_notes(ACC).unwrap().into_iter().filter(|n| n.uuid != UUID).collect()
    }

    #[test]
    fn a_remote_append_merges_into_the_pending_edit_instead_of_forking_a_copy() {
        let db = crate::test_support::temp_db();
        let before = synced_then_edited(&db);
        let fetched = remote("m2", TITLE, &format!("{BASE}{APPENDED}"));

        reconcile_one_db(&db, ACC, &fetched, db::RemotePin::LocalWins, "copy-1");

        assert!(copies(&db).is_empty(), "{:?}", copies(&db));
        let row = db.get(UUID, ACC).unwrap().unwrap();
        assert_eq!(row.body_html, doc(&format!("{MAC}{APPENDED}")), "both edits, nothing doubled");
        assert_eq!(row.sync_state, db::SyncState::Dirty, "the merge is pushed");
        assert_eq!(row.id, "m2", "the next push replaces the live message");
        assert_eq!(row.remote_version.as_deref(), Some("m2"));
        assert_eq!(db.remote_base_body(UUID, ACC).unwrap().as_deref(), Some(fetched.body_html.as_str()));
        // Like keep-both: an open editor's save based on the pre-merge text
        // must not land over the merged row.
        assert!(row.local_version > before.local_version);
    }

    /// Review finding: a refusal recorded against the remote's OLD document
    /// (gotcha #14/#24) is stale once the row sits on the new one — the merge
    /// re-arms the push, as `upsert_from_remote` does for keep-both.
    #[test]
    fn a_merge_clears_a_push_block_recorded_against_the_old_document() {
        let db = crate::test_support::temp_db();
        synced_then_edited(&db);
        db.mark_push_blocked_by_remote(UUID, ACC, "open it in Apple Notes").unwrap();

        reconcile_one_db(&db, ACC, &remote("m2", TITLE, &format!("{BASE}{APPENDED}")), db::RemotePin::LocalWins, "copy-1");

        let row = db.get(UUID, ACC).unwrap().unwrap();
        assert_eq!(row.push_blocked_reason, None);
        assert!(!row.push_blocked_by_remote);
    }

    /// The same listing again (or the next poll) finds nothing to merge: the
    /// row's version and base are the remote's now.
    #[test]
    fn a_merge_is_not_applied_twice() {
        let db = crate::test_support::temp_db();
        synced_then_edited(&db);
        let fetched = remote("m2", TITLE, &format!("{BASE}{APPENDED}"));
        reconcile_one_db(&db, ACC, &fetched, db::RemotePin::LocalWins, "copy-1");
        let once = db.get(UUID, ACC).unwrap().unwrap();

        reconcile_one_db(&db, ACC, &fetched, db::RemotePin::LocalWins, "copy-2");

        let twice = db.get(UUID, ACC).unwrap().unwrap();
        assert_eq!(twice.body_html, once.body_html);
        assert_eq!(twice.local_version, once.local_version);
        assert!(copies(&db).is_empty());
    }

    #[test]
    fn a_remote_that_rewrote_the_note_still_keeps_both() {
        let db = crate::test_support::temp_db();
        synced_then_edited(&db);

        reconcile_one_db(&db, ACC, &remote("m2", TITLE, "<div>rewritten elsewhere</div>"), db::RemotePin::LocalWins, "copy-1");

        assert!(db.get("copy-1", ACC).unwrap().is_some(), "keep-both, as before");
    }

    /// The title is the first line (gotcha #11): a renamed note is not an
    /// append, whatever its body says.
    #[test]
    fn a_remote_rename_still_keeps_both() {
        let db = crate::test_support::temp_db();
        synced_then_edited(&db);

        reconcile_one_db(&db, ACC, &remote("m2", "Renamed", &format!("{BASE}{APPENDED}")), db::RemotePin::LocalWins, "copy-1");

        assert!(db.get("copy-1", ACC).unwrap().is_some());
    }

    /// A row synced before migration 30 has no recorded base: keep both.
    #[test]
    fn a_row_with_no_recorded_base_still_keeps_both() {
        let db = crate::test_support::temp_db();
        let mut n = crate::test_support::note(ACC, UUID).id("m1").label(LABEL).title(TITLE).build();
        n.remote_version = Some("m1".into());
        n.body_html = doc(BASE);
        db.insert_local_new(&n, &Actor::test()).unwrap();
        db.apply_local_edit(UUID, ACC, TITLE, &doc(MAC), LABEL, &Actor::Human).unwrap();
        assert_eq!(db.remote_base_body(UUID, ACC).unwrap(), None, "precondition");

        reconcile_one_db(&db, ACC, &remote("m2", TITLE, &format!("{BASE}{APPENDED}")), db::RemotePin::LocalWins, "copy-1");

        assert!(db.get("copy-1", ACC).unwrap().is_some());
    }
}

/// A listing that could not read every message it found is not a picture of
/// the remote, so it must not prune. Measured 2026-10-22: a throttled first
/// sync returned 162 of 422 notes; on a device that already held the other 260,
/// `prune_clean` would have dropped each one older than the grace window
/// (gotcha #35) until a later pull brought it back.
#[cfg(test)]
mod incomplete_listing_prune_tests {
    use super::*;

    const ACC: &str = "gmail:kaiwan@bbmedia.co.th";
    const LABEL: &str = "Notes/_test_";

    fn two_old_clean_notes() -> db::Db {
        let db = crate::test_support::temp_db();
        for uuid in ["LISTED", "SKIPPED"] {
            let n = crate::test_support::note(ACC, uuid).label(LABEL).build();
            db.insert_local_new(&n, &Actor::test()).unwrap();
        }
        db.age_sync_for_test(ACC, db::PRUNE_GRACE_MS + 1_000);
        db
    }

    #[test]
    fn an_incomplete_full_listing_prunes_nothing() {
        let db = two_old_clean_notes();
        assert_eq!(prune_after_listing_db(&db, ACC, &["LISTED".into()], true), 0);
        assert!(db.get("SKIPPED", ACC).unwrap().is_some(), "a note the listing could not read stays");
    }

    #[test]
    fn a_complete_full_listing_still_prunes() {
        let db = two_old_clean_notes();
        assert_eq!(prune_after_listing_db(&db, ACC, &["LISTED".into()], false), 1);
        assert!(db.get("SKIPPED", ACC).unwrap().is_none());
    }

    #[test]
    fn an_incomplete_folder_listing_prunes_nothing() {
        let db = two_old_clean_notes();
        assert_eq!(prune_folder_after_listing_db(&db, ACC, LABEL, &["LISTED".into()], true), 0);
        assert!(db.get("SKIPPED", ACC).unwrap().is_some());
        assert_eq!(prune_folder_after_listing_db(&db, ACC, LABEL, &["LISTED".into()], false), 1);
    }

    /// Both listing commands prune only through these helpers.
    #[test]
    fn both_listings_prune_through_the_completeness_check() {
        let src = include_str!("lib.rs");
        for (sig, helper, raw) in [
            ("async fn list_notes(\n", "prune_after_listing_db(", "state.db.prune_clean("),
            ("async fn list_notes_in_folder(\n", "prune_folder_after_listing_db(", "state.db.prune_clean_in_label("),
        ] {
            let body = crate::test_support::extract_fn_body(src, sig);
            assert!(body.contains(helper), "{sig:?} must prune through {helper}");
            assert!(!body.contains(raw), "{sig:?} must not call {raw} directly");
        }
    }
}

/// An incomplete listing skips the prune (gotcha #43), but the frontend shows
/// what the listing RETURNS, so a note it could not read still vanished from
/// the screen while SQLite held it. Measured 2026-10-22 08:31: one label's
/// listing failed, the cache kept 415 rows and the list showed 414.
#[cfg(test)]
mod incomplete_listing_display_tests {
    use super::*;

    const ACC: &str = "gmail:kaiwan@bbmedia.co.th";

    fn cached(db: &db::Db, uuid: &str, label: &str) {
        let n = crate::test_support::note(ACC, uuid).label(label).build();
        db.insert_local_new(&n, &Actor::test()).unwrap();
    }

    fn read(uuid: &str, label: &str) -> gmail::Note {
        db_note_to_wire(uuid, label)
    }

    fn db_note_to_wire(uuid: &str, label: &str) -> gmail::Note {
        crate::test_support::note(ACC, uuid).label(label).build().to_frontend_note()
    }

    #[test]
    fn an_incomplete_full_listing_returns_the_cached_notes_it_could_not_read() {
        let db = crate::test_support::temp_db();
        cached(&db, "READ", "Notes");
        cached(&db, "UNREAD", "Notes/Personal/ID");
        let mut result = vec![read("READ", "Notes")];
        assert_eq!(carry_unread_cached_notes_db(&db, ACC, None, &mut result), 1);
        let mut uuids: Vec<_> = result.iter().map(|n| n.uuid.clone()).collect();
        uuids.sort();
        assert_eq!(uuids, vec!["READ", "UNREAD"]);
    }

    #[test]
    fn an_incomplete_folder_listing_returns_only_that_folders_cached_notes() {
        let db = crate::test_support::temp_db();
        cached(&db, "IN_FOLDER", "Notes/_test_");
        cached(&db, "ELSEWHERE", "Notes/Inbox");
        let mut result = vec![];
        assert_eq!(carry_unread_cached_notes_db(&db, ACC, Some("Notes/_test_"), &mut result), 1);
        assert_eq!(result[0].uuid, "IN_FOLDER");
    }

    #[test]
    fn both_listings_carry_cached_notes_when_incomplete() {
        let src = include_str!("lib.rs");
        for sig in ["async fn list_notes(\n", "async fn list_notes_in_folder(\n"] {
            let body = crate::test_support::extract_fn_body(src, sig);
            assert!(body.contains("carry_unread_cached_notes_db("), "{sig:?} must carry unread cached notes");
        }
    }

    /// The duplicates a listing dropped are remembered, so the next sign-in
    /// index can leave them out of the count.
    #[test]
    fn known_duplicates_are_replaced_by_a_complete_listing_and_added_by_others() {
        let db = crate::test_support::temp_db();
        db.record_duplicate_ids(ACC, &["d1".into(), "d2".into()], true).unwrap();
        db.record_duplicate_ids(ACC, &["d3".into()], false).unwrap();
        let mut ids: Vec<_> = db.known_duplicate_ids(ACC).unwrap().into_iter().collect();
        ids.sort();
        assert_eq!(ids, vec!["d1", "d2", "d3"]);
        db.record_duplicate_ids(ACC, &["d2".into()], true).unwrap();
        assert_eq!(db.known_duplicate_ids(ACC).unwrap().into_iter().collect::<Vec<_>>(), vec!["d2".to_string()]);
        db.delete_account(ACC).unwrap();
        assert!(db.known_duplicate_ids(ACC).unwrap().is_empty());
    }

    #[test]
    fn the_index_leaves_known_duplicates_out() {
        let db = crate::test_support::temp_db();
        db.record_duplicate_ids(ACC, &["dup".into()], true).unwrap();
        let idx = vec![
            gmail::MessageIndex { id: "keep".into(), label: "Notes".into() },
            gmail::MessageIndex { id: "dup".into(), label: "Notes".into() },
        ];
        let out = drop_known_duplicates_db(&db, ACC, idx);
        assert_eq!(out.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["keep"]);
    }

    #[test]
    fn the_listings_record_and_the_index_drops_duplicates() {
        let src = include_str!("lib.rs");
        for sig in ["async fn list_notes(\n", "async fn list_notes_in_folder(\n"] {
            let body = crate::test_support::extract_fn_body(src, sig);
            assert!(body.contains("record_duplicate_ids("), "{sig:?} must record dropped duplicate ids");
        }
        let body = crate::test_support::extract_fn_body(src, "async fn index_account(\n");
        assert!(body.contains("drop_known_duplicates_db("), "index_account must leave known duplicates out");
    }
}
