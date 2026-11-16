//! Tauri face of Research Studio (spec 2026-10-23 §5.1, §6, §7). `studio_plan`
//! feeds the dialog and never calls the AI; `generate_studio_note` is the one
//! explicit user-triggered remote operation, and the note it writes is still
//! one synchronous SQLite write.

use tauri::State;

use crate::studio::plan::{self, StudioPlan};
use crate::studio::related::{self, StudioRelated};
use crate::studio::run::{StudioError, StudioProgress, StudioRequest};
use crate::studio::StudioOutput;
use crate::{AppState, ExtractedNoteDto};

/// What the dialog shows for a failed run. Only text meant for a person
/// reaches it: a local failure is logged and replaced, never shown.
fn ui_error(e: StudioError) -> String {
    match e {
        StudioError::Cancelled => "cancelled".to_string(),
        StudioError::Refused(s) => s,
        StudioError::Ai(s) => crate::ingest::run::short_reason(&s),
        StudioError::Local(s) => {
            crate::log!("studio: could not save the note: {}", crate::ingest::run::short_reason(&s));
            "Could not save the note. Try again.".to_string()
        }
    }
}

/// The longest Focus line a run sends; the dialog's input stops at the same.
const MAX_FOCUS_CHARS: usize = 200;

/// The Focus as the plan and the run both use it: trimmed and cut to
/// `MAX_FOCUS_CHARS` characters, never inside one.
fn clip_focus(focus: &str) -> String {
    focus.trim().chars().take(MAX_FOCUS_CHARS).collect::<String>().trim_end().to_string()
}

fn active_account(state: &State<'_, AppState>, account_id: &str) -> Result<crate::accounts::Account, String> {
    state
        .accounts
        .lock()
        .unwrap()
        .iter()
        .find(|a| a.id == account_id && a.is_active())
        .cloned()
        .ok_or_else(|| format!("No active account '{account_id}'."))
}

/// The folder's candidates, the default ticks and the estimate for the
/// current ticks. Reads SQLite and the AI scope file only.
#[tauri::command]
pub async fn studio_plan(
    account_id: String,
    label: String,
    output: StudioOutput,
    focus: String,
    selected: Option<Vec<String>>,
    state: State<'_, AppState>,
) -> Result<StudioPlan, String> {
    let account = active_account(&state, &account_id)?;
    let focus = clip_focus(&focus);
    // Fail closed: an unreadable scope file may be the one that hides folders.
    let ai = crate::privacy::ai_scope::AiScope::load()?;
    let (notes, not_shown) = plan::list_candidates(&state.db, &ai, &account_id, &label)?;
    // The dialog is only a preview: a uuid that is neither listed nor a note
    // of this account the AI may read (a ticked related note from another
    // folder, stage 2a) is ignored here; the run itself re-checks and refuses.
    let selected = match selected {
        None => plan::default_selection(&notes),
        Some(ticked) => {
            let mut keep = Vec::new();
            for u in ticked {
                if notes.iter().any(|n| n.uuid == u) || plan::resolve_readable_note(&state.db, &ai, &account_id, &u)?.is_some() {
                    keep.push(u);
                }
            }
            keep
        }
    };
    // Related notes alone are not a run (the note lands in this folder): that
    // reads as an empty estimate, not an error, so the dialog keeps working.
    let sources = match plan::load_sources(&state.db, &ai, &account_id, &label, &selected) {
        Ok(s) => s,
        Err(e) if e == plan::msg::NOTHING_SELECTED => Vec::new(),
        Err(e) => return Err(e),
    };
    let limits = crate::llm::budget::get_ai_limits().unwrap_or_default();
    let cap = crate::ingest::run::map_input_cap(crate::llm::resolve::prompt_delivery_for_account(&account), std::env::consts::OS);
    let existing_tags = ai.tags_visible_to_ai(&state.db, &account_id);
    let estimate = plan::estimate(output, &focus, &sources, &label, cap, &limits, &existing_tags);
    Ok(StudioPlan { notes, not_shown, selected, estimate })
}

