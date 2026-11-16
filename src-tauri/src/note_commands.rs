//! Note IPC adapters: permission checks and response mapping over local mutation services.
use super::*;

/// Local-first save. Writes to the SQLite replica synchronously and returns
/// immediately. The actual Gmail push happens in the background sync worker.
///
/// What this does NOT do:
///   - Call Gmail. Network round-trip is the worker's job.
///   - Wait for sync to complete. UI gets "Saved" feedback as soon as the
///     local row is committed.
///
/// What the worker eventually does with this row:
///   - Reads `dirty` rows
///   - Calls gmail::save_note (insert new + trash old)
///   - On success: mark_pushed(uuid, new_id) → sync_state = clean, id updated
///   - On failure: leaves dirty, retries next cycle
#[tauri::command]
pub(super) async fn save_note(
    account_id: String,
    title: String,
    body_html: String,
    // `existing_gmail_id` is no longer used — Rust reads it from cache.
    // Kept as a parameter for backward compat during the migration; will
    // be dropped once the frontend stops sending it.
    #[allow(unused_variables)]
    existing_gmail_id: Option<String>,
    existing_uuid: Option<String>,
    existing_x_mail_created_date: Option<String>,
    label: String,
    // The `local_version` this editor session saw when it loaded (or last
    // successfully saved) this note — NOT re-derived inside this command,
    // which is exactly what made Task 5's first attempt at this guard
    // ineffective (see the Addendum above). `None` means the frontend never
    // learned a version (e.g. it hasn't picked up this feature yet, or this
    // is a brand-new note) — falls back to an unguarded write rather than
    // blocking a save the frontend can't version.
    expected_local_version: Option<i64>,
    ai_result_id: Option<String>,
    // The uuid of the note this save duplicates, so the copy inherits its
    // provenance history instead of starting as `human_written`.
    duplicate_of: Option<String>,
    state: State<'_, AppState>,
) -> Result<gmail::SavedNote, String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Notes)?;
    let db = state.db.clone();

    // Hoisted above the save: `save_note_db` needs it to decide how this
    // backend's identity may be spelled (`backend::canonical_uuid_for`), and
    // the LocalFs synchronous-push branch below reads the same value.
    //
    // `unwrap_or_default()` (Gmail) for an account that is not in the list:
    // that is already a bug the row write will surface, and Gmail is the
    // back-compat default everywhere else identity is concerned (see
    // `backend_kind_default_is_gmail`).
    let acct_kind = state.accounts.lock().unwrap()
        .iter()
        .find(|a| a.id == account_id)
        .map(|a| a.backend_kind);

    let _localfs_lane = if matches!(acct_kind, Some(accounts::BackendKind::LocalFs)) {
        state.sync_schedule.enter_local(&account_id, false).await
    } else { None };

    if _localfs_lane.is_some() && account_backend_kind(&state, &account_id).is_none() {
        return Err("Account was removed while waiting to save".into());
    }
    let cached = {
        let _policy_gate = ai_result_id.as_ref().map(|_| state.ai_policy.gate.lock().unwrap());
        if let Some(id) = &ai_result_id { validate_ai_result(&state, &account_id, id)?; }
        let actor = actor_for(&state, ai_result_id.as_deref());
        save_note_db(
            &db,
            &account_id,
            acct_kind.unwrap_or_default(),
            existing_uuid.as_deref(),
            &title,
            &body_html,
            &label,
            existing_x_mail_created_date.as_deref(),
            expected_local_version,
            &actor,
            duplicate_of.as_deref(),
        )?
    };
    let real_uuid = cached.uuid.clone();

    // For LocalFS accounts push the file to disk right now, synchronously.
    // A local filesystem write completes in < 1ms — there is no benefit to
    // deferring it to the 5-second worker tick, and deferring creates a race:
    // if the user deletes the note before the worker runs, id is still "" and
    // push_one_deletion drops the DB row without moving anything to .trash/,
    // so the note never appears in "Recently Deleted".
    //
    // When that write cannot happen, the save still succeeded: the row is
    // committed and dirty, and the worker's push owns what comes next
    // (gotcha #14's refusal, a retry). See `push_localfs_now`.
    if matches!(acct_kind, Some(accounts::BackendKind::LocalFs)) {
        match vertical_for(&state, &account_id).await {
            Ok(v) => {
                if let Some(saved) = push_localfs_now(&db, v.as_ref(), &account_id, &cached).await? {
                    return Ok(saved);
                }
            }
            Err(e) => log!("save_note (localfs-sync): uuid={} no vertical, left to the sync worker: {}", real_uuid, e),
        }
    }

    log!(
        "save_note (local-first): uuid={} sync_state={:?} id={}",
        real_uuid, cached.sync_state, if cached.id.is_empty() { "<pending>" } else { &cached.id }
    );

    Ok(gmail::SavedNote {
        id: cached.id,
        // Nothing was pushed on this call (the worker will push in the
        // background) — echo the DB's current remote_version, same as
        // `to_frontend_note` does for `Note::version`.
        version: cached.remote_version.clone().unwrap_or_default(),
        uuid: cached.uuid,
        date: cached.date,
        created_date: cached.x_mail_created_date,
        body_html: cached.body_html,
        local_version: cached.local_version,
    })
}

