//! Tauri face of Share to Jodd (spec 2026-10-06-share-to-jodd §3.2). All
//! SQLite-only: the capture sheet never waits on the remote.

use tauri::{Emitter, Manager, State};

use crate::capture::{self, PendingCapture, Route};
use crate::{db, AppState, ExtractedNoteDto};

/// Event the capture sheet listens for. Carries nothing: the sheet always
/// re-reads the queue with `take_pending_captures`, so a capture queued
/// before the page was listening is still shown (gotcha #6, #32).
pub const CAPTURE_RECEIVED: &str = "capture-received";
/// A capture link that could not be read. Payload: the reason, for ErrorBar.
pub const CAPTURE_ERROR: &str = "capture-error";

/// Every deep link on every platform comes through here.
pub fn handle_deep_link(app: &tauri::AppHandle, url: &str, on_oauth: impl FnOnce(String, String)) {
    match capture::route(url, crate::parse_oauth_callback) {
        Route::OAuth(code, state) => on_oauth(code, state),
        Route::Capture(Ok(payload)) => {
            let state = app.state::<AppState>();
            let today = chrono::Local::now().format("%Y-%m-%d").to_string();
            let pushed = state.captures.lock().unwrap().push(payload, db::now_ms(), &today);
            match pushed.result {
                Ok(id) => {
                    crate::log!("capture: queued {id}");
                    // Raise at most every few seconds: a page looping jodd://
                    // links must not be able to keep stealing focus.
                    if pushed.raise {
                        focus_main_window(app);
                    }
                    let _ = app.emit(CAPTURE_RECEIVED, ());
                }
                Err(capture::Dropped::Duplicate) => crate::log!("capture: dropped a duplicate"),
                Err(capture::Dropped::Full) => {
                    crate::log!("capture: dropped — {} already waiting", capture::MAX_PENDING);
                    if pushed.notify_full {
                        let _ = app.emit(
                            CAPTURE_ERROR,
                            format!(
                                "{} shared items are already waiting in Jodd. Save or discard some, then share again.",
                                capture::MAX_PENDING
                            ),
                        );
                    }
                }
            }
        }
        Route::Capture(Err(e)) => {
            crate::log!("capture: refused a capture link: {e}");
            let _ = app.emit(CAPTURE_ERROR, format!("Couldn't read what was shared: {e}."));
        }
        Route::Ignore => crate::log!("deep link ignored (not a capture or sign-in link)"),
    }
}

pub fn focus_main_window(app: &tauri::AppHandle) {
    #[cfg(desktop)]
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
    #[cfg(not(desktop))]
    let _ = app;
}

#[tauri::command]
pub fn take_pending_captures(state: State<'_, AppState>) -> Vec<PendingCapture> {
    state.captures.lock().unwrap().pending()
}

#[tauri::command]
pub fn discard_capture(capture_id: String, state: State<'_, AppState>) -> bool {
    state.captures.lock().unwrap().remove(&capture_id)
}

/// The Summarize path files its note through Extract / ingest; the sheet
/// calls this once that note exists, so the capture leaves the queue.
#[tauri::command]
pub fn complete_capture(capture_id: String, state: State<'_, AppState>) -> bool {
    state.captures.lock().unwrap().remove(&capture_id)
}

/// The "Share to Jodd" Shortcut, signed with `shortcuts sign --mode anyone`
/// (spec §3.6). Marked "Show in Share Sheet", so once added it appears in the
/// Share menu of Safari, Chrome, Finder and Notes; it URL-encodes whatever it
/// is given and opens `jodd://capture?text=…`. Regenerate it with
/// `scripts/gen-share-shortcut.py` — an unsigned file is refused by Shortcuts.
const SHARE_SHORTCUT: &[u8] = include_bytes!("../assets/Share to Jodd.shortcut");

/// macOS: hand the Shortcut to the Shortcuts app, which asks the person to
/// add it. Nothing is installed without that click.
#[tauri::command]
pub fn install_share_shortcut() -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return Err("The Share menu shortcut is for macOS.".into());
    }
    let path = std::env::temp_dir().join("Share to Jodd.shortcut");
    std::fs::write(&path, SHARE_SHORTCUT).map_err(|e| format!("Couldn't write the shortcut: {e}"))?;
    let status = std::process::Command::new("open")
        .arg(&path)
        .status()
        .map_err(|e| format!("Couldn't open Shortcuts: {e}"))?;
    if !status.success() {
        return Err(format!("Shortcuts didn't open the shortcut ({status})."));
    }
    Ok(())
}

