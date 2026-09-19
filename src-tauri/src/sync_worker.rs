//! Extracted behavior; transport and transaction contracts are unchanged.
use super::*;

// ─── Background sync worker ──────────────────────────────────────────────────
//
// Scans the SQLite replica for rows that need to be propagated to Gmail
// (sync_state IN ('dirty', 'deleted_pending')) and tries each one. Loops
// forever with a small interval. Designed to be resilient:
//   - If the network is down, push fails → row stays dirty → retried next cycle
//   - If a token has expired, ensure_token refreshes via the keychain RT
//   - If save_note fails for a permanent reason (e.g. invalid label),
//     we log and move on — the row stays dirty so the user has a chance
//     to fix it. We DON'T silently lose data by marking clean on failure.
//
// Future hardening:
//   - Exponential backoff per uuid on repeated failures
//   - Emit "sync-status" events for the UI to show "1 unsynced" etc.
//   - Coalesce rapid edits to the same uuid (push only the latest version)

/// Milliseconds a content-dirty note must be quiet (no local edit) before the
/// worker pushes it — debounces autosave churn. See
/// docs/superpowers/specs/2026-07-21-debounce-note-push-design.md.
pub(super) const PUSH_SETTLE_MS: i64 = 5_000;

/// Upper bound on how long an already-synced note may keep deferring while it
/// is edited nonstop; past this it is force-pushed once so other devices/Apple
/// don't go stale. Never-synced notes are exempt (SQLite is the source of
/// truth and survives restart), so they push only via the settle branch.
pub(super) const MAX_DEFER_MS: i64 = 60_000;

/// Should this content-dirty note be pushed to Gmail on the current tick?
/// `settled` = the user has paused editing for at least `settle_ms`.
/// `overdue` = it has been at least `max_defer_ms` since the last successful
/// push (only meaningful once the note has been synced at all).
pub(super) fn note_push_due(
    now_ms: i64,
    last_local_modified_at: i64,
    last_synced_at: Option<i64>,
    settle_ms: i64,
    max_defer_ms: i64,
) -> bool {
    let settled = now_ms - last_local_modified_at >= settle_ms;
    let overdue = matches!(last_synced_at, Some(s) if now_ms - s >= max_defer_ms);
    settled || overdue
}

pub(super) const SYNC_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// RAII guard for one `(account_id, uuid)` entry in `AppState.pushing`.
///
/// `push_one_dirty` needs to extend the in-flight-push marker to a note's
/// NEW uuid partway through the function, when a create on the Microsoft
/// backend rekeys the row — see the call site below. Two fallible steps
/// (`rekey_note_uuid`, `mark_pushed`) sit between that insert and the point
/// where the entry would naturally come out, and an early `?` on either one
/// would return past a bare `.remove(...)` call, leaking the entry forever.
/// A leaked entry makes `reconcile_one` treat every future remote change to
/// that uuid as "our own in-flight push" and silently ignore it — the
/// self-induced false-conflict race fixed in a693d11, reopened by a partial
/// insert with no matching remove. Tying the removal to `Drop` instead makes
/// every exit path — a normal return, an early `?`, even a panic — release
/// the entry, with no second call site to keep in sync with the first.
pub(super) struct PushingGuard<'a> {
    pushing: &'a Mutex<std::collections::HashSet<(String, String)>>,
    key: (String, String),
}

impl<'a> PushingGuard<'a> {
    fn new(pushing: &'a Mutex<std::collections::HashSet<(String, String)>>, key: (String, String)) -> Self {
        pushing.lock().unwrap().insert(key.clone());
        Self { pushing, key }
    }
}

impl Drop for PushingGuard<'_> {
    fn drop(&mut self) {
        self.pushing.lock().unwrap().remove(&self.key);
    }
}

#[cfg(test)]
mod pushing_guard_tests {
    use super::*;

    #[test]
    fn releases_on_normal_drop() {
        let pushing: Mutex<std::collections::HashSet<(String, String)>> = Mutex::new(Default::default());
        let key = ("acct".to_string(), "new-uuid".to_string());
        {
            let _guard = PushingGuard::new(&pushing, key.clone());
            assert!(pushing.lock().unwrap().contains(&key), "insert must happen on construction");
        }
        assert!(!pushing.lock().unwrap().contains(&key), "drop must release the entry");
    }

    /// The exact shape of the leak this guard exists to prevent: a fallible
    /// step between insert and the natural removal point returns early via
    /// `?`. Without RAII, that `?` skips a bare `.remove(...)` call and the
    /// entry survives forever — reopening a693d11.
    #[test]
    fn releases_on_early_return_via_question_mark() {
        let pushing: Mutex<std::collections::HashSet<(String, String)>> = Mutex::new(Default::default());
        let key = ("acct".to_string(), "new-uuid".to_string());

        fn fallible_step(
            pushing: &Mutex<std::collections::HashSet<(String, String)>>,
            key: (String, String),
        ) -> Result<(), String> {
            let _guard = PushingGuard::new(pushing, key);
            Err("simulated rekey_note_uuid/mark_pushed failure".to_string())?;
            Ok(())
        }

        let res = fallible_step(&pushing, key.clone());
        assert!(res.is_err(), "the simulated failure must actually propagate");
        assert!(
            !pushing.lock().unwrap().contains(&key),
            "an early ? return must not leak the pushing entry"
        );
    }
}

/// Whether a `TransportError::NotFound` from a content push means "the
/// remote object is genuinely gone" rather than a transient hiccup worth
/// retrying.
///
/// Gated on `SaveSemantics::InPlaceUpdateNeedsExplicitMove`, not just
/// `was_create` — a backend with `RelocatesOnContentPush` semantics (Gmail
/// today) can and does reach a `NotFound` on a content push: Gmail's
/// `classify_str` (`backend/gmail/transport.rs`) substring-matches any HTTP
/// 404 embedded in `save_note_full`'s error string, regardless of why the
/// 404 happened. There, `NotFound` is not evidence the note is gone (the
/// insert failed — the old message is untouched), so dropping the row would
/// destroy an unpushed edit for no reason. On an `InPlaceUpdateNeedsExplicit
/// Move` backend (Microsoft) it IS that evidence, because `save_note_full`
/// PATCHes an existing id in place; a 404 there really is "this id no longer
/// exists." Within that gate, only true for an UPDATE (`was_create ==
/// false`, i.e. there was an existing remote id the PATCH 404'd against) — a
/// CREATE 404ing means the destination FOLDER vanished, a different and much
/// rarer failure this fix does not attempt to characterise.
///
/// Takes `Option<SaveSemantics>` rather than `Option<BackendKind>` — reading
/// this decision straight off `Capabilities` (via `push_one_dirty`'s
/// `save_semantics` local) rather than re-deriving it from a raw
/// `backend_kind == Microsoft` check is the fix itself: a future backend
/// declares its semantics once, in `Capabilities::for_backend`, instead of
/// this function (and the move-dispatch gate below it) each needing their
/// own `== Microsoft` updated by hand. See `push_one_dirty`'s call site for
/// the full reasoning (Microsoft fix 3, whole-branch review 2026-08-15;
/// Gmail gate, pre-merge review 2026-08-15; `Capabilities` routing, altitude
/// fix 8).
pub(super) fn not_found_means_deleted_remotely(save_semantics: Option<backend::SaveSemantics>, was_create: bool) -> bool {
    use backend::SaveSemantics::*;
    // An exhaustive match with no wildcard arm, so a fifth backend's semantics
    // cannot inherit an answer by default. The failure is silent in both
    // directions: answering `true` for a REPLACE-shaped save destroys an
    // unpushed edit over an unrelated 404, and answering `false` for a
    // PATCH-shaped one retries a note the user deleted on another device until
    // the account can never be drained (gotcha #2).
    !was_create
        && match save_semantics {
            Some(InPlaceUpdateNeedsExplicitMove) | Some(InPlaceUpdateIncludingMove) => true,
            Some(RelocatesOnContentPush) | None => false,
        }
}

/// Whether this backend's content push leaves the note's FOLDER to a separate
/// `Transport::move_note` call at all.
///
/// The companion to [`not_found_means_deleted_remotely`], and split out for
/// the same reason: `push_one_dirty` needs a live `tauri::State` and cannot be
/// unit-tested, so the decision lives where a test can reach it. Exhaustive,
/// with no wildcard arm — a fifth backend must say which shape its save is,
/// and both wrong answers are silent. Dispatching a move a backend does not
/// need is a second write against a version token the first write just bumped;
/// NOT dispatching one a backend does need leaves the remote note in its old
/// folder forever while the cache believes it moved.
pub(super) fn push_needs_explicit_move_dispatch(
    save_semantics: Option<backend::SaveSemantics>,
    was_create: bool,
) -> bool {
    use backend::SaveSemantics::*;
    // A CREATE already lands in the right folder on every backend — there is
    // nothing to move.
    !was_create
        && match save_semantics {
            Some(InPlaceUpdateNeedsExplicitMove) => true,
            Some(RelocatesOnContentPush) | Some(InPlaceUpdateIncludingMove) | None => false,
        }
}

/// Whether `push_one_dirty` must issue an explicit `Transport::move_note`
/// after a content push, given the label Jodd last confirmed the remote
/// holds (`remote_label`, `None` meaning "never confirmed") and the label
/// the row currently wants (`current_label`). `None` safely defaults to "no
/// move" rather than firing spuriously — see `push_one_dirty`'s call site
/// for why that gap is bounded and self-healing (Microsoft fix 2,
/// whole-branch review 2026-08-15).
pub(super) fn note_needs_explicit_move(remote_label: Option<&str>, current_label: &str) -> bool {
    remote_label.is_some_and(|prev| prev != current_label)
}

/// What `push_one_dirty` should do with a dirty row, given what this backend
/// can write and whether the row is a CREATE.
///
/// **iCloud is why this exists.** Every other backend's `writes.notes` covers
/// content edit AND move/delete/restore in one bit, so `push_one_dirty`
/// always attempted a content push and it was always right to. iCloud's live
/// pass (2026-08-24) proved the two are independent there: move/trash/
/// restore never send `TextDataEncrypted` and are measured safe;
/// `save_note_full` refuses almost every note (`WRITABLE: 0/776`, per-
/// character CRDT identity). A dirty row can only reach here on such a
/// backend via `move_notes_batch` — the editor itself is read-only when
/// `writes.notes` is false (`canWriteAccount`, notes.ts) — so every such row
/// is, by construction, a label-only change with nothing new to send.
///
/// `MoveOnly` is refused for a CREATE: relocating an object that does not
/// exist yet is meaningless, and a backend that cannot write content cannot
/// originate a note at all.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum DirtyPushKind { Content, MoveOnly, Unsupported }

pub(super) fn dirty_push_kind(writes: backend::Writes, was_create: bool) -> DirtyPushKind {
    if writes.notes {
        DirtyPushKind::Content
    } else if !was_create && writes.relocate {
        DirtyPushKind::MoveOnly
    } else {
        DirtyPushKind::Unsupported
    }
}

/// What the sync worker tells the frontend after it puts a note's content on
/// the remote.
///
/// Carries the body rather than only an id, deliberately diverging from
/// `remote-changed`'s "an id, never content" rule. That rule exists so a
/// change NOTIFICATION cannot drift into being a second, weaker copy of the
/// read path. This is not a notification to go and read — it is a receipt
/// naming exactly which bytes the remote accepted, and the frontend has no
/// other way to learn that: re-reading the cache would hand back whatever
/// the row holds NOW, which a local edit landing mid-push has already moved
/// past.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PushConfirmation {
    pub(super) account_id: String,
    pub(super) uuid: String,
    pub(super) body_html: String,
}

/// Whether a completed push put NEW CONTENT on the remote — i.e. whether the
/// frontend's `lastPushedBody` may advance to the body that just went out.
///
/// Only a `Content` push does. A `MoveOnly` push (gotcha #24's iCloud path)
/// deliberately never sends `TextDataEncrypted`, so the remote still holds
/// whatever body it held before; telling the editor otherwise would anchor
/// `isEcho` to a body the remote has NOT got, which is the exact defect this
/// confirmation channel exists to close. `Unsupported` pushed nothing at all.
pub(super) fn push_confirms_content(kind: DirtyPushKind) -> bool {
    match kind {
        DirtyPushKind::Content => true,
        DirtyPushKind::MoveOnly | DirtyPushKind::Unsupported => false,
    }
}

#[cfg(test)]
mod push_confirms_content_tests {
    use super::*;

    #[test]
    fn a_content_push_confirms_the_body_it_sent() {
        assert!(push_confirms_content(DirtyPushKind::Content));
    }

