//! Tauri face of Ask Jodd history (spec 2026-10-14 §6), and "Save as note"
//! for an answer. Everything here is SQLite-only except `resume_ask`, which
//! opens an AI session after R1–R4.

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

/// Write an answer as a new local note (see `ask::save`). SQLite only — the
/// sync worker pushes it like any other new note. Split from the command so
/// it is testable without Tauri.
#[allow(clippy::too_many_arguments)]
pub(crate) fn save_answer_db(
    db: &crate::db::Db,
    account_id: &str,
    backend_kind: crate::accounts::BackendKind,
    can_create_folders: bool,
    question: &str,
    markdown: &str,
    cited: &[crate::ask::CitedNote],
    actor: &crate::provenance::Actor,
) -> Result<crate::ExtractedNoteDto, String> {
    if markdown.trim().is_empty() {
        return Err("There is no answer to save.".into());
    }
    let label = crate::llm::filing::resolve_destination(db, account_id, can_create_folders)
        .map_err(|e| format!("resolve destination: {e}"))?;
    let body_html = crate::ask::save::answer_body(
        account_id,
        question,
        markdown,
        cited,
        &chrono::Local::now().format("%Y-%m-%d").to_string(),
        crate::db::tag_model_for_account(account_id),
    );
    let uuid = crate::backend::mint_uuid_for(backend_kind);
    let now = crate::db::now_ms();
    db.insert_local_new(&crate::db::CachedNote {
        uuid: uuid.clone(),
        account_id: account_id.to_string(),
        id: String::new(),
        title: crate::ask::save::answer_title(question),
        body_html,
        date: chrono::Local::now().to_rfc2822(),
        x_mail_created_date: None,
        label: label.clone(),
        local_version: 1,
        remote_version: None,
        sync_state: crate::db::SyncState::Dirty,
        last_synced_at: None,
        last_local_modified_at: now,
        last_remote_modified_at: None,
        pinned: false,
        meta_msg_id: None,
        pin_dirty: false,
        push_blocked_reason: None,
        push_blocked_by_remote: false,
    }, actor)
    .map_err(|e| format!("insert_local_new: {e}"))?;
    Ok(crate::ExtractedNoteDto { uuid, label })
}

/// "Save as note" on an Ask Jodd answer: files the question and answer in
/// `account_id` where Extract files, recorded as written by the `ask-jodd`
/// workflow on the app-wide provider. Refused where the backend cannot
/// write notes.
#[tauri::command]
pub fn save_ask_answer(
    account_id: String,
    question: String,
    markdown: String,
    cited: Vec<crate::ask::CitedNote>,
    state: State<'_, AppState>,
) -> Result<crate::ExtractedNoteDto, String> {
    crate::refuse_write(&state, &account_id, crate::backend::Write::Notes)?;
    let backend_kind = state
        .accounts
        .lock()
        .unwrap()
        .iter()
        .find(|a| a.id == account_id && a.is_active())
        .map(|a| a.backend_kind)
        .ok_or_else(|| format!("No active account '{account_id}'."))?;
    let actor = crate::provenance::Actor::Workflow {
        name: "ask-jodd".into(),
        model: crate::provenance::app_model_label(),
    };
    let saved = save_answer_db(
        &state.db,
        &account_id,
        backend_kind,
        crate::account_can_create_folders(&state, &account_id),
        &question,
        &markdown,
        &cited,
        &actor,
    )?;
    crate::log!("save_ask_answer: created note uuid={} in {}", saved.uuid, saved.label);
    Ok(saved)
}

#[cfg(test)]
mod save_tests {
    use super::*;
    use crate::provenance::Actor;
    use crate::test_support::{note, temp_db};

    const A: &str = "gmail:a@x";

    fn actor() -> Actor {
        Actor::Workflow { name: "ask-jodd".into(), model: "m".into() }
    }

    #[test]
    fn an_answer_is_filed_in_the_inbox_as_ai_written() {
        let db = temp_db();
        note(A, "src").title("Sync").insert(&db);
        let cited = vec![crate::ask::CitedNote {
            uuid: "src".into(), account_id: A.into(), title: "Sync".into(), slug: "sync-src".into(),
        }];
        let saved = save_answer_db(
            &db, A, crate::accounts::BackendKind::Gmail, true,
            "Why keep-both?", "Because nothing is lost. [[sync-src]]", &cited, &actor(),
        ).unwrap();
        assert_eq!(saved.label, crate::llm::filing::INBOX_PATH);
        let n = db.note_by_uuid(A, &saved.uuid).unwrap().expect("saved note");
        assert_eq!(n.title, "Ask Jodd — Why keep-both?");
        assert!(n.body_html.contains("Because nothing is lost."), "{}", n.body_html);
        assert!(n.body_html.contains("[[sync-src]]"), "{}", n.body_html);
        assert_eq!(n.sync_state, crate::db::SyncState::Dirty, "the worker must push it");
        let events = db.events_for_test(A, &saved.uuid);
        assert_eq!(events.first().and_then(|e| e.actor.as_deref()), Some(actor().as_okf().as_str()),
            "never recorded as human-written: {events:?}");
    }

    #[test]
    fn without_folder_writes_it_goes_to_the_root() {
        let db = temp_db();
        let saved = save_answer_db(&db, A, crate::accounts::BackendKind::Microsoft, false, "q", "a", &[], &actor()).unwrap();
        assert_eq!(saved.label, crate::llm::filing::NOTES_ROOT);
    }

    #[test]
    fn an_empty_answer_is_refused() {
        let db = temp_db();
        assert!(save_answer_db(&db, A, crate::accounts::BackendKind::Gmail, true, "q", "  ", &[], &actor()).is_err());
    }
}