/// What the sheet's Folder picker offers for one account. SQLite only.
#[derive(serde::Serialize)]
pub struct CaptureFolders {
    /// Where "no choice" files the note (`filing::default_destination`) —
    /// the Inbox, or the root when it can't be used — so the picker names it.
    default: String,
    /// `filing::chosen_destinations`: exactly what `save_capture` accepts.
    folders: Vec<String>,
}

#[tauri::command]
pub fn list_capture_folders(account_id: String, state: State<'_, AppState>) -> Result<CaptureFolders, String> {
    let can_create = crate::account_can_create_folders(&state, &account_id);
    let db = &state.db;
    Ok(CaptureFolders {
        default: crate::llm::filing::default_destination(db, &account_id, can_create).map_err(|e| e.to_string())?,
        folders: crate::llm::filing::chosen_destinations(db, &account_id).map_err(|e| e.to_string())?,
    })
}

/// The active account's backend, or the refusal both writes give.
fn active_backend(state: &AppState, account_id: &str) -> Result<crate::accounts::BackendKind, String> {
    let list = state.accounts.lock().unwrap();
    list.iter()
        .find(|a| a.id == account_id && a.is_active())
        .map(|a| a.backend_kind)
        .ok_or_else(|| format!("No active account '{account_id}'."))
}

/// Save as-is. Takes the capture's id, never its content: what is saved is
/// exactly what the OS delivered, escaped in Rust (`capture::body_html`).
/// `folder` is one the sheet offered (`list_capture_folders`); `None` is the
/// default, `resolve_destination`.
#[tauri::command]
pub fn save_capture(
    account_id: String,
    capture_id: String,
    title: Option<String>,
    folder: Option<String>,
    state: State<'_, AppState>,
) -> Result<ExtractedNoteDto, String> {
    // Refuse a write this account can't accept BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    crate::refuse_write(&state, &account_id, crate::backend::Write::Notes)?;
    let backend_kind = active_backend(&state, &account_id)?;
    let capture = state
        .captures
        .lock()
        .unwrap()
        .get(&capture_id)
        .ok_or("That shared item is no longer waiting — it may have been saved or discarded already.")?;
    let can_create = crate::account_can_create_folders(&state, &account_id);
    let created =
        write_capture_note(&state.db, &account_id, backend_kind, can_create, &capture, title.as_deref(), folder.as_deref())?;
    state.captures.lock().unwrap().remove(&capture_id);
    crate::log!("save_capture: created note uuid={} in {}", created.uuid, created.label);
    Ok(created)
}

/// Add to an existing note. Like `save_capture`, takes the capture's id and
/// never its content. Appends only — never renames or moves the note.
#[tauri::command]
pub fn append_capture(
    account_id: String,
    capture_id: String,
    target_uuid: String,
    state: State<'_, AppState>,
) -> Result<ExtractedNoteDto, String> {
    // Write::Notes only: an append creates no folder. See `refuse_write`.
    crate::refuse_write(&state, &account_id, crate::backend::Write::Notes)?;
    // Like save_capture: an account being removed (or signed out) takes no
    // new work — refuse_write alone lets an inactive account through.
    active_backend(&state, &account_id)?;
    let capture = state
        .captures
        .lock()
        .unwrap()
        .get(&capture_id)
        .ok_or("That shared item is no longer waiting — it may have been saved or discarded already.")?;
    let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    let done = append_capture_to_note(&state.db, &account_id, &target_uuid, &capture, &stamp)?;
    state.captures.lock().unwrap().remove(&capture_id);
    crate::log!("append_capture: appended to note uuid={} in {}", done.uuid, done.label);
    Ok(done)
}

