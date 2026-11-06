//! Tauri face of Ask Jodd history (spec 2026-10-14 §6). Everything here is
//! SQLite-only except `resume_ask`, which opens an AI session after R1–R4.

use serde::Serialize;
use tauri::State;

use crate::ask::history;
use crate::{AppState, AskSessionInfo};

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum AskResume {
    Ready { session: AskSessionInfo },
    NeedsConfirm { previous: String, current: String },
    ReadOnly { reason: String },
}

pub const ARCHIVED: &str = "This conversation is archived. Unarchive it to continue.";

/// End every live session bound to `conversation_id` (or all bound ones).
fn end_bound_sessions(state: &AppState, conversation_id: Option<&str>) {
    let mut sessions = state.ai_policy.sessions.lock().unwrap();
    sessions.retain(|_, s| {
        let bound = match (&s.conversation_id, conversation_id) {
            (Some(c), Some(id)) => c == id,
            (Some(_), None) => true,
            _ => false,
        };
        if bound { s.cancel.cancel(); }
        !bound
    });
}

/// `async` for the same reason as `begin_ask`: its preflight may walk a note
/// scope's related notes, which must not run on the main thread.
#[tauri::command]
pub async fn resume_ask(
    conversation_id: String,
    accept_destination: Option<String>,
    state: State<'_, AppState>,
) -> Result<AskResume, String> {
    let conv = state.db.ask_load_conversation(&conversation_id).map_err(|e| e.to_string())?
        .ok_or("This conversation no longer exists.")?;
    if conv.archived {
        return Ok(AskResume::ReadOnly { reason: ARCHIVED.into() });
    }
    // R1 — the same checks and messages as begin_ask.
    let pre = crate::ask_preflight(&state, &conv.scope)?;
    // R2 + R4 in one pass (fail-closed). The accounts lock is not held
    // across it: clone, then release.
    let accounts = state.accounts.lock().unwrap().clone();
    let ai = crate::privacy::ai_scope::AiScope::load()?;
    let known = match history::resume_known(&state.db, &accounts, &ai, &conv.turns) {
        Ok(k) => k,
        Err(reason) => return Ok(AskResume::ReadOnly { reason }),
    };
    // R3
    if !history::destination_accepted(&conv.destination, &pre.destination, accept_destination.as_deref()) {
        return Ok(AskResume::NeedsConfirm { previous: conv.destination, current: pre.destination });
    }
    // Seed. `open_ask_session` re-checks the stamp, so anything read above
    // is void if AI settings moved in between.
    let session = crate::open_ask_session(
        &state, pre, conv.scope.clone(), history::chat_turns(&conv.turns), known, Some(conversation_id),
    )?;
    Ok(AskResume::Ready { session })
}

#[tauri::command]
pub fn list_ask_conversations(archived: bool, state: State<'_, AppState>) -> Result<Vec<history::ConversationSummary>, String> {
    state.db.ask_list_conversations(archived).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn load_ask_conversation(conversation_id: String, state: State<'_, AppState>) -> Result<history::StoredConversation, String> {
    state.db.ask_load_conversation(&conversation_id).map_err(|e| e.to_string())?
        .ok_or_else(|| "This conversation no longer exists.".into())
}

#[tauri::command]
pub fn rename_ask_conversation(conversation_id: String, title: String, state: State<'_, AppState>) -> Result<String, String> {
    let title = history::conversation_title(&title);
    if title.is_empty() { return Err("Title cannot be empty.".into()); }
    if !state.db.ask_rename_conversation(&conversation_id, &title).map_err(|e| e.to_string())? {
        return Err("This conversation no longer exists.".into());
    }
    Ok(title)
}

#[tauri::command]
pub fn set_ask_conversation_archived(conversation_id: String, archived: bool, state: State<'_, AppState>) -> Result<(), String> {
    if archived { end_bound_sessions(&state, Some(&conversation_id)); }
    let at = archived.then(crate::db::now_ms);
    if !state.db.ask_set_archived(&conversation_id, at).map_err(|e| e.to_string())? {
        return Err("This conversation no longer exists.".into());
    }
    Ok(())
}

#[tauri::command]
pub fn delete_ask_conversation(conversation_id: String, state: State<'_, AppState>) -> Result<(), String> {
    end_bound_sessions(&state, Some(&conversation_id));
    state.db.ask_delete_conversation(&conversation_id).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn clear_ask_history(state: State<'_, AppState>) -> Result<(), String> {
    end_bound_sessions(&state, None);
    state.db.ask_clear_history().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn get_ask_history_enabled() -> bool {
    history::enabled()
}

#[tauri::command]
pub fn set_ask_history_enabled(enabled: bool) -> Result<(), String> {
    history::set_enabled(enabled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ask_resume_wire_shape_is_stable() {
        let s = AskSessionInfo { session_id: "s".into(), destination: "d".into(), scope: "x".into() };
        assert_eq!(serde_json::to_value(AskResume::Ready { session: s }).unwrap(),
            json!({ "kind": "ready", "session": { "session_id": "s", "destination": "d", "scope": "x" } }));
        assert_eq!(serde_json::to_value(AskResume::NeedsConfirm { previous: "a".into(), current: "b".into() }).unwrap(),
            json!({ "kind": "needs_confirm", "previous": "a", "current": "b" }));
        assert_eq!(serde_json::to_value(AskResume::ReadOnly { reason: "r".into() }).unwrap(),
            json!({ "kind": "read_only", "reason": "r" }));
    }
}