    /// A move never sends the body, so the remote's content is unchanged —
    /// advancing `lastPushedBody` here would point it at a body the remote
    /// does not hold, re-opening the false-banner bug from the other side.
    #[test]
    fn a_move_only_push_confirms_no_content() {
        assert!(!push_confirms_content(DirtyPushKind::MoveOnly));
    }

    #[test]
    fn an_unsupported_push_confirms_nothing() {
        assert!(!push_confirms_content(DirtyPushKind::Unsupported));
    }
}

#[cfg(test)]
mod dirty_push_kind_tests {
    use super::*;

    fn writes(notes: bool, relocate: bool) -> backend::Writes {
        backend::Writes { notes, relocate, folders: false, sidecars: false }
    }

    #[test]
    fn a_backend_that_writes_content_always_takes_the_content_path() {
        // Even with relocate also true — Content covers everything a
        // content-writing backend's move/delete already do as part of the
        // save itself (Gmail) or a follow-up dispatch (Microsoft).
        assert_eq!(dirty_push_kind(writes(true, true), false), DirtyPushKind::Content);
        assert_eq!(dirty_push_kind(writes(true, false), true), DirtyPushKind::Content);
    }

    #[test]
    fn an_update_on_a_relocate_only_backend_is_move_only() {
        assert_eq!(dirty_push_kind(writes(false, true), false), DirtyPushKind::MoveOnly);
    }

    #[test]
    fn a_create_on_a_relocate_only_backend_is_unsupported_not_move_only() {
        // Nothing to relocate — there is no remote object yet, and this
        // backend cannot send the content that would create one.
        assert_eq!(dirty_push_kind(writes(false, true), true), DirtyPushKind::Unsupported);
    }

    #[test]
    fn a_backend_with_neither_bit_is_unsupported_either_way() {
        assert_eq!(dirty_push_kind(writes(false, false), false), DirtyPushKind::Unsupported);
        assert_eq!(dirty_push_kind(writes(false, false), true), DirtyPushKind::Unsupported);
    }
}

/// Applies an explicit move's `RemoteNoteVersion`, if any, onto the
/// already-pushed `SavedNote` before `push_one_dirty_db`/`mark_pushed` reads
/// it. Isolated as its own function for the same reason
/// `not_found_means_deleted_remotely`/`note_needs_explicit_move` are:
/// `push_one_dirty` needs a live `tauri::State` and cannot be unit-tested
/// directly, so the actual decision this fix depends on — a move's response,
/// when present, must WIN over the earlier content push's version, not be
/// discarded — is pulled out where a test can exercise it directly.
///
/// `None` (Gmail/LocalFs, always) is a no-op: `saved` already carries the
/// content push's version, which is correct on backends where a move never
/// touches the note's own remote object — see `Transport::move_note`'s doc
/// comment.
pub(super) fn apply_move_version(saved: &mut backend::SavedNote, moved: Option<backend::RemoteNoteVersion>) {
    if let Some(v) = moved {
        saved.version = v.version;
        saved.date = v.date;
    }
}

#[cfg(test)]
mod push_dispatch_decision_tests {
    use super::*;

    #[test]
    fn a_not_found_on_an_in_place_update_backend_update_means_deleted_remotely() {
        assert!(
            not_found_means_deleted_remotely(
                Some(backend::SaveSemantics::InPlaceUpdateNeedsExplicitMove), false
            ),
            "existing_remote_id was Some — an UPDATE, on the semantics where PATCH-404 is trustworthy"
        );
    }

    #[test]
    fn a_not_found_on_an_in_place_update_backend_create_is_not_treated_as_a_remote_deletion() {
        assert!(
            !not_found_means_deleted_remotely(
                Some(backend::SaveSemantics::InPlaceUpdateNeedsExplicitMove), true
            ),
            "no existing_remote_id — the FOLDER 404'd, not the note"
        );
    }

    /// The critical regression this gate exists to prevent: Gmail's
    /// `classify_str` substring-matches any HTTP 404 onto `NotFound`
    /// regardless of cause, so an UPDATE-shaped 404 on a `RelocatesOnContent
    /// Push` backend (Gmail) must NOT be read as "the note is gone" — that
    /// would drop a dirty row (and its tags/attachments/FTS/edges) for what
    /// is, on Gmail, a harmless retry. This is the test that fails if the
    /// `save_semantics` guard is removed or a future backend is wired to
    /// the wrong variant.
    #[test]
    fn a_not_found_on_a_relocates_on_content_push_update_is_never_treated_as_a_remote_deletion() {
        assert!(
            !not_found_means_deleted_remotely(
                Some(backend::SaveSemantics::RelocatesOnContentPush), false
            ),
            "Gmail's classify_str reaches NotFound via substring match on any 404 — \
             it is never trustworthy evidence the note is gone"
        );
    }

    #[test]
    fn a_not_found_with_no_known_save_semantics_is_not_treated_as_a_remote_deletion() {
        assert!(
            !not_found_means_deleted_remotely(None, false),
            "an unresolved account (no save_semantics) must fail closed, not drop the row"
        );
    }

    /// Confirms the routing this fix depends on: `Capabilities::for_backend`
    /// is the single place a `BackendKind` becomes a `SaveSemantics`, so
    /// `not_found_means_deleted_remotely`/the move-dispatch gate in
    /// `push_one_dirty` never need to know about `BackendKind` at all.
    #[test]
    fn capabilities_routes_each_backend_to_the_matching_save_semantics() {
        use backend::{Capabilities, SaveSemantics};
        assert_eq!(
            Capabilities::for_backend(accounts::BackendKind::Gmail).save_semantics,
            SaveSemantics::RelocatesOnContentPush
        );
        assert_eq!(
            Capabilities::for_backend(accounts::BackendKind::LocalFs).save_semantics,
            SaveSemantics::RelocatesOnContentPush
        );
        assert_eq!(
            Capabilities::for_backend(accounts::BackendKind::Microsoft).save_semantics,
            SaveSemantics::InPlaceUpdateNeedsExplicitMove
        );
        assert_eq!(
            Capabilities::for_backend(accounts::BackendKind::ICloud).save_semantics,
            SaveSemantics::InPlaceUpdateIncludingMove
        );
    }

    /// The two halves of `SaveSemantics` travel together, and iCloud is the
    /// first backend that needs one of each: its content push carries the
    /// folder (so no explicit move), and its `NotFound` on an update is still
    /// trustworthy (so a note deleted on the iPhone stops retrying).
    #[test]
    fn a_push_that_carries_the_folder_issues_no_move_but_still_believes_a_not_found() {
        use backend::SaveSemantics::*;
        assert!(
            !push_needs_explicit_move_dispatch(Some(InPlaceUpdateIncludingMove), false),
            "the folder went out with the content — a second write would CONFLICT on a \
             tag the first one just bumped"
        );
        assert!(
            not_found_means_deleted_remotely(Some(InPlaceUpdateIncludingMove), false),
            "the write targets an existing recordName, so a NotFound means what it says"
        );
        assert!(
            !not_found_means_deleted_remotely(Some(InPlaceUpdateIncludingMove), true),
            "a CREATE that 404s means the destination folder vanished, not the note"
        );
    }

    #[test]
    fn no_move_is_issued_when_the_label_has_not_changed() {
        assert!(!note_needs_explicit_move(Some("Notes"), "Notes"), "must not move on an ordinary content-only save");
    }

    #[test]
    fn a_move_is_issued_when_the_label_changed() {
        assert!(note_needs_explicit_move(Some("Notes"), "Notes/Ideas"));
    }

    #[test]
    fn an_unconfirmed_remote_label_defaults_to_no_move() {
        // Never yet confirmed (pre-migration row, or a row that has never
        // been pushed or pulled) — the safe default is NOT to move, rather
        // than firing spuriously on every edit. Self-heals via the very
        // next upsert_from_remote or mark_pushed.
        assert!(!note_needs_explicit_move(None, "Notes/Ideas"));
    }

    fn saved_note(version: &str, date: &str) -> backend::SavedNote {
        backend::SavedNote {
            id: "MSG-ID".into(),
            version: version.into(),
            uuid: "note-uuid".into(),
            date: date.into(),
            body_html: "<div>b</div>".into(),
            local_version: 0,
        }
    }

    /// The regression this guards: before this fix, a move's response was
    /// discarded and the content push's (now stale) version/date were
    /// persisted regardless — silently desyncing `notes.remote_version` and
    /// risking a spurious conflict copy on the next poll.
    #[test]
    fn a_move_response_overrides_the_content_pushs_version() {
        let mut saved = saved_note("2026-08-16T10:00:00Z", "Sun, 16 Aug 2026 10:00:00 +0000");
        let moved = Some(backend::RemoteNoteVersion {
            version: "2026-08-16T10:05:00Z".into(),
            date: "Sun, 16 Aug 2026 10:05:00 +0000".into(),
        });

        apply_move_version(&mut saved, moved);

        assert_eq!(saved.version, "2026-08-16T10:05:00Z", "the move's version must win");
        assert_eq!(saved.date, "Sun, 16 Aug 2026 10:05:00 +0000", "the move's date must win");
    }

    /// Gmail/LocalFs always return `None` from `move_note` — must be a
    /// pure no-op, leaving the content push's version/date exactly as they
    /// were (those backends' move never touches the note's own object).
    #[test]
    fn no_move_response_leaves_the_content_pushs_version_untouched() {
        let mut saved = saved_note("2026-08-16T10:00:00Z", "Sun, 16 Aug 2026 10:00:00 +0000");
        let original = saved.clone();

        apply_move_version(&mut saved, None);

        assert_eq!(saved.version, original.version);
        assert_eq!(saved.date, original.date);
    }
}

