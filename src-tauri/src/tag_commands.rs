//! Pins and tags: the commands behind the pin toggle, the tag chips and the
//! sidebar's tag list.
//!
//! Both are local-first writes — SQLite first, the sidecar or note body
//! pushed by the sync worker — and both go through `refuse_write`, so a
//! backend that cannot store them says so here rather than at push time.
//!
//! Moved out of `lib.rs` 2026-11-16, unchanged.

use tauri::State;

use crate::backend::gmail::wire as gmail;
use crate::db::normalize_tag;
use crate::provenance::Actor;
use crate::{backend, hidden_account_ids, log, refuse_write, AppState};

#[tauri::command]
pub async fn set_pin(
    account_id: String,
    uuid: String,
    pinned: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Sidecars)?;
    state.db.set_pin(&uuid, &account_id, pinned).map_err(|e| e.to_string())?;
    log!(
        "set_pin: account={} uuid={} pinned={}",
        account_id, uuid, pinned
    );
    Ok(())
}

/// Batch pin/unpin — flips the column on every uuid in one SQLite
/// transaction. Same atomicity argument as `move_notes_batch`. The
/// `pinned` flag is uniform across the batch; the menu decides which
/// direction by inspecting whether the selection is all-pinned or
/// all-unpinned before calling.
#[tauri::command]
pub async fn set_pin_batch(
    account_id: String,
    uuids: Vec<String>,
    pinned: bool,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Sidecars)?;
    let touched = state.db
        .set_pin_batch(&account_id, &uuids, pinned)
        .map_err(|e| e.to_string())?;
    log!(
        "set_pin_batch: account={} touched={}/{} pinned={}",
        account_id, touched, uuids.len(), pinned
    );
    Ok(touched)
}

// ─── Tags (Jodd-local, mirrors Pin wave 1) ───────────────────────────────────
//
// Tags live ONLY in SQLite (the note_tags table), never in the note body, so
// they never collide with `#` in URLs/code and never round-trip to Apple Notes
// (which has no tagging). Pure local-first: each command is a single SQLite
// write/read with no Gmail involvement and no worker path.

#[derive(serde::Serialize)]
pub struct TagCount {
    tag: String,
    count: i64,
}

#[derive(serde::Serialize)]
pub struct NoteTag {
    uuid: String,
    tag: String,
}


/// Add a tag to a note. Returns the normalized form so the frontend can
/// reconcile its optimistic value with what was actually stored.
#[tauri::command]
pub async fn add_tag(
    account_id: String,
    uuid: String,
    tag: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Sidecars)?;
    let norm = normalize_tag(&tag).ok_or_else(|| format!("Invalid tag: {:?}", tag))?;
    state.db.add_tag(&account_id, &uuid, &norm).map_err(|e| e.to_string())?;
    log!("add_tag: account={} uuid={} tag={}", account_id, uuid, norm);
    Ok(norm)
}

/// Remove a tag from a note.
#[tauri::command]
pub async fn remove_tag(
    account_id: String,
    uuid: String,
    tag: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Sidecars)?;
    let norm = normalize_tag(&tag).unwrap_or_else(|| tag.clone());
    state.db.remove_tag(&account_id, &uuid, &norm).map_err(|e| e.to_string())?;
    log!("remove_tag: account={} uuid={} tag={}", account_id, uuid, norm);
    Ok(())
}

/// Every tag for an account with its note count — drives the sidebar.
#[tauri::command]
pub async fn list_tags(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<TagCount>, String> {
    let rows = state.db.list_all_tags(&account_id).map_err(|e| e.to_string())?;
    Ok(rows.into_iter().map(|(tag, count)| TagCount { tag, count }).collect())
}

/// (uuid, tag) for every tagged note — the frontend folds this into a
/// uuid → tags[] map for rendering chips.
#[tauri::command]
pub async fn list_note_tags(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<NoteTag>, String> {
    let rows = state.db.list_all_note_tags(&account_id).map_err(|e| e.to_string())?;
    Ok(rows.into_iter().map(|(uuid, tag)| NoteTag { uuid, tag }).collect())
}

/// Cached notes carrying ANY of `tags` (the union). Pure local read — the
/// tag-navigation parallel of `list_cached_notes_in_folder`. The frontend
/// narrows the union to AND/OR per the active match mode, so loading the
/// union here serves either mode without a re-query on toggle.
#[tauri::command]
pub async fn list_cached_notes_with_tags(
    account_id: Option<String>,
    tags: Vec<String>,
    state: State<'_, AppState>,
) -> Result<Vec<gmail::Note>, String> {
    let norm: Vec<String> = tags
        .iter()
        .filter_map(|t| normalize_tag(t))
        .collect();
    // account_id = None → search every account (cross-account tag filter).
    let cached = state.db
        .list_notes_with_tags(account_id.as_deref(), &norm, &hidden_account_ids(&state))
        .map_err(|e| e.to_string())?;
    Ok(cached.into_iter().map(|c| c.to_frontend_note()).collect())
}

/// Rename a tag across every note in the account (global). Returns the
/// normalized new tag so the frontend can reconcile its optimistic value.
#[tauri::command]
pub async fn rename_tag(
    account_id: String,
    old_tag: String,
    new_tag: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    // Tagged Sidecars, not Notes, even though the mechanism below edits note
    // BODIES: tag identity is the sidecar concern being changed here (see
    // `add_tag`/`remove_tag`), so it must gate with the rest of the tag
    // commands. Do not "fix" this to Notes just because it touches content.
    refuse_write(&state, &account_id, backend::Write::Sidecars)?;
    let from = normalize_tag(&old_tag).ok_or_else(|| format!("Invalid tag: {:?}", old_tag))?;
    let to = normalize_tag(&new_tag).ok_or_else(|| format!("Invalid tag: {:?}", new_tag))?;
    // Inline model: rewrites #from → #to in every carrying note's BODY, marks
    // them content-dirty (the normal content push round-trips the rename to
    // Apple), and re-derives note_tags. No tag sidecar involved.
    let n = state.db.rename_tag(&account_id, &from, &to, &Actor::Human).map_err(|e| e.to_string())?;
    log!("rename_tag: account={} '{}' -> '{}' ({} notes)", account_id, from, to, n);
    Ok(to)
}

/// Delete a tag from every note in the account (global).
#[tauri::command]
pub async fn delete_tag(
    account_id: String,
    tag: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    // Tagged Sidecars, not Notes, even though the mechanism below edits note
    // BODIES: tag identity is the sidecar concern being changed here (see
    // `add_tag`/`remove_tag`), so it must gate with the rest of the tag
    // commands. Do not "fix" this to Notes just because it touches content.
    refuse_write(&state, &account_id, backend::Write::Sidecars)?;
    let t = normalize_tag(&tag).unwrap_or_else(|| tag.clone());
    // Inline model: strips #tag from every carrying note's BODY + content-dirty
    // (round-trips the removal to Apple) + re-derives note_tags. No sidecar.
    let n = state.db.delete_tag(&account_id, &t, &Actor::Human).map_err(|e| e.to_string())?;
    log!("delete_tag: account={} tag={} ({} notes)", account_id, t, n);
    Ok(())
}
