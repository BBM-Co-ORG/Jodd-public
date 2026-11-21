//! AI commands: Extract (`extract_note`, `append_extract_note`,
//! `re_extract_note`), the LLM workflows (Summarize, Action Items, ...),
//! Ask Jodd sessions, wiki-link and folder suggestions, and the helpers that
//! stamp, scope and preserve every AI result. Moved out of `lib.rs` 2026-11-16;
//! `ai_scope_tripwire` and the workflow source scans read this file.

use super::*;

/// What an Extract created, and where — the frontend navigates to `label`
/// instead of assuming a folder.
#[derive(Debug, serde::Serialize)]
pub struct ExtractedNoteDto {
    pub(crate) uuid: String,
    pub(crate) label: String,
    /// A link ingest's saved copies: which were reused, checked or updated
    /// (spec 2026-11-19 §5.2). Empty for every other command.
    pub(crate) copies: Vec<crate::ingest::run::CopyNotice>,
}

/// Where `extract_note_into` files the new note.
pub enum ExtractDestination {
    /// A fresh Extract: `filing::resolve_destination`.
    Resolve,
    /// A Re-extract: beside the source note, via `filing::destination_beside`.
    BesideSource(String),
}

/// Whether `account_id`'s backend can create folders. `false` for an
/// unknown account, which also routes its extract to the root.
pub(crate) fn account_can_create_folders(state: &AppState, account_id: &str) -> bool {
    let list = state.accounts.lock().unwrap();
    list.iter()
        .find(|a| a.id == account_id)
        .map(|a| Capabilities::for_backend(a.backend_kind).writes.folders)
        .unwrap_or(false)
}

/// No credentials or note content in the eligibility snapshot. Changes to any
/// account's visibility/permission or provider invalidate retained AI history.
pub(crate) fn ai_stamp(state: &AppState) -> serde_json::Value {
    let accounts = state.accounts.lock().unwrap();
    let rows: Vec<_> = accounts.iter().map(|a| serde_json::json!([
        a.id, a.status, a.backend_kind, a.llm, a.blocked_reason, a.pending_removal
    ])).collect();
    serde_json::json!([state.ai_policy.revision.load(std::sync::atomic::Ordering::SeqCst), rows, app_llm_config::load()])
}

pub struct AiRequestCleanup<'a> {
    pub(crate) requests: &'a Mutex<HashMap<String, tokio_util::sync::CancellationToken>>,
    pub(crate) id: String,
}
impl Drop for AiRequestCleanup<'_> {
    fn drop(&mut self) { self.requests.lock().unwrap().remove(&self.id); }
}

pub(crate) fn checked_account_provider<'a>(state: &'a AppState, account: &accounts::Account)
    -> Result<Box<dyn llm::provider::LlmProvider + 'a>, llm::provider::ExtractError>
{
    let _gate = state.ai_policy.gate.lock().unwrap();
    let accounts = state.accounts.lock().unwrap().clone();
    let stamp = ai_stamp(state);
    let inner = llm::policy::build_account_provider(&accounts, &account.id, llm::resolve::resolve_provider_for_account)?;
    Ok(Box::new(llm::policy::CheckedProvider { cancel: Mutex::new(None), gate: &state.ai_policy.gate, inner, valid: Box::new(move || ai_stamp(state) == stamp), known: Default::default() }))
}

pub(crate) fn issue_ai_result(state: &AppState, account_id: &str, actor: Actor) -> String {
    state.ai_policy.issue_result(account_id, ai_stamp(state), actor)
}
/// Who wrote what a command is about to save: the workflow behind a live AI
/// result, otherwise the user. An empty id (the frontend sends `''` when it has
/// none) is the user.
pub(crate) fn actor_for(state: &AppState, ai_result_id: Option<&str>) -> Actor {
    actor_for_result(&state.ai_policy, ai_result_id)
}
/// `actor_for` over the result registry alone (no AppState), so it is testable.
pub(crate) fn actor_for_result(policy: &llm::policy::Runtime, ai_result_id: Option<&str>) -> Actor {
    ai_result_id.filter(|id| !id.is_empty())
        .and_then(|id| policy.result_actor(id))
        .unwrap_or(Actor::Human)
}
pub(crate) fn validate_ai_result(state: &AppState, account_id: &str, id: &str) -> Result<(), String> {
    llm::policy::require_account(&state.accounts.lock().unwrap(), account_id).map_err(|e| e.to_string())?;
    let stamp = ai_stamp(state);
    if state.ai_policy.result_valid(id, account_id, stamp) { Ok(()) }
    else { Err("AI result expired after account, permission or provider changes. Generate it again.".into()) }
}

/// The AI scope for a command whose text came from `source`, refusing a note
/// in a hidden folder (spec 2026-10-08 §4.4). On refusal the command's
/// in-flight entry is removed, as every other early exit does. The returned
/// scope also filters the tag vocabulary (`tags_visible_to_ai`).
pub(crate) fn ai_scope_for_source(
    state: &AppState,
    account_id: &str,
    source: &crate::privacy::ai_scope::AiSource,
    request_id: &str,
) -> Result<crate::privacy::ai_scope::AiScope, String> {
    let checked = crate::privacy::ai_scope::AiScope::load()
        .and_then(|ai| ai.check_source(&state.db, account_id, source).map(|()| ai));
    if checked.is_err() {
        state.in_flight_extracts.lock().unwrap().remove(request_id);
    }
    checked
}

pub(crate) fn invalidate_ai(state: &AppState, app: &tauri::AppHandle) {
    state.ai_policy.invalidate();
    for cancel in state.in_flight_extracts.lock().unwrap().values() { cancel.cancel(); }
    for cancel in state.in_flight_asks.lock().unwrap().values() { cancel.cancel(); }
    let _ = app.emit("ai-policy-changed", ());
}

#[derive(serde::Serialize, Clone)]
pub(crate) struct AskSessionInfo {
    pub(crate) session_id: String,
    pub(crate) destination: String,
    pub(crate) scope: String,
}

/// The destination text Ask shows and history stores (R3). No keychain, no
/// provider construction.
pub(crate) fn ask_destination_label() -> Result<String, String> {
    let cfg = app_llm_config::load().ok_or("Ask Jodd needs an LLM provider. Open App Settings.")?;
    match cfg.llm.provider {
        accounts::LlmProviderKind::Http => {
            let base = cfg.llm.http_base_url.as_deref().ok_or("HTTP destination is missing. Open App Settings.")?;
            let mut url = reqwest::Url::parse(base).map_err(|_| "HTTP destination is invalid. Open App Settings.")?;
            let _ = url.set_username(""); let _ = url.set_password(None); url.set_query(None); url.set_fragment(None);
            Ok(format!("HTTP {} · model {}", url, cfg.llm.http_model.as_deref().ok_or("HTTP model is missing. Open App Settings.")?))
        },
        accounts::LlmProviderKind::AgentCli | accounts::LlmProviderKind::ClaudeCode => Ok(format!(
            "Agent CLI {} · destination/model follow the CLI configuration; may use cloud services",
            llm::resolve::destination_label(&cfg.llm).ok_or("Agent CLI is missing. Open App Settings.")?)),
        _ => Err("Ask Jodd needs an LLM provider. Open App Settings.".into()),
    }
}

pub struct AskPreflight { pub(crate) stamp: serde_json::Value, pub(crate) destination: String, pub(crate) scope_description: String }

/// R1 (spec 2026-10-14 §4) and `begin_ask`'s original checks, unchanged.
pub(crate) fn ask_preflight(state: &AppState, scope: &ask::AskScope) -> Result<AskPreflight, String> {
    let (stamp, ai, count) = {
        let _gate = state.ai_policy.gate.lock().unwrap();
        let stamp = ai_stamp(state);
        let accounts = state.accounts.lock().unwrap();
        if let Some(id) = scope.account_id() {
            llm::policy::require_account(&accounts, id).map_err(|e| e.to_string())?;
        }
        // Read-only and before any provider: a hidden folder is refused at preflight.
        let ai = crate::privacy::ai_scope::AiScope::load()?;
        if let ask::AskScope::Folder { account_id, label } = scope {
            ai.check_folder(account_id, label)?;
        }
        let count = accounts.iter().filter(|a| llm::policy::account_allowed(a)).count();
        if count == 0 { return Err("No accounts allow AI data access. Review Settings › Privacy & Data.".into()); }
        (stamp, ai, count)
    };
    // Outside both locks: a note scope's walk can read hundreds of notes, and
    // must not block other AI commands while it does (spec 2026-10-23 §5.1).
    // The stamp check below, and open_ask_session's under the gate, void it
    // if AI settings moved meanwhile.
    let note_description = match scope {
        ask::AskScope::Note { .. } => Some(ask::related::preflight_note(&state.db, &ai, scope)?),
        ask::AskScope::Notes { .. } => Some(ask::picked::preflight_picked(&state.db, &ai, scope)?),
        _ => None,
    };
    let destination = ask_destination_label()?;
    let scope_description = match scope {
        ask::AskScope::AllAccounts => format!("All AI-allowed active accounts ({count}); disabled accounts excluded"),
        ask::AskScope::Account { account_id } => format!("Account {account_id}"),
        ask::AskScope::Folder { account_id, label } => format!("{account_id} · {label} and subfolders"),
        ask::AskScope::Note { .. } | ask::AskScope::Notes { .. } => note_description.unwrap_or_default(),
    };
    if ai_stamp(state) != stamp { return Err("AI settings changed. Reopen Ask Jodd.".into()); }
    Ok(AskPreflight { stamp, destination, scope_description })
}

/// Insert a session, re-checking the stamp under the gate: anything read
/// between the preflight and here (resume's R2/R4) is void if it moved.
pub(crate) fn open_ask_session(
    state: &AppState,
    pre: AskPreflight,
    scope: ask::AskScope,
    turns: Vec<llm::provider::ChatTurn>,
    known: crate::privacy::vault::Known,
    conversation_id: Option<String>,
) -> Result<AskSessionInfo, String> {
    let _gate = state.ai_policy.gate.lock().unwrap();
    if ai_stamp(state) != pre.stamp { return Err("AI settings changed. Reopen Ask Jodd.".into()); }
    let id = uuid::Uuid::new_v4().to_string();
    let mut sessions = state.ai_policy.sessions.lock().unwrap();
    // Ephemeral conversations are bounded even if a webview disappears.
    if sessions.len() >= 16 {
        for (_, old) in sessions.drain() { old.cancel.cancel(); }
    }
    sessions.insert(id.clone(), llm::policy::Session {
        stamp: pre.stamp, scope, turns, cancel: tokio_util::sync::CancellationToken::new(), busy: false,
        known: std::sync::Arc::new(std::sync::Mutex::new(known)), conversation_id,
        destination: pre.destination.clone(),
    });
    Ok(AskSessionInfo { session_id: id, destination: pre.destination, scope: pre.scope_description })
}

/// Read-only preflight: no provider construction, keychain access or AI call.
/// `async` so it runs off the main thread: a note scope's preflight walks up
/// to 400 related notes (spec 2026-10-23 §5.1).
#[tauri::command]
pub async fn begin_ask(scope: ask::AskScope, state: State<'_, AppState>) -> Result<AskSessionInfo, String> {
    let pre = ask_preflight(&state, &scope)?;
    open_ask_session(&state, pre, scope, Vec::new(), Default::default(), None)
}

#[tauri::command]
pub fn end_ask(session_id: String, state: State<'_, AppState>) {
    if let Some(session) = state.ai_policy.sessions.lock().unwrap().remove(&session_id) { session.cancel.cancel(); }
}

/// Workflow entry point: take a chunk of source text, ship it to the
/// configured LLM provider, render the structured response into a note, and
/// persist it locally in `filing::resolve_destination` (`Notes/Inbox`, or the
/// root where the backend cannot create folders). On LLM failure, creates a
/// fallback note containing only the verbatim source so the paste is never
/// lost.
///
/// Refuses `Write::Notes` only. It used to refuse `Write::Folders` as well,
/// which made Extract unavailable on Outlook, whose folder writes are
/// permanently false (gotcha #12) — while the root it now falls back to is
/// writable there by well-known name.
#[tauri::command]
pub async fn extract_note(
    account_id: String,
    source_text: String,
    source: crate::privacy::ai_scope::AiSource,
    title_override: Option<String>,
    request_id: String,
    state: State<'_, AppState>,
) -> Result<ExtractedNoteDto, String> {
    let receipt_request = request_id.clone();
    llm::receipts::run(&receipt_request, None, "extract_note", async {
        llm::receipts::source_version(serde_json::to_vec(&(&account_id, &source_text)).unwrap_or_default().as_slice());
        // Refuse a write this area can't accept yet, BEFORE SQLite, so no
        // unpushable row is created. See `refuse_write`.
        refuse_write(&state, &account_id, backend::Write::Notes)?;
        extract_note_into(account_id, source_text, source, title_override, request_id, ExtractDestination::Resolve, state).await
    }).await
}

