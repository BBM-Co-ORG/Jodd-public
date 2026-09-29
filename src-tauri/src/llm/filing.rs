//! Where does a note belong? — the same kind of question `autolink.rs`
//! answers for links. Extract's destination (`resolve_destination`,
//! `destination_beside`) and the LLM folder proposal (`suggest_folder`).
//!
//! See docs/superpowers/specs/2026-09-15-extract-filing-design.md.

use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::db::{Db, FolderSyncState};
use crate::llm::provider::{ExtractError, LlmProvider};

/// Where new extracts land. An ORDINARY folder (kind `user`): it shows as
/// "Inbox" on an iPhone and its backlog is the sidebar's folder count
/// (spec Decision 2). Never `__Inbox__`.
pub const INBOX_PATH: &str = "Notes/Inbox";

/// The account's Notes root — writable on every backend, on Microsoft by
/// well-known name even when undiscoverable (gotcha #12).
///
/// **Hardcoded, like the five folder-side sites gotcha #9 lists.** Roadmap
/// #0c must route this module through `effective_notes_label()` too.
pub const NOTES_ROOT: &str = "Notes";

/// Destination for a new extract note:
///   1. `Notes/Inbox` exists and is not `deleted_pending` → it;
///   2. else, if the backend can write folders → create it (kind `user`);
///   3. else → the root.
///
/// A `deleted_pending` Inbox goes to step 3, not step 2 — see the test
/// `an_inbox_pending_deletion_falls_through_to_the_root_not_to_create`.
pub fn resolve_destination(db: &Db, account_id: &str, can_create_folders: bool) -> rusqlite::Result<String> {
    match db.folder_sync_state(account_id, INBOX_PATH)? {
        Some(FolderSyncState::DeletedPending) => return Ok(NOTES_ROOT.to_string()),
        Some(_) => return Ok(INBOX_PATH.to_string()),
        None => {}
    }
    if can_create_folders {
        db.create_folder_local_new(account_id, INBOX_PATH)?;
        return Ok(INBOX_PATH.to_string());
    }
    Ok(NOTES_ROOT.to_string())
}

/// Destination for a Re-extract: beside its source note, so the two can be
/// compared (spec Decision 5) — unless that folder is gone or going, in
/// which case `resolve_destination`. The root always exists.
pub fn destination_beside(
    db: &Db,
    account_id: &str,
    source_label: &str,
    can_create_folders: bool,
) -> rusqlite::Result<String> {
    if source_label == NOTES_ROOT {
        return Ok(NOTES_ROOT.to_string());
    }
    match db.folder_sync_state(account_id, source_label)? {
        Some(state) if state != FolderSyncState::DeletedPending => Ok(source_label.to_string()),
        _ => resolve_destination(db, account_id, can_create_folders),
    }
}

/// How much of the note the model reads. Characters, not bytes.
const NOTE_TEXT_LIMIT: usize = 6_000;

/// What `suggest_folder` concluded. Four outcomes, not an `Option`, because
/// the explicit "Suggest folder" action words "no folders yet" and "nothing
/// fits" differently — and the automatic caller shows neither.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FolderSuggestionOutcome {
    Suggested {
        /// The note's CURRENT uuid — may differ from the uuid `suggest_folder`
        /// was asked about, when a backend rekeyed the note mid-call (a
        /// create push assigning remote identity and rewriting the SQLite
        /// primary key, gotcha #16). The frontend must store the chip under
        /// THIS uuid, not the one it asked about, or the chip never appears
        /// against the note the editor now shows.
        uuid: String,
        path: String,
        reason: Option<String>,
    },
    /// The account has no folder a note could be filed into — no LLM call.
    NoCandidates,
    /// `null`, a path that was not offered, or the note's current folder.
    NoneFits,
    NoteNotFound,
}