pub(super) async fn push_one_dirty(
    state: &State<'_, AppState>,
    n: &db::CachedNote,
) -> Result<Option<PushConfirmation>, String> {
    let existing_gmail_id = if n.id.is_empty() { None } else { Some(n.id.as_str()) };
    // Captured before save_note_full runs: whether THIS push is a create
    // (no remote object exists yet) is exactly "no id going in" — the
    // signal push_one_dirty_db needs to decide whether to persist `id`
    // unconditionally afterward. See push_one_dirty_db's doc comment.
    let was_create = existing_gmail_id.is_none();
    let existing_uuid = Some(n.uuid.as_str());
    let existing_x_mail = n.x_mail_created_date.as_deref();
    // Load this note's stored attachments so save_note can re-emit any the body
    // still references (multipart/related) instead of stripping them.
    let attachments = state
        .db
        .list_attachments(&n.account_id, &n.uuid)
        .unwrap_or_default();
    // `Capabilities::for_backend(_).save_semantics`, not the raw
    // `backend_kind` — see `not_found_means_deleted_remotely`'s doc comment
    // for why this indirection is itself the altitude fix, not decoration.
    let (save_semantics, writes) = {
        let list = state.accounts.lock().unwrap();
        let caps = list.iter().find(|a| a.id == n.account_id)
            .map(|a| backend::Capabilities::for_backend(a.backend_kind));
        (caps.map(|c| c.save_semantics), caps.map(|c| c.writes))
    };
    let v = vertical_for(state, &n.account_id).await?;

    // `None` (account not found — should not happen, `vertical_for` above
    // would already have refused) defaults to `Writes::ALL` rather than
    // `Unsupported`: this branch must never CHANGE behaviour for a backend
    // it does not recognise, and every backend before iCloud always took the
    // content path regardless.
    match dirty_push_kind(writes.unwrap_or(backend::Writes::ALL), was_create) {
        DirtyPushKind::Unsupported => {
            let reason = "this backend cannot accept this write".to_string();
            log!(
                "push_one_dirty: uuid={} — {} (writes.notes=false, writes.relocate=false, \
                 or a create with content refused)",
                n.uuid, reason
            );
            state.db.mark_push_blocked(&n.uuid, &n.account_id, &reason)
                .map_err(|e| e.to_string())?;
            return Err(reason);
        }
        // **Bypasses `save_note_full` entirely.** The whole point: a backend
        // in this branch refuses almost every content push
        // (`compose::writability`'s CRDT gate on iCloud), and this row's
        // content did not change — only `move_notes_batch` can dirty a row
        // here, per this function's own doc comment. Sending it through
        // `save_note_full` anyway would hit that gate for no reason and
        // permanently block a note whose folder change CloudKit accepts fine
        // on its own (measured live, `icloud_relocation_selftest`, 2026-08-24).
        DirtyPushKind::MoveOnly => {
            let moved = match v.move_note(&n.id, std::slice::from_ref(&n.label), &[]).await {
                Ok(m) => m,
                Err(backend::TransportError::NotFound)
                    if not_found_means_deleted_remotely(save_semantics, /* was_create */ false) =>
                {
                    log!(
                        "push_one_dirty: uuid={} remote id={} 404'd on a move-only push — \
                         deleted remotely while dirty locally; dropping the local row",
                        n.uuid, n.id
                    );
                    state.db.delete(&n.uuid, &n.account_id).map_err(|e| e.to_string())?;
                    return Ok(None);
                }
                Err(e @ backend::TransportError::Permanent { .. }) => {
                    let reason = e.to_string();
                    log!(
                        "push_one_dirty: uuid={} move-only push refused permanently: {}",
                        n.uuid, reason
                    );
                    state.db.mark_push_blocked(&n.uuid, &n.account_id, &reason)
                        .map_err(|e| e.to_string())?;
                    return Err(reason);
                }
                Err(e) => return Err(e.to_string()),
            };
            // iCloud's `move_note` always returns `Some` — it writes the
            // note's own record, so it always has a fresh version to report
            // (see `Transport::move_note`'s doc comment). `None` is only
            // possible for a hypothetical future MoveOnly backend whose move
            // is genuinely separate from the note object; falling back to
            // what was already cached is the safe default for that case,
            // not a path this account's writes take.
            let (version, date) = match moved {
                Some(v) => (v.version, v.date),
                None => (n.remote_version.clone().unwrap_or_default(), n.date.clone()),
            };
            let saved = backend::SavedNote {
                id: n.id.clone(),
                version,
                uuid: n.uuid.clone(),
                date,
                body_html: n.body_html.clone(),
                local_version: 0,
            };
            // A move never assigns a new identity — rekeyed is always false.
            push_one_dirty_db(&state.db, n, &saved, &n.uuid, false, false)?;
            // No content receipt: `push_confirms_content(MoveOnly)` is false
            // because this path never sent a body.
            debug_assert!(!push_confirms_content(DirtyPushKind::MoveOnly));
            return Ok(None);
        }
        DirtyPushKind::Content => {}
    }

    let op = backend::SaveOp {
        title: &n.title, body_html: &n.body_html,
        existing_remote_id: existing_gmail_id, existing_uuid,
        existing_created_date: existing_x_mail, label: &n.label,
    };
    let mut saved = match v.save_note_full(&op, &attachments).await {
        Ok(s) => s,
        // The note exists locally and is dirty, but no longer exists
        // remotely — measured on Microsoft: `classify_status` maps a PATCH's
        // 404 to `NotFound`, which is exactly what an Apple-side hard delete
        // (no undo path — `Capabilities::has_trash` is false on measured
        // evidence) leaves behind for a stale id. Left unhandled the row
        // retries every tick forever: `has_pending_pushes` never clears for
        // this uuid, the account can never leave Draining, and
        // `remove_account` refuses a Draining account (gotcha #2) — the
        // account becomes permanently unremovable.
        //
        // MUST be gated to `InPlaceUpdateNeedsExplicitMove`
        // (`not_found_means_deleted_remotely` takes `save_semantics`, not
        // just `was_create`). `RelocatesOnContentPush` backends (Gmail) are
        // NOT exempt by construction: every save failure routes through
        // `classify_str` (`backend/gmail/transport.rs`), a substring test —
        // `err.contains(" 404")` against `format!("Save failed {}: {}",
        // status, text)` (`backend/gmail/wire.rs`) — so any HTTP 404 from
        // `POST /gmail/v1/users/me/messages` (rate limiting, a malformed
        // body, an account-level restriction — nothing to do with the note
        // being gone) yields `TransportError::NotFound` too, with
        // `was_create == false` for any note that already has an id. Without
        // the gate this arm would silently destroy the user's unpushed
        // edit — tags, attachments, FTS, edges, all of it — on Gmail, the
        // product's primary backend, for what was previously a harmless
        // retry.
        //
        // Only on an UPDATE (an existing remote id that's now gone) — a
        // CREATE 404ing means the destination FOLDER vanished, a different
        // and much rarer failure this fix does not attempt to characterise;
        // it falls through to the generic error path below and keeps
        // retrying, same as before this fix.
        Err(backend::TransportError::NotFound) if not_found_means_deleted_remotely(save_semantics, was_create) => {
            // Drop the row rather than re-create it. Re-creating would
            // silently resurrect the note under a fresh remote identity —
            // undoing an Apple-side delete that has, by design, no undo path
            // to offer (measured: Deleted Items held zero items afterward).
            // The pending local edit is lost, but that mirrors what already
            // happened on every OTHER replica the moment the user deleted
            // the note there; clearing `dirty` without deleting the row
            // would be worse — it would mark a note "synced" that was never
            // actually written anywhere.
            log!(
                "push_one_dirty: uuid={} remote id={} 404'd on push — deleted remotely \
                 while dirty locally; dropping the local row rather than retrying forever \
                 or resurrecting a note the user explicitly deleted",
                n.uuid, n.id
            );
            state.db.delete(&n.uuid, &n.account_id).map_err(|e| e.to_string())?;
            return Ok(None);
        }
        // Refused permanently: retrying THIS push unchanged can never
        // succeed (see `TransportError`'s doc comment). Every other arm
        // below flattens the error to a String and lets the worker log it,
        // which for a Permanent means re-issuing the identical doomed
        // request every 5 seconds until the app is closed — measured
        // 2026-08-17 on a Microsoft account whose Notes folder had no
        // discoverable id: 5,816 attempts, zero successes, and the note
        // meanwhile showed "Saved" in the toolbar and then dropped out of
        // the list entirely (no list path returns a note the remote has
        // never seen).
        //
        // Record the reason on the row and stop. Three things this
        // deliberately does NOT do:
        //
        //   - It does not clear `dirty`. The note really does hold unsent
        //     content; saying otherwise is the lie being fixed.
        //   - It does not delete the row the way `push_folder` does for a
        //     folder it cannot create. For a note that is data loss.
        //   - It does not schedule its own retry. `Db::clear_push_blocks`
        //     (user action) and `apply_local_edit` (user edits again) are
        //     the re-arm paths; see `clear_push_blocks` for why there is no
        //     automatic one.
        //
        // `has_pending_pushes` stops counting the row once it is blocked, so
        // this cannot wedge a Draining account the way the unhandled case
        // could have (gotcha #2).
        Err(e @ backend::TransportError::Permanent { .. }) => {
            let reason = e.to_string();
            log!(
                "push_one_dirty: uuid={} refused permanently — recording it on the row and \
                 giving up rather than retrying every tick: {}",
                n.uuid, reason
            );
            state.db.mark_push_blocked(&n.uuid, &n.account_id, &reason)
                .map_err(|e| e.to_string())?;
            return Err(reason);
        }
        Err(e) => return Err(e.to_string()),
    };

    // Exchange's PATCH is a real in-place update that never touches
    // `parentFolderId` (spec §B1's third dispatch — `POST /me/messages/
    // {id}/move` — was never wired anywhere before this fix). A label change
    // on this backend therefore needs an EXPLICIT move after the PATCH, or
    // the remote note stays in its old folder forever while the local cache
    // believes it moved (Critical 2, whole-branch review 2026-08-15).
    //
    // Scoped to `InPlaceUpdateNeedsExplicitMove` backends, not dispatched
    // generically for every backend: `RelocatesOnContentPush` backends
    // (Gmail, LocalFs) already relocate the note as an intrinsic part of
    // `save_note_full` (Gmail inserts under the new label and trashes the
    // old message; LocalFs writes into the new folder's directory and
    // removes the old file), so an extra `move_note` call there would be
    // redundant at best — and for Gmail specifically WRONG, since its
    // `Transport::move_note` expects resolved Gmail label ids (see
    // `restore_note`'s `id_of` resolution), not the Jodd PATH this check
    // has on hand.
    //
    // "Previous remote folder" comes from the cached row's `remote_label`
    // (db.rs migration #16) rather than a fetch or a `folder_ids` reverse
    // lookup: a fetch would cost a full mailbox scan on EVERY content edit
    // (`fetch_note`'s own doc comment prices that at "an explicit user
    // action", which a 5-second worker tick is not), and `folder_ids` is
    // Microsoft-internal to the vertical with no path here to read it from.
    // `remote_label` is a plain local-first SQLite read, and it doubles as
    // the "did the label actually change" signal: `None` (never confirmed,
    // e.g. before this migration ships or before the row's first push)
    // safely defaults to "no move" rather than firing on every edit — the
    // gap is bounded to one push and self-heals via the very next
    // `upsert_from_remote` or `mark_pushed`. Only runs on an UPDATE: a
    // CREATE already lands directly in the right folder via `folder_ids`
    // inside `save_note_full`, so there is nothing to move.
    if push_needs_explicit_move_dispatch(save_semantics, was_create) {
        let remote_label = state.db.get_remote_label(&n.uuid, &n.account_id).map_err(|e| e.to_string())?;
        if note_needs_explicit_move(remote_label.as_deref(), &n.label) {
            // The move PATCHes the SAME message the content push above may
            // have just PATCHed — its response, not the content push's, is
            // the authoritative version afterward. See `apply_move_version`.
            let moved = v.move_note(&saved.id, std::slice::from_ref(&n.label), &[])
                .await
                .map_err(|e| e.to_string())?;
            apply_move_version(&mut saved, moved);
        }
    }

    // The backend may assign an identity Jodd cannot choose, and the cache
    // must converge to it. Exchange does this on every create
    // (internetMessageId); Gmail does it too whenever the cached uuid fails
    // to canonicalize and `resolve_uuid_for_save` (gmail/wire.rs) mints a
    // fresh one — so this is backend-agnostic, not Microsoft-only, even
    // though Microsoft's create path is the only place it fires today in
    // practice. Guarding on is_empty keeps LocalFs (which may not fill uuid)
    // on its current path.
    let pushed_uuid = if saved.uuid.is_empty() { n.uuid.clone() } else { saved.uuid.clone() };
    let rekeyed = pushed_uuid != n.uuid;

    // Extend the in-flight guard to the NEW uuid before the row moves — see
    // PushingGuard's doc comment for the race this closes. `_guard` releases
    // on every exit from this scope, including the `?`s inside
    // push_one_dirty_db, so no fallible step between here and mark_pushed
    // can leak the entry.
    let _guard = rekeyed.then(|| {
        PushingGuard::new(&state.pushing, (n.account_id.clone(), pushed_uuid.clone()))
    });

    push_one_dirty_db(&state.db, n, &saved, &pushed_uuid, rekeyed, was_create)?;

    // The content receipt. `saved.body_html` is what actually reached the
    // backend, and `pushed_uuid` is the identity the row now lives under (a
    // create may have rekeyed it out from under the open editor — gotcha
    // #16), so the frontend can match it against the note it is showing.
    Ok(push_confirms_content(DirtyPushKind::Content).then(|| PushConfirmation {
        account_id: n.account_id.clone(),
        uuid: pushed_uuid,
        body_html: saved.body_html.clone(),
    }))
}

/// The DB-only tail of `push_one_dirty` — rekey (if the backend assigned a
/// new identity), persist the remote id unconditionally when this push was a
/// CREATE, then the optimistic-locked `mark_pushed`. Split out the same way
/// `reconcile_one_db` is (see its doc comment): `tauri::State` has no public
/// constructor outside a running Tauri app, so this is what makes the
/// sequence testable at all — including the duplicate-create race a
/// fix-round-1 review on Task 6 caught (`push_one_dirty_db_tests` below).
pub(super) fn push_one_dirty_db(
    db: &db::Db,
    n: &db::CachedNote,
    saved: &backend::SavedNote,
    pushed_uuid: &str,
    rekeyed: bool,
    was_create: bool,
) -> Result<(), String> {
    if rekeyed {
        db.rekey_note_uuid(&n.uuid, pushed_uuid, &n.account_id).map_err(|e| e.to_string())?;
    }

    if was_create {
        // Unconditional, deliberately NOT gated by mark_pushed's
        // local_version guard below — see `Db::set_remote_id`'s doc comment.
        // Without this, a local edit landing mid-flight makes mark_pushed's
        // guarded UPDATE a no-op, `id` stays empty, and the NEXT tick reads
        // `existing_remote_id: None` again — taking the CREATE branch a
        // second time. On Microsoft that mints a second, genuinely distinct
        // `internetMessageId`: a visible duplicate note with no dedupe to
        // collapse it (`find_ids_for_uuid` is still unimplemented,
        // `DedupSummary` is always empty for this backend).
        db.set_remote_id(pushed_uuid, &n.account_id, &saved.id).map_err(|e| e.to_string())?;
    }

    // No separate re-derive call follows: `rekey_note_uuid` already moves
    // `note_tags`/`tag_tombstones`/`attachments`/`edges` to `pushed_uuid` and
    // re-indexes FTS under it, all inside its own transaction (see that
    // function's doc comment in db.rs). `saved.body_html` is byte-identical
    // to `n.body_html` here (save_note_full always returns the editor-view
    // input unchanged), so the content `rekey_note_uuid` just re-derived FTS
    // from is exactly what `mark_pushed` below writes back — nothing for a
    // second derivation pass to catch that the first one missed.
    db.mark_pushed(
        pushed_uuid,
        &n.account_id,
        &saved.id,
        &saved.version,
        &saved.date,
        &saved.body_html,
        n.local_version,
    ).map_err(|e| e.to_string())?;
    Ok(())
}