pub(crate) async fn extract_note_into(
    account_id: String,
    source_text: String,
    source: crate::privacy::ai_scope::AiSource,
    title_override: Option<String>,
    request_id: String,
    destination: ExtractDestination,
    state: State<'_, AppState>,
) -> Result<ExtractedNoteDto, String> {
    let source_text = ai_source_text(&source, source_text);
    let _request_cleanup = AiRequestCleanup { requests: &state.in_flight_extracts, id: request_id.clone() };
    log!(
        "extract_note: account={} source_len={} request_id={}",
        account_id,
        source_text.len(),
        request_id
    );

    // Register a CancellationToken under this request_id so the frontend can
    // abort via cancel_extraction(request_id). The token is removed on any
    // exit path — success, error, or cancel — by the guard below.
    let cancel = tokio_util::sync::CancellationToken::new();
    state
        .in_flight_extracts
        .lock()
        .unwrap()
        .insert(request_id.clone(), cancel.clone());

    // Resolve provider from account config. Clone the Account out so we don't
    // hold the accounts Mutex across the LLM await.
    let account = {
        let list = state.accounts.lock().unwrap();
        list.iter()
            .find(|a| a.id == account_id)
            .ok_or_else(|| {
                state.in_flight_extracts.lock().unwrap().remove(&request_id);
                format!("account not found: {account_id}")
            })?
            .clone()
    };
    // Spec 2026-10-08 §4.4: a note in a hidden folder never reaches a
    // provider. Checked before the provider is built, so a refusal spends nothing.
    let ai = ai_scope_for_source(&state, &account_id, &source, &request_id)?;
    let provider = checked_account_provider(&state, &account).map_err(|e| {
        state.in_flight_extracts.lock().unwrap().remove(&request_id);
        e.to_string()
    })?;

    // Roadmap #0: show the model this account's existing tag vocabulary so
    // it prefers reusing a tag over minting a near-duplicate. Best-effort —
    // a lookup failure here must not block extraction; an empty list just
    // falls back to the plain prompt (`extract_system_prompt`).
    let existing_tags: Vec<String> =
        ai.tags_visible_to_ai(&state.db, &account_id);

    // Call LLM. The provider races its I/O against `cancel` and returns
    // ExtractError::Cancelled if the token fires.
    llm::receipts::source_version(&serde_json::to_vec(&(&account_id, &source_text, &existing_tags)).unwrap_or_default());
    llm::receipts::stage("extract");
    let envelope = match provider.extract(&source_text, &existing_tags, cancel).await {
        Ok(env) => env,
        Err(crate::llm::provider::ExtractError::Cancelled) => {
            // User-initiated abort: discard everything. Do NOT create a
            // fallback note — the user actively chose to cancel, not "lose"
            // their paste. The textarea still holds the source if they want
            // to retry.
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            log!("extract_note: cancelled by user (request_id={request_id})");
            return Err("cancelled".to_string());
        }
        Err(e) => {
            log!("extract_note: provider error (details omitted) — preserving the source");
            // Remove the cancel token BEFORE `?`, as append_extract_note
            // does: a failed fallback write must not leak it.
            let _policy_gate = provider.mutation_guard().map_err(|e| e.to_string())?;
            let fallback = preserve_failed_source(&state, &account_id, &source, &source_text);
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            let kept = fallback?;
            return Err(format!(
                "LLM call failed; {kept}. {e}"
            ));
        }
    };

    let _policy_gate = provider.mutation_guard().map_err(|e| e.to_string())?;
    llm::receipts::stage("saving_locally");
    save_workflow_envelope(
        &state,
        &account,
        &account_id,
        &request_id,
        &source_text,
        envelope,
        title_override,
        "Extract",
        destination,
        &Actor::workflow("extract", &account),
    )
}

/// Shared tail of every "LLM envelope -> saved local note" workflow: derive
/// the title, resolve/confirm the destination folder, mint a uuid (gotcha
/// #18 — per-backend, never assume Apple's wire format), and insert. Called
/// once the caller already has an `ExtractEnvelope`, however it got one --
/// covers `extract_note_into` (Extract) and `run_workflow_note_into` (the
/// three new workflows) alike; they diverge only in HOW they obtain the
/// envelope, not in what happens to it afterward.
pub(crate) fn save_workflow_envelope(
    state: &State<'_, AppState>,
    account: &accounts::Account,
    account_id: &str,
    request_id: &str,
    source_text: &str,
    envelope: crate::llm::provider::ExtractEnvelope,
    title_override: Option<String>,
    default_title_prefix: &str,
    destination: ExtractDestination,
    actor: &Actor,
) -> Result<ExtractedNoteDto, String> {
    // Assemble note body.
    let body_html = crate::llm::markdown::assemble_note_body(&envelope, source_text, crate::db::tag_model_for_account(account_id));

    // Derive title: override → envelope.title → first H2 from markdown → date.
    let title = title_override
        .filter(|s| !s.trim().is_empty())
        .or_else(|| envelope.title.clone())
        .or_else(|| crate::llm::markdown::derive_title_from_markdown(&envelope.lessons_markdown))
        .unwrap_or_else(|| format!("{default_title_prefix} — {}", chrono::Local::now().format("%Y-%m-%d")));

    // Decide where the note lives. May create `Notes/Inbox` (a dirty_new
    // folder row) — only after the LLM call succeeded, so a cancelled or
    // failed extract leaves no empty Inbox behind.
    let can_create = account_can_create_folders(state, account_id);
    let folder = match &destination {
        ExtractDestination::Resolve => {
            crate::llm::filing::resolve_destination(&state.db, account_id, can_create)
        }
        ExtractDestination::BesideSource(source_label) => {
            crate::llm::filing::destination_beside(&state.db, account_id, source_label, can_create)
        }
    }
    .map_err(|e| {
        state.in_flight_extracts.lock().unwrap().remove(request_id);
        format!("resolve destination: {e}")
    })?;

    // Create a brand-new local note. apply_local_edit is UPDATE-only — we have
    // to insert_local_new for a freshly-generated UUID.
    // Per-backend shape (gotcha #18): a CloudKit recordName is lowercase.
    let uuid = crate::backend::mint_uuid_for(account.backend_kind);
    let now = db::now_ms();
    let new_note = db::CachedNote {
        uuid: uuid.clone(),
        account_id: account_id.to_string(),
        id: String::new(),
        title: title.clone(),
        body_html,
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
    state.db.insert_local_new(&new_note, actor).map_err(|e| {
        state.in_flight_extracts.lock().unwrap().remove(request_id);
        format!("insert_local_new: {e}")
    })?;

    // Tag persistence: NO explicit add_tag calls here. The body is the single
    // source of truth — reconcile_tags_from_body_conn runs inside
    // insert_local_new and derives the tag set via db::tags_for_account.
    // assemble_note_body renders the envelope's tags under the account's
    // TagModel: `<p>#tag</p>` where `#word` text IS the tag (Gmail, Microsoft,
    // LocalFs, SSH), and a plain `<p>Keywords: …</p>` line on iCloud, where a
    // tag is Apple's inline object that Jodd cannot create yet — so an iCloud
    // Extract writes keywords and no tags (gotcha #38). An earlier version
    // called add_tag per envelope tag; the body-derived reconciliation
    // overwrote those rows a moment later, so it was removed.

    // Success path: remove the in-flight cancel token. A late cancel call
    // arriving after this point becomes a no-op (the lookup misses), which
    // is the correct semantics — the work already completed.
    state.in_flight_extracts.lock().unwrap().remove(request_id);

    log!(
        "save_workflow_envelope: created note uuid={uuid} in {folder} with {} body-derived tag(s)",
        envelope.tags.len()
    );
    Ok(ExtractedNoteDto { uuid, label: folder, copies: Vec::new() })
}

#[derive(serde::Serialize)]
pub struct ActionItemsPreview {
    pub(crate) ai_result_id: String,
    pub(crate) title: String,
    pub(crate) body_html: String,
    pub(crate) before_html: Option<String>,
}

/// Read-only preview; B policy precedes source/target retrieval. D/E wrap the
/// existing ActionItems provider route. No receipt ID authorizes application.
#[tauri::command]
pub async fn preview_action_items(
    account_id: String,
    source_text: String,
    source_uuid: Option<String>,
    source_incomplete: Option<bool>,
    target_uuid: Option<String>,
    title_override: Option<String>,
    request_id: String,
    state: State<'_, AppState>,
) -> Result<ActionItemsPreview, String> {
    llm::receipts::run(&request_id, None, "preview_action_items", async {
        refuse_write(&state, &account_id, backend::Write::Notes)?;
        // Spec 2026-10-08 §4.4: a source note in a hidden folder never reaches a
        // provider. `source_uuid == None` is the modal's paste mode — pasted
        // text by construction.
        let ai = crate::privacy::ai_scope::AiScope::load()?;
        if let Some(uuid) = &source_uuid {
            ai.check_source(&state.db, &account_id, &crate::privacy::ai_scope::AiSource::Note { uuid: uuid.clone() })?;
        }
        let account = state
            .accounts
            .lock()
            .unwrap()
            .iter()
            .find(|a| a.id == account_id)
            .cloned()
            .ok_or("Account unavailable")?;
        let provider = checked_account_provider(&state, &account).map_err(|e| e.to_string())?;
        let cancel = tokio_util::sync::CancellationToken::new();
        let _cleanup = AiRequestCleanup {
            requests: &state.in_flight_extracts,
            id: request_id.clone(),
        };
        state
            .in_flight_extracts
            .lock()
            .unwrap()
            .insert(request_id.clone(), cancel.clone());
        let read = |uuid: &str| -> Result<db::CachedNote, String> {
            let uuid = state
                .db
                .resolve_note_uuid(uuid, &account_id)
                .map_err(|e| e.to_string())?;
            state
                .db
                .get(&uuid, &account_id)
                .map_err(|e| e.to_string())?
                .filter(|n| n.sync_state != db::SyncState::DeletedPending)
                .ok_or("Note unavailable".into())
        };
        let source_is_html = source_uuid.is_some();
        let source = if let Some(uuid) = source_uuid {
            let note = read(&uuid)?;
            llm::receipts::source_version(
                &serde_json::to_vec(&(&account_id, &note.uuid, note.local_version))
                    .unwrap_or_default(),
            );
            // Preserve original HTML in the Source block, quote readable text.
            note.body_html
        } else {
            source_text
        };
        let target = target_uuid.as_deref().map(read).transpose()?;
        llm::meeting::passages(&source).map_err(|e| e.to_string())?;
        let text = if source_is_html {
            llm::meeting::text_from_html(&source)
        } else {
            source.clone()
        };
        let text = if source_incomplete.unwrap_or(false) {
            format!("[INCOMPLETE] Supplied source is incomplete.\n{text}")
        } else {
            text
        };
        if let Some(note) = &target {
            llm::receipts::source_version(
                &serde_json::to_vec(&(
                    &account_id,
                    &note.uuid,
                    note.local_version,
                    &note.body_html,
                ))
                .unwrap_or_default(),
            );
        }
        llm::meeting::request(&text).map_err(|e| e.to_string())?;
        llm::receipts::source_version(source.as_bytes());
        llm::receipts::metric("source_count", 1);
        llm::receipts::stage("action_items");
        let envelope = provider
            .run_workflow(llm::provider::WorkflowKind::ActionItems, &text, &[], cancel)
            .await
            .map_err(|e| e.to_string())?;
        let _gate = provider.mutation_guard().map_err(|e| e.to_string())?;
        let title = target
            .as_ref()
            .map(|n| n.title.clone())
            .or(title_override.filter(|s| !s.trim().is_empty()))
            .unwrap_or_else(|| "Meeting actions — draft".into());
        let body_html = llm::markdown::assemble_note_body(&envelope, &source, crate::db::tag_model_for_account(&account_id));
        let before_html = target
            .as_ref()
            .map(|n| llm::markdown::sanitize_note_html(&n.body_html));
        let ai_result_id = issue_ai_result(&state, &account_id, workflow_actor(&state, &account_id, llm::provider::WorkflowKind::ActionItems.slug()));
        let mut drafts = state.ai_policy.meeting_drafts.lock().unwrap();
        drafts.retain(|_, d| d.created.elapsed().as_secs() < 900);
        if drafts.len() >= 16 {
            drafts.clear();
        }
        drafts.insert(
            ai_result_id.clone(),
            llm::meeting::Draft {
                created: std::time::Instant::now(),
                account_id,
                source,
                envelope,
                title: title.clone(),
                target,
            },
        );
        llm::receipts::stage("awaiting_review");
        Ok(ActionItemsPreview {
            ai_result_id,
            title,
            body_html,
            before_html,
        })
    })
    .await
}

#[tauri::command]
pub fn discard_action_items(ai_result_id: String, state: State<'_, AppState>) {
    let _gate = state.ai_policy.gate.lock().unwrap();
    state
        .ai_policy
        .meeting_drafts
        .lock()
        .unwrap()
        .remove(&ai_result_id);
    state
        .ai_policy
        .results
        .lock()
        .unwrap()
        .remove(&ai_result_id);
}

#[tauri::command]
pub fn apply_action_items(
    account_id: String,
    ai_result_id: String,
    state: State<'_, AppState>,
) -> Result<ExtractedNoteDto, String> {
    refuse_write(&state, &account_id, backend::Write::Notes)?;
    let _gate = state.ai_policy.gate.lock().unwrap();
    validate_ai_result(&state, &account_id, &ai_result_id)?;
    let draft = state
        .ai_policy
        .meeting_drafts
        .lock()
        .unwrap()
        .remove(&ai_result_id)
        .ok_or("Draft expired; generate it again.")?;
    state
        .ai_policy
        .results
        .lock()
        .unwrap()
        .remove(&ai_result_id);
    if draft.account_id != account_id || draft.created.elapsed().as_secs() >= 900 {
        return Err("Draft expired; generate it again.".into());
    }
    if draft.target.is_some() {
        let (uuid, label) = llm::meeting::append_reviewed(
            &state.db,
            &draft,
            &workflow_actor(&state, &account_id, llm::provider::WorkflowKind::ActionItems.slug()),
        )?;
        return Ok(ExtractedNoteDto { uuid, label, copies: Vec::new() });
    }
    let account = state
        .accounts
        .lock()
        .unwrap()
        .iter()
        .find(|a| a.id == account_id)
        .cloned()
        .ok_or("Account unavailable")?;
    save_workflow_envelope(
        &state,
        &account,
        &account_id,
        &ai_result_id,
        &draft.source,
        draft.envelope,
        Some(draft.title),
        "Meeting actions — draft",
        ExtractDestination::Resolve,
        &Actor::workflow(llm::provider::WorkflowKind::ActionItems.slug(), &account),
    )
}

/// Default title prefix for a fresh note created by `run_workflow_note_into`,
/// matching `extract_note_into`'s "Extract" default — see
/// `save_workflow_envelope`'s title derivation (override → envelope.title →
/// first H2 → `"{prefix} — {date}"`).
pub(crate) fn workflow_title_prefix(w: crate::llm::provider::WorkflowKind) -> &'static str {
    use crate::llm::provider::WorkflowKind;
    match w {
        WorkflowKind::Summarize => "Summary",
        WorkflowKind::ActionItems => "Action Items",
        WorkflowKind::ExpandBullets => "Expanded",
        WorkflowKind::CleanTranscript => "Transcript",
        WorkflowKind::StudioBriefing => "Briefing",
        WorkflowKind::StudioFaq => "FAQ",
        WorkflowKind::StudioTimeline => "Timeline",
        WorkflowKind::StudioStudyGuide => "Study guide",
        WorkflowKind::StudioSlideDeck => "Slide deck",
        WorkflowKind::StudioCourseModule => "Course module",
        WorkflowKind::StudioPodcastScript => "Podcast script",
        WorkflowKind::StudioVideoScript => "Video script",
        WorkflowKind::StudioDigest => "Digest",
    }
}