/// Folders a note may be filed into: the account's folders minus the ones
/// that are pending deletion, Jodd-managed (`system_workflow` — including
/// `jodd-mcp`'s `__Claude__`), the Inbox itself, and the root.
pub fn candidate_folders(db: &Db, account_id: &str) -> rusqlite::Result<Vec<String>> {
    Ok(db
        .list_folders(account_id)?
        .into_iter()
        .filter(|f| f.sync_state != FolderSyncState::DeletedPending)
        .filter(|f| f.kind != "system_workflow")
        .filter(|f| f.path != INBOX_PATH && f.path != NOTES_ROOT)
        .map(|f| f.path)
        .collect())
}

fn db_error(e: rusqlite::Error) -> ExtractError {
    ExtractError::Transport(format!("local database: {e}"))
}

/// Propose one EXISTING folder for note `uuid`. The model's answer is never
/// trusted: it must equal a path offered here, and differ from the note's
/// current label (spec Decision 3). A rejected answer is logged.
pub async fn suggest_folder(
    provider: &dyn LlmProvider,
    db: &Db,
    account_id: &str,
    uuid: &str,
    cancel: CancellationToken,
) -> Result<FolderSuggestionOutcome, ExtractError> {
    // The caller may be holding a uuid a backend has since rekeyed (a create
    // push assigning remote identity, gotcha #16) — follow the forwarding
    // address before the read, or a note that moved seconds ago reads as
    // gone.
    provider.check()?;
    super::receipts::stage("suggesting_folder");
    let resolved = db.resolve_note_uuid(uuid, account_id).map_err(db_error)?;
    let Some(note) = db.get(&resolved, account_id).map_err(db_error)? else {
        return Ok(FolderSuggestionOutcome::NoteNotFound);
    };
    super::receipts::source_version(format!("{}:{}:{}", account_id, resolved, note.local_version).as_bytes());
    let folders = candidate_folders(db, account_id).map_err(db_error)?;
    if folders.is_empty() {
        return Ok(FolderSuggestionOutcome::NoCandidates);
    }

    // Finding F1: never send the `## Sources` list or the verbatim Source
    // block to a provider — see `markdown::text_for_suggestions`.
    let visible_body = crate::llm::markdown::text_for_suggestions(&note.body_html);
    let full = format!("{}\n\n{}", note.title, crate::db::strip_html_to_text(visible_body));
    let note_text: String = full.chars().take(NOTE_TEXT_LIMIT).collect();

    super::receipts::source_version(&serde_json::to_vec(&folders).unwrap_or_default());
    let envelope = provider.suggest_folder(&note_text, &folders, cancel).await?;
    super::receipts::check("folder_candidate_membership_checked");
    let Some(path) = envelope.folder else {
        return Ok(FolderSuggestionOutcome::NoneFits);
    };
    if !folders.iter().any(|f| f == &path) {
        crate::log!("suggest_folder: rejected '{path}' — not among the {} offered folders", folders.len());
        return Ok(FolderSuggestionOutcome::NoneFits);
    }
    if path == note.label {
        crate::log!("suggest_folder: rejected '{path}' — the note is already there");
        return Ok(FolderSuggestionOutcome::NoneFits);
    }
    let reason = envelope.reason.filter(|r| !r.trim().is_empty());
    // The rekey may have happened WHILE the provider call was in flight —
    // resolve once more so the outcome is stamped with the uuid the note
    // lives under now, not the one it lived under when the call started.
    let current = db.resolve_note_uuid(&resolved, account_id).map_err(db_error)?;
    Ok(FolderSuggestionOutcome::Suggested { uuid: current, path, reason })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::FolderSyncState;
    use crate::llm::provider::{
        CandidateSummary, ChatTurn, ExtractEnvelope, ExtractError, FolderSuggestionEnvelope,
        LinkSuggestionsEnvelope, LlmProvider, SourceDigest,
    };
    use crate::test_support::temp_db;
    use std::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    const ACCT: &str = "gmail:test@example.com";

    fn folder_row(db: &Db, path: &str) -> Option<crate::db::CachedFolder> {
        db.list_folders(ACCT).unwrap().into_iter().find(|f| f.path == path)
    }

    #[test]
    fn an_existing_inbox_is_used() {
        let db = temp_db();
        db.create_folder_local_new(ACCT, INBOX_PATH).unwrap();
        assert_eq!(resolve_destination(&db, ACCT, false).unwrap(), INBOX_PATH);
    }

    #[test]
    fn an_absent_inbox_is_created_as_an_ordinary_user_folder() {
        let db = temp_db();
        assert_eq!(resolve_destination(&db, ACCT, true).unwrap(), INBOX_PATH);
        let row = folder_row(&db, INBOX_PATH).expect("Inbox row created");
        assert_eq!(row.sync_state, FolderSyncState::DirtyNew);
        assert_eq!(row.kind, "user", "Inbox is not a workflow folder (spec Decision 2)");
    }

    /// Outlook: folder writes are permanently false (gotcha #12), and the
    /// root is writable by well-known name — so the root, and no row.
    #[test]
    fn an_absent_inbox_on_a_backend_that_cannot_create_folders_is_the_root() {
        let db = temp_db();
        assert_eq!(resolve_destination(&db, ACCT, false).unwrap(), NOTES_ROOT);
        assert!(folder_row(&db, INBOX_PATH).is_none(), "no unpushable folder row");
    }

    /// `create_folder_local_new`'s ON CONFLICT DO NOTHING would leave a
    /// pending-deletion row in place and file the note into a folder about
    /// to disappear.
    #[test]
    fn an_inbox_pending_deletion_falls_through_to_the_root_not_to_create() {
        let db = temp_db();
        db.create_folder_local_new(ACCT, INBOX_PATH).unwrap();
        db.force_folder_sync_state(ACCT, INBOX_PATH, FolderSyncState::DeletedPending);
        assert_eq!(resolve_destination(&db, ACCT, true).unwrap(), NOTES_ROOT);
    }

    #[test]
    fn re_extract_lands_beside_its_source() {
        let db = temp_db();
        db.create_folder_local_new(ACCT, "Notes/Research").unwrap();
        assert_eq!(destination_beside(&db, ACCT, "Notes/Research", true).unwrap(), "Notes/Research");
        assert_eq!(destination_beside(&db, ACCT, NOTES_ROOT, true).unwrap(), NOTES_ROOT);
    }

    #[test]
    fn re_extract_from_a_folder_that_is_gone_resolves_the_destination() {
        let db = temp_db();
        assert_eq!(destination_beside(&db, ACCT, "Notes/Vanished", true).unwrap(), INBOX_PATH);

        // Fresh db: the assertion above creates `Notes/Inbox` as a side
        // effect of falling through to `resolve_destination`, and that
        // would leak into this second scenario (a `deleted_pending`
        // source with `can_create_folders: false`) if reused — Inbox
        // would already exist and get returned instead of the root.
        let db = temp_db();
        db.create_folder_local_new(ACCT, "Notes/Leaving").unwrap();
        db.force_folder_sync_state(ACCT, "Notes/Leaving", FolderSyncState::DeletedPending);
        assert_eq!(destination_beside(&db, ACCT, "Notes/Leaving", false).unwrap(), NOTES_ROOT);
    }

    /// Answers every `suggest_folder` with `folder`, and records each call so
    /// a test can prove the provider was NOT called.
    struct CountingProvider {
        folder: Option<String>,
        calls: Mutex<Vec<(String, Vec<String>)>>,
    }

    impl CountingProvider {
        fn answering(folder: Option<&str>) -> Self {
            CountingProvider { folder: folder.map(String::from), calls: Mutex::new(Vec::new()) }
        }
        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for CountingProvider {
        async fn extract(&self, _s: &str, _t: &[String], _c: CancellationToken) -> Result<ExtractEnvelope, ExtractError> {
            unreachable!("filing never calls extract")
        }
        async fn run_workflow(
            &self,
            _w: crate::llm::provider::WorkflowKind,
            _s: &str,
            _t: &[String],
            _c: CancellationToken,
        ) -> Result<ExtractEnvelope, ExtractError> {
            unreachable!("filing never calls run_workflow")
        }
        async fn suggest_links(
            &self,
            _s: &str,
            _c: &[CandidateSummary],
            _t: CancellationToken,
        ) -> Result<LinkSuggestionsEnvelope, ExtractError> {
            unreachable!("filing never calls suggest_links")
        }
        async fn suggest_folder(
            &self,
            note_text: &str,
            folders: &[String],
            _c: CancellationToken,
        ) -> Result<FolderSuggestionEnvelope, ExtractError> {
            self.calls.lock().unwrap().push((note_text.to_string(), folders.to_vec()));
            Ok(FolderSuggestionEnvelope { folder: self.folder.clone(), reason: Some("fits".into()) })
        }
        async fn synthesize(&self, _d: &[SourceDigest], _c: &str, _t: CancellationToken) -> Result<ExtractEnvelope, ExtractError> {
            unreachable!("filing never calls synthesize")
        }
        async fn chat(&self, _s: &str, _t: &[ChatTurn], _c: CancellationToken) -> Result<String, ExtractError> {
            unreachable!("filing never calls chat")
        }
    }

    const NOTE_UUID: &str = "AAAAAAAA-0000-0000-0000-000000000001";

    /// An account with an Inbox note and two user folders to choose from.
    fn db_with_note_in_inbox() -> Db {
        let db = temp_db();
        db.create_folder_local_new(ACCT, INBOX_PATH).unwrap();
        db.create_folder_local_new(ACCT, "Notes/Trading").unwrap();
        db.create_folder_local_new(ACCT, "Notes/Health").unwrap();
        crate::test_support::note(ACCT, NOTE_UUID)
            .title("Settrade order types")
            .label(INBOX_PATH)
            .insert(&db);
        db
    }

    #[test]
    fn candidates_exclude_pending_deletion_workflow_inbox_and_root() {
        let db = temp_db();
        db.create_folder_local_new(ACCT, INBOX_PATH).unwrap();
        db.create_folder_local_new(ACCT, "Notes/Trading").unwrap();
        db.create_folder_local_new(ACCT, "Notes/Leaving").unwrap();
        db.force_folder_sync_state(ACCT, "Notes/Leaving", FolderSyncState::DeletedPending);
        db.ensure_workflow_folder(ACCT, "__Claude__").unwrap();
        db.create_folder_local_new(ACCT, NOTES_ROOT).unwrap();

        assert_eq!(candidate_folders(&db, ACCT).unwrap(), vec!["Notes/Trading".to_string()]);
    }

    #[tokio::test]
    async fn a_path_it_offered_is_suggested() {
        let db = db_with_note_in_inbox();
        let p = CountingProvider::answering(Some("Notes/Trading"));
        let out = suggest_folder(&p, &db, ACCT, NOTE_UUID, CancellationToken::new()).await.unwrap();
        assert_eq!(
            out,
            FolderSuggestionOutcome::Suggested {
                uuid: NOTE_UUID.into(),
                path: "Notes/Trading".into(),
                reason: Some("fits".into()),
            }
        );
        let (text, folders) = p.calls.lock().unwrap()[0].clone();
        assert!(text.starts_with("Settrade order types"), "title leads the note text: {text}");
        assert_eq!(folders, vec!["Notes/Health".to_string(), "Notes/Trading".to_string()]);
    }

    #[tokio::test]
    async fn a_path_it_did_not_offer_is_rejected() {
        let db = db_with_note_in_inbox();
        let p = CountingProvider::answering(Some("Notes/Invented"));
        let out = suggest_folder(&p, &db, ACCT, NOTE_UUID, CancellationToken::new()).await.unwrap();
        assert_eq!(out, FolderSuggestionOutcome::NoneFits);
    }

    #[tokio::test]
    async fn the_folder_the_note_is_already_in_is_not_a_suggestion() {
        let db = db_with_note_in_inbox();
        db.move_notes_batch(ACCT, &[NOTE_UUID.to_string()], "Notes/Trading").unwrap();
        let p = CountingProvider::answering(Some("Notes/Trading"));
        let out = suggest_folder(&p, &db, ACCT, NOTE_UUID, CancellationToken::new()).await.unwrap();
        assert_eq!(out, FolderSuggestionOutcome::NoneFits);
    }

    #[tokio::test]
    async fn a_null_answer_is_none_fits() {
        let db = db_with_note_in_inbox();
        let p = CountingProvider::answering(None);
        let out = suggest_folder(&p, &db, ACCT, NOTE_UUID, CancellationToken::new()).await.unwrap();
        assert_eq!(out, FolderSuggestionOutcome::NoneFits);
    }

    #[tokio::test]
    async fn no_candidate_folders_means_no_provider_call() {
        let db = temp_db();
        crate::test_support::note(ACCT, NOTE_UUID).insert(&db);
        let p = CountingProvider::answering(Some("Notes/Trading"));
        let out = suggest_folder(&p, &db, ACCT, NOTE_UUID, CancellationToken::new()).await.unwrap();
        assert_eq!(out, FolderSuggestionOutcome::NoCandidates);
        assert_eq!(p.call_count(), 0, "an empty candidate list must not spend an LLM call");
    }

    #[tokio::test]
    async fn a_missing_note_is_reported_without_a_provider_call() {
        let db = db_with_note_in_inbox();
        let p = CountingProvider::answering(Some("Notes/Trading"));
        let out = suggest_folder(&p, &db, ACCT, "no-such-uuid", CancellationToken::new()).await.unwrap();
        assert_eq!(out, FolderSuggestionOutcome::NoteNotFound);
        assert_eq!(p.call_count(), 0);
    }

    /// Chars, not bytes: a byte cut would split a Thai character and panic.
    #[tokio::test]
    async fn the_note_text_is_cut_at_six_thousand_characters() {
        let db = db_with_note_in_inbox();
        let long = "ก".repeat(10_000);
        db.apply_local_edit(NOTE_UUID, ACCT, "T", &format!("<div>T</div><div>{long}</div>"), INBOX_PATH)
            .unwrap();
        let p = CountingProvider::answering(None);
        suggest_folder(&p, &db, ACCT, NOTE_UUID, CancellationToken::new()).await.unwrap();
        assert_eq!(p.calls.lock().unwrap()[0].0.chars().count(), 6_000);
    }

    /// Finding F1: `suggest_folder` must never send the `## Sources` list
    /// (full URLs, query strings and all) or the verbatim Source block to a
    /// provider — only the note text a human sees before them.
    #[tokio::test]
    async fn the_note_text_excludes_the_sources_list_and_source_block() {
        let db = db_with_note_in_inbox();
        let body = "<p>Real visible content.</p>\n\
            <h2>Sources</h2>\n<ul><li><a href=\"https://e.example/?token=SECRET\">SECRET label</a> — ok</li></ul>\n\
            <hr>\n<details>\n<summary>Source (verbatim)</summary>\n<pre>SECRET stored body text</pre>\n</details>\n";
        db.apply_local_edit(NOTE_UUID, ACCT, "Settrade order types", body, INBOX_PATH).unwrap();
        let p = CountingProvider::answering(Some("Notes/Trading"));
        suggest_folder(&p, &db, ACCT, NOTE_UUID, CancellationToken::new()).await.unwrap();
        let (text, _) = p.calls.lock().unwrap()[0].clone();
        assert!(text.contains("Real visible content"), "{text}");
        assert!(!text.contains("SECRET"), "{text}");
    }

    #[test]
    fn the_outcome_serializes_with_a_kind_tag() {
        let s = serde_json::to_value(FolderSuggestionOutcome::Suggested {
            uuid: NOTE_UUID.into(),
            path: "Notes/Trading".into(),
            reason: None,
        })
        .unwrap();
        assert_eq!(
            s,
            serde_json::json!({"kind": "suggested", "uuid": NOTE_UUID, "path": "Notes/Trading", "reason": null})
        );
        assert_eq!(
            serde_json::to_value(FolderSuggestionOutcome::NoCandidates).unwrap(),
            serde_json::json!({"kind": "no_candidates"})
        );
    }

    /// gotcha #16: a Microsoft create push rekeys the note's PRIMARY KEY
    /// before `suggest_folder` is ever called. Asking about the OLD uuid
    /// must still resolve to the live row and stamp the outcome with the
    /// NEW uuid — the one the editor now shows the note under.
    #[tokio::test]
    async fn a_rekey_that_already_happened_is_followed_to_the_live_uuid() {
        let db = db_with_note_in_inbox();
        const NEW_UUID: &str = "<msg1@exchange.com>";
        db.rekey_note_uuid(NOTE_UUID, NEW_UUID, ACCT).unwrap();

        let p = CountingProvider::answering(Some("Notes/Trading"));
        let out = suggest_folder(&p, &db, ACCT, NOTE_UUID, CancellationToken::new()).await.unwrap();
        assert_eq!(
            out,
            FolderSuggestionOutcome::Suggested {
                uuid: NEW_UUID.into(),
                path: "Notes/Trading".into(),
                reason: Some("fits".into()),
            }
        );
        assert_eq!(p.call_count(), 1, "the provider must still be called for the (now-resolved) note");
    }

    /// A provider whose `suggest_folder` call performs a rekey against the
    /// same `Db` mid-call — the sync worker's 5s tick landing while the LLM
    /// round trip is still in flight. The outcome must carry the uuid the
    /// note lives under AFTER the call, not the one it started under.
    struct RekeyingProvider<'a> {
        db: &'a Db,
        old_uuid: &'a str,
        new_uuid: &'a str,
        folder: Option<String>,
    }

    #[async_trait::async_trait]
    impl LlmProvider for RekeyingProvider<'_> {
        async fn extract(&self, _s: &str, _t: &[String], _c: CancellationToken) -> Result<ExtractEnvelope, ExtractError> {
            unreachable!("filing never calls extract")
        }
        async fn run_workflow(
            &self,
            _w: crate::llm::provider::WorkflowKind,
            _s: &str,
            _t: &[String],
            _c: CancellationToken,
        ) -> Result<ExtractEnvelope, ExtractError> {
            unreachable!("filing never calls run_workflow")
        }
        async fn suggest_links(
            &self,
            _s: &str,
            _c: &[CandidateSummary],
            _t: CancellationToken,
        ) -> Result<LinkSuggestionsEnvelope, ExtractError> {
            unreachable!("filing never calls suggest_links")
        }
        async fn suggest_folder(
            &self,
            _note_text: &str,
            _folders: &[String],
            _c: CancellationToken,
        ) -> Result<FolderSuggestionEnvelope, ExtractError> {
            // Simulates the sync worker's tick rekeying the note WHILE this
            // provider call is "in flight" — the exact race gotcha #16
            // exists for.
            self.db.rekey_note_uuid(self.old_uuid, self.new_uuid, ACCT).unwrap();
            Ok(FolderSuggestionEnvelope { folder: self.folder.clone(), reason: Some("fits".into()) })
        }
        async fn synthesize(&self, _d: &[SourceDigest], _c: &str, _t: CancellationToken) -> Result<ExtractEnvelope, ExtractError> {
            unreachable!("filing never calls synthesize")
        }
        async fn chat(&self, _s: &str, _t: &[ChatTurn], _c: CancellationToken) -> Result<String, ExtractError> {
            unreachable!("filing never calls chat")
        }
    }

    #[tokio::test]
    async fn a_rekey_during_the_provider_call_is_reflected_in_the_outcome() {
        let db = db_with_note_in_inbox();
        const NEW_UUID: &str = "<msg2@exchange.com>";
        let p = RekeyingProvider {
            db: &db,
            old_uuid: NOTE_UUID,
            new_uuid: NEW_UUID,
            folder: Some("Notes/Trading".into()),
        };

        let out = suggest_folder(&p, &db, ACCT, NOTE_UUID, CancellationToken::new()).await.unwrap();
        assert_eq!(
            out,
            FolderSuggestionOutcome::Suggested {
                uuid: NEW_UUID.into(),
                path: "Notes/Trading".into(),
                reason: Some("fits".into()),
            }
        );
    }
}