/// The merge that keeps an unsendable note on screen.
///
/// Measured 2026-08-17: `list_cached_notes` reported 2 notes for the account
/// while `list_notes` reported 0 on three consecutive polls, because both list
/// paths are built from what the backend returned and a note whose create was
/// refused has no backend object. The frontend reads "absent from the fetch"
/// as "gone" after a 30 s grace window, so the note the toolbar had just
/// called "Saved" left the list on its own.
#[cfg(test)]
mod append_blocked_notes_tests {
    use super::*;
    use crate::test_support::note;

    fn temp_db() -> db::Db {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::mem::forget(dir);
        db::Db::open_unencrypted(&path).expect("open temp db")
    }

    #[test]
    fn a_note_the_remote_has_never_seen_is_added_back_to_the_listing() {
        let db = temp_db();
        let acct = "a@example.com";
        note(acct, "u1").insert(&db);
        db.mark_push_blocked("u1", acct, "permanent: no Exchange folder id").unwrap();

        // What the backend returned: nothing. This IS the measured case.
        let mut result: Vec<gmail::Note> = Vec::new();
        append_blocked_notes(&db, acct, None, &mut result);

        assert_eq!(result.len(), 1, "the note must not vanish just because Graph never saw it");
        assert_eq!(result[0].uuid, "u1");
        assert!(result[0].push_blocked_reason.is_some(), "and it must carry WHY");
    }

    /// A blocked note that the remote DOES return (a blocked update, where the
    /// old remote copy still exists) must appear once, not twice.
    #[test]
    fn a_blocked_note_the_remote_still_holds_is_not_duplicated() {
        let db = temp_db();
        let acct = "a@example.com";
        note(acct, "u1").insert(&db);
        db.mark_push_blocked("u1", acct, "permanent: nope").unwrap();

        let mut result = vec![db.get("u1", acct).unwrap().unwrap().to_frontend_note()];
        append_blocked_notes(&db, acct, None, &mut result);

        assert_eq!(result.len(), 1);
    }

    /// The folder-scoped path replaces exactly the notes in one folder, so a
    /// blocked note from a different folder would be filed in the wrong place
    /// rather than merely shown twice.
    #[test]
    fn the_folder_scoped_merge_only_adds_notes_from_that_folder() {
        let db = temp_db();
        let acct = "a@example.com";
        note(acct, "u1").label("Notes").insert(&db);
        note(acct, "u2").label("Notes/Other").insert(&db);
        db.mark_push_blocked("u1", acct, "permanent: nope").unwrap();
        db.mark_push_blocked("u2", acct, "permanent: nope").unwrap();

        let mut result: Vec<gmail::Note> = Vec::new();
        append_blocked_notes(&db, acct, Some("Notes"), &mut result);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].uuid, "u1");
    }

    /// Another account's problems must not appear in this account's listing.
    #[test]
    fn the_merge_is_scoped_to_one_account() {
        let db = temp_db();
        note("a@example.com", "u1").insert(&db);
        note("b@example.com", "u2").insert(&db);
        db.mark_push_blocked("u2", "b@example.com", "permanent: nope").unwrap();

        let mut result: Vec<gmail::Note> = Vec::new();
        append_blocked_notes(&db, "a@example.com", None, &mut result);

        assert!(result.is_empty());
    }
}

#[cfg(test)]
mod push_one_dirty_db_tests {
    use super::*;

    fn saved_note(id: &str, uuid: &str, version: &str, body_html: &str) -> backend::SavedNote {
        backend::SavedNote {
            id: id.to_string(),
            version: version.to_string(),
            uuid: uuid.to_string(),
            date: "Mon, 14 Aug 2026 10:00:00 +0700".to_string(),
            body_html: body_html.to_string(),
            local_version: 0,
        }
    }

    /// The exact race fix-round-1's review (finding 1) caught, reproduced at
    /// the DB layer `reconcile_one_db` already proved out is testable
    /// without `tauri::State`.
    ///
    /// A note is created locally (dirty, `id=""`, `local_version=1`). The
    /// worker starts pushing it — captures `n.local_version=1` — but before
    /// the push's DB tail runs, a second local edit lands and bumps
    /// `local_version` to 2. The push then succeeds remotely (Exchange
    /// assigns `saved.id`), so `push_one_dirty_db` runs with the STALE
    /// `n.local_version=1` a caller captured before that edit.
    ///
    /// `mark_pushed`'s optimistic lock correctly refuses to match (the row
    /// really has moved past what this push reflects) — the row must stay
    /// `Dirty` with the newer body intact. But `id` must NOT stay empty:
    /// that would make the next tick's `existing_remote_id` read `None`
    /// again and re-CREATE, minting a second Exchange identity for what the
    /// user experiences as one note.
    #[test]
    fn a_concurrent_edit_during_a_create_does_not_leave_id_empty() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        let n0 = db::CachedNote {
            uuid: "placeholder-uuid".to_string(),
            account_id: "acc".to_string(),
            id: String::new(),
            title: "Title".to_string(),
            body_html: "<p>v1</p>".to_string(),
            date: "Mon, 14 Aug 2026 09:00:00 +0700".to_string(),
            x_mail_created_date: None,
            label: "Notes".to_string(),
            local_version: 1,
            remote_version: None,
            sync_state: db::SyncState::Dirty,
            last_synced_at: None,
            last_local_modified_at: db::now_ms(),
            last_remote_modified_at: None,
            pinned: false,
            meta_msg_id: None,
            pin_dirty: false,
            push_blocked_reason: None,
        };
        db.insert_local_new(&n0).unwrap();
        // The worker's snapshot of the row (n), captured before the push —
        // still local_version 1.
        let n = db.get(&n0.uuid, "acc").unwrap().unwrap();
        assert_eq!(n.local_version, 1);

        // A second edit lands mid-flight: local_version -> 2, still dirty.
        db.apply_local_edit(&n0.uuid, "acc", "Title", "<p>v1</p><p>appended</p>", "Notes").unwrap();

        // The push completes: Exchange assigned a real identity, different
        // from the local placeholder — a create, so `was_create=true` and
        // `rekeyed=true`.
        let saved = saved_note("GRAPH-ID", "real@exchange", "2026-08-14T10:00:00Z", "<p>v1</p>");
        push_one_dirty_db(&db, &n, &saved, "real@exchange", true, true).unwrap();