/// The Research Studio kinds work on a whole folder and run only from the
/// Studio orchestrator. Every per-note command refuses them first, before an
/// account, a provider or a receipt exists.
pub(crate) fn refuse_studio_kind(w: llm::provider::WorkflowKind) -> Result<(), String> {
    if w.is_studio() {
        return Err(llm::provider::STUDIO_ONLY.into());
    }
    Ok(())
}

/// Summarize / Action Items / Expand Bullets — the roadmap #2 sibling of
/// `extract_note`. Same refusal, same filing, same cancellation and
/// fallback-on-error behavior; differs only in which prompt the provider
/// runs (`run_workflow` instead of `extract`).
#[tauri::command]
pub async fn run_llm_workflow(
    account_id: String,
    workflow: crate::llm::provider::WorkflowKind,
    source_text: String,
    source: crate::privacy::ai_scope::AiSource,
    title_override: Option<String>,
    request_id: String,
    state: State<'_, AppState>,
) -> Result<ExtractedNoteDto, String> {
    refuse_studio_kind(workflow)?;
    if workflow == llm::provider::WorkflowKind::ActionItems {
        return Err("Action items require preview and explicit application.".into());
    }
    let receipt_request = request_id.clone();
    llm::receipts::run(&receipt_request, None, "run_llm_workflow", async {
        llm::receipts::source_version(serde_json::to_vec(&(&account_id, &source_text)).unwrap_or_default().as_slice());
        // Refuse a write this area can't accept yet, BEFORE SQLite, so no
        // unpushable row is created. See `refuse_write`.
        refuse_write(&state, &account_id, backend::Write::Notes)?;
        run_workflow_note_into(account_id, workflow, source_text, source, title_override, request_id, ExtractDestination::Resolve, state).await
    }).await
}

pub(crate) async fn run_workflow_note_into(
    account_id: String,
    workflow: crate::llm::provider::WorkflowKind,
    source_text: String,
    source: crate::privacy::ai_scope::AiSource,
    title_override: Option<String>,
    request_id: String,
    destination: ExtractDestination,
    state: State<'_, AppState>,
) -> Result<ExtractedNoteDto, String> {
    let source_text = ai_source_text(&source, source_text);
    let _request_cleanup = AiRequestCleanup { requests: &state.in_flight_extracts, id: request_id.clone() };
    log!(
        "run_llm_workflow: account={} workflow={:?} source_len={} request_id={}",
        account_id, workflow, source_text.len(), request_id
    );

    let cancel = tokio_util::sync::CancellationToken::new();
    state.in_flight_extracts.lock().unwrap().insert(request_id.clone(), cancel.clone());

    let account = {
        let list = state.accounts.lock().unwrap();
        list.iter().find(|a| a.id == account_id).ok_or_else(|| {
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            format!("account not found: {account_id}")
        })?.clone()
    };
    // Spec 2026-10-08 §4.4: a note in a hidden folder never reaches a
    // provider. Checked before the provider is built, so a refusal spends nothing.
    let ai = ai_scope_for_source(&state, &account_id, &source, &request_id)?;
    let provider = checked_account_provider(&state, &account).map_err(|e| {
        state.in_flight_extracts.lock().unwrap().remove(&request_id);
        e.to_string()
    })?;

    let existing_tags: Vec<String> =
        ai.tags_visible_to_ai(&state.db, &account_id);

    llm::receipts::source_version(&serde_json::to_vec(&(&account_id, &source_text, &existing_tags)).unwrap_or_default());
    llm::receipts::stage(match workflow {
        llm::provider::WorkflowKind::Summarize => "summarize",
        llm::provider::WorkflowKind::ActionItems => "action_items",
        llm::provider::WorkflowKind::ExpandBullets => "expand_bullets",
        llm::provider::WorkflowKind::CleanTranscript => "clean_transcript",
        llm::provider::WorkflowKind::StudioBriefing => "studio_briefing",
        llm::provider::WorkflowKind::StudioFaq => "studio_faq",
        llm::provider::WorkflowKind::StudioTimeline => "studio_timeline",
        llm::provider::WorkflowKind::StudioStudyGuide => "studio_study_guide",
        llm::provider::WorkflowKind::StudioSlideDeck => "studio_slide_deck",
        llm::provider::WorkflowKind::StudioCourseModule => "studio_course_module",
        llm::provider::WorkflowKind::StudioPodcastScript => "studio_podcast_script",
        llm::provider::WorkflowKind::StudioVideoScript => "studio_video_script",
        llm::provider::WorkflowKind::StudioDigest => "studio_digest",
    });
    let clean_limits = crate::llm::transcript::CleanLimits::from_settings(&llm::budget::get_ai_limits().unwrap_or_default());
    if workflow == llm::provider::WorkflowKind::CleanTranscript {
        // `_request_cleanup` (above) removes the in-flight entry on this early return.
        crate::llm::transcript::check_attempts(
            crate::llm::transcript::chunk(&source_text, clean_limits.chunk_chars).len(),
            clean_limits.max_attempts,
            crate::llm::transcript::AttemptContext::Pasted,
        )?;
    }
    let envelope = match crate::llm::transcript::run_workflow_or_clean(provider.as_ref(), workflow, &source_text, &existing_tags, &clean_limits, cancel, crate::llm::transcript::AttemptContext::Pasted).await {
        Ok(env) => env,
        Err(crate::llm::provider::ExtractError::Cancelled) => {
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            log!("run_llm_workflow: cancelled by user (request_id={request_id})");
            return Err("cancelled".to_string());
        }
        Err(e) => {
            log!("run_llm_workflow: provider error (details omitted) — preserving the source");
            let _policy_gate = provider.mutation_guard().map_err(|e| e.to_string())?;
            let fallback = preserve_failed_source(&state, &account_id, &source, &source_text);
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            let kept = fallback?;
            return Err(format!("LLM call failed; {kept}. {e}"));
        }
    };

    let _policy_gate = provider.mutation_guard().map_err(|e| e.to_string())?;
    llm::receipts::stage("saving_locally");
    save_workflow_envelope(
        &state, &account, &account_id, &request_id, &source_text, envelope,
        title_override, workflow_title_prefix(workflow), destination,
        &Actor::workflow(workflow.slug(), &account),
    )
}

/// Append variant, mirroring `append_extract_note` exactly — see its doc
/// comment for the full existence-check-before-and-after-the-LLM-call
/// reasoning, unchanged here. Only two things differ: the LLM call runs
/// `provider.run_workflow(workflow, ...)` instead of `provider.extract(...)`,
/// and the `request_id` bookkeeping happens under `run_llm_workflow`'s own
/// log prefix.
#[tauri::command]
pub async fn append_llm_workflow_note(
    account_id: String,
    workflow: crate::llm::provider::WorkflowKind,
    target_uuid: String,
    source_text: String,
    source: crate::privacy::ai_scope::AiSource,
    request_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    refuse_studio_kind(workflow)?;
    if workflow == llm::provider::WorkflowKind::ActionItems {
        return Err("Action items require preview and explicit application.".into());
    }
    let source_text = ai_source_text(&source, source_text);
    let receipt_request = request_id.clone();
    llm::receipts::run(&receipt_request, None, "append_llm_workflow_note", async {
        llm::receipts::source_version(serde_json::to_vec(&(&account_id, &source_text)).unwrap_or_default().as_slice());
        let _request_cleanup = AiRequestCleanup { requests: &state.in_flight_extracts, id: request_id.clone() };
        // Refuse a write this area can't accept yet, BEFORE SQLite, so no
        // unpushable row is created. See `refuse_write`.
        // Write::Notes only: appending to an existing note creates no folder.
        // (A fallback note, if the LLM fails, goes through resolve_destination,
        // which never creates a folder the backend cannot write.)
        refuse_write(&state, &account_id, backend::Write::Notes)?;
        log!(
            "append_llm_workflow_note: account={} workflow={:?} target={} source_len={} request_id={}",
            account_id,
            workflow,
            target_uuid,
            source_text.len(),
            request_id
        );

        let cancel = tokio_util::sync::CancellationToken::new();
        state
            .in_flight_extracts
            .lock()
            .unwrap()
            .insert(request_id.clone(), cancel.clone());

        // Existence check ONLY — before spending an LLM call, confirm the target
        // note is real. Deliberately NOT reused for the write below: the LLM call
        // can run long enough for the note to change underneath us, so the actual
        // write re-fetches fresh (see step below).
        match state.db.get(&target_uuid, &account_id) {
            Ok(Some(_)) => {}
            Ok(None) => {
                state.in_flight_extracts.lock().unwrap().remove(&request_id);
                return Err(format!("target note not found: {target_uuid}"));
            }
            Err(e) => {
                state.in_flight_extracts.lock().unwrap().remove(&request_id);
                return Err(format!("get target note: {e}"));
            }
        }

        let account = {
            let list = state.accounts.lock().unwrap();
            list.iter()
                .find(|a| a.id == account_id)
                .ok_or_else(|| {
                    state.in_flight_extracts.lock().unwrap().remove(&request_id);
                    format!("account not found: {account_id}")
                })?
                .clone()
        };
        // Spec 2026-10-08 §4.4: a note in a hidden folder never reaches a
        // provider. Checked before the provider is built, so a refusal spends nothing.
        let ai = ai_scope_for_source(&state, &account_id, &source, &request_id)?;
        let provider = checked_account_provider(&state, &account).map_err(|e| {
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            e.to_string()
        })?;

        // Roadmap #0: same tag-vocabulary nudge as `run_workflow_note_into`.
        let existing_tags: Vec<String> =
            ai.tags_visible_to_ai(&state.db, &account_id);

        llm::receipts::source_version(&serde_json::to_vec(&(&account_id, &source_text, &existing_tags)).unwrap_or_default());
        llm::receipts::stage(match workflow {
            llm::provider::WorkflowKind::Summarize => "summarize",
            llm::provider::WorkflowKind::ActionItems => "action_items",
            llm::provider::WorkflowKind::ExpandBullets => "expand_bullets",
            llm::provider::WorkflowKind::CleanTranscript => "clean_transcript",
            llm::provider::WorkflowKind::StudioBriefing => "studio_briefing",
            llm::provider::WorkflowKind::StudioFaq => "studio_faq",
            llm::provider::WorkflowKind::StudioTimeline => "studio_timeline",
            llm::provider::WorkflowKind::StudioStudyGuide => "studio_study_guide",
            llm::provider::WorkflowKind::StudioSlideDeck => "studio_slide_deck",
            llm::provider::WorkflowKind::StudioCourseModule => "studio_course_module",
            llm::provider::WorkflowKind::StudioPodcastScript => "studio_podcast_script",
            llm::provider::WorkflowKind::StudioVideoScript => "studio_video_script",
            llm::provider::WorkflowKind::StudioDigest => "studio_digest",
        });
        let clean_limits = crate::llm::transcript::CleanLimits::from_settings(&llm::budget::get_ai_limits().unwrap_or_default());
        if workflow == llm::provider::WorkflowKind::CleanTranscript {
            crate::llm::transcript::check_attempts(
                crate::llm::transcript::chunk(&source_text, clean_limits.chunk_chars).len(),
                clean_limits.max_attempts,
                crate::llm::transcript::AttemptContext::Pasted,
            ).map_err(|e| {
                state.in_flight_extracts.lock().unwrap().remove(&request_id);
                e
            })?;
        }
        let envelope = match crate::llm::transcript::run_workflow_or_clean(provider.as_ref(), workflow, &source_text, &existing_tags, &clean_limits, cancel, crate::llm::transcript::AttemptContext::Pasted).await {
            Ok(env) => env,
            Err(crate::llm::provider::ExtractError::Cancelled) => {
                state.in_flight_extracts.lock().unwrap().remove(&request_id);
                log!("append_llm_workflow_note: cancelled by user (request_id={request_id})");
                return Err("cancelled".to_string());
            }
            Err(e) => {
                log!("append_llm_workflow_note: provider error (details omitted) — preserving the source");
                let _policy_gate = provider.mutation_guard().map_err(|e| e.to_string())?;
                let fallback = preserve_failed_source(&state, &account_id, &source, &source_text);
                state.in_flight_extracts.lock().unwrap().remove(&request_id);
                let kept = fallback?;
                return Err(format!(
                    "LLM call failed; {kept}. {e}"
                ));
            }
        };

        // Existence pre-check — gives the fallback-note UX its "target
        // disappeared" message. The write itself goes through
        // apply_local_edit_with_retry below, which re-reads and recomputes the
        // append on every attempt rather than reusing this snapshot.
        if state.db.get(&target_uuid, &account_id).map_err(|e| {
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            format!("re-fetch target note: {e}")
        })?.is_none() {
            log!("append_llm_workflow_note: target note disappeared during LLM call — preserving the source");
            let _policy_gate = provider.mutation_guard().map_err(|e| e.to_string())?;
            let fallback = preserve_failed_source(&state, &account_id, &source, &source_text);
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            let kept = fallback?;
            return Err(format!(
                "target note was deleted while extracting; {kept}"
            ));
        }

        let _policy_gate = provider.mutation_guard().map_err(|e| e.to_string())?;
        let tag_model = crate::db::tag_model_for_account(&account_id);
        apply_local_edit_with_retry(&state.db, &account_id, &target_uuid, 5, &Actor::workflow(workflow.slug(), &account), |existing| {
            let new_body = crate::llm::markdown::append_to_note_body(
                &existing.body_html,
                &envelope,
                &source_text,
                tag_model,
            );
            Ok((existing.title.clone(), new_body))
        })
        .map_err(|e| {
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            format!("apply_local_edit: {e}")
        })?;

        state.in_flight_extracts.lock().unwrap().remove(&request_id);

        log!("append_llm_workflow_note: appended to note uuid={target_uuid}");
        Ok(target_uuid)
    }).await
}

