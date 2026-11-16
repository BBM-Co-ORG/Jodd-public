//! Copy to / Move to another account: IPC over `transfer.rs` (spec
//! `docs/superpowers/specs/2026-11-13-cross-account-move-copy-design.md`).
//! SQLite only: the worker pushes the copies, and a move's original goes
//! only once its copy is safe (`Db::settle_transfers`).
use super::*;
use transfer::{NoteRef, TransferMode, TransferReport};

/// Every account, and the target among the active ones.
fn accounts_and_target(state: &AppState, target_account: &str) -> Result<(Vec<accounts::Account>, accounts::Account), String> {
    let list = state.accounts.lock().unwrap().clone();
    let target = list
        .iter()
        .find(|a| a.id == target_account && a.is_active())
        .cloned()
        .ok_or_else(|| format!("No active account '{target_account}'."))?;
    Ok((list, target))
}

/// What a Copy to / Move to would do, writing nothing: the confirm sheet
/// reads its losses and refusals (spec §2.3).
#[tauri::command]
pub(super) async fn preview_note_transfer(
    sources: Vec<NoteRef>,
    target_account: String,
    target_folder: String,
    mode: TransferMode,
    state: State<'_, AppState>,
) -> Result<TransferReport, String> {
    let (list, target) = accounts_and_target(&state, &target_account)?;
    transfer::preview(&state.db, &list, &target, &target_folder, mode, &sources)
}

/// Copy or move notes into `target_account`/`target_folder`. Each note is
/// planned again, never trusted from the preview. Returns what each did.
#[tauri::command]
pub(super) async fn transfer_notes(
    sources: Vec<NoteRef>,
    target_account: String,
    target_folder: String,
    mode: TransferMode,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<TransferReport, String> {
    // Refuse a write this account can't accept, BEFORE SQLite (`refuse_write`).
    refuse_write(&state, &target_account, backend::Write::Notes)?;
    let (list, target) = accounts_and_target(&state, &target_account)?;
    let report = transfer::transfer(&state.db, &list, &target, &target_folder, mode, &sources)?;
    log!(
        "transfer_notes: {:?} {} of {} into {} '{}'",
        mode,
        report.written(),
        sources.len(),
        target_account,
        target_folder
    );
    // The copies are rows the target's listing has not read yet; the same
    // nudge a restore sends (App.svelte: remote-changed → refresh).
    if report.written() > 0 {
        let _ = app.emit("remote-changed", target_account.clone());
    }
    Ok(report)
}
