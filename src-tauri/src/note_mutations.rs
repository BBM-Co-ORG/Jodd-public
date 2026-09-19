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
    if existing.is_some() {
        let applied = match expected_local_version {
            Some(expected) => db
                .apply_local_edit_versioned(&real_uuid, account_id, title, body_html, label, expected)
                .map_err(|e| e.to_string())?,
            None => {
                db.apply_local_edit(&real_uuid, account_id, title, body_html, label)
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
        };
        db.insert_local_new(&new_note).map_err(|e| e.to_string())?;
    }

    // Read back the row so the response reflects current state (most
    // importantly: the cached `id` if any prior push has succeeded).
    db.get(&real_uuid, account_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "save_note: row vanished after write".to_string())
}
