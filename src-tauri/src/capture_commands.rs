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

/// Save as-is. Takes the capture's id, never its content: what is saved is
/// exactly what the OS delivered, escaped in Rust (`capture::body_html`).
#[tauri::command]
pub fn save_capture(
    account_id: String,
    capture_id: String,
    title: Option<String>,
    state: State<'_, AppState>,
) -> Result<ExtractedNoteDto, String> {
    // Refuse a write this account can't accept BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    crate::refuse_write(&state, &account_id, crate::backend::Write::Notes)?;
    let backend_kind = {
        let list = state.accounts.lock().unwrap();
        list.iter()
            .find(|a| a.id == account_id && a.is_active())
            .map(|a| a.backend_kind)
            .ok_or_else(|| format!("No active account '{account_id}'."))?
    };
    let capture = state
        .captures
        .lock()
        .unwrap()
        .get(&capture_id)
        .ok_or("That shared item is no longer waiting — it may have been saved or discarded already.")?;
    let can_create = crate::account_can_create_folders(&state, &account_id);
    let created = write_capture_note(&state.db, &account_id, backend_kind, can_create, &capture, title.as_deref())?;
    state.captures.lock().unwrap().remove(&capture_id);
    crate::log!("save_capture: created note uuid={} in {}", created.uuid, created.label);
    Ok(created)
}

/// The write itself, free of `AppState` so it is testable against a temp DB
/// (gotcha #16: test save → DB, not the wire). Extract's shape exactly:
/// `resolve_destination` → `mint_uuid_for` → `insert_local_new` (dirty).
pub(crate) fn write_capture_note(
    db: &db::Db,
    account_id: &str,
    backend_kind: crate::accounts::BackendKind,
    can_create_folders: bool,
    capture: &PendingCapture,
    title: Option<&str>,
) -> Result<ExtractedNoteDto, String> {
    let title = title
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|t| !t.is_empty())
        .map(|t| t.chars().take(capture::MAX_TITLE_CHARS).collect::<String>())
        .unwrap_or_else(|| capture.default_title.clone());
    let folder = crate::llm::filing::resolve_destination(db, account_id, can_create_folders)
        .map_err(|e| format!("resolve destination: {e}"))?;
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
    db.insert_local_new(&note).map_err(|e| format!("insert_local_new: {e}"))?;
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
        let created = write_capture_note(&db, "gmail:a@b.c", BackendKind::Gmail, true, &c, Some("  My\n title ")).unwrap();
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
        let created = write_capture_note(&db, "microsoft:a@b.c", BackendKind::Microsoft, false, &pending("x"), None).unwrap();
        assert_eq!(created.label, crate::llm::filing::NOTES_ROOT);
    }

    #[test]
    fn blank_title_uses_the_default_and_uuid_follows_the_backend_shape() {
        let (_dir, db) = temp_db();
        let c = pending("First line\nmore");
        let created = write_capture_note(&db, "icloud:a@b.c", BackendKind::ICloud, true, &c, Some("   ")).unwrap();
        let row = db.get(&created.uuid, "icloud:a@b.c").unwrap().unwrap();
        assert_eq!(row.title, "First line");
        assert_eq!(created.uuid, created.uuid.to_lowercase(), "a CloudKit recordName is lowercase (gotcha #18)");
    }
}