        let row = db.get("real@exchange", "acc").unwrap().unwrap();
        assert_eq!(row.id, "GRAPH-ID",
            "id must be remembered even though the concurrent edit made mark_pushed's guard miss — \
             otherwise the next tick reads existing_remote_id=None and creates a SECOND note");
        assert_eq!(row.sync_state, db::SyncState::Dirty,
            "mark_pushed's guard must still refuse to clean the row — the newer edit isn't pushed yet");
        assert_eq!(row.body_html, "<p>v1</p><p>appended</p>",
            "the newer local content must survive, not the stale pushed body");
    }

    /// Companion to the race test above: when nothing races the push (the
    /// common case), the DB tail still ends up fully converged — rekeyed,
    /// `id`/`remote_version` stamped, and clean.
    #[test]
    fn an_uncontested_create_ends_up_clean_with_the_real_identity() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        let n0 = db::CachedNote {
            uuid: "placeholder-uuid".to_string(),
            account_id: "acc".to_string(),
            id: String::new(),
            title: "Title".to_string(),
            body_html: "<p>v1</p>".to_string(),
            date: "Mon, 14 Aug 2026 09:00:00 +0700".to_string(),
            x_mail_created_date: None,
            label: "Notes".to_string(),
            local_version: 1,
            remote_version: None,
            sync_state: db::SyncState::Dirty,
            last_synced_at: None,
            last_local_modified_at: db::now_ms(),
            last_remote_modified_at: None,
            pinned: false,
            meta_msg_id: None,
            pin_dirty: false,
            push_blocked_reason: None,
        };
        db.insert_local_new(&n0).unwrap();
        let n = db.get(&n0.uuid, "acc").unwrap().unwrap();

        let saved = saved_note("GRAPH-ID", "real@exchange", "2026-08-14T10:00:00Z", "<p>v1</p>");
        push_one_dirty_db(&db, &n, &saved, "real@exchange", true, true).unwrap();

        let row = db.get("real@exchange", "acc").unwrap().unwrap();
        assert_eq!(row.id, "GRAPH-ID");
        assert_eq!(row.remote_version, Some("2026-08-14T10:00:00Z".to_string()));
        assert_eq!(row.sync_state, db::SyncState::Clean);
        assert!(db.get(&n0.uuid, "acc").unwrap().is_none(), "the placeholder uuid must be gone after rekey");
    }

    /// The duplicate-on-edit defect, end to end: push → DB → push again.
    ///
    /// This is deliberately NOT a transport test. `ms_write_probe` drove the
    /// Graph calls directly and reported "create / patch / retitle / move /
    /// delete all 2xx", which was true and said nothing about whether the
    /// next edit finds the row the push left behind — so M2 shipped a defect
    /// that multiplied the user's real mailbox. CLAUDE.md's own rule, in a
    /// new costume: a gate that differs from the merge gate only proves it
    /// agrees with itself.
    ///
    /// The sequence measured on kaiwan.h@live.com 2026-08-18:
    ///
    ///   1. The editor creates a note. Row is dirty, `id=""`, keyed by a
    ///      locally-minted Apple-shaped uuid.
    ///   2. The worker pushes. Exchange assigns `internetMessageId`, so the
    ///      row is REKEYED to it — the uuid the editor is holding no longer
    ///      names any row.
    ///   3. The user keeps typing. The editor autosaves under the uuid it
    ///      still holds, because nothing told it otherwise.
    ///
    /// Before the fix, step 3 inserted a second dirty row with an empty
    /// `id`, the worker CREATEd it, and one note became two — then three.
    #[test]
    fn an_edit_arriving_under_a_pre_rekey_uuid_updates_the_note_instead_of_duplicating_it() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();

        // 1. The editor creates the note and saves it once.
        let created = save_note_db(
            &db, "acc", accounts::BackendKind::Microsoft, None, "Note 1", "<p>first line</p>", "Notes", None, None,
        ).unwrap();
        let editor_uuid = created.uuid.clone();
        assert!(created.id.is_empty(), "nothing pushed yet");

        // 2. The worker pushes it; Exchange assigns the identity.
        let n = db.get(&editor_uuid, "acc").unwrap().unwrap();
        let saved = saved_note("GRAPH-ID", "<msg1@exchange>", "2026-08-18T04:21:04Z", "<p>first line</p>");
        push_one_dirty_db(&db, &n, &saved, "<msg1@exchange>", true, true).unwrap();
        assert!(db.get(&editor_uuid, "acc").unwrap().is_none(),
            "precondition: the rekey really did move the row out from under the editor's uuid");

        // 3. The user types more. The editor still holds the PRE-rekey uuid
        //    — it was never told about the change — and `expected_local_
        //    version` is likewise the value it last saw.
        let after = save_note_db(
            &db, "acc", accounts::BackendKind::Microsoft, Some(&editor_uuid), "Note 1", "<p>first line</p><p>second line</p>",
            "Notes", None, Some(created.local_version),
        ).unwrap();

        let all = db.list_notes("acc").unwrap();
        assert_eq!(all.len(), 1,
            "one note the user edited twice must be ONE row, not two — a second row here is a \
             second Exchange note on the next tick, with no dedupe to collapse it");
        assert_eq!(after.uuid, "<msg1@exchange>",
            "the edit must land on the rekeyed row, and the response must echo the note's \
             CURRENT uuid — that echo is how the editor stops using the stale one");
        assert_eq!(after.id, "GRAPH-ID",
            "the surviving row keeps the remote id, so the next push PATCHes in place \
             instead of taking save_note_full's create branch again");
        assert_eq!(after.body_html, "<p>first line</p><p>second line</p>",
            "the user's newer text must be what gets pushed");
        assert_eq!(after.sync_state, db::SyncState::Dirty, "the new edit still needs pushing");
    }

    /// The forwarding address must survive being followed twice, and must
    /// not depend on the editor ever catching up.
    ///
    /// A user typing steadily produces exactly this: every push rekeys, and
    /// every autosave in between is still addressed to the ORIGINAL uuid the
    /// editor was handed when the note was created. Chain collapsing in
    /// `rekey_note_uuid` is what keeps that resolvable after the second
    /// rekey rather than dead-ending at an intermediate identity.
    #[test]
    fn a_stale_uuid_still_resolves_after_a_second_rekey() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();

        let created = save_note_db(&db, "acc", accounts::BackendKind::Microsoft, None, "N", "<p>a</p>", "Notes", None, None).unwrap();
        let original_uuid = created.uuid.clone();

        let n = db.get(&original_uuid, "acc").unwrap().unwrap();
        let s1 = saved_note("GRAPH-1", "<msg1@exchange>", "2026-08-18T04:21:00Z", "<p>a</p>");
        push_one_dirty_db(&db, &n, &s1, "<msg1@exchange>", true, true).unwrap();

        // A second rekey of the SAME note (the identity changed again).
        let n2 = db.get("<msg1@exchange>", "acc").unwrap().unwrap();
        let s2 = saved_note("GRAPH-2", "<msg2@exchange>", "2026-08-18T04:21:19Z", "<p>a</p>");
        push_one_dirty_db(&db, &n2, &s2, "<msg2@exchange>", true, true).unwrap();

        // The editor never learned either new uuid.
        let after = save_note_db(
            &db, "acc", accounts::BackendKind::Microsoft, Some(&original_uuid), "N", "<p>a</p><p>b</p>", "Notes", None, None,
        ).unwrap();

        assert_eq!(after.uuid, "<msg2@exchange>",
            "A→B→C must resolve straight to C; stopping at B would leave the edit on a row \
             that no longer exists and insert a duplicate anyway");
        assert_eq!(db.list_notes("acc").unwrap().len(), 1);
    }

    /// The forwarding address must not resurrect a deleted note.
    ///
    /// If the rekeyed row is gone, the alias points nowhere. Following it
    /// anyway would file the user's edit under the REMOTE identity of a note
    /// they deleted, and the next poll would show it back. Falling back to
    /// the uuid as sent keeps this on the ordinary brand-new-note path.
    #[test]
    fn an_alias_to_a_deleted_row_does_not_resurrect_it_under_the_remote_identity() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();

        let created = save_note_db(&db, "acc", accounts::BackendKind::Microsoft, None, "N", "<p>a</p>", "Notes", None, None).unwrap();
        let editor_uuid = created.uuid.clone();
        let n = db.get(&editor_uuid, "acc").unwrap().unwrap();
        let saved = saved_note("GRAPH-ID", "<msg1@exchange>", "2026-08-18T04:21:00Z", "<p>a</p>");
        push_one_dirty_db(&db, &n, &saved, "<msg1@exchange>", true, true).unwrap();

        db.delete("<msg1@exchange>", "acc").unwrap();

        let after = save_note_db(
            &db, "acc", accounts::BackendKind::Microsoft, Some(&editor_uuid), "N", "<p>a</p>", "Notes", None, None,
        ).unwrap();
        assert_eq!(after.uuid, editor_uuid,
            "with the target gone the alias must be ignored, not followed onto a dead row");
        assert!(after.id.is_empty(), "this is a genuinely new note, not the deleted one revived");
    }

    /// The iCloud identity trap, in the shape gotcha #16 says a regression
    /// test here must take: save → DB → save, never the wire.
    ///
    /// A CloudKit `recordName` is a plain **lowercase** UUID, so it parses —
    /// which is exactly why the old unconditional `canonicalize_uuid` call
    /// would have rewritten it. The row is keyed by the id as stored, so the
    /// second save would miss, fall into the brand-new-note branch, and leave
    /// TWO rows for one note. That is the same defect that turned one
    /// Exchange note into three (measured 2026-08-18), reached by a different
    /// road, and it would have surfaced only as "a lookup that 404s after the
    /// first save".
    #[test]
    fn an_icloud_record_name_survives_a_save_without_being_uppercased() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();

        // Exactly the recordName the live probe returned (docs/PRIOR-ART.md).
        let record_name = "f8bf619a-1b84-40eb-932d-6318ee9aeeb4";

        let first = save_note_db(
            &db, "acc", accounts::BackendKind::ICloud, Some(record_name),
            "Note", "<div>one</div>", "Notes", None, None,
        ).unwrap();
        assert_eq!(first.uuid, record_name, "the record's own identity must be what keys the row");

        // The editor saves again under the id it was handed.
        let second = save_note_db(
            &db, "acc", accounts::BackendKind::ICloud, Some(record_name),
            "Note", "<div>one</div><div>two</div>", "Notes", None,
            Some(first.local_version),
        ).unwrap();

        assert_eq!(second.uuid, record_name);
        assert_eq!(
            db.list_notes("acc").unwrap().len(),
            1,
            "one note edited twice must be ONE row — an uppercased uuid misses the lookup and \
             inserts a second"
        );
        assert_eq!(second.body_html, "<div>one</div><div>two</div>");

        // And the contrast that makes the policy visible: the same string on
        // Gmail IS uppercased, because Apple's email backend reconciles
        // `X-Universally-Unique-Identifier` by strcmp.
        let on_gmail = save_note_db(
            &db, "gmail-acc", accounts::BackendKind::Gmail, Some(record_name),
            "Note", "<p>x</p>", "Notes", None, None,
        ).unwrap();
        assert_eq!(on_gmail.uuid, record_name.to_uppercase());
    }
}

#[cfg(test)]
mod sidecar_gate_tests {
    use super::*;

    fn dirty_note(uuid: &str, account_id: &str) -> db::CachedNote {
        db::CachedNote {
            uuid: uuid.to_string(),
            account_id: account_id.to_string(),
            id: "remote-id".to_string(),
            title: "Title".to_string(),
            body_html: "<p>#work</p>".to_string(),
            date: "Mon, 14 Aug 2026 09:00:00 +0700".to_string(),
            x_mail_created_date: None,
            label: "Notes".to_string(),
            local_version: 1,
            remote_version: Some("v1".to_string()),
            sync_state: db::SyncState::Clean,
            last_synced_at: None,
            last_local_modified_at: db::now_ms(),
            last_remote_modified_at: None,
            pinned: false,
            meta_msg_id: None,
            pin_dirty: false,
        push_blocked_reason: None,
        }
    }

    /// Before M4, a Microsoft account's `pin_dirty` row (set via `set_pin`)
    /// would retry `put_sidecar` forever (`Err(milestone_2())`), so this
    /// short-circuit cleared it instead of pushing — otherwise
    /// `has_pending_pushes` would never clear, permanently blocking Draining
    /// -> Inactive and so `remove_account`. M4 turned
    /// `Capabilities::for_backend(Microsoft).writes.sidecars` on (a named
    /// MAPI property on the note itself, not a second message), so
    /// `sidecars_supported` now reports Microsoft as supported and this
    /// short-circuit must NOT fire — the row is correctly left for the real
    /// push (`push_one_pin`, `wire::put_pin`). Mirrors
    /// `a_gmail_pin_dirty_row_is_left_for_the_real_push` below.
    #[test]
    fn a_microsoft_pin_dirty_row_is_now_left_for_the_real_push_too() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        let mut n = dirty_note("u3", "ms-acct-2");
        n.pin_dirty = true;
        n.pinned = true;
        db.insert_local_new(&n).unwrap();
        assert!(db.has_pending_pushes("ms-acct-2").unwrap());

        let handled = clear_pin_if_sidecars_unsupported(&db, Some(accounts::BackendKind::Microsoft), &n)
            .unwrap();

        assert!(!handled, "M4: Microsoft has a sidecar store now — the drain must attempt the real push");
        assert!(db.has_pending_pushes("ms-acct-2").unwrap(), "pin_dirty must stay set for the real push");
        let row = db.get("u3", "ms-acct-2").unwrap().unwrap();
        assert!(row.pin_dirty, "must not have been cleared out from under the real push path");
        assert!(row.pinned);
    }

    /// Negative case: Gmail and LocalFs DO have a sidecar store, so a
    /// pin_dirty row must NOT be short-circuited — it has to stay dirty for
    /// `push_one_pin`'s real vertical call to handle.
    #[test]
    fn a_gmail_pin_dirty_row_is_left_for_the_real_push() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        let mut n = dirty_note("u2", "gmail-acct");
        n.pin_dirty = true;
        n.pinned = true;
        db.insert_local_new(&n).unwrap();

        let handled = clear_pin_if_sidecars_unsupported(&db, Some(accounts::BackendKind::Gmail), &n)
            .unwrap();

        assert!(!handled, "Gmail has a sidecar store — the drain must still attempt the real push");
        let row = db.get("u2", "gmail-acct").unwrap().unwrap();
        assert!(row.pin_dirty, "must not have been cleared out from under the real push path");
    }

    /// `None` (account not found) reads as supported, matching
    /// `write_refusal_for`'s "defer to the caller's own not-found path"
    /// convention. Never exercised in practice — the sync worker's drain
    /// loops already filter to accounts present in `state.accounts` before
    /// calling this — but pinning the default matters since a silent flip
    /// here would clear a real backend's dirty row.
    #[test]
    fn unknown_account_defers_rather_than_clearing() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        let mut n = dirty_note("u4", "ghost-acct");
        n.pin_dirty = true;
        db.insert_local_new(&n).unwrap();

        assert!(!clear_pin_if_sidecars_unsupported(&db, None, &n).unwrap());
    }
}

pub(super) async fn push_one_deletion(
    state: &State<'_, AppState>,
    n: &db::CachedNote,
) -> Result<(), String> {
    // If the note never reached Gmail (no remote_version), just drop the row.
    // Nothing on the server to trash. Sidecars only exist after a push, and
    // a push can only happen after Gmail has a copy of the note, so no
    // sidecar can exist for a never-pushed note either.
    if n.id.is_empty() {
        state.db.delete(&n.uuid, &n.account_id).map_err(|e| e.to_string())?;
        return Ok(());
    }
    let v = vertical_for(state, &n.account_id).await?;
    // **A deletion can be refused permanently, and until 2026-08-27 this
    // flattened that to a `String` and let the worker retry forever** — the
    // exact defect gotcha #14 records for the CONTENT path, on the path that
    // never got the rule. Measured live on iCloud: deleting a
    // password-protected note answers HTTP 400 every time, and the worker
    // re-issued the identical request 491 times over 3.5 hours, one every
    // ~5 seconds, with nothing anywhere saying why.
    //
    // Same three refusals as `push_one_dirty`'s Permanent arm: do not clear
    // the state (the note really is still on the server), do not drop the
    // row (that would hide a note the remote still has), and do not schedule
    // a retry. `list_deleted_pending` skips blocked rows, so the loop stops;
    // `has_pending_pushes` stops counting it, so one unsendable deletion
    // cannot wedge a Draining account (gotcha #2).
    if let Err(e) = v.delete(&n.id).await {
        if matches!(e, backend::TransportError::Permanent { .. }) {
            let reason = e.to_string();
            log!(
                "push_one_deletion: uuid={} refused permanently — recording it on the row and \
                 giving up rather than retrying every tick: {}",
                n.uuid, reason
            );
            state
                .db
                .mark_push_blocked(&n.uuid, &n.account_id, &reason)
                .map_err(|e| e.to_string())?;
            return Err(reason);
        }
        return Err(e.to_string());
    }
    // Best-effort trash of any pin sidecar in Notes-Meta. Without this,
    // deleted notes leave an orphan metadata message that accumulates over
    // time. Failure is logged but doesn't fail the deletion — the user's
    // intent ("remove this note") is more important than sidecar hygiene,
    // and the next sync_pin_state pass will notice the orphan (it has no
    // matching note locally) and the user can clean it up via the
    // dup-cleanup flow.
    if let Some(pin_sidecar) = n.meta_msg_id.as_deref().filter(|s| !s.is_empty()) {
        if let Err(e) = v.remove_sidecar(pin_sidecar).await {
            log!("push_one_deletion: trash pin sidecar {} failed: {}", pin_sidecar, e);
        }
    }
    state.db.delete(&n.uuid, &n.account_id).map_err(|e| e.to_string())?;
    Ok(())
}

