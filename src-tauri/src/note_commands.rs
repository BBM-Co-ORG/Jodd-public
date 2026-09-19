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
        )?
    };
    let real_uuid = cached.uuid.clone();

    // For LocalFS accounts push the file to disk right now, synchronously.
    // A local filesystem write completes in < 1ms — there is no benefit to
    // deferring it to the 5-second worker tick, and deferring creates a race:
    // if the user deletes the note before the worker runs, id is still "" and
    // push_one_deletion drops the DB row without moving anything to .trash/,
    // so the note never appears in "Recently Deleted".
    if matches!(acct_kind, Some(accounts::BackendKind::LocalFs)) {
        let existing_id = if cached.id.is_empty() { None } else { Some(cached.id.as_str()) };
        let attachments = db.list_attachments(&account_id, &real_uuid).unwrap_or_default();
        let v = vertical_for(&state, &account_id).await?;
        let op = crate::backend::SaveOp {
            title: &cached.title,
            body_html: &cached.body_html,
            existing_remote_id: existing_id,
            existing_uuid: Some(cached.uuid.as_str()),
            existing_created_date: cached.x_mail_created_date.as_deref(),
            label: &cached.label,
        };
        let saved = v.save_note_full(&op, &attachments).await.map_err(|e| e.to_string())?;
        db.mark_pushed(&real_uuid, &account_id, &saved.id, &saved.version, &saved.date, &saved.body_html, cached.local_version)
            .map_err(|e| e.to_string())?;
        log!(
            "save_note (localfs-sync): uuid={} id={}",
            real_uuid, saved.id
        );
        return Ok(gmail::SavedNote {
            id: saved.id,
            version: saved.version,
            uuid: cached.uuid,
            date: saved.date,
            body_html: saved.body_html,
            local_version: cached.local_version,
        });
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
        body_html: cached.body_html,
        local_version: cached.local_version,
    })
}

#[tauri::command]
pub(super) fn note_persistence(account_id: String, uuid: String, state: State<'_, AppState>) -> Result<Option<NotePersistence>, String> {
    note_persistence_db(&state.db, &account_id, &uuid)
}