/// The append itself, free of `AppState` (gotcha #16: test save → DB). The
/// body is recomputed from a fresh read on every attempt
/// (`apply_local_edit_with_retry`), so a concurrent edit is never lost, and
/// lands inside the document (`mime822::append_html`, gotcha #37).
pub(crate) fn append_capture_to_note(
    db: &db::Db,
    account_id: &str,
    target_uuid: &str,
    capture: &PendingCapture,
    stamp: &str,
) -> Result<ExtractedNoteDto, String> {
    // The sheet may hold a uuid a create push has since rekeyed (gotcha #16).
    let uuid = db.resolve_note_uuid(target_uuid, account_id).map_err(|e| e.to_string())?;
    let gone = || "That note no longer exists in Jodd. Pick another note.".to_string();
    if db.get(&uuid, account_id).map_err(|e| e.to_string())?.is_none() {
        return Err(gone());
    }
    let fragment = capture::appended_html(&capture.payload, stamp);
    let mut label = String::new();
    crate::apply_local_edit_with_retry(db, account_id, &uuid, 5, &crate::provenance::Actor::Human, |existing| {
        if existing.sync_state == db::SyncState::DeletedPending {
            return Err(gone());
        }
        // The editor locks such a note (`NoteEditor`'s `editable`); an append
        // must not get around that.
        if existing.push_blocked_by_remote {
            return Err(format!(
                "That note can't be changed from Jodd: {}",
                existing.push_blocked_reason.as_deref().unwrap_or("its own app refuses the edit")
            ));
        }
        label = existing.label.clone();
        Ok((existing.title.clone(), crate::mime822::append_html(&existing.body_html, &fragment)))
    })?;
    Ok(ExtractedNoteDto { uuid, label })
}