/// The DB-only tail of the sidecars-unsupported short-circuit used by
/// `push_one_pin` — no `State`, no vertical, so it's testable without a
/// running Tauri app (same reason `push_one_dirty_db` is split out).
/// Clearing `pin_dirty` here (rather than leaving it to retry `put_sidecar`
/// forever) is the truthful state, not a fudge: a backend with no sidecar
/// store genuinely has nothing pending to push, and the pin stays
/// Jodd-local — already how the compatibility tiers describe it. Returns
/// `true` if it handled the row (caller should return early).
pub(super) fn clear_pin_if_sidecars_unsupported(
    db: &db::Db,
    kind: Option<accounts::BackendKind>,
    n: &db::CachedNote,
) -> Result<bool, String> {
    if sidecars_supported(kind) {
        return Ok(false);
    }
    db.mark_pin_pushed(&n.uuid, &n.account_id, None, n.pinned, None).map_err(|e| e.to_string())?;
    Ok(true)
}

/// Push one note's pin sidecar to Gmail. Reads the configured meta_label
/// from the account's settings, ensures the label exists (creates it on
/// first push for this account), then either:
///   - pinned=true  → insert a new sidecar message (Subject "___<uuid>"),
///                    trash the previous one if `meta_msg_id` is set
///   - pinned=false → trash the existing sidecar (if any), set
///                    meta_msg_id=NULL on success
/// After the network round-trip, calls mark_pin_pushed which conditionally
/// clears pin_dirty IF the SQLite-side pinned value still equals what we
/// just pushed (a concurrent re-toggle mid-push leaves pin_dirty=1 so the
/// worker re-pushes next tick). Short-circuits first via
/// `clear_pin_if_sidecars_unsupported` when this backend has no sidecar
/// store at all — see that function's doc comment.
///
/// **Threads the sidecar write's `RemoteNoteVersion` through to
/// `mark_pin_pushed`.** On Microsoft the pin lives on the note's own remote
/// object, so the PATCH that (un)pins it also advances that object's
/// `lastModifiedDateTime` — discarding that response used to leave
/// `notes.remote_version` stale, so a fast follow-up local edit would read
/// as a remote conflict that never happened (see `RemoteNoteVersion`'s doc
/// comment). On Gmail/LocalFs the sidecar is a separate object and this is
/// always `None`, so `mark_pin_pushed` leaves `remote_version` untouched —
/// exactly the prior behavior for those backends.
pub(super) async fn push_one_pin(
    state: &State<'_, AppState>,
    n: &db::CachedNote,
) -> Result<(), String> {
    let kind = {
        let list = state.accounts.lock().unwrap();
        list.iter().find(|a| a.id == n.account_id).map(|a| a.backend_kind)
    };
    if clear_pin_if_sidecars_unsupported(&state.db, kind, n)? {
        return Ok(());
    }
    let v = vertical_for(state, &n.account_id).await?;

    let mut new_note_version: Option<backend::RemoteNoteVersion> = None;
    let new_meta_id: Option<String> = if n.pinned {
        let payload = serde_json::json!({ "pinned": true }).to_string();
        let (id, version) = v.put_sidecar(&n.uuid, SidecarKind::Pin, Some(payload.as_bytes()), n.meta_msg_id.as_deref()).await.map_err(|e| e.to_string())?;
        new_note_version = version;
        Some(id)
    } else {
        if let Some(old) = n.meta_msg_id.as_deref().filter(|s| !s.is_empty()) {
            // Best-effort trash. If the sidecar was already trashed by
            // another Jodd instance we still want to clear meta_msg_id
            // locally — mark_pin_pushed runs regardless.
            match v.remove_sidecar(old).await {
                Ok(version) => new_note_version = version,
                Err(e) => log!("push_one_pin: trash sidecar {} failed: {}", old, e),
            }
        }
        None
    };
    let _ = state.db.mark_pin_pushed(
        &n.uuid,
        &n.account_id,
        new_meta_id.as_deref(),
        n.pinned,
        new_note_version.as_ref().map(|v| (v.version.as_str(), v.date.as_str())),
    ).map_err(|e| e.to_string())?;
    Ok(())
}

/// Worker-level backstop for `Capabilities::for_backend(_).writes.folders`,
/// mirroring `clear_pin_if_sidecars_unsupported` (see that function's doc
/// comment for why the execution point needs its own guard rather than
/// trusting every caller to have checked `refuse_write` first).
///
/// Today every path that can insert a `dirty_new`/`dirty_renamed`/
/// `deleted_pending` folder row already calls `refuse_write(Write::Folders)`
/// first, so this should never actually fire — but `push_one_folder` is the
/// point that turns such a row into a real Graph call, and on Microsoft that
/// call is not merely rejected, it *succeeds* (per M3: Graph accepts a
/// `POST /me/mailFolders/{root}/childFolders` and returns 201) while
/// producing a folder Apple's sync will never show, with no error surfaced
/// anywhere. A caller-side gate that gets bypassed by a future command, a
/// migration, or a reordered check would fail silently without this. Drop
/// the row rather than retry: retrying forever would count against
/// `has_pending_pushes` and wedge a Draining account's path to Inactive,
/// same reasoning as the `Permanent`-error handling below. Returns `true`
/// if it handled the row (caller should return early).
pub(super) fn drop_folder_row_if_writes_unsupported(
    db: &db::Db,
    kind: Option<accounts::BackendKind>,
    f: &db::CachedFolder,
) -> Result<bool, String> {
    if write_refusal_for(kind, backend::Write::Folders).is_none() {
        return Ok(false);
    }
    log!(
        "push_folder: dropping '{}' for account {} — folder writes are not \
         supported on this backend (Capabilities::writes.folders is false)",
        f.path, f.account_id
    );
    db.drop_folder_row(&f.account_id, &f.path).map_err(|e| e.to_string())?;
    Ok(true)
}

pub(super) async fn push_one_folder(
    state: &State<'_, AppState>,
    f: &db::CachedFolder,
) -> Result<(), String> {
    use db::FolderSyncState::*;
    let kind = {
        let list = state.accounts.lock().unwrap();
        list.iter().find(|a| a.id == f.account_id).map(|a| a.backend_kind)
    };
    if drop_folder_row_if_writes_unsupported(&state.db, kind, f)? {
        return Ok(());
    }
    let v = vertical_for(state, &f.account_id).await?;
    match f.sync_state {
        DirtyNew => {
            // Create on the backend. Returns the new label_id.
            match v.create_folder(&f.path).await {
                Ok(folder) => {
                    state.db.mark_folder_created(&f.account_id, &f.path, &folder.id)
                        .map_err(|e| e.to_string())?;
                    // Invalidate label_map cache so subsequent note saves see
                    // the new label (they look up label_id by path in that map).
                    invalidate_label_cache(state, &f.account_id);
                }
                // `Permanent` means retrying THIS operation will never
                // succeed (see `TransportError`'s doc comment / gmail
                // transport's `classify_str`) — e.g. Microsoft's create_folder
                // refuses outright when the Notes root has no reachable
                // Exchange id (gotcha #12: no note has ever been filed
                // directly in Notes, so there is nothing to derive a parent
                // id from). Retrying forever would leave this row `dirty_new`
                // permanently — `has_pending_pushes` counts it, so a Draining
                // account could never reach Inactive and `remove_account`
                // would refuse it forever. Same wedge the sidecar fix
                // (`clear_pin_if_sidecars_unsupported`) closes one layer up.
                // Drop the row and name the failure loudly instead: the user
                // loses a folder they just created in this rare case, which
                // is strictly better than an account that can never be
                // removed. Mirrors `DirtyRenamed`'s `NotFound` handling below.
                Err(e @ crate::backend::TransportError::Permanent { .. }) => {
                    log!(
                        "push_folder: dropping stale dirty_new '{}' — create refused \
                         permanently: {}",
                        f.path, e
                    );
                    state.db.drop_folder_row(&f.account_id, &f.path).map_err(|e| e.to_string())?;
                    invalidate_label_cache(state, &f.account_id);
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        DirtyRenamed => {
            // Need label_id. If we don't have one yet, the folder was
            // created locally and the create push hasn't fired yet — skip
            // this tick; we'll be back.
            let Some(label_id) = f.label_id.as_deref() else {
                return Err("rename pending but label_id is None — wait for create push".into());
            };
            // Empty label_id is a stale artifact from pre-fix builds where
            // reconcile_folders_from_paths passed "" as the label_id. There is
            // no valid source path to rename from. Mark clean so the retry loop
            // stops; the next full-sync reconcile re-derives the correct state.
            if label_id.is_empty() {
                log!("push_folder: dirty_renamed '{}' has empty label_id (stale row) — marking clean", f.path);
                state.db.mark_folder_renamed(&f.account_id, &f.path).map_err(|e| e.to_string())?;
                invalidate_label_cache(state, &f.account_id);
                return Ok(());
            }
            match v.rename_folder(label_id, &f.path).await {
                Ok(()) => {}
                Err(crate::backend::TransportError::NotFound) => {
                    // Source gone AND destination absent — the folder was deleted
                    // externally while a rename was pending. Drop the stale row
                    // instead of retrying forever.
                    log!(
                        "push_folder: stale dirty_renamed '{}' (source '{}' gone) — dropping row",
                        f.path, label_id
                    );
                    state.db.drop_folder_row(&f.account_id, &f.path).map_err(|e| e.to_string())?;
                    invalidate_label_cache(state, &f.account_id);
                    return Ok(());
                }
                Err(e) => return Err(e.to_string()),
            }
            // For LocalFs, note `id` = file path, so renaming the folder
            // changes the path. Update cached IDs so future fetches/saves
            // use the correct path. For Gmail this is a safe no-op (Gmail
            // message IDs don't follow the `Notes/<folder>/…` path pattern).
            if let Err(e) = state.db.rename_note_ids_for_folder(&f.account_id, label_id, &f.path) {
                log!("push_folder: rename_note_ids_for_folder failed: {}", e);
            }
            state.db.mark_folder_renamed(&f.account_id, &f.path)
                .map_err(|e| e.to_string())?;
            invalidate_label_cache(state, &f.account_id);
        }
        DeletedPending => {
            // If no label_id, this folder was created locally and never pushed —
            // the mark_folder_deleted helper already dropped the row in that
            // case, so we shouldn't see it here. Belt-and-suspenders: handle
            // gracefully.
            let Some(label_id) = f.label_id.as_deref() else {
                state.db.drop_folder_row(&f.account_id, &f.path)
                    .map_err(|e| e.to_string())?;
                return Ok(());
            };
            // Empty label_id is as dangerous as None here: delete_folder("") on
            // LocalFs resolves to notes_dir() and would wipe the entire vault.
            // Drop the stale row instead.
            if label_id.is_empty() {
                log!("push_folder: deleted_pending '{}' has empty label_id — dropping stale row", f.path);
                state.db.drop_folder_row(&f.account_id, &f.path).map_err(|e| e.to_string())?;
                invalidate_label_cache(state, &f.account_id);
                return Ok(());
            }
            v.delete_folder(label_id).await.map_err(|e| e.to_string())?;
            state.db.drop_folder_row(&f.account_id, &f.path)
                .map_err(|e| e.to_string())?;
            invalidate_label_cache(state, &f.account_id);
        }
        Clean => {} // shouldn't get here — list_dirty_folders filters Clean
    }
    Ok(())
}

#[cfg(test)]
mod folder_write_guard_tests {
    use super::*;

    /// The regression this guard exists to prevent: a `dirty_new` folder row
    /// for a Microsoft account reaching the worker must never turn into a
    /// real `create_folder` Graph call — M3 proved that call succeeds (201)
    /// while producing a folder Apple's sync will never show, silently.
    #[test]
    fn a_microsoft_dirty_folder_row_is_dropped_not_pushed() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        db.create_folder_local_new("ms-acct", "Notes/Ideas").unwrap();
        assert!(db.has_pending_pushes("ms-acct").unwrap());
        let f = db.get_folder("ms-acct", "Notes/Ideas").unwrap().unwrap();

        let handled =
            drop_folder_row_if_writes_unsupported(&db, Some(accounts::BackendKind::Microsoft), &f)
                .unwrap();

        assert!(handled, "folder writes are permanently unsupported on Microsoft");
        assert!(db.get_folder("ms-acct", "Notes/Ideas").unwrap().is_none(), "row must be dropped");
        assert!(!db.has_pending_pushes("ms-acct").unwrap(), "must not wedge Draining -> Inactive");
    }

    /// Negative case: Gmail and LocalFs DO support folder writes, so a dirty
    /// row must be left alone for `push_one_folder`'s real vertical call.
    #[test]
    fn a_gmail_dirty_folder_row_is_left_for_the_real_push() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        db.create_folder_local_new("gmail-acct", "Notes/Ideas").unwrap();
        let f = db.get_folder("gmail-acct", "Notes/Ideas").unwrap().unwrap();

        let handled =
            drop_folder_row_if_writes_unsupported(&db, Some(accounts::BackendKind::Gmail), &f)
                .unwrap();

        assert!(!handled, "Gmail supports folder writes — the drain must still attempt the real push");
        assert!(db.get_folder("gmail-acct", "Notes/Ideas").unwrap().is_some(), "row must survive");
    }

    /// `None` (account not found) reads as supported, matching
    /// `write_refusal_for`'s "defer to the caller's own not-found path"
    /// convention — same reasoning as the pin guard's equivalent test.
    #[test]
    fn unknown_account_defers_rather_than_dropping() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        db.create_folder_local_new("ghost-acct", "Notes/Ideas").unwrap();
        let f = db.get_folder("ghost-acct", "Notes/Ideas").unwrap().unwrap();

        assert!(!drop_folder_row_if_writes_unsupported(&db, None, &f).unwrap());
    }
}