/// Answer a question from the local cache. Read-only: touches no note,
/// folder, edge, or sidecar, and never calls Gmail.
#[tauri::command]
pub async fn ask_jodd(
    session_id: String,
    question: String,
    request_id: String,
    state: State<'_, AppState>,
) -> Result<ask::AskAnswer, String> {
    let receipt_request = request_id.clone();
    llm::receipts::run(&receipt_request, None, "ask_jodd", async {
        let stamp = ai_stamp(&state);
        let (session, turns) = {
            let mut sessions = state.ai_policy.sessions.lock().unwrap();
            let session = sessions.get_mut(&session_id).ok_or("Conversation expired. Reopen Ask Jodd.")?;
            let turns = session.begin_turn(&stamp, &question)?;
            (session.clone(), turns)
        };
        let cancel = session.cancel.child_token();
        state.in_flight_asks.lock().unwrap().insert(request_id.clone(), cancel.clone());
        let result: Result<ask::AskAnswer, String> = async {
            let ai = crate::privacy::ai_scope::AiScope::load()?;
            // A folder can be hidden mid-conversation, so the refusal must
            // precede provider resolution (`run_ask` re-checks as defense in depth).
            if let ask::AskScope::Folder { account_id, label } = &session.scope {
                ai.check_folder(account_id, label)?;
            }
            // Likewise a note can be deleted, or moved into a hidden folder.
            if let ask::AskScope::Note { account_id, uuid, .. } = &session.scope {
                ask::related::check_note_scope(&state.db, &ai, account_id, uuid)?;
            }
            // And every picked note can be; refused only when none is left.
            if let ask::AskScope::Notes { account_id, uuids, .. } = &session.scope {
                ask::picked::resolve_picked(&state.db, &ai, account_id, uuids)?;
            }
            let inner = llm::resolve::resolve_app_provider().map_err(|e| e.to_string())?;
            let provider = llm::policy::CheckedProvider {
                cancel: Mutex::new(None),
                gate: &state.ai_policy.gate, inner, valid: Box::new(|| ai_stamp(&state) == stamp && !session.cancel.is_cancelled()),
                // Earlier turns' masked values, so an answer that repeats one
                // is masked when it comes back as history (final review C2).
                known: session.known.clone(),
            };
            // An allowlist from authoritative accounts, expanded to exclusions for
            // SQL. Unknown/orphaned database accounts fail closed too.
            let allowed: Vec<String> = state.accounts.lock().unwrap().iter()
                .filter(|a| llm::policy::account_allowed(a)).map(|a| a.id.clone()).collect();
            let excluded = state.db.ai_excluded_accounts(&allowed).map_err(|e| e.to_string())?;
            ask::run::run_ask(&state.db, &provider, &session.scope, &turns, cancel.clone(), &excluded, &ai)
                .await.map_err(|e| e.to_string())
        }.await;
        state.in_flight_asks.lock().unwrap().remove(&request_id);
        let current = ai_stamp(&state);
        let conversation_id = {
            let mut sessions = state.ai_policy.sessions.lock().unwrap();
            let live = sessions.get_mut(&session_id).ok_or("Conversation expired. Start again.")?;
            live.finish_turn(&current, question.clone(), result.as_ref().ok().map(|a| a.markdown.as_str()), cancel.is_cancelled())?;
            live.conversation_id.clone()
        };
        let mut answer = result?;
        // Only after finish_turn accepted the turn: history holds exactly
        // what the session holds (answered turns only). A failed save is
        // logged and never fails the answer (spec §6).
        if ask::history::enabled() {
            let stats = ask::history::TurnStats {
                notes_in_scope: answer.notes_in_scope, notes_considered: answer.notes_considered,
                notes_used: answer.notes_used, trimmed: answer.trimmed, dropped_citations: answer.dropped_citations,
            };
            let turn = ask::history::NewTurn {
                question: &question, answer_markdown: &answer.markdown,
                cited: &answer.cited, read: &answer.read, stats: &stats,
            };
            // Under the gate, and only if the stamp `finish_turn` accepted is
            // still current: `remove_account` and every other `invalidate_ai`
            // take the gate too, so a save can neither land after an account
            // removal deleted its conversations nor outlive an AI-off. Lock
            // order gate → DB; the sessions lock is never held across the DB
            // call. The destination is the one this session opened with —
            // what the user saw and accepted (R3) — not a fresh read.
            let saved = {
                let _gate = state.ai_policy.gate.lock().unwrap();
                if ai_stamp(&state) != current {
                    log!("ask_jodd: history save skipped: AI settings changed after the answer");
                    None
                } else {
                    match state.db.ask_append_turn(conversation_id.as_deref(), &session.scope, &session.destination, &turn, db::now_ms()) {
                        Ok(id) => Some(id),
                        Err(e) => { log!("ask_jodd: history save failed: {e}"); None }
                    }
                }
            };
            if let Some(id) = saved {
                if let Some(live) = state.ai_policy.sessions.lock().unwrap().get_mut(&session_id) {
                    live.conversation_id = Some(id.clone());
                }
                answer.conversation_id = Some(id);
            }
        }
        Ok(answer)
    }).await
}

#[tauri::command]
pub fn cancel_ask(request_id: String, state: State<'_, AppState>) {
    if let Some(tok) = state.in_flight_asks.lock().unwrap().remove(&request_id) { tok.cancel(); }
}

/// Mirrors `jodd-mcp`'s `write_with_retry` — same race, different crate and
/// lookup (`Db::get`, not `note_by_uuid`, matching what these two callers
/// already used). Re-reads the note fresh on every attempt so a retry
/// recomputes against the latest body instead of resubmitting a stale one.
/// See `Db::apply_local_edit_versioned` for the underlying guarantee.
///
/// `Db::get` here (not `note_by_uuid`) is the deliberate opposite choice
/// from `jodd-mcp`'s `write_with_retry`: `note_by_uuid` filters out
/// `deleted_pending` rows so an external MCP write can't resurrect a note
/// the user just deleted, but neither of this function's callers (Extract
/// re-ingest's `append_extract_note`, auto-link's `apply_wiki_link_appends`)
/// can target a `deleted_pending` uuid in the first place — both derive
/// their target uuid from state that already excludes deleted notes, so the
/// `note_by_uuid` filter would be dead weight here, not a needed guard.
/// That asymmetry is exactly why these two crates keep separate retry
/// helpers instead of one shared one (see the plan's Global Constraints,
/// docs/superpowers/plans/2026-08-12-concurrent-local-writer-race.md, and
/// `write_with_retry`'s doc comment in jodd-mcp/src/write.rs).
pub(crate) fn apply_local_edit_with_retry(
    db: &db::Db,
    account_id: &str,
    uuid: &str,
    max_attempts: u32,
    actor: &Actor,
    mut compute: impl FnMut(&db::CachedNote) -> Result<(String, String), String>,
) -> Result<(), String> {
    for _ in 0..max_attempts {
        let existing = db
            .get(uuid, account_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("note '{uuid}' not found"))?;
        let (new_title, new_body) = compute(&existing)?;
        let applied = db
            .apply_local_edit_versioned(uuid, account_id, &new_title, &new_body, &existing.label, existing.local_version, actor)
            .map_err(|e| e.to_string())?;
        if applied {
            return Ok(());
        }
    }
    Err(format!(
        "note '{uuid}' is being edited concurrently; giving up after {max_attempts} attempts"
    ))
}