/// The write itself, free of `AppState` so it is testable against a temp DB
/// (gotcha #16: test save → DB, not the wire). Extract's shape exactly:
/// `resolve_destination` (or the chosen folder) → `mint_uuid_for` →
/// `insert_local_new` (dirty).
pub(crate) fn write_capture_note(
    db: &db::Db,
    account_id: &str,
    backend_kind: crate::accounts::BackendKind,
    can_create_folders: bool,
    capture: &PendingCapture,
    title: Option<&str>,
    folder: Option<&str>,
) -> Result<ExtractedNoteDto, String> {
    let title = title
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|t| !t.is_empty())
        .map(|t| t.chars().take(capture::MAX_TITLE_CHARS).collect::<String>())
        .unwrap_or_else(|| capture.default_title.clone());
    let folder = match folder {
        None => crate::llm::filing::resolve_destination(db, account_id, can_create_folders)
            .map_err(|e| format!("resolve destination: {e}"))?,
        // Refused, never filed elsewhere: the person chose this folder, and
        // one blocked or being deleted would hold the note unsynced (#162).
        Some(chosen) => {
            let offered = crate::llm::filing::chosen_destinations(db, account_id).map_err(|e| e.to_string())?;
            if !offered.iter().any(|p| p == chosen) {
                return Err(format!(
                    "Can't save into \"{chosen}\" — it is being deleted, can't sync, or no longer exists. Pick another folder."
                ));
            }
            chosen.to_string()
        }
    };
    // Per-backend shape (gotcha #18): a CloudKit recordName is lowercase.
    let uuid = crate::backend::mint_uuid_for(backend_kind);
    let now = db::now_ms();
    let note = db::CachedNote {
        uuid: uuid.clone(),
        account_id: account_id.to_string(),
        id: String::new(),
        title,
        body_html: capture::body_html(&capture.payload),
        date: chrono::Local::now().to_rfc2822(),
        x_mail_created_date: None,
        label: folder.clone(),
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
    db.insert_local_new(&note, &crate::provenance::Actor::Human).map_err(|e| format!("insert_local_new: {e}"))?;
    Ok(ExtractedNoteDto { uuid, label: folder })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::BackendKind;
    use crate::capture::{CapturePayload, Queue};

    /// A temp, unencrypted DB; the TempDir must outlive it.
    fn temp_db() -> (tempfile::TempDir, db::Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        (dir, db)
    }

    fn pending(text: &str) -> PendingCapture {
        let mut q = Queue::default();
        let p = CapturePayload { text: Some(text.into()), url: Some("https://a.example/".into()), title: None };
        let id = q.push(p, 0, "2026-10-06").result.unwrap();
        q.get(&id).unwrap()
    }

    #[test]
    fn the_bundled_share_shortcut_is_signed() {
        // `shortcuts sign` output is an Apple Encrypted Archive; a plain
        // plist (unsigned) starts with "bplist" and Shortcuts refuses it.
        assert_eq!(&SHARE_SHORTCUT[..4], b"AEA1", "re-sign it: shortcuts sign --mode anyone");
    }

    #[test]
    fn save_capture_refuses_before_it_touches_the_queue_or_sqlite() {
        let src = include_str!("capture_commands.rs");
        let body = crate::test_support::extract_fn_body(src, "pub fn save_capture(");
        let refuse = body.find("refuse_write(").expect("save_capture must call refuse_write");
        for later in ["captures", "write_capture_note(", "resolve_destination"] {
            let at = body.find(later).unwrap_or(usize::MAX);
            assert!(refuse < at, "refuse_write must come before `{later}`");
        }
    }

    #[test]
    fn writes_a_dirty_note_into_the_inbox_with_an_escaped_body() {
        let (_dir, db) = temp_db();
        let c = pending("<img src=x onerror=alert(1)> hello");
        let created = write_capture_note(&db, "gmail:a@b.c", BackendKind::Gmail, true, &c, Some("  My\n title "), None).unwrap();
        assert_eq!(created.label, crate::llm::filing::INBOX_PATH);
        let row = db.get(&created.uuid, "gmail:a@b.c").unwrap().expect("row written");
        assert_eq!(row.sync_state, db::SyncState::Dirty);
        assert_eq!(row.title, "My title");
        assert!(!row.body_html.contains("<img"), "body must be escaped: {}", row.body_html);
        assert!(row.body_html.contains("<a href=\"https://a.example/\">"));
    }

    #[test]
    fn falls_back_to_the_root_when_the_backend_cannot_create_folders() {
        let (_dir, db) = temp_db();
        let created = write_capture_note(&db, "microsoft:a@b.c", BackendKind::Microsoft, false, &pending("x"), None, None).unwrap();
        assert_eq!(created.label, crate::llm::filing::NOTES_ROOT);
    }

    #[test]
    fn a_chosen_folder_is_where_the_note_goes() {
        let (_dir, db) = temp_db();
        db.create_folder_local_new("gmail:a@b.c", "Notes/Reading").unwrap();
        let created =
            write_capture_note(&db, "gmail:a@b.c", BackendKind::Gmail, true, &pending("x"), None, Some("Notes/Reading")).unwrap();
        assert_eq!(created.label, "Notes/Reading");
        assert_eq!(db.get(&created.uuid, "gmail:a@b.c").unwrap().unwrap().label, "Notes/Reading");
        let root = write_capture_note(&db, "gmail:a@b.c", BackendKind::Gmail, true, &pending("y"), None, Some("Notes")).unwrap();
        assert_eq!(root.label, "Notes");
    }

    /// The picker lists `filing::chosen_destinations`; a folder that left
    /// that list after the sheet loaded is refused, never filed elsewhere —
    /// a note in a blocked folder would sit unsynced (#162).
    #[test]
    fn a_chosen_folder_that_is_blocked_going_or_gone_is_refused_before_sqlite() {
        let (_dir, db) = temp_db();
        let acct = "gmail:a@b.c";
        db.create_folder_local_new(acct, "Notes/Blocked/Sub").unwrap();
        db.mark_folder_push_blocked(acct, "Notes/Blocked", db::FolderSyncState::DirtyNew, "permanent: refused").unwrap();
        db.upsert_folder_from_remote(acct, "Notes/Going", "Label_1").unwrap();
        db.force_folder_sync_state(acct, "Notes/Going", db::FolderSyncState::DeletedPending);
        for path in ["Notes/Blocked", "Notes/Blocked/Sub", "Notes/Going", "Notes/Nowhere"] {
            let err = write_capture_note(&db, acct, BackendKind::Gmail, true, &pending("x"), None, Some(path)).unwrap_err();
            assert!(err.contains(path), "{path}: {err}");
        }
        assert!(db.list_notes(acct).unwrap().is_empty(), "nothing written");
    }

    fn existing(db: &db::Db, uuid: &str, body: &str) {
        let n = db::CachedNote {
            uuid: uuid.into(),
            account_id: "gmail:a@b.c".into(),
            id: "msg1".into(),
            title: "Reading list".into(),
            body_html: body.into(),
            date: String::new(),
            x_mail_created_date: None,
            label: "Notes/Reading".into(),
            local_version: 1,
            remote_version: None,
            sync_state: db::SyncState::Dirty,
            last_synced_at: None,
            last_local_modified_at: 0,
            last_remote_modified_at: None,
            pinned: false,
            meta_msg_id: None,
            pin_dirty: false,
            push_blocked_reason: None,
            push_blocked_by_remote: false,
        };
        db.insert_local_new(&n, &crate::provenance::Actor::test()).unwrap();
    }

    #[test]
    fn append_adds_the_dated_capture_inside_the_body_and_keeps_title_and_folder() {
        let (_dir, db) = temp_db();
        existing(&db, "N1", "<html><head></head><body><div>Reading list</div><div>old</div></body></html>");
        let c = pending("<script>x</script> new");
        let done = append_capture_to_note(&db, "gmail:a@b.c", "N1", &c, "2026-10-09 14:32").unwrap();
        assert_eq!((done.uuid.as_str(), done.label.as_str()), ("N1", "Notes/Reading"));
        let row = db.get("N1", "gmail:a@b.c").unwrap().unwrap();
        assert_eq!(row.title, "Reading list");
        assert_eq!(row.label, "Notes/Reading");
        assert_eq!(row.sync_state, db::SyncState::Dirty);
        // Inside the document (gotcha #37), after the old content.
        let added = capture::appended_html(&c.payload, "2026-10-09 14:32");
        assert!(row.body_html.ends_with(&format!("<div>old</div>{added}</body></html>")), "{}", row.body_html);
        assert!(!row.body_html.contains("<script>"), "escaped: {}", row.body_html);
    }

    #[test]
    fn append_refuses_a_note_that_is_gone_being_deleted_or_locked_by_icloud() {
        let (_dir, db) = temp_db();
        let acct = "gmail:a@b.c";
        assert!(append_capture_to_note(&db, acct, "MISSING", &pending("x"), "t").unwrap_err().contains("no longer exists"));

        existing(&db, "DEL", "<div>a</div>");
        db.mark_deleted("DEL", acct).unwrap();
        assert!(append_capture_to_note(&db, acct, "DEL", &pending("x"), "t").is_err());

        existing(&db, "LOCK", "<div>a</div>");
        db.mark_push_blocked_by_remote("LOCK", acct, "permanent: inline objects").unwrap();
        let err = append_capture_to_note(&db, acct, "LOCK", &pending("x"), "t").unwrap_err();
        assert!(err.contains("inline objects"), "{err}");
        assert_eq!(db.get("LOCK", acct).unwrap().unwrap().body_html, "<div>a</div>", "untouched");
    }

    /// Gotcha #16: the sheet may hold a uuid a create push has since rekeyed.
    #[test]
    fn append_follows_a_rekeyed_note_to_its_new_uuid() {
        let (_dir, db) = temp_db();
        existing(&db, "local-uuid", "<div>a</div>");
        db.rekey_note_uuid("local-uuid", "remote-uuid", "gmail:a@b.c").unwrap();
        let done = append_capture_to_note(&db, "gmail:a@b.c", "local-uuid", &pending("x"), "t").unwrap();
        assert_eq!(done.uuid, "remote-uuid");
        assert!(db.get("remote-uuid", "gmail:a@b.c").unwrap().unwrap().body_html.contains("— t —"));
    }

    #[test]
    fn append_capture_refuses_before_it_touches_the_queue_or_sqlite() {
        let src = include_str!("capture_commands.rs");
        let body = crate::test_support::extract_fn_body(src, "pub fn append_capture(");
        let refuse = body.find("refuse_write(").expect("append_capture must call refuse_write");
        let active = body.find("active_backend(").expect("append_capture must refuse an inactive account");
        for later in ["captures", "append_capture_to_note("] {
            assert!(active < body.find(later).unwrap_or(usize::MAX), "the active check must come before `{later}`");
            let at = body.find(later).unwrap_or(usize::MAX);
            assert!(refuse < at, "refuse_write must come before `{later}`");
        }
    }

    #[test]
    fn blank_title_uses_the_default_and_uuid_follows_the_backend_shape() {
        let (_dir, db) = temp_db();
        let c = pending("First line\nmore");
        let created = write_capture_note(&db, "icloud:a@b.c", BackendKind::ICloud, true, &c, Some("   "), None).unwrap();
        let row = db.get(&created.uuid, "icloud:a@b.c").unwrap().unwrap();
        assert_eq!(row.title, "First line");
        assert_eq!(created.uuid, created.uuid.to_lowercase(), "a CloudKit recordName is lowercase (gotcha #18)");
    }
}