/// Explicit flush waits for each account; periodic rounds coalesce if already queued.
pub(super) async fn sync_worker_tick(app: &AppHandle) {
    let mut tasks = tokio::task::JoinSet::new();
    dispatch_accounts(app, false, &mut tasks);
    while let Some(result) = tasks.join_next().await {
        if let Err(e) = result { log!("sync worker task failed: {e}"); }
    }
}

fn dispatch_accounts(app: &AppHandle, periodic: bool, tasks: &mut tokio::task::JoinSet<()>) {
    let ids: Vec<_> = app.state::<AppState>().accounts.lock().unwrap()
        .iter().map(|a| (a.id.clone(), a.backend_kind)).collect();
    for (id, kind) in ids {
        let app = app.clone();
        tasks.spawn(async move {
            let state = app.state::<AppState>();
            let lease = if kind == accounts::BackendKind::LocalFs {
                state.sync_schedule.enter_local(&id, periodic).await
            } else {
                state.sync_schedule.enter(&id, periodic).await
            };
            let Some(_lease) = lease else { return };
            sync_account_round(&app, &id).await;
        });
    }
}

/// Account lease is held before reading ANY queue through lifecycle and pull.
async fn sync_account_round(app: &AppHandle, account_id: &str) {
    let state = app.state::<AppState>();
    // Snapshot live accounts at the top of the tick. Dirty/deletion rows
    // for an account that no longer exists (signed out mid-cycle) would
    // otherwise generate ~5 errors per tick until the next index sweep —
    // and would burn refresh-token lookups against the keychain for
    // accounts the user has explicitly removed. Skip them silently.
    //
    // Accounts the worker will still push for. Inactive accounts are excluded
    // outright; Draining accounts stay IN, because draining is the whole point
    // of that state — they are hidden from the user but still owe the backend
    // whatever was queued before they were dismissed.
    let live_accts: std::collections::HashSet<String> = state
        .accounts
        .lock()
        .unwrap()
        .iter()
        .filter(|a| a.id == account_id && a.status != accounts::AccountStatus::Inactive)
        .map(|a| a.id.clone())
        .collect();

    // FOLDERS FIRST. Creates must reach Gmail before notes that target the
    // new label try to save (otherwise save_note's label_map lookup fails
    // and the note falls back to "Notes" root). Renames must propagate
    // before list_notes sees a stale name. Deletions go last (deepest
    // first via list_dirty_folders' ordering) so children are cleared
    // before parents.
    let dirty_folders = match state.db.list_dirty_folders() {
        Ok(v) => v,
        Err(e) => { log!("sync_worker: list_dirty_folders failed: {}", e); vec![] }
    };
    for f in dirty_folders {
        if !live_accts.contains(&f.account_id) { continue; }
        if let Err(e) = push_one_folder(&state, &f).await {
            log!(
                "sync_worker: push folder '{}' ({:?}) failed: {}",
                f.path, f.sync_state, e
            );
        } else {
            log!(
                "sync_worker: pushed folder '{}' ({:?})",
                f.path, f.sync_state
            );
        }
    }

    // Drain dirty rows first (creates/edits), then deletions. Order matters
    // a little: pushing edits before deletions means that if the user
    // edits-then-deletes the same uuid in quick succession, the edit's
    // network call still goes (and gets trashed by the delete). Harmless.
    let dirty = match state.db.list_dirty() {
        Ok(v) => v,
        Err(e) => { log!("sync_worker: list_dirty failed: {}", e); vec![] }
    };
    for n in dirty {
        if !live_accts.contains(&n.account_id) { continue; }
        // Debounce: don't re-push a note that is still being actively edited.
        // Push once it has settled (quiet >= PUSH_SETTLE_MS) or is overdue
        // (synced-note cap). Cuts insert-new/trash-old churn that wedges
        // Apple Notes' per-mailbox sync. See note_push_due.
        if !note_push_due(
            db::now_ms(),
            n.last_local_modified_at,
            n.last_synced_at,
            PUSH_SETTLE_MS,
            MAX_DEFER_MS,
        ) {
            continue;
        }
        // Mark in-flight BEFORE gmail::save_note so any concurrent poll/
        // reconcile sees this push as "ours, don't conflict" — closes the
        // race that caused spurious self-conflicts.
        let key = (n.account_id.clone(), n.uuid.clone());
        let pushing_guard = PushingGuard::new(&state.pushing, key);
        let res = push_one_dirty(&state, &n).await;
        drop(pushing_guard);
        // Always nudge after an attempt: permanent failure changes state too.
        // SQLite resolves the pre-push UUID and guards the current version.
        let _ = app.emit("note-persistence-changed", serde_json::json!({
            "accountId": n.account_id, "uuid": n.uuid,
        }));
        match res {
            Err(e) => log!("sync_worker: push dirty uuid={} failed: {}", n.uuid, e),
            Ok(confirmation) => {
                log!("sync_worker: pushed dirty uuid={}", n.uuid);
                // Tell the open editor which bytes the backend accepted.
                //
                // Without this the editor anchored `isEcho` to `save_note`'s
                // return — but `save_note` is local-first (SQLite, then
                // return; the push happens here, up to a tick later), so that
                // value named a body the backend had never seen. Any refresh
                // landing in the gap (App.svelte's 10s focus/folder settle)
                // read the PREVIOUS body, matched neither the rendered nor the
                // "pushed" one, and raised "Edited on another device" for this
                // device's own sync lag.
                //
                // Emitted at the CALLER, not inside `push_one_dirty`, for the
                // same reason `remote-changed` is: the push path takes a
                // `State` and stays a pure DB/transport sequence, while the
                // `AppHandle` lives only out here (gotcha #6).
                if let Some(c) = confirmation {
                    let _ = app.emit("note-pushed", c);
                }
            }
        }
    }
    let deleted = match state.db.list_deleted_pending() {
        Ok(v) => v,
        Err(e) => { log!("sync_worker: list_deleted_pending failed: {}", e); vec![] }
    };
    for n in deleted {
        if !live_accts.contains(&n.account_id) { continue; }
        // Same in-flight tracking applies to deletes — a poll during trash
        // would see the message has not yet been trashed (if Apple Notes/
        // Gmail web hasn't refreshed) and could incorrectly re-upsert it.
        // For deletions the row is already in deleted_pending state, which
        // reconcile_one skips anyway, but we mark it for symmetry and
        // future-proofing.
        let key = (n.account_id.clone(), n.uuid.clone());
        let pushing_guard = PushingGuard::new(&state.pushing, key);
        let res = push_one_deletion(&state, &n).await;
        drop(pushing_guard);
        if let Err(e) = res {
            log!("sync_worker: push deletion uuid={} failed: {}", n.uuid, e);
        } else {
            log!("sync_worker: trashed + removed cached row uuid={}", n.uuid);
        }
    }

    // Drain pin sidecars. Independent of content-dirty / deleted_pending:
    // a row can be content-dirty AND pin-dirty in the same tick and both
    // push paths run for it (the sidecar lives in a different label, so
    // there's no Gmail-side ordering constraint). We drain AFTER content
    // and deletes only because pin-sync is the lowest-priority operation
    // (purely UX, not correctness) and starving it briefly is fine if a
    // large content backlog is in flight.
    let dirty_pin = match state.db.list_pin_dirty() {
        Ok(v) => v,
        Err(e) => { log!("sync_worker: list_pin_dirty failed: {}", e); vec![] }
    };
    for n in dirty_pin {
        if !live_accts.contains(&n.account_id) { continue; }
        if let Err(e) = push_one_pin(&state, &n).await {
            log!("sync_worker: push pin uuid={} failed: {}", n.uuid, e);
        } else {
            log!(
                "sync_worker: pushed pin sidecar uuid={} pinned={}",
                n.uuid, n.pinned
            );
        }
    }

    // A Draining account with every queue empty has finished quiescing. This
    // is the ONLY transition into Inactive, which is what lets the rest of the
    // app treat Inactive as "nothing is pending" rather than a mere label.
    //
    // `queued_for_removal` is snapshotted in the SAME lock acquisition, and
    // deliberately BEFORE the flip loop below writes anything — so an account
    // the flip is about to move to Inactive in THIS tick is excluded here.
    // That is what makes "flip now, remove on the next tick" true rather than
    // incidental: the two are separate proofs (has_pending_pushes now vs.
    // status already settled as of the top of this tick), not one loop
    // treating its own write as trustworthy in the same breath.
    let (draining, queued_for_removal): (Vec<String>, Vec<String>) = {
        let list = state.accounts.lock().unwrap();
        (
            list.iter()
                .filter(|a| a.id == account_id && a.status == accounts::AccountStatus::Draining)
                .map(|a| a.id.clone())
                .collect(),
            list.iter()
                .filter(|a| a.id == account_id && should_complete_pending_removal(a.status, a.pending_removal))
                .map(|a| a.id.clone())
                .collect(),
        )
    };
    for id in draining {
        match state.db.has_pending_pushes(&id) {
            Ok(false) => {
                let mut list = state.accounts.lock().unwrap();
                if let Some(a) = list.iter_mut().find(|a| a.id == id) {
                    // Re-check under the lock: the `draining` snapshot above
                    // is from earlier in the tick, and a reactivate
                    // (Inactive -> Active, Task 5) can land in between.
                    // Flipping unconditionally would clobber the user's
                    // Active back to Inactive.
                    if should_flip_to_inactive(a.status, true) {
                        a.status = accounts::AccountStatus::Inactive;
                        if let Err(e) = accounts::save_accounts(&list) {
                            log!("sync_worker: saving drained status for {} failed: {}", id, e);
                        }
                        log!("sync_worker: {} finished draining — now inactive", id);
                    }
                }
            }
            Ok(true) => {}
            Err(e) => log!("sync_worker: has_pending_pushes({}) failed: {}", id, e),
        }
    }

    // An account queued for removal while it was still Draining
    // (`remove_account`'s gentler `pending_removal` path) finishes here —
    // one tick after the snapshot above found it already `Inactive`, per the
    // comment there.
    for id in queued_for_removal {
        log!(
            "sync_worker: {} is inactive and was queued for removal — removing it now",
            id
        );
        if let Err(e) = perform_account_removal(&state, &id).await {
            log!("sync_worker: pending_removal completion failed for {}: {}", id, e);
        }
    }

    // LAST in the tick, and after the drain flip on purpose. It is the only
    // step that talks to a remote without a local row asking it to, so a tick
    // whose pushes are backed up should spend its time on those; and a
    // Draining account that just went Inactive must not then be polled.
    //
    // Runs on its own cadence (`PULL_INTERVAL`), not the worker's: 5 s is right
    // for draining a queue and far too fast for somebody's private API.
    let due: Vec<String> = {
        let now = std::time::Instant::now();
        let candidates: Vec<String> = state
            .accounts
            .lock()
            .unwrap()
            .iter()
            .filter(|a| a.id == account_id && a.status == accounts::AccountStatus::Active)
            // An account a read has already found unusable — Advanced Data
            // Protection is the only producer today — cannot become usable by
            // being asked again on a timer. Polling it would be a request a
            // minute against somebody's private API for an answer that is
            // already known, and `index_account` re-stamps the field on every
            // pass, so it clears itself the moment the account works again.
            .filter(|a| a.blocked_reason.is_none())
            .filter(|a| incremental_pull_supported(a.backend_kind))
            .map(|a| a.id.clone())
            .collect();
        let mut last = state.last_pull.lock().unwrap();
        let mut due = Vec::new();
        for id in candidates {
            if last.get(&id).is_none_or(|t| now.duration_since(*t) >= PULL_INTERVAL) {
                // Stamped BEFORE the run, not after: a detector that hangs on a
                // dead session would otherwise be re-entered on every 5 s tick,
                // each one harvesting cookies from the webview again.
                last.insert(id.clone(), now);
                due.push(id);
            }
        }
        due
    };
    for id in due {
        if detect_remote_changes(&state, &id).await {
            // gotcha #6: state the worker changes on its own needs a route back
            // to the frontend. Dropping the cache is invisible from up there —
            // nothing re-reads through it until the ten-minute authoritative
            // poll comes round, so an edit made in Apple Notes sat unseen for
            // up to ten minutes with the detector having known about it the
            // whole time.
            //
            // The payload is the account id and nothing else, on purpose. The
            // detector's own design rule is that a `RemoteChange` carries an id
            // and a kind but never content, because applying one here would
            // cost a whole zone read; the event keeps that shape — it says "go
            // and look", and the paths that already read authoritatively do the
            // reading. A backend with no incremental detector never reaches
            // this line, so nothing about its behaviour changes.
            let _ = app.emit("remote-changed", id.clone());
        }
    }
}