/// Ingest a new source by appending the LLM's extraction to an EXISTING
/// note's body, instead of creating a new note (contrast `extract_note`).
/// Never renames, relabels, or otherwise restructures the target note — only
/// appends. See docs/superpowers/specs/2026-07-10-extract-ingest-entrypoint-design.md.
#[tauri::command]
pub async fn append_extract_note(
    account_id: String,
    target_uuid: String,
    source_text: String,
    source: crate::privacy::ai_scope::AiSource,
    request_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let source_text = ai_source_text(&source, source_text);
    let receipt_request = request_id.clone();
    llm::receipts::run(&receipt_request, None, "append_extract_note", async {
        llm::receipts::source_version(serde_json::to_vec(&(&account_id, &source_text)).unwrap_or_default().as_slice());
        let _request_cleanup = AiRequestCleanup { requests: &state.in_flight_extracts, id: request_id.clone() };
        // Refuse a write this area can't accept yet, BEFORE SQLite, so no
        // unpushable row is created. See `refuse_write`.
        // Write::Notes only: appending to an existing note creates no folder.
        // (A fallback note, if the LLM fails, goes through resolve_destination,
        // which never creates a folder the backend cannot write.)
        refuse_write(&state, &account_id, backend::Write::Notes)?;
        log!(
            "append_extract_note: account={} target={} source_len={} request_id={}",
            account_id,
            target_uuid,
            source_text.len(),
            request_id
        );

        let cancel = tokio_util::sync::CancellationToken::new();
        state
            .in_flight_extracts
            .lock()
            .unwrap()
            .insert(request_id.clone(), cancel.clone());

        // Existence check ONLY — before spending an LLM call, confirm the target
        // note is real. Deliberately NOT reused for the write below: the LLM call
        // can run long enough for the note to change underneath us, so the actual
        // write re-fetches fresh (see step below).
        match state.db.get(&target_uuid, &account_id) {
            Ok(Some(_)) => {}
            Ok(None) => {
                state.in_flight_extracts.lock().unwrap().remove(&request_id);
                return Err(format!("target note not found: {target_uuid}"));
            }
            Err(e) => {
                state.in_flight_extracts.lock().unwrap().remove(&request_id);
                return Err(format!("get target note: {e}"));
            }
        }

        let account = {
            let list = state.accounts.lock().unwrap();
            list.iter()
                .find(|a| a.id == account_id)
                .ok_or_else(|| {
                    state.in_flight_extracts.lock().unwrap().remove(&request_id);
                    format!("account not found: {account_id}")
                })?
                .clone()
        };
        // Spec 2026-10-08 §4.4: a note in a hidden folder never reaches a
        // provider. Checked before the provider is built, so a refusal spends nothing.
        let ai = ai_scope_for_source(&state, &account_id, &source, &request_id)?;
        let provider = checked_account_provider(&state, &account).map_err(|e| {
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            e.to_string()
        })?;

        // Roadmap #0: same tag-vocabulary nudge as `extract_note_into`.
        let existing_tags: Vec<String> =
            ai.tags_visible_to_ai(&state.db, &account_id);

        llm::receipts::source_version(&serde_json::to_vec(&(&account_id, &source_text, &existing_tags)).unwrap_or_default());
        llm::receipts::stage("extract");
        let envelope = match provider.extract(&source_text, &existing_tags, cancel).await {
            Ok(env) => env,
            Err(crate::llm::provider::ExtractError::Cancelled) => {
                state.in_flight_extracts.lock().unwrap().remove(&request_id);
                log!("append_extract_note: cancelled by user (request_id={request_id})");
                return Err("cancelled".to_string());
            }
            Err(e) => {
                log!("append_extract_note: provider error (details omitted) — preserving the source");
                let _policy_gate = provider.mutation_guard().map_err(|e| e.to_string())?;
                let fallback = preserve_failed_source(&state, &account_id, &source, &source_text);
                state.in_flight_extracts.lock().unwrap().remove(&request_id);
                let kept = fallback?;
                return Err(format!(
                    "LLM call failed; {kept}. {e}"
                ));
            }
        };

        // Existence pre-check — gives the fallback-note UX its "target
        // disappeared" message. The write itself goes through
        // apply_local_edit_with_retry below, which re-reads and recomputes the
        // append on every attempt rather than reusing this snapshot.
        if state.db.get(&target_uuid, &account_id).map_err(|e| {
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            format!("re-fetch target note: {e}")
        })?.is_none() {
            log!("append_extract_note: target note disappeared during LLM call — preserving the source");
            let _policy_gate = provider.mutation_guard().map_err(|e| e.to_string())?;
            let fallback = preserve_failed_source(&state, &account_id, &source, &source_text);
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            let kept = fallback?;
            return Err(format!(
                "target note was deleted while extracting; {kept}"
            ));
        }

        let _policy_gate = provider.mutation_guard().map_err(|e| e.to_string())?;
        let tag_model = crate::db::tag_model_for_account(&account_id);
        apply_local_edit_with_retry(&state.db, &account_id, &target_uuid, 5, &Actor::workflow("extract", &account), |existing| {
            let new_body = crate::llm::markdown::append_to_note_body(
                &existing.body_html,
                &envelope,
                &source_text,
                tag_model,
            );
            Ok((existing.title.clone(), new_body))
        })
        .map_err(|e| {
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            format!("apply_local_edit: {e}")
        })?;

        state.in_flight_extracts.lock().unwrap().remove(&request_id);

        log!("append_extract_note: appended to note uuid={target_uuid}");
        Ok(target_uuid)
    }).await
}

#[derive(serde::Serialize)]
pub struct LinkTargetDto {
    pub(crate) uuid: String,
    pub(crate) title: String,
    /// Rename-safe slug (title-slug + uuid8), matching `db::note_slug` — the
    /// same format the `[[` autocomplete picker inserts. Callers should
    /// build inserted wikilinks as `[[{slug}]]`, never `[[{title}]]`, so
    /// they survive a later note rename.
    pub(crate) slug: String,
}

#[derive(serde::Serialize)]
pub struct ProposedAppendDto {
    pub(crate) ai_result_id: String,
    pub(crate) uuid: String,
    pub(crate) title: String,
    pub(crate) addition_text: String,
}

#[derive(serde::Serialize)]
pub struct LinkSuggestionsResponse {
    pub(crate) ai_result_id: String,
    pub(crate) auto_links: Vec<LinkTargetDto>,
    pub(crate) proposed_appends: Vec<ProposedAppendDto>,
}

/// Suggest wiki links for `text` — either the just-created/updated note's
/// own body (#2a, called after extract_note/append_extract_note
/// succeeds) or an existing note's current body as-is (#2b, "Link into
/// wiki"). See docs/superpowers/specs/2026-07-20-auto-link-ingest-design.md.
/// Caller is responsible for auto-inserting `auto_links` into the relevant
/// note's body (a separate apply_local_edit call) and for presenting
/// `proposed_appends` to the user for confirmation before calling
/// apply_wiki_link_appends.
#[tauri::command]
pub async fn suggest_wiki_links(
    account_id: String,
    text: String,
    exclude_uuid: Option<String>,
    new_note_title: String,
    new_note_uuid: String,
    request_id: String,
    parent_request_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<LinkSuggestionsResponse, String> {
    let receipt_request = request_id.clone();
    llm::receipts::run(&receipt_request, parent_request_id.as_deref(), "suggest_wiki_links", async {
        llm::receipts::source_version(account_id.as_bytes());
        let _request_cleanup = AiRequestCleanup { requests: &state.in_flight_extracts, id: request_id.clone() };
        log!(
            "suggest_wiki_links: account={} text_len={} request_id={}",
            account_id,
            text.len(),
            request_id
        );

        let cancel = tokio_util::sync::CancellationToken::new();
        state
            .in_flight_extracts
            .lock()
            .unwrap()
            .insert(request_id.clone(), cancel.clone());

        let account = {
            let list = state.accounts.lock().unwrap();
            list.iter()
                .find(|a| a.id == account_id)
                .ok_or_else(|| {
                    state.in_flight_extracts.lock().unwrap().remove(&request_id);
                    format!("account not found: {account_id}")
                })?
                .clone()
        };
        let ai = match crate::privacy::ai_scope::AiScope::load() {
            Ok(ai) => ai,
            Err(e) => {
                state.in_flight_extracts.lock().unwrap().remove(&request_id);
                return Err(e);
            }
        };
        // The text sent is always this note's own body, so a note in a hidden
        // folder is refused here (`autolink::suggest_links` re-checks as
        // defense in depth).
        if let Err(e) = ai.check_source(&state.db, &account_id, &crate::privacy::ai_scope::AiSource::Note { uuid: new_note_uuid.clone() }) {
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            return Err(e);
        }
        let provider = checked_account_provider(&state, &account).map_err(|e| {
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            e.to_string()
        })?;

        // Finding F1 (2026-09-15 whole-branch review): `text` is the just-written
        // note's own body — an ingest note's `## Sources` list (full URLs, query
        // strings and all) and its verbatim Source block (up to 400 000 stored
        // chars) must never reach a provider. Cut before both, then cap what's
        // left the same way every other LLM input in this codebase is capped.
        let suggestion_text: String = crate::llm::markdown::text_for_suggestions(&text)
            .chars()
            .take(crate::llm::markdown::SUGGESTION_TEXT_CHARS)
            .collect();

        let result = crate::llm::autolink::suggest_links(
            provider.as_ref(),
            &state.db,
            &account_id,
            exclude_uuid.as_deref(),
            &new_note_title,
            &new_note_uuid,
            &suggestion_text,
            cancel,
            &ai,
        )
        .await;

        state.in_flight_extracts.lock().unwrap().remove(&request_id);

        let _policy_gate = provider.mutation_guard().map_err(|e| e.to_string())?;
        let ai_result_id = issue_ai_result(&state, &account_id, workflow_actor(&state, &account_id, "autolink"));
        match result {
            Ok(suggestions) => Ok(LinkSuggestionsResponse {
                ai_result_id: ai_result_id.clone(),
                auto_links: suggestions
                    .auto_links
                    .into_iter()
                    .map(|t| {
                        let slug = crate::db::note_slug(&t.title, &t.uuid);
                        LinkTargetDto { uuid: t.uuid, title: t.title, slug }
                    })
                    .collect(),
                proposed_appends: suggestions
                    .proposed_appends
                    .into_iter()
                    .map(|a| ProposedAppendDto { ai_result_id: ai_result_id.clone(), uuid: a.uuid, title: a.title, addition_text: a.addition_text })
                    .collect(),
            }),
            Err(crate::llm::provider::ExtractError::Cancelled) => Err("cancelled".to_string()),
            Err(e) => Err(e.to_string()),
        }
    }).await
}

/// Propose an existing folder for note `uuid` — after an Extract, and from
/// the note context menu's "Suggest folder". Read-only (no `refuse_write`):
/// accepting the proposal is a separate `move_notes_batch`.
///
/// The caller decides how loud each outcome is (spec "Suggestion" table), so
/// this returns the outcome as-is. The cancel token is removed on every exit
/// path: everything fallible runs inside one block, then the removal.
#[tauri::command]
pub async fn suggest_note_folder(
    account_id: String,
    uuid: String,
    request_id: String,
    parent_request_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let receipt_request = request_id.clone();
    llm::receipts::run(&receipt_request, parent_request_id.as_deref(), "suggest_note_folder", async {
        llm::receipts::source_version(account_id.as_bytes());
        let _request_cleanup = AiRequestCleanup { requests: &state.in_flight_extracts, id: request_id.clone() };
        let result_stamp = ai_stamp(&state);
        log!("suggest_note_folder: account={} uuid={} request_id={}", account_id, uuid, request_id);

        let cancel = tokio_util::sync::CancellationToken::new();
        state
            .in_flight_extracts
            .lock()
            .unwrap()
            .insert(request_id.clone(), cancel.clone());

        let result: Result<crate::llm::filing::FolderSuggestionOutcome, String> = async {
            let ai = crate::privacy::ai_scope::AiScope::load()?;
            // Note gone, no folders, every folder blocked: SQLite alone says
            // so — before the provider check, so "configure a provider"
            // never hides an answer that needs no LLM. It applies `ai` too:
            // a hidden note is refused here, and hidden folders are neither
            // offered nor named. Asked before `check_source` so a gone note
            // still answers NoteNotFound rather than an error.
            if let Some(outcome) = crate::llm::filing::answer_without_llm(&state.db, &account_id, &uuid, &ai)
                .map_err(|e| e.to_string())?
            {
                return Ok(outcome);
            }
            let account = {
                let list = state.accounts.lock().unwrap();
                list.iter()
                    .find(|a| a.id == account_id)
                    .cloned()
                    .ok_or_else(|| format!("account not found: {account_id}"))?
            };
            // Refuse a hidden note before any provider exists; `suggest_folder`
            // keeps its own check as defense in depth.
            ai.check_source(&state.db, &account_id, &crate::privacy::ai_scope::AiSource::Note { uuid: uuid.clone() })?;
            let provider = checked_account_provider(&state, &account).map_err(|e| e.to_string())?;
            crate::llm::filing::suggest_folder(provider.as_ref(), &state.db, &account_id, &uuid, cancel.clone(), &ai)
                .await
                .map_err(|e| match e {
                    crate::llm::provider::ExtractError::Cancelled => "cancelled".to_string(),
                    other => other.to_string(),
                })
        }
        .await;

        state.in_flight_extracts.lock().unwrap().remove(&request_id);

        let mut value = serde_json::to_value(result?).map_err(|e| e.to_string())?;
        // The provider was checked above; validate the run's original policy once
        // more while issuing eligibility for a later user-confirmed move.
        let _policy_gate = state.ai_policy.gate.lock().unwrap();
        if cancel.is_cancelled() || ai_stamp(&state) != result_stamp { return Err("AI settings changed. Generate the suggestion again.".into()); }
        if value["kind"] == "suggested" { value["ai_result_id"] = issue_ai_result(&state, &account_id, workflow_actor(&state, &account_id, "filing")).into(); }
        Ok(value)
    }).await
}

#[derive(serde::Deserialize)]
pub struct ConfirmedAppend {
    pub(crate) ai_result_id: String,
    pub(crate) uuid: String,
    pub(crate) addition_text: String,
}

/// Apply confirmed append suggestions from suggest_wiki_links — one
/// apply_local_edit per note, pure concatenation onto the existing body
/// (never rewrites existing content, design spec decision 6). Skips (rather
/// than aborts on) any target that's disappeared since the suggestion was
/// made; returns the uuids that were successfully updated.
/// The body after a confirmed "Link into wiki" append: the addition goes
/// inside the document, never after `</html>` (gotcha #37).
pub(crate) fn wiki_link_append_body(existing: &str, addition: &str) -> String {
    mime822::append_html(existing, &format!("\n<p>{addition}</p>"))
}

#[cfg(test)]
mod wiki_link_append_tests {
    #[test]
    fn a_wiki_link_append_stays_inside_a_whole_document() {
        let doc = "<html><head></head><body style=\"x\"><div>human text</div></body></html>";
        let out = super::wiki_link_append_body(doc, "Related: [[x]]");
        assert!(out.ends_with("</body></html>"), "{out}");
        assert!(crate::mime822::body_inner(&out).contains("Related: [[x]]"), "{out}");
    }
}

#[tauri::command]
pub fn apply_wiki_link_appends(
    account_id: String,
    appends: Vec<ConfirmedAppend>,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Notes)?;
    let _policy_gate = state.ai_policy.gate.lock().unwrap();
    for append in &appends { validate_ai_result(&state, &account_id, &append.ai_result_id)?; }
    let autolink_actor = workflow_actor(&state, &account_id, "autolink");
    let mut applied = Vec::new();
    for a in appends {
        let addition = a.addition_text.clone();
        match apply_local_edit_with_retry(&state.db, &account_id, &a.uuid, 5, &autolink_actor, |existing| {
            Ok((existing.title.clone(), wiki_link_append_body(&existing.body_html, &addition)))
        }) {
            Ok(()) => applied.push(a.uuid),
            Err(e) => log!("apply_wiki_link_appends: apply_local_edit {} failed: {e}", a.uuid),
        }
    }
    Ok(applied)
}