/// `save_note`'s synchronous LocalFs write of a row it just committed.
/// `Ok(None)` means "not written now": the caller answers as any local-first
/// save does, and the worker's next push sends the row.
///
/// A failed write is `Ok(None)`, never an error. The text is already in
/// SQLite, so an error would tell the editor its save was lost when it was
/// not. Measured on a Linux build 2026-11-09 with the vault made read-only:
/// the editor kept its edit marked unsaved, so "Try again" never left
/// "Trying again"; it never learned the row's new `local_version`, so every
/// next save was refused as "changed elsewhere" and became a conflict copy
/// (three in 15 seconds); and the raw "permanent: Permission denied" showed
/// in the error bar. Left to the worker, the refusal is recorded on the row
/// (gotcha #14) and the editor's notice says it once.
///
/// A file that changed since the row's base is `Ok(None)` too, unwritten:
/// before 2026-11-09 this write skipped `check_push_base`, so typing in Jodd
/// erased a change another device had made to the file (gotcha #40).
pub(super) async fn push_localfs_now(
    db: &db::Db,
    v: &dyn Vertical,
    account_id: &str,
    cached: &db::CachedNote,
) -> Result<Option<gmail::SavedNote>, String> {
    let real_uuid = cached.uuid.as_str();
    let existing_id = if cached.id.is_empty() { None } else { Some(cached.id.as_str()) };
    let attachments = db.list_attachments(account_id, real_uuid).unwrap_or_default();
    let op = crate::backend::SaveOp {
        title: &cached.title,
        body_html: &cached.body_html,
        existing_remote_id: existing_id,
        existing_uuid: Some(real_uuid),
        existing_created_date: cached.x_mail_created_date.as_deref(),
        label: &cached.label,
        base_version: cached.remote_version.as_deref(),
    };
    // A file write has no compare-and-swap: one based on a stale copy erases
    // what another device (Syncthing, a second Jodd) wrote since this row
    // last read the file (gotcha #40). Leave that row to the worker, whose
    // push runs the whole check and reconciles keep-both or merges (#41);
    // a check that could not read is not "nothing changed" either.
    match stale_push_base_via(db, cached, accounts::BackendKind::LocalFs, v).await {
        Ok(None) => {}
        Ok(Some(_)) => {
            log!("save_note (localfs-sync): uuid={} the file changed since this edit's base — left to the sync worker", real_uuid);
            return Ok(None);
        }
        Err(e) => {
            log!("save_note (localfs-sync): uuid={} could not read the file's version, left to the sync worker: {}", real_uuid, e);
            return Ok(None);
        }
    }
    let saved = match v.save_note_full(&op, &attachments).await {
        Ok(saved) => saved,
        Err(e) => {
            log!("save_note (localfs-sync): uuid={} not written now, left to the sync worker: {}", real_uuid, e);
            return Ok(None);
        }
    };
    // See the same call in `push_one_dirty_db`.
    if let Some(created) = saved.created_date.as_deref().filter(|s| !s.is_empty()) {
        db.keep_created_date(real_uuid, account_id, created).map_err(|e| e.to_string())?;
    }
    db.mark_pushed(real_uuid, account_id, &saved.id, &saved.version, &saved.date, &saved.body_html, cached.local_version)
        .map_err(|e| e.to_string())?;
    log!("save_note (localfs-sync): uuid={} id={}", real_uuid, saved.id);
    Ok(Some(gmail::SavedNote {
        id: saved.id,
        version: saved.version,
        uuid: cached.uuid.clone(),
        date: saved.date,
        created_date: saved.created_date,
        body_html: saved.body_html,
        local_version: cached.local_version,
    }))
}