/// Notes from other folders that the dialog may offer as extra sources.
/// Reads SQLite and the AI scope file only; never builds a provider.
#[tauri::command]
pub async fn studio_related(
    account_id: String,
    label: String,
    focus: String,
    state: State<'_, AppState>,
) -> Result<StudioRelated, String> {
    active_account(&state, &account_id)?;
    let focus = clip_focus(&focus);
    // Fail closed: an unreadable scope file may be the one that hides folders.
    let ai = crate::privacy::ai_scope::AiScope::load()?;
    related::find_related(&state.db, &ai, &account_id, &label, &focus)
}

/// Generate `output` over the ticked notes and write it as a new note in
/// `label`. Cancellable through the existing `cancel_extraction(request_id)`.
#[tauri::command]
pub async fn generate_studio_note(
    account_id: String,
    label: String,
    output: StudioOutput,
    focus: String,
    uuids: Vec<String>,
    request_id: String,
    on_progress: tauri::ipc::Channel<StudioProgress>,
    state: State<'_, AppState>,
) -> Result<ExtractedNoteDto, String> {
    let receipt_request = request_id.clone();
    crate::llm::receipts::run(&receipt_request, None, "studio_generate", async {
        crate::llm::receipts::source_version(&serde_json::to_vec(&(&account_id, &label, &uuids)).unwrap_or_default());
        // Everything that can refuse, before anything that can spend.
        crate::refuse_write(&state, &account_id, crate::backend::Write::Notes)?;
        let account = active_account(&state, &account_id)?;
        let focus = clip_focus(&focus);
        let ai = crate::privacy::ai_scope::AiScope::load()?;
        // The re-check: the dialog may be stale.
        let sources = plan::load_sources(&state.db, &ai, &account_id, &label, &uuids)?;
        let limits = crate::llm::budget::get_ai_limits().unwrap_or_default();
        let map_cap = crate::ingest::run::map_input_cap(crate::llm::resolve::prompt_delivery_for_account(&account), std::env::consts::OS);
        let existing_tags = ai.tags_visible_to_ai(&state.db, &account_id);
        if let Some(refusal) = plan::estimate(output, &focus, &sources, &label, map_cap, &limits, &existing_tags).refusal {
            return Err(refusal);
        }
        // The AI-permission refusal passes through unchanged: the frontend matches its text.
        let provider = crate::checked_account_provider(&state, &account).map_err(|e| e.to_string())?;
        crate::log!("generate_studio_note: account={account_id} output={} sources={} outside={} request_id={request_id}", output.name(), sources.len(), sources.iter().filter(|s| s.outside).count());
        let req = StudioRequest {
            account_id: account_id.clone(),
            backend_kind: account.backend_kind,
            label: label.clone(),
            output,
            focus,
            map_cap,
            output_tokens: limits.output_tokens,
            existing_tags,
            actor: crate::provenance::Actor::workflow(output.workflow().slug(), &account),
            today: chrono::Local::now().format("%Y-%m-%d").to_string(),
        };
        let cancel = tokio_util::sync::CancellationToken::new();
        state.in_flight_extracts.lock().unwrap().insert(request_id.clone(), cancel.clone());
        let result = crate::studio::run::generate(
            &state.db,
            provider.as_ref(),
            sources,
            &req,
            cancel,
            // A closed dialog drops the channel; a failed send is not a run failure.
            &|p| {
                let _ = on_progress.send(p);
            },
        )
        .await;
        // One removal on every exit path: the code between the insert and here never returns early.
        state.in_flight_extracts.lock().unwrap().remove(&request_id);
        match result {
            Ok(note) => {
                crate::log!("generate_studio_note: created note uuid={} in {}", note.uuid, note.label);
                Ok(ExtractedNoteDto { uuid: note.uuid, label: note.label })
            }
            Err(e) => Err(ui_error(e)),
        }
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_focus_is_cut_to_200_characters_on_a_character_boundary() {
        assert_eq!(clip_focus("  pricing  "), "pricing");
        assert_eq!(clip_focus(""), "");
        let thai = "ราคา🔋".repeat(60); // 300 characters, multi-byte
        let cut = clip_focus(&thai);
        assert_eq!(cut.chars().count(), MAX_FOCUS_CHARS);
        assert!(thai.starts_with(&cut));
        let exact = "ก".repeat(MAX_FOCUS_CHARS);
        assert_eq!(clip_focus(&exact), exact);
    }
}