/// Cancel an in-flight extract_note call. The frontend passes the same
/// request_id it gave to extract_note; we look up the CancellationToken
/// and fire it. The extract_note command's tokio::select! sees the token
/// fire and unwinds (HTTP: drops the in-flight reqwest future; Claude CLI:
/// kills the child process). Returns Ok(true) if a token was found and
/// cancelled; Ok(false) if the request_id wasn't registered (likely the
/// extract already completed). Never returns Err — cancellation is
/// idempotent and best-effort.
#[tauri::command]
pub fn cancel_extraction(
    request_id: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let token = state.in_flight_extracts.lock().unwrap().remove(&request_id);
    match token {
        Some(t) => {
            t.cancel();
            log!("cancel_extraction: cancelled request_id={request_id}");
            Ok(true)
        }
        None => {
            log!("cancel_extraction: no in-flight request with id={request_id}");
            Ok(false)
        }
    }
}

/// What Redo key points (`re_extract_note`) runs on a note.
pub(crate) enum ReExtractPlan {
    /// Through `ingest_to_note`: a legacy ingest note's inline block
    /// (`Stored`, beside the note) or source notes (`SourceNotes`, spec
    /// 2026-11-19 §5.5).
    Ingest(crate::ingest::run::IngestInput, crate::ingest::run::IngestDestination),
    /// A legacy Extract note: its Source section's text, beside the note.
    Extract(String),
}

/// Decide what Redo runs, from SQLite alone, checking every label it will
/// read against AI scope before anything is sent (spec 2026-11-19 §5.5,
/// §5.7). In order: a derived note reads its source notes and its result goes
/// beside it; a source note reads itself and its result goes to the Inbox,
/// never `__Sources__`; anything else is a legacy note, unchanged (§6).
pub(crate) fn plan_re_extract(
    db: &db::Db,
    scope: &crate::privacy::ai_scope::AiScope,
    account_id: &str,
    note: &db::CachedNote,
) -> Result<ReExtractPlan, String> {
    use crate::ingest::run::{IngestDestination, IngestInput, LinkedSource};
    use crate::ingest::source_note;
    scope.check_label(account_id, &note.label)?;

    if source_note::is_derived(&note.body_html) {
        // A failed source has a Sources-list entry and no `Derived from`
        // line, so the k-th line pairs with the k-th entry that did not fail.
        let urls = source_note::derived_source_urls(&note.body_html);
        let mut linked: Vec<LinkedSource> = Vec::new();
        for (k, link) in source_note::parse_derived_from(&note.body_html).into_iter().enumerate() {
            let gone = || {
                let url = urls.get(k).cloned().unwrap_or_else(|| link.link_key.clone());
                format!("The saved copy of {url} is gone. Ingest the link again to make a new one.")
            };
            // Follows a rekey alias (gotcha #16), or a rekey made on another
            // device (Ruling 14); deleted rows and other accounts do not resolve.
            let found = source_note::resolve_derived_from(db, account_id, &link, urls.get(k).map(String::as_str))
                .map_err(|e| format!("resolve source note: {e}"))?;
            let Some(found) = found else { return Err(gone()) };
            let Some((header, source)) = source_note::parse_source_note(&found.body_html) else { return Err(gone()) };
            scope.check_label(account_id, &found.label)?;
            if !linked.iter().any(|l| l.uuid == found.uuid) {
                linked.push(LinkedSource { uuid: found.uuid, title: found.title, header, source });
            }
        }
        return Ok(ReExtractPlan::Ingest(IngestInput::SourceNotes(linked), IngestDestination::BesideSource(note.label.clone())));
    }

    if let Some((header, source)) = source_note::parse_source_note(&note.body_html) {
        let me = LinkedSource { uuid: note.uuid.clone(), title: note.title.clone(), header, source };
        return Ok(ReExtractPlan::Ingest(IngestInput::SourceNotes(vec![me]), IngestDestination::Resolve));
    }

    let source = crate::llm::markdown::extract_source(&note.body_html)
        .ok_or_else(|| "note has no Source section to re-extract from".to_string())?;
    // A URL-ingest note holds one or more sources in one block (spec
    // Decision 8): re-run map-reduce over the stored text, never refetching.
    // Finding F2 (2026-09-15 whole-branch review): a `1 of 1` block can only
    // come from `ingest_to_note` itself (Decision 6: one map call, no reduce,
    // for a single source) — the ORIGINAL Extract path (`extract_note_into`)
    // never writes this header shape at all. Routing every recognised block
    // through here, not just `> 1`, is what gives a single-source ingest note
    // the same no-refetch, no-map-cap-bypass Re-extract as a multi-source one.
    if let Some(sources) = crate::ingest::stored::parse_sources(&source).filter(|s| !s.is_empty()) {
        return Ok(ReExtractPlan::Ingest(IngestInput::Stored(sources), IngestDestination::BesideSource(note.label.clone())));
    }
    Ok(ReExtractPlan::Extract(source))
}

/// Redo key points: run Key points again on what a note was made from, as
/// `plan_re_extract` decides — a derived note's source notes, a source note
/// itself, or a legacy note's preserved Source section. Creates a NEW note
/// (never overwrites — spec Decision 5): beside the note for a derived or
/// legacy note, so the two can be compared; in the Inbox for a source note,
/// never in `__Sources__` (spec 2026-11-19 §5.5).
#[tauri::command]
pub async fn re_extract_note(
    account_id: String,
    uuid: String,
    request_id: String,
    state: State<'_, AppState>,
) -> Result<ExtractedNoteDto, String> {
    let receipt_request = request_id.clone();
    llm::receipts::run(&receipt_request, None, "re_extract_note", async {
        llm::receipts::source_version(account_id.as_bytes());
        refuse_write(&state, &account_id, backend::Write::Notes)?;
        // The menu may hold a pre-rekey uuid (gotcha #16); follow its alias.
        let uuid = state
            .db
            .resolve_note_uuid(&uuid, &account_id)
            .map_err(|e| format!("resolve note uuid: {e}"))?;
        let note = state
            .db
            .get(&uuid, &account_id)
            .map_err(|e| format!("get note: {e}"))?
            .ok_or_else(|| format!("note not found: {uuid}"))?;
        // Spec 2026-10-08 §4.4 — every label checked before any branch below
        // sends anything; the plan reads SQLite only, never the network.
        let scope = crate::privacy::ai_scope::AiScope::load()?;
        let (input, destination) = match plan_re_extract(&state.db, &scope, &account_id, &note)? {
            ReExtractPlan::Extract(source) => {
                return extract_note_into(
                    account_id,
                    source,
                    crate::privacy::ai_scope::AiSource::Note { uuid: uuid.clone() },
                    None,
                    request_id,
                    ExtractDestination::BesideSource(note.label),
                    state,
                )
                .await;
            }
            ReExtractPlan::Ingest(input, destination) => (input, destination),
        };
        let cancel = tokio_util::sync::CancellationToken::new();
        state.in_flight_extracts.lock().unwrap().insert(request_id.clone(), cancel.clone());
        let result = run_ingest_command(
            &state,
            &account_id,
            input,
            String::new(),
            None,
            destination,
            crate::ingest::run::IngestMode::KeyPoints,
            // Stored text and source notes are never refetched, so nothing is
            // downloaded or transcribed.
            crate::ingest::HttpFetcher::default(),
            // …and nothing is asked of a fetch.
            Default::default(),
            // Nothing is fetched, so nothing is followed or pasted.
            Default::default(),
            Default::default(),
            cancel,
            &|_| {},
        )
        .await;
        state.in_flight_extracts.lock().unwrap().remove(&request_id);
        result
    }).await
}


/// The actor for a Jodd LLM workflow run against `account_id`. Used where only
/// the id is in scope; if the account has gone away the model is `unknown` —
/// never `Actor::Human`, because a workflow wrote this.
pub(crate) fn workflow_actor(state: &AppState, account_id: &str, name: &str) -> Actor {
    let account = state.accounts.lock().unwrap().iter().find(|a| a.id == account_id).cloned();
    match account {
        Some(a) => Actor::workflow(name, &a),
        None => Actor::Workflow { name: name.into(), model: "unknown".into() },
    }
}

/// The text an AI run sends for its source. A note picked as the source
/// arrives as its stored HTML; `note_text_for_ai` drops a Source block the
/// note already restates, so a transcript note is not sent twice over.
pub(crate) fn ai_source_text(source: &crate::privacy::ai_scope::AiSource, source_text: String) -> String {
    match source {
        crate::privacy::ai_scope::AiSource::Note { .. } => crate::llm::markdown::note_text_for_ai(&source_text).into_owned(),
        crate::privacy::ai_scope::AiSource::Pasted => source_text,
    }
}

/// What a failed AI run does with its source, as the phrase its error ends
/// with. Pasted text exists nowhere else, so it is kept in a new note
/// (`create_fallback_source_note`). A note picked as the source is already
/// saved: copying it only made clutter — two "Source (extraction failed)"
/// notes of one transcript on 2026-10-24 — so nothing is written.
pub(crate) fn preserve_failed_source(
    state: &AppState,
    account_id: &str,
    source: &crate::privacy::ai_scope::AiSource,
    source_text: &str,
) -> Result<String, String> {
    match source {
        crate::privacy::ai_scope::AiSource::Note { .. } => Ok("your source note is unchanged".into()),
        crate::privacy::ai_scope::AiSource::Pasted => {
            create_fallback_source_note(state, account_id, source_text).map(|uuid| format!("source preserved in note {uuid}"))
        }
    }
}

