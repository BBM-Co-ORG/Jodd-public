//! Tauri face of Curate (spec 2026-10-06-curate-m2 §3.6). The scan is an
//! explicit user-triggered remote operation (it calls the AI); list,
//! preview, apply and dismiss are SQLite-only.

use serde::Serialize;
use tauri::State;

use crate::curate::{self, apply::ApplyError, Action, Proposal};
use crate::AppState;

fn account(state: &State<'_, AppState>, account_id: &str) -> Result<crate::accounts::Account, String> {
    state
        .accounts
        .lock()
        .unwrap()
        .iter()
        .find(|a| a.id == account_id && a.is_active())
        .cloned()
        .ok_or_else(|| format!("No active account '{account_id}'."))
}

#[tauri::command]
pub async fn curate_scan(account_id: String, request_id: String, state: State<'_, AppState>) -> Result<curate::decide::ScanSummary, String> {
    let account = account(&state, &account_id)?;
    crate::llm::receipts::run(&request_id, None, "curate_scan", async {
        // Fail closed (review finding 6, spec 2026-10-08 §4.4): an unreadable
        // scope file may be the one that hides private folders. Loaded before
        // the provider, like every AI command.
        let ai = crate::privacy::ai_scope::AiScope::load()?;
        // No provider (or a refused one) is not an error: secrets still scan.
        // A refusal caused only by the AI data permission is named, so the UI
        // can offer the one-click allow (privacy PR3) instead of "no provider".
        let (provider, consent_needed) = match crate::checked_account_provider(&state, &account) {
            Ok(p) => (Some(p), false),
            Err(e) => (None, crate::llm::policy::is_consent_refusal(&e)),
        };
        let summary = curate::decide::scan(
            &state.db,
            provider.as_deref(),
            ai.file(),
            &account,
            crate::db::now_ms(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await?;
        let summary = if consent_needed { summary.consent_needed() } else { summary };
        crate::log!(
            "curate: scan {} → {} duplicate, {} misfiled, {} secret, {} skipped",
            account.id, summary.duplicates, summary.misfiled, summary.secrets, summary.skipped
        );
        Ok(summary)
    })
    .await
}

#[tauri::command]
pub fn curate_list(account_id: String, state: State<'_, AppState>) -> Result<Vec<Proposal>, String> {
    curate::list(&state.db, &account_id, &["pending", "stale", "failed"])
}

#[derive(Serialize)]
pub struct NoteText {
    pub uuid: String,
    pub title: String,
    pub label: String,
    pub text: String,
}

/// The text of every note a proposal touches, for the side-by-side preview.
/// A secret proposal's preview masks nothing: it is the user's own note on
/// the user's own screen, never an agent's input.
#[tauri::command]
pub fn curate_preview(account_id: String, id: i64, state: State<'_, AppState>) -> Result<Vec<NoteText>, String> {
    let p = curate::list(&state.db, &account_id, &["pending", "stale", "failed"])?
        .into_iter()
        .find(|p| p.id == id)
        .ok_or("No such proposal.")?;
    Ok(p.payload
        .notes
        .iter()
        .filter_map(|r| state.db.note_by_uuid(&account_id, &r.uuid).ok().flatten())
        .map(|n| NoteText {
            text: crate::agent_memory::text::html_to_text(&n.body_html, false),
            uuid: n.uuid,
            title: n.title,
            label: n.label,
        })
        .collect())
}

#[tauri::command]
pub fn curate_apply(account_id: String, id: i64, action: Option<Action>, state: State<'_, AppState>) -> Result<(), String> {
    let account = account(&state, &account_id)?;
    curate::apply::apply(&state.db, &account, id, action, &crate::mcp_scope::scope_path()).map_err(|e: ApplyError| {
        crate::log!("curate: apply {id} refused: {e}");
        e.to_string()
    })?;
    crate::log!("curate: applied proposal {id} in {account_id}");
    Ok(())
}

#[tauri::command]
pub fn curate_dismiss(account_id: String, id: i64, state: State<'_, AppState>) -> Result<(), String> {
    match state.db.curate_get(id).map_err(|e| e.to_string())? {
        Some((owner, ..)) if owner == account_id => curate::dismiss(&state.db, id),
        _ => Err("No such proposal.".into()),
    }
}