#[tauri::command]
pub(super) async fn get_cached_note(account_id: String, uuid: String, state: State<'_, AppState>) -> Result<Option<gmail::Note>, String> {
    cached_note_db(&state.db, &account_id, &uuid)
}

/// The editor's refused or held-back save, merged with the row instead of
/// becoming a conflict copy when the two changed different lines. Reads only;
/// the editor saves the answer through `save_note`. See `merge_onto_row_db`.
#[tauri::command]
pub(super) fn merge_onto_row(
    account_id: String,
    uuid: String,
    title: String,
    base_body: String,
    body_html: String,
    state: State<'_, AppState>,
) -> Result<Option<MovedRowMerge>, String> {
    merge_onto_row_db(&state.db, &account_id, &uuid, &title, &base_body, &body_html)
}

#[tauri::command]
pub(super) fn note_persistence(account_id: String, uuid: String, state: State<'_, AppState>) -> Result<Option<NotePersistence>, String> {
    note_persistence_db(&state.db, &account_id, &uuid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::localfs::LocalFsVertical;
    use crate::backend::NoteStore;

    const ACCT: &str = "localfs:TEST";

    fn save(db: &db::Db, uuid: Option<&str>, body: &str, expected: Option<i64>) -> Result<db::CachedNote, String> {
        save_note_db(db, ACCT, accounts::BackendKind::LocalFs, uuid, "T", body, "Notes", None, expected,
            &crate::provenance::Actor::test(), None)
    }

    /// The vault cannot take the write (here its root is a file, which fails
    /// for any user, root included; on the device it was a read-only folder).
    /// The save already happened in SQLite: it must not be reported lost, the
    /// row stays dirty for the worker, and the version the command hands back
    /// is the one the editor's next save is checked against — so that save
    /// is applied, not refused as "changed elsewhere" (which made a conflict
    /// copy per pause, 2026-11-09).
    #[tokio::test]
    async fn a_localfs_write_that_fails_leaves_the_saved_row_to_the_worker() {
        let db = crate::test_support::temp_db();
        let dir = tempfile::tempdir().unwrap();
        let not_a_folder = dir.path().join("vault");
        std::fs::write(&not_a_folder, b"a file where the vault should be").unwrap();
        let v = LocalFsVertical::new(not_a_folder, ACCT.into());

        let cached = save(&db, None, "<div>first</div>", None).unwrap();
        let pushed = push_localfs_now(&db, &v, ACCT, &cached).await;
        assert!(matches!(pushed, Ok(None)), "a failed write is not a failed save: {pushed:?}");

        let row = db.get(&cached.uuid, ACCT).unwrap().expect("the row is kept");
        assert_eq!(row.sync_state, db::SyncState::Dirty, "still to be pushed by the worker");
        assert!(row.body_html.contains("first"));
        assert_eq!(row.local_version, cached.local_version);

        let next = save(&db, Some(&cached.uuid), "<div>first, more</div>", Some(cached.local_version));
        assert!(next.is_ok(), "the editor's next save must be applied: {next:?}");
        let rows = db.list_notes(ACCT).unwrap();
        assert_eq!(rows.len(), 1, "no conflict copy: {:?}", rows.iter().map(|r| &r.title).collect::<Vec<_>>());
    }

    /// Gotcha #40 on the save command's own write: another device rewrote the
    /// file after this row last read it, then the user typed. The write must
    /// not erase the other device's text; the row stays dirty and the
    /// worker's push reconciles it.
    #[tokio::test]
    async fn a_localfs_write_over_a_file_changed_elsewhere_is_left_to_the_worker() {
        let db = crate::test_support::temp_db();
        let vault = tempfile::tempdir().unwrap();
        let v = LocalFsVertical::new(vault.path().to_path_buf(), ACCT.into());

        let first = save(&db, None, "<div>base</div>", None).unwrap();
        let written = push_localfs_now(&db, &v, ACCT, &first).await.unwrap().expect("written now");

        // LocalFs's version is the Date header at second resolution: wait one
        // out, or the other device's write is indistinguishable from ours.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        let theirs = crate::backend::SaveOp { title: "T", body_html: "<div>base</div><div>theirs</div>",
            existing_remote_id: Some(&written.id), existing_uuid: Some(&first.uuid), existing_created_date: None,
            label: "Notes", base_version: None };
        v.save_note_full(&theirs, &[]).await.unwrap();

        let mine = save(&db, Some(&first.uuid), "<div>base</div><div>mine</div>", Some(first.local_version)).unwrap();
        let pushed = push_localfs_now(&db, &v, ACCT, &mine).await;
        assert!(matches!(pushed, Ok(None)), "must not write over the other device: {pushed:?}");

        let on_disk = std::fs::read_to_string(vault.path().join(&written.id)).unwrap();
        assert!(on_disk.contains("theirs"), "the other device's text is still in the file: {on_disk}");
        let row = db.get(&first.uuid, ACCT).unwrap().unwrap();
        assert_eq!(row.sync_state, db::SyncState::Dirty, "left to the worker");
        assert!(row.body_html.contains("mine"));
    }

    /// Jodd's own last write is not "changed elsewhere": the next save writes.
    #[tokio::test]
    async fn a_localfs_write_after_jodds_own_write_proceeds() {
        let db = crate::test_support::temp_db();
        let vault = tempfile::tempdir().unwrap();
        let v = LocalFsVertical::new(vault.path().to_path_buf(), ACCT.into());

        let first = save(&db, None, "<div>one</div>", None).unwrap();
        push_localfs_now(&db, &v, ACCT, &first).await.unwrap().expect("written now");
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        let second = save(&db, Some(&first.uuid), "<div>one</div><div>two</div>", Some(first.local_version)).unwrap();
        let saved = push_localfs_now(&db, &v, ACCT, &second).await.unwrap().expect("written now");
        assert!(std::fs::read_to_string(vault.path().join(&saved.id)).unwrap().contains("two"));
    }

    #[tokio::test]
    async fn a_localfs_write_that_lands_marks_the_row_pushed() {
        let db = crate::test_support::temp_db();
        let vault = tempfile::tempdir().unwrap();
        let v = LocalFsVertical::new(vault.path().to_path_buf(), ACCT.into());

        let cached = save(&db, None, "<div>hello</div>", None).unwrap();
        let saved = push_localfs_now(&db, &v, ACCT, &cached).await.unwrap().expect("written now");
        assert!(vault.path().join(&saved.id).exists());
        assert_eq!(saved.local_version, cached.local_version);
        let row = db.get(&cached.uuid, ACCT).unwrap().unwrap();
        assert_eq!(row.sync_state, db::SyncState::Clean);
        assert_eq!(row.id, saved.id);
    }
}