// ─── Incremental pull: the change detector ───────────────────────────────────

/// How often one account may ask its backend "what changed?".
///
/// The worker ticks every `SYNC_INTERVAL` (5 s), which is right for draining a
/// local queue and far too fast for a request against somebody's private API.
/// A minute is slower than the 2500 ms folder sweep the user actually watches
/// and twenty times faster than the ten-minute authoritative poll it front-runs.
pub(super) const PULL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Bound on how many `changes_since` pages one detector run will walk.
///
/// `more` is server-controlled, so an unbounded loop is a server-controlled
/// loop. The detector only needs to know **whether** anything changed and what
/// the resume token now is, so stopping early costs one extra run rather than
/// anything correctness-shaped.
pub(super) const MAX_PULL_PAGES: usize = 20;

/// Does this backend have a real incremental read to drive a detector with?
///
/// Written as an exhaustive match rather than `== ICloud` so a fifth backend
/// fails closed — excluded by default — rather than silently inheriting a poll
/// against an endpoint it does not have. Same shape, and the same reason, as
/// `orphan_cleanup_supported`: the alternative is a raw `backend_kind` check
/// buried in shared sync code, which is the altitude defect `Capabilities`
/// exists to prevent.
///
/// Gmail and Microsoft return an inert cursor from `changes_since` — a detector
/// there would report "nothing changed" forever, which is worse than not
/// running. LocalFs implements it, but its notes are files on the user's own
/// disk with no rate limit and no session to spend; its refresh is already
/// cheap.
pub(super) fn incremental_pull_supported(kind: accounts::BackendKind) -> bool {
    match kind {
        accounts::BackendKind::ICloud => true,
        accounts::BackendKind::Gmail
        | accounts::BackendKind::Microsoft
        | accounts::BackendKind::LocalFs => false,
    }
}

/// What one detector run learned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PullOutcome {
    /// There was no stored cursor, so this run established one. **It reports
    /// nothing changed even when the backend handed back the whole account**:
    /// with no previous token there is nothing to have changed *since*, and
    /// treating a from-scratch read as "everything just changed" would drop the
    /// cache on every fresh install and every restart that lost the file.
    Primed,
    /// A cursor was in hand and the backend reported no changes.
    Quiet,
    /// A cursor was in hand and this many records changed.
    Changed(usize),
}

/// The detector's whole decision, as a pure function over what it observed.
///
/// Split out because the run itself needs a live `AppState` and a network, and
/// the interesting rule — priming is not a change — is decidable without either.
pub(super) fn pull_outcome(stored_cursor: Option<&str>, changed: usize) -> PullOutcome {
    match (stored_cursor, changed) {
        (None, _) => PullOutcome::Primed,
        (Some(_), 0) => PullOutcome::Quiet,
        (Some(_), n) => PullOutcome::Changed(n),
    }
}

/// Does this outcome justify rewriting `accounts.json`?
///
/// The file also holds the user's settings, and the backend hands back a fresh
/// token on every call — so persisting each one would churn it once a minute
/// for the life of the process to record that nothing happened. A token only
/// has to advance past changes that were actually acted on.
pub(super) fn cursor_write_needed(outcome: PullOutcome) -> bool {
    match outcome {
        PullOutcome::Primed | PullOutcome::Changed(_) => true,
        PullOutcome::Quiet => false,
    }
}

/// Does this outcome justify telling the frontend to go and read again?
///
/// Deliberately **not** the same rule as `cursor_write_needed`, which they
/// would otherwise look like: `Primed` writes a cursor and must NOT wake the
/// frontend. Priming happens on every fresh install and every restart that
/// lost the file, and it hands back the whole account — so treating it as a
/// change would make the app re-read everything at startup for nothing, which
/// is the same mistake `PullOutcome::Primed`'s own doc comment exists to
/// prevent one layer down.
pub(super) fn frontend_wakeup_needed(outcome: PullOutcome) -> bool {
    match outcome {
        PullOutcome::Changed(_) => true,
        PullOutcome::Primed | PullOutcome::Quiet => false,
    }
}

/// Stores an account's resume token, in memory and on disk.
///
/// Failure to persist is logged and swallowed: a cursor is a hint, and an
/// account whose accounts.json write failed must not stop syncing over it.
pub(super) fn store_sync_cursor(state: &State<'_, AppState>, account_id: &str, cursor: Option<String>) {
    let mut list = state.accounts.lock().unwrap();
    let Some(a) = list.iter_mut().find(|a| a.id == account_id) else { return };
    if a.sync_cursor == cursor {
        return;
    }
    a.sync_cursor = cursor;
    if let Err(e) = accounts::save_accounts(&list) {
        log!("pull: could not persist the sync cursor for {account_id}: {e}");
    }
}

/// Ensures the account has a CRDT replica id, persisting a freshly minted
/// one immediately — mirrors `store_sync_cursor`'s lock/find/mutate/save
/// shape. `None` only if the account has vanished from the list since the
/// caller looked it up.
///
/// Compares the replica id before and after the call, so both "never had one"
/// and "had a corrupted one that got replaced" cases persist correctly. A call
/// where nothing changed (already has a valid id) does not write.
///
/// Called by `icloud_content_write_selftest` — the CRDT engine's live proof
/// needs a real replica id to mint `CharID`s under, same as any future
/// caller once `Capabilities::for_backend(ICloud).writes.notes` turns on.
pub(super) fn ensure_icloud_replica_id(state: &State<'_, AppState>, account_id: &str) -> Option<[u8; 16]> {
    let mut list = state.accounts.lock().unwrap();
    let a = list.iter_mut().find(|a| a.id == account_id)?;
    let before = a.icloud_replica_id.clone();
    let id = a.ensure_icloud_replica_id();
    if a.icloud_replica_id != before {
        if let Err(e) = accounts::save_accounts(&list) {
            log!("icloud: could not persist the replica id for {account_id}: {e}");
        }
    }
    Some(id)
}

/// Asks one account's backend what changed, and drops its cached read if
/// anything did.
///
/// **A detector, not a pull.** It deliberately does NOT apply the changes: a
/// `RemoteChange` carries a `remote_id` and a kind, never content, so applying
/// one would mean fetching each changed note — and on the backend this runs
/// against, a per-note fetch costs a whole zone read (`fetch_note`'s own doc
/// comment prices it at "an explicit user action"). Dropping the cached scan
/// instead hands the work to the paths that already do it authoritatively, and
/// the user sees the result on the next folder sweep.
///
/// That is also the answer to gotcha #6 — state the worker changes on its own
/// needs a route back to the frontend. This one needs no new channel because it
/// changes nothing the frontend reads directly: it invalidates a cache the
/// frontend's existing 2500 ms sweep re-reads through. **Do not "improve" this
/// by reconciling into SQLite here** without deciding how the UI learns.
/// Returns whether this run observed a real remote change — i.e. whether the
/// cached read was dropped. The caller turns that into the `remote-changed`
/// event, because **this function has no `AppHandle` and must not grow one**:
/// everything it decides is decidable from `AppState` alone, which is what
/// keeps `pull_outcome` a pure function with unit tests. Emitting here would
/// thread a `tauri::AppHandle` through a code path whose whole design is that
/// it needs no window.
///
/// Priming and a quiet run both answer `false`. Priming especially: a
/// from-scratch read is not "everything changed" (see `PullOutcome::Primed`),
/// so it must not wake the frontend either.
pub(super) async fn detect_remote_changes(state: &State<'_, AppState>, account_id: &str) -> bool {
    let stored = {
        let list = state.accounts.lock().unwrap();
        list.iter().find(|a| a.id == account_id).and_then(|a| a.sync_cursor.clone())
    };

    let v = match vertical_for(state, account_id).await {
        Ok(v) => v,
        // Not an error worth shouting about on a background tick: a session
        // that has expired is exactly what the next user action will surface.
        Err(e) => {
            log!("pull: {account_id} has no usable vertical right now ({e})");
            return false;
        }
    };

    // **Priming reads nothing.** The zone walk the app already performs ends on
    // a token that covers the whole zone, so the cursor can be taken from it
    // instead of paged for a second time through `changes_since`.
    //
    // Doing it the other way was measured wrong on a live account
    // (2026-08-24): priming walked all 29 pages a second time, hit
    // `MAX_PULL_PAGES` before the end, and stored a token from the MIDDLE of
    // the zone — so the next run reported **648 records changed** on an account
    // where nothing had, and dropped the cache for it. A truncated prime is not
    // a prime; it is a cursor pointing at a place the reader has not reached.
    if stored.is_none() {
        #[cfg(icloud_webview)]
        if let Ok(v) = icloud_vertical_for(state, account_id).await {
            if let Some(token) = v.sync_token().await {
                store_sync_cursor(state, account_id, Some(token));
                log!("pull: {account_id} took its first cursor from the zone read it already did");
                return false;
            }
        }
        log!("pull: {account_id} has no cursor and no completed read to take one from");
        return false;
    }

    let mut cursor = stored.clone().map(|c| backend::SyncCursor(c.into_bytes()));
    let mut changed = 0usize;
    for _ in 0..MAX_PULL_PAGES {
        let set = match v.changes_since(cursor.as_ref()).await {
            Ok(s) => s,
            Err(e) => {
                log!("pull: changes_since failed for {account_id}: {e}");
                return false;
            }
        };
        changed += set.changes.len();
        let more = set.more;
        cursor = Some(set.next_cursor);
        if !more {
            break;
        }
    }

    let next = cursor
        .map(|c| String::from_utf8_lossy(&c.0).into_owned())
        .filter(|c| !c.is_empty());

    match pull_outcome(stored.as_deref(), changed) {
        // Unreachable from here — the no-cursor case returned above, having
        // taken its cursor from the app's own read. Kept as an arm rather than
        // an `unreachable!` because the rule it encodes is the one that was
        // got wrong: a from-scratch read is not "everything changed".
        outcome @ PullOutcome::Primed => {
            debug_assert!(cursor_write_needed(outcome));
            debug_assert!(!frontend_wakeup_needed(outcome));
            store_sync_cursor(state, account_id, next);
            false
        }
        // **Nothing is written on a quiet run**, and that is not laziness. The
        // backend hands back a fresh token on every call, so persisting each
        // one would rewrite accounts.json every `PULL_INTERVAL` for the life of
        // the process — a file that also holds the user's settings, churned
        // once a minute to record that nothing happened. The cursor already in
        // hand is still valid: re-sending it next time asks the same question
        // and gets the same empty answer. A token only has to advance past
        // changes that were actually acted on, and icloud-md measured a
        // fifteen-day-old one still syncing incrementally.
        PullOutcome::Quiet => false,
        outcome @ PullOutcome::Changed(n) => {
            debug_assert!(cursor_write_needed(outcome));
            debug_assert!(frontend_wakeup_needed(outcome));
            // Stored BEFORE the invalidate: if the process dies between the
            // two, the worst case is a cursor that has moved past a change the
            // cache never dropped — and the ten-minute authoritative poll reads
            // everything regardless. Storing after would risk re-reporting the
            // same change every minute forever if the write kept failing.
            store_sync_cursor(state, account_id, next);
            log!("pull: {n} record(s) changed on {account_id} — dropping the cached read");
            icloud_scan_cache(state, account_id).invalidate().await;
            true
        }
    }
}

pub(super) fn spawn_sync_worker(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + SYNC_INTERVAL, SYNC_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = interval.tick() => dispatch_accounts(&app, true, &mut tasks),
                Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                    if let Err(e) = result { log!("sync worker task failed: {e}"); }
                }
            }
        }
    });
}