/// Doctrine compliance: if the LLM call fails, we still owe the user a note
/// preserving the verbatim source paste so they can retry or recover by hand.
pub(crate) fn create_fallback_source_note(
    state: &AppState,
    account_id: &str,
    source: &str,
) -> Result<String, String> {
    let folder = crate::llm::filing::resolve_destination(
        &state.db,
        account_id,
        account_can_create_folders(state, account_id),
    )
    .map_err(|e| format!("resolve destination: {e}"))?;
    let body = format!(
        "<p><em>Extraction failed. Source preserved below.</em></p>\n<hr>\n\
         <details open>\n<summary>Source (verbatim)</summary>\n<pre>{}</pre>\n</details>\n",
        crate::llm::markdown::escape_html(source)
    );
    // Per-backend shape (gotcha #18): a CloudKit recordName is lowercase.
    let backend_kind = state
        .accounts
        .lock()
        .unwrap()
        .iter()
        .find(|a| a.id == account_id)
        .map(|a| a.backend_kind)
        .ok_or_else(|| format!("account not found: {account_id}"))?;
    let uuid = crate::backend::mint_uuid_for(backend_kind);
    let title = format!(
        "Source (extraction failed) — {}",
        chrono::Local::now().format("%Y-%m-%d")
    );
    let now = db::now_ms();
    let new_note = db::CachedNote {
        uuid: uuid.clone(),
        account_id: account_id.to_string(),
        id: String::new(),
        title,
        body_html: body,
        date: chrono::Local::now().to_rfc2822(),
        x_mail_created_date: None,
        label: folder,
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
    state
        .db
        .insert_local_new(&new_note, &Actor::Process("fallback-source"))
        .map_err(|e| format!("insert_local_new: {e}"))?;
    Ok(uuid)
}

/// Gotcha #18's mirror half — the uuid Jodd MINTS — for the two Extract
/// create sites. `format_apple_uuid` uppercases, which is right for Apple's
/// email wire format and wrong for a CloudKit `recordName`; every site that
/// produces a note the worker will CREATE must go through
/// `backend::mint_uuid_for(kind)`. These are Tauri-state functions with no
/// unit-test harness, so the invariant is pinned the way jodd-mcp pins
/// `do_create_note`'s write checks: by reading the function's own source.
#[cfg(test)]
mod extract_mint_tests {
    #[test]
    fn extract_create_sites_mint_through_the_backend_policy() {
        let src = include_str!("ai_commands.rs");
        // `extract_note_into` used to mint inline; Task 4 (save_workflow_envelope)
        // moved that call into the shared tail it now delegates to — pin the
        // invariant at its new site, not the delegator. `save_workflow_envelope`
        // covers every "LLM envelope -> saved note" workflow, Extract included.
        for signature in ["fn save_workflow_envelope(", "fn create_fallback_source_note("] {
            let body = crate::test_support::extract_fn_body(src, signature);
            assert!(
                body.contains("mint_uuid_for("),
                "`{signature}` must mint its note uuid with backend::mint_uuid_for (gotcha #18)"
            );
            assert!(
                !body.contains("format_apple_uuid("),
                "`{signature}` must not mint with format_apple_uuid — it uppercases a CloudKit recordName (gotcha #18)"
            );
        }
    }
}

#[cfg(test)]
mod failed_source_tests {
    const COMMANDS: [&str; 4] = [
        "async fn extract_note_into(",
        "async fn run_workflow_note_into(",
        "async fn append_llm_workflow_note(",
        "async fn append_extract_note(",
    ];

    /// A note picked as the AI source must not be copied into a new
    /// "Source (extraction failed)" note when the run fails: every failure
    /// path goes through `preserve_failed_source`, which only writes for
    /// pasted text.
    #[test]
    fn failure_paths_preserve_the_source_through_one_rule() {
        let src = include_str!("ai_commands.rs");
        let mut sites = 0;
        for f in COMMANDS {
            let body = crate::test_support::extract_fn_body(src, f);
            assert!(!body.contains("create_fallback_source_note("), "{f} must go through preserve_failed_source");
            sites += body.matches("preserve_failed_source(&state, &account_id, &source, &source_text)").count();
        }
        assert_eq!(sites, 6, "every failure path in the four commands");
        let rule = crate::test_support::extract_fn_body(src, "fn preserve_failed_source(");
        let note_arm = rule.find("AiSource::Note").unwrap();
        let pasted_arm = rule.find("AiSource::Pasted").unwrap();
        let write = rule.find("create_fallback_source_note(").unwrap();
        assert!(note_arm < pasted_arm && pasted_arm < write, "only the Pasted arm writes a note");
    }

    /// The source is converted once, before anything reads it — the receipt
    /// hash, the log, the provider and the failure path all see one text.
    #[test]
    fn commands_convert_a_note_source_before_using_it() {
        let src = include_str!("ai_commands.rs");
        for f in COMMANDS {
            let body = crate::test_support::extract_fn_body(src, f);
            let convert = body.find("let source_text = ai_source_text(&source, source_text);").unwrap_or_else(|| panic!("{f} must convert its source"));
            let first_use = body.find("source_text.len()").or_else(|| body.find("&source_text")).unwrap();
            assert!(convert < first_use, "{f}: convert before the first use");
        }
    }

    #[test]
    fn a_note_source_is_sent_without_a_block_it_restates() {
        use crate::privacy::ai_scope::AiSource;
        let words = (0..300).map(|i| format!("w{i}")).collect::<Vec<_>>().join(" ");
        let body = format!("<p>{words}</p><hr>\n<details>\n{}\n<pre>{words}</pre>\n</details>\n", crate::llm::markdown::SOURCE_MARKER);
        assert_eq!(super::ai_source_text(&AiSource::Note { uuid: "u".into() }, body.clone()), format!("<p>{words}</p>"));
        assert_eq!(super::ai_source_text(&AiSource::Pasted, body.clone()), body, "pasted text is sent as typed");
    }
}

#[cfg(test)]
mod save_workflow_envelope_tests {
    #[test]
    fn extract_note_into_delegates_its_tail_to_save_workflow_envelope() {
        let src = include_str!("ai_commands.rs");
        let body = crate::test_support::extract_fn_body(src, "async fn extract_note_into(");
        assert!(
            body.contains("save_workflow_envelope("),
            "extract_note_into must delegate title/folder/uuid/insert to the \
             shared helper, not duplicate that logic inline"
        );
    }

    #[test]
    fn save_workflow_envelope_mints_a_uuid_and_never_formats_one() {
        let src = include_str!("ai_commands.rs");
        let body = crate::test_support::extract_fn_body(src, "fn save_workflow_envelope(");
        assert!(body.contains("mint_uuid_for("), "must mint via the per-backend rule (gotcha #18)");
        assert!(!body.contains("format_apple_uuid("), "must not assume the Apple wire format");
    }
}

/// `run_llm_workflow` is new code (unlike `save_workflow_envelope`'s uuid
/// minting, which `extract_note_into` already exercised before Task 4 moved
/// it) — pin the refusal-before-write ordering explicitly rather than
/// inheriting it silently. Same rationale and technique as
/// `extract_mint_tests`: no unit-test harness for `State`-bound Tauri
/// commands, so the invariant is pinned by reading the function's own source.
#[cfg(test)]
mod run_llm_workflow_tests {
    #[test]
    fn run_llm_workflow_refuses_before_touching_sqlite() {
        let src = include_str!("ai_commands.rs");
        let body = crate::test_support::extract_fn_body(src, "async fn run_llm_workflow(");
        let refuse_pos = body.find("refuse_write(").expect("must call refuse_write");
        let into_pos = body.find("run_workflow_note_into(").expect("must delegate to run_workflow_note_into");
        assert!(refuse_pos < into_pos, "refuse_write must run before the write path starts");
    }

    /// "No folders" / "every folder blocked" / "note gone" need no LLM, so
    /// they must not be hidden behind "configure a provider" — the answer
    /// that needs no LLM is asked before the provider is checked.
    #[test]
    fn suggest_note_folder_answers_without_an_llm_before_checking_the_provider() {
        let src = include_str!("ai_commands.rs");
        let body = crate::test_support::extract_fn_body(src, "async fn suggest_note_folder(");
        let answer = body.find("filing::answer_without_llm(").expect("must try the no-LLM answer");
        let provider = body.find("checked_account_provider(").expect("must still check the provider");
        assert!(answer < provider, "the no-LLM answer comes before the provider check");
    }

    #[test]
    fn ingest_sources_validates_the_mode_before_any_fetch() {
        let src = include_str!("ingest_commands.rs");
        let body = crate::test_support::extract_fn_body(src, "async fn ingest_sources(");
        let validate = body.find("mode.validate()").expect("must validate the mode");
        let run = body.find("run_ingest_command(").expect("must delegate");
        assert!(validate < run, "an invalid mode is refused before anything is fetched");
    }

    #[test]
    fn per_note_workflow_commands_refuse_studio_kinds_before_any_provider() {
        let src = include_str!("ai_commands.rs");
        for sig in ["async fn run_llm_workflow(", "async fn append_llm_workflow_note("] {
            let body = crate::test_support::extract_fn_body(src, sig);
            let refuse = body.find("refuse_studio_kind(").unwrap_or_else(|| panic!("{sig} must refuse Studio kinds"));
            let provider = body.find("checked_account_provider(").unwrap_or(usize::MAX);
            assert!(refuse < provider, "{sig}: refuse before any provider is built");
            let first_stmt = body.find("let receipt_request").expect("receipt statement");
            assert!(refuse < first_stmt, "{sig}: the refusal is the first statement");
        }
    }

    #[test]
    fn refuse_studio_kind_refuses_only_studio() {
        use crate::llm::provider::WorkflowKind::*;
        for k in [
            StudioBriefing,
            StudioFaq,
            StudioTimeline,
            StudioStudyGuide,
            StudioSlideDeck,
            StudioCourseModule,
            StudioPodcastScript,
            StudioVideoScript,
            StudioDigest,
        ] {
            assert_eq!(super::refuse_studio_kind(k), Err(crate::llm::provider::STUDIO_ONLY.to_string()), "{}", k.slug());
        }
        for k in [Summarize, ActionItems, ExpandBullets, CleanTranscript] {
            assert_eq!(super::refuse_studio_kind(k), Ok(()), "{}", k.slug());
        }
    }

    #[test]
    fn video_links_may_be_transcribed_and_nothing_else_is() {
        use crate::ingest::urls::classify;
        for url in [
            "https://www.tiktok.com/@tonsr_/video/7696048386981645569",
            "https://vt.tiktok.com/ZSabc123/",
            "https://www.instagram.com/reel/DeeyrDqyckk/",
            "https://www.facebook.com/reel/1120404190712037/",
            "https://x.com/someone/status/2111113436314648758",
        ] {
            assert!(super::may_transcribe(&classify(url)), "{url}");
        }
        // The fetched page decides what a Facebook link is (og:video), not its shape.
        assert!(super::may_transcribe(&classify("https://www.facebook.com/61574743782103/posts/122100000000000000")));
        for url in [
            "https://www.youtube.com/watch?v=jXtnhyro-QE",
            "https://example.com/article",
            "https://www.instagram.com/someone/",
        ] {
            assert!(!super::may_transcribe(&classify(url)), "{url}");
        }
    }

    #[test]
    fn ingest_sources_reads_speech_to_text_once_off_the_runtime_before_delegating() {
        let body = crate::test_support::extract_fn_body(include_str!("ingest_commands.rs"), "async fn ingest_sources(");
        let ready = body.find("spawn_blocking(stt_config::ready)").expect("ready() runs on the blocking pool");
        assert_eq!(body.matches("stt_config::ready").count(), 1, "once per run, never per source");
        assert!(body.contains("may_transcribe(&crate::ingest::urls::classify(u))"), "the gate is the analysis's own rule");
        assert!(ready < body.find("run_ingest_command(").unwrap());
        let analyze = crate::test_support::extract_fn_body(include_str!("ingest_commands.rs"), "fn analyze_ingest_sources(");
        assert!(!analyze.contains("stt_config::ready"), "the analysis must not reach the keychain (gotcha #15)");
    }

    #[test]
    fn ingest_sources_dedupes_canonical_urls_before_the_count_check() {
        let src = include_str!("ingest_commands.rs");
        let body = crate::test_support::extract_fn_body(src, "async fn ingest_sources(");
        let dedupe = body.find("canonical_unique(&urls)").expect("must canonicalise and dedupe the links");
        let cap = body.find("MAX_URLS_PER_INGEST").expect("must cap the link count");
        assert!(dedupe < cap, "two shares of one post must not count twice against the cap");
    }

    #[test]
    fn workflow_commands_route_clean_transcript_through_the_chunked_loop() {
        let src = include_str!("ai_commands.rs");
        for f in ["async fn run_workflow_note_into(", "async fn append_llm_workflow_note("] {
            let body = crate::test_support::extract_fn_body(src, f);
            assert!(body.contains("run_workflow_or_clean("), "{f} must use run_workflow_or_clean");
            assert!(!body.contains("provider.run_workflow("), "{f} must not bypass the chunked loop");
            let check = body.find("check_attempts(").expect("pre-checks the attempt budget");
            assert!(check < body.find("run_workflow_or_clean(").unwrap());
        }
    }
}

#[cfg(test)]
mod ask_note_scope_wiring_tests {
    // Commands can't be invoked in tests (no `State` harness), so these read
    // the source, like `allow_ai_for_account_decides_before_it_invalidates`.
    // Signatures are split so this module's own text never matches them.

    /// A note scope's preflight walks up to 400 related notes; a sync command
    /// runs it on the main thread and freezes the window, on every checkbox
    /// or Depth change (whole-branch review, 2026-10-24).
    #[test]
    fn ask_commands_that_walk_are_async() {
        assert!(include_str!("ai_commands.rs").contains(concat!("pub async fn begin", "_ask(")));
        assert!(include_str!("ask_history_commands.rs").contains(concat!("pub async fn resume", "_ask(")));
    }

    /// Review Focus 1: a note deleted, or moved into a hidden folder, between
    /// turns is refused before any provider is resolved.
    #[test]
    fn ask_jodd_rechecks_the_note_before_resolving_a_provider() {
        let body = crate::test_support::extract_fn_body(include_str!("ai_commands.rs"), concat!("async fn ask", "_jodd("));
        let check = body.find("check_note_scope(").expect("ask_jodd re-checks a note scope");
        let provider = body.find("resolve_app_provider(").expect("ask_jodd resolves a provider");
        assert!(check < provider);
    }
}

/// Redo key points from a derived note, a source note and a legacy note
/// (spec 2026-11-19 §5.5, §5.7, §6). `re_extract_note` has no `State`
/// harness, so its decision is `plan_re_extract`, tested here on a real DB.
#[cfg(test)]
mod re_extract_plan_tests {
    use super::{plan_re_extract, ReExtractPlan};
    use crate::ingest::run::{IngestDestination, IngestInput, LinkedSource};
    use crate::ingest::source_note::{content_hash, derived_from_line, render_source_note, SourceHeader, Validators};
    use crate::ingest::{FetchStatus, FetchedSource, SourceKind};
    use crate::llm::markdown::{assemble_ingest_body, IngestTail, SourceLine};
    use crate::privacy::ai_scope::{AiScope, HIDDEN_NOTE};
    use crate::test_support::{note, temp_db};

    const ACCT: &str = "gmail:a@example.com";
    const SOURCES: &str = "Notes/__Sources__";
    const S1: &str = "A1B2C3D4-1111-2222-3333-444444444444";
    const S2: &str = "B1B2C3D4-1111-2222-3333-444444444444";
    const DERIVED: &str = "C1B2C3D4-1111-2222-3333-444444444444";
    const URL1: &str = "https://one.example/a";
    const URL2: &str = "https://two.example/b?x=1&y=2";
    const FAILED: &str = "https://failed.example/";

    fn fetched(url: &str, title: &str, text: &str) -> FetchedSource {
        FetchedSource { url: url.into(), kind: SourceKind::Web, title: Some(title.into()), text: text.into(), status: FetchStatus::Ok }
    }

    fn header(text: &str) -> SourceHeader {
        SourceHeader { fetched_at: "2026-11-19 14:05".into(), sha256: content_hash(text), with: vec![], validators: Validators::default() }
    }

    /// A source note as ingest writes it; returns what a derivation links to.
    fn source_note(db: &crate::db::Db, uuid: &str, label: &str, src: &FetchedSource) -> LinkedSource {
        let h = header(&src.text);
        let (title, body) = render_source_note(src, &h);
        note(ACCT, uuid).title(&title).label(label).body(&body).insert(db);
        LinkedSource { uuid: uuid.into(), title, header: h, source: src.clone() }
    }

    /// A derived note as ingest writes it: the Sources list (failures
    /// included) and one `Derived from` line per source note.
    fn derived_note(db: &crate::db::Db, label: &str, sources: &[(&str, &str)], linked: &[&LinkedSource]) -> crate::db::CachedNote {
        let envelope = crate::llm::provider::ExtractEnvelope {
            title: None,
            lessons_markdown: "- a point".into(),
            meta_lessons_markdown: None,
            tags: vec![],
            confidence: None,
        };
        let lines: Vec<SourceLine> =
            sources.iter().map(|(url, status)| SourceLine { url: url.to_string(), title: Some(format!("T {url}")), status: status.to_string() }).collect();
        let tail = IngestTail::Derived(linked.iter().map(|l| derived_from_line(&l.title, &l.uuid, &l.header)).collect());
        let body = assemble_ingest_body(&envelope, None, &lines, tail, crate::db::tag_model_for_account(ACCT));
        note(ACCT, DERIVED).title("Key points").label(label).body(&body).insert(db);
        db.get(DERIVED, ACCT).unwrap().unwrap()
    }

    fn plan(db: &crate::db::Db, scope: &AiScope, n: &crate::db::CachedNote) -> Result<ReExtractPlan, String> {
        plan_re_extract(db, scope, ACCT, n)
    }

    fn gone(url: &str) -> String {
        format!("The saved copy of {url} is gone. Ingest the link again to make a new one.")
    }

    #[test]
    fn a_derived_note_derives_again_from_its_source_notes_beside_itself() {
        let db = temp_db();
        let l1 = source_note(&db, S1, SOURCES, &fetched(URL1, "One", "first text"));
        let l2 = source_note(&db, S2, SOURCES, &fetched(URL2, "Two", "second text"));
        let d = derived_note(&db, "Notes/Research", &[(URL1, "ok"), (URL2, "ok")], &[&l1, &l2]);
        match plan(&db, &AiScope::default(), &d).unwrap() {
            ReExtractPlan::Ingest(IngestInput::SourceNotes(got), IngestDestination::BesideSource(label)) => {
                assert_eq!(got, vec![l1, l2]);
                assert_eq!(label, "Notes/Research", "beside the derived note, as today");
            }
            _ => panic!("a derived note must derive again from its source notes"),
        }
    }

    #[test]
    fn a_derived_note_follows_a_rekeyed_source_note() {
        let db = temp_db();
        let l1 = source_note(&db, S1, SOURCES, &fetched(URL1, "One", "first text"));
        let d = derived_note(&db, "Notes/Research", &[(URL1, "ok")], &[&l1]);
        // The worker rekeyed the source note at CREATE (gotcha #16).
        let new_uuid = "D1B2C3D4-9999-2222-3333-444444444444";
        db.rekey_note_uuid(S1, new_uuid, ACCT).unwrap();
        match plan(&db, &AiScope::default(), &d).unwrap() {
            ReExtractPlan::Ingest(IngestInput::SourceNotes(got), _) => {
                assert_eq!(got.len(), 1);
                assert_eq!(got[0].uuid, new_uuid);
                assert_eq!(got[0].source.text, "first text");
            }
            _ => panic!("expected SourceNotes"),
        }
    }

    #[test]
    fn a_source_note_derives_into_the_inbox_never_beside_itself() {
        let db = temp_db();
        let l1 = source_note(&db, S1, SOURCES, &fetched(URL1, "One", "first text"));
        let n = db.get(S1, ACCT).unwrap().unwrap();
        match plan(&db, &AiScope::default(), &n).unwrap() {
            ReExtractPlan::Ingest(IngestInput::SourceNotes(got), IngestDestination::Resolve) => assert_eq!(got, vec![l1]),
            ReExtractPlan::Ingest(_, IngestDestination::BesideSource(l)) => panic!("must not land beside the source note in {l}"),
            _ => panic!("a source note must derive from itself, into the Inbox"),
        }
    }

    #[test]
    fn a_missing_source_note_names_its_link_and_does_not_fetch() {
        let db = temp_db();
        let l1 = source_note(&db, S1, SOURCES, &fetched(URL1, "One", "first text"));
        let l2 = source_note(&db, S2, SOURCES, &fetched(URL2, "Two", "second text"));
        // A failed source sits between them in the Sources list and has no line.
        let d = derived_note(&db, "Notes/Research", &[(URL1, "ok"), (FAILED, "failed: HTTP 404"), (URL2, "ok")], &[&l1, &l2]);
        db.mark_deleted(S2, ACCT).unwrap();
        assert_eq!(plan(&db, &AiScope::default(), &d).err().unwrap(), gone(URL2));
    }

    /// iCloud/SSH give the body back without markup: the Sources list's
    /// `href`s are gone, so the message names the link by its key.
    #[test]
    fn a_missing_source_note_in_a_synced_back_body_is_named_by_its_link() {
        let db = temp_db();
        let l1 = source_note(&db, S1, SOURCES, &fetched(URL1, "One", "first text"));
        let d = derived_note(&db, "Notes/Research", &[(URL1, "ok")], &[&l1]);
        let plain = crate::db::CachedNote { body_html: crate::ingest::source_note::plain_form(&d.body_html), ..d };
        assert!(crate::ingest::source_note::is_derived(&plain.body_html), "the line survives as text");
        db.mark_deleted(S1, ACCT).unwrap();
        let key = crate::db::note_slug(&l1.title, S1);
        assert_eq!(plan(&db, &AiScope::default(), &plain).err().unwrap(), gone(&key));
    }

    #[test]
    fn a_link_to_a_note_that_is_not_a_source_note_is_missing_too() {
        let db = temp_db();
        let l1 = source_note(&db, S1, SOURCES, &fetched(URL1, "One", "first text"));
        let d = derived_note(&db, "Notes/Research", &[(URL1, "ok")], &[&l1]);
        // The user rewrote the source note: no marker, no block.
        db.apply_local_edit(S1, ACCT, "Source — One", "<div>Source — One</div><div>my own words</div>", SOURCES, &crate::provenance::Actor::test()).unwrap();
        assert_eq!(plan(&db, &AiScope::default(), &d).err().unwrap(), gone(URL1));
    }

    /// Ruling 14 (I4): a Microsoft source note rekeyed on another device has
    /// no alias here, so its `[[…-uuid8]]` link resolves to nothing. The saved
    /// copy of the Sources list's URL at that position stands in — only when
    /// its sha256 is the line's.
    #[test]
    fn a_link_rekeyed_elsewhere_falls_back_to_the_saved_copy_with_the_same_hash() {
        let db = temp_db();
        // The derived note was written against S1; this device only has the
        // source note under the uuid the other device's push gave it.
        let l1 = LinkedSource { uuid: S1.into(), ..source_note(&db, S2, SOURCES, &fetched(URL2, "Two", "second text")) };
        let d = derived_note(&db, "Notes/Research", &[(URL2, "ok")], &[&l1]);
        assert!(db.resolve_note_link(ACCT, &crate::db::note_slug(&l1.title, S1)).unwrap().is_none(), "the link itself is dead here");
        match plan(&db, &AiScope::default(), &d).unwrap() {
            ReExtractPlan::Ingest(IngestInput::SourceNotes(got), _) => {
                assert_eq!(got.len(), 1);
                assert_eq!((got[0].uuid.as_str(), got[0].source.text.as_str()), (S2, "second text"));
            }
            _ => panic!("expected SourceNotes"),
        }
    }

    #[test]
    fn a_fallback_copy_with_another_hash_is_not_the_one_derived_from() {
        let db = temp_db();
        let l1 = source_note(&db, S2, SOURCES, &fetched(URL2, "Two", "second text, since updated"));
        let derived_from = LinkedSource { uuid: S1.into(), header: header("second text"), ..l1 };
        let d = derived_note(&db, "Notes/Research", &[(URL2, "ok")], &[&derived_from]);
        assert_eq!(plan(&db, &AiScope::default(), &d).err().unwrap(), gone(URL2));
    }

    #[test]
    fn a_source_note_in_a_hidden_folder_is_refused() {
        let db = temp_db();
        let l1 = source_note(&db, S1, SOURCES, &fetched(URL1, "One", "first text"));
        let d = derived_note(&db, "Notes/Research", &[(URL1, "ok")], &[&l1]);
        let scope = AiScope::hiding(ACCT, &[SOURCES]);
        assert_eq!(plan(&db, &scope, &d).err().unwrap(), HIDDEN_NOTE, "from the derived note");
        let n = db.get(S1, ACCT).unwrap().unwrap();
        assert_eq!(plan(&db, &scope, &n).err().unwrap(), HIDDEN_NOTE, "from the source note itself");
    }

    /// Owner's override of Ruling 15: Redo from a source note writes to the
    /// Inbox, and the one destination check every ingest passes through
    /// (`run_ingest_command` → `check_destination_scope`) refuses a hidden one.
    #[test]
    fn redo_from_a_source_note_refuses_when_the_inbox_is_hidden() {
        use crate::ingest::run::check_destination_scope;
        let db = temp_db();
        source_note(&db, S1, SOURCES, &fetched(URL1, "One", "first text"));
        let n = db.get(S1, ACCT).unwrap().unwrap();
        let scope = AiScope::hiding(ACCT, &[crate::llm::filing::INBOX_PATH]);
        let ReExtractPlan::Ingest(_, destination) = plan(&db, &scope, &n).expect("the source note itself is visible") else {
            panic!("a source note derives through ingest")
        };
        assert_eq!(check_destination_scope(&db, &scope, ACCT, &destination, true).err().as_deref(), Some(HIDDEN_NOTE));
        // The Inbox exists already: the same.
        db.create_folder_local_new(ACCT, crate::llm::filing::INBOX_PATH).unwrap();
        assert_eq!(check_destination_scope(&db, &scope, ACCT, &destination, true).err().as_deref(), Some(HIDDEN_NOTE));
        // Hiding something else leaves it allowed.
        assert!(check_destination_scope(&db, &AiScope::hiding(ACCT, &["Notes/Private"]), ACCT, &destination, true).is_ok());
    }

    #[test]
    fn a_legacy_inline_note_runs_as_before() {
        let db = temp_db();
        let block = crate::ingest::stored::render_sources(&[fetched(URL1, "One", "inline text")]);
        let body = format!("<p>points</p>{}", crate::llm::markdown::source_block(&block));
        note(ACCT, DERIVED).title("Legacy").label("Notes/Old").body(&body).insert(&db);
        let n = db.get(DERIVED, ACCT).unwrap().unwrap();
        match plan(&db, &AiScope::default(), &n).unwrap() {
            ReExtractPlan::Ingest(IngestInput::Stored(s), IngestDestination::BesideSource(l)) => {
                assert_eq!((s.len(), s[0].text.as_str(), l.as_str()), (1, "inline text", "Notes/Old"));
            }
            _ => panic!("a legacy inline note keeps the Stored path"),
        }
        let extract = format!("<p>lessons</p>{}", crate::llm::markdown::source_block("a pasted transcript"));
        note(ACCT, S1).title("Extract").label("Notes/Old").body(&extract).insert(&db);
        let n = db.get(S1, ACCT).unwrap().unwrap();
        assert!(matches!(plan(&db, &AiScope::default(), &n).unwrap(), ReExtractPlan::Extract(s) if s.contains("a pasted transcript")));
    }
}
