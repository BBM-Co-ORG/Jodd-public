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

/// Destination for a new extract or capture note:
///   1. `Notes/Inbox` exists, is not `deleted_pending` and is not blocked → it;
///   2. else, if the backend can write folders → create it (kind `user`);
///   3. else → the root.
///
/// A `deleted_pending` or blocked Inbox goes to step 3, not step 2 — see the
/// tests `an_inbox_pending_deletion_falls_through_to_the_root_not_to_create`
/// and `a_blocked_inbox_falls_through_to_the_root`.
pub fn resolve_destination(db: &Db, account_id: &str, can_create_folders: bool) -> rusqlite::Result<String> {
    let path = default_destination(db, account_id, can_create_folders)?;
    if path == INBOX_PATH && db.folder_sync_state(account_id, INBOX_PATH)?.is_none() {
        db.create_folder_local_new(account_id, INBOX_PATH)?;
    }
    Ok(path)
}

/// Where `resolve_destination` would file a note, without creating anything
/// — so the capture sheet can name its default truthfully (the root, on a
/// backend that cannot create folders or behind a blocked Inbox).
pub fn default_destination(db: &Db, account_id: &str, can_create_folders: bool) -> rusqlite::Result<String> {
    match db.folder_sync_state(account_id, INBOX_PATH)? {
        Some(FolderSyncState::DeletedPending) => Ok(NOTES_ROOT.to_string()),
        Some(_) if blocked_at_or_above(&db.folders_blocking_notes(account_id)?, INBOX_PATH) => Ok(NOTES_ROOT.to_string()),
        Some(_) => Ok(INBOX_PATH.to_string()),
        None if can_create_folders => Ok(INBOX_PATH.to_string()),
        None => Ok(NOTES_ROOT.to_string()),
    }
}

/// Destination for a Re-extract: beside its source note, so the two can be
/// compared (spec Decision 5) — unless that folder is gone, going, or
/// blocked (itself or above it), in which case `resolve_destination`. The
/// root always exists.
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
        Some(state)
            if state != FolderSyncState::DeletedPending
                && !blocked_at_or_above(&db.folders_blocking_notes(account_id)?, source_label) =>
        {
            Ok(source_label.to_string())
        }
        _ => resolve_destination(db, account_id, can_create_folders),
    }
}

/// Is `path` one of `blocked` (`Db::folders_blocking_notes`) or under one? A
/// note filed there is held by its push, so it would never sync. The push
/// side pins agreement with it (`sync_worker`'s
/// `filing_and_the_note_push_agree_on_every_folder_state`).
pub(crate) fn blocked_at_or_above(blocked: &[String], path: &str) -> bool {
    blocked.iter().any(|b| crate::folder_scope::matches(path, b, crate::folder_scope::Mode::Subtree))
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
    /// It has, but every one is blocked or under a blocked folder
    /// (`Db::folders_blocking_notes`) — no LLM call, and "none yet" would be
    /// wrong. `folders` are the blocked ones that left the others out: the
    /// rows the sidebar marks "!" (a folder under one shows no marker).
    OnlyBlockedFolders { folders: Vec<String> },
    /// `null`, a path that was not offered, or the note's current folder.
    NoneFits,
    NoteNotFound,
}

/// Folders a note may be filed into: the account's folders minus the ones
/// that are pending deletion, blocking notes (`Db::folders_blocking_notes`,
/// or under such a folder), Jodd-managed (`system_workflow` — including
/// `jodd-mcp`'s `__Claude__`), the Inbox itself, and the root.
pub fn candidate_folders(
    db: &Db,
    account_id: &str,
    ai: &crate::privacy::ai_scope::AiScope,
) -> rusqlite::Result<Vec<String>> {
    Ok(candidates_and_blockers(db, account_id, ai)?.0)
}

/// Folders a person may choose for a new note from Share to Jodd: the root,
/// then `candidate_folders` (the Inbox is the sheet's default, chosen by
/// choosing nothing — `resolve_destination`). The sheet's picker lists
/// exactly these and `save_capture` refuses anything else, so the picker
/// cannot offer a folder the save would refuse.
pub fn chosen_destinations(db: &Db, account_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut paths = vec![NOTES_ROOT.to_string()];
    // The person chooses here, not an AI: a folder hidden from AI (§4.4) is
    // still theirs to file into, as it is in the sidebar.
    let person = crate::privacy::ai_scope::AiScope::default();
    paths.extend(candidate_folders(db, account_id, &person)?);
    Ok(paths)
}

/// `candidate_folders`, plus the blocked folders (`Db::folders_blocking_notes`)
/// that left an otherwise-offerable folder out — so an empty list can say
/// which of the two it means, and name where to look. Sorted.
fn candidates_and_blockers(
    db: &Db,
    account_id: &str,
    ai: &crate::privacy::ai_scope::AiScope,
) -> rusqlite::Result<(Vec<String>, Vec<String>)> {
    let blocked = db.folders_blocking_notes(account_id)?;
    let mut blockers = std::collections::BTreeSet::new();
    let mut candidates = Vec::new();
    for f in db.list_folders(account_id)? {
        if f.sync_state == FolderSyncState::DeletedPending
            || f.kind == "system_workflow"
            || f.path == INBOX_PATH
            || f.path == NOTES_ROOT
            // Spec 2026-10-08 §4.4: a hidden folder is neither offered nor
            // counted as a blocker — before the blocked/offerable split, so
            // OnlyBlockedFolders can never name one.
            || !ai.allows(account_id, &f.path)
        {
            continue;
        }
        let over: Vec<&String> = blocked
            .iter()
            .filter(|b| crate::folder_scope::matches(&f.path, b, crate::folder_scope::Mode::Subtree))
            .collect();
        if over.is_empty() {
            candidates.push(f.path);
        } else {
            blockers.extend(over.into_iter().cloned());
        }
    }
    Ok((candidates, blockers.into_iter().collect()))
}

/// What `suggest_folder` can say from SQLite alone, before any provider:
/// either the answer, or the note and folders to put to the model.
enum Preflight {
    Answer(FolderSuggestionOutcome),
    Ask { resolved: String, note: crate::db::CachedNote, folders: Vec<String> },
}

fn preflight(
    db: &Db,
    account_id: &str,
    uuid: &str,
    ai: &crate::privacy::ai_scope::AiScope,
) -> Result<Preflight, ExtractError> {
    // The caller may be holding a uuid a backend has since rekeyed (a create
    // push assigning remote identity, gotcha #16) — follow the forwarding
    // address before the read, or a note that moved seconds ago reads as
    // gone.
    let resolved = db.resolve_note_uuid(uuid, account_id).map_err(db_error)?;
    let Some(note) = db.get(&resolved, account_id).map_err(db_error)? else {
        return Ok(Preflight::Answer(FolderSuggestionOutcome::NoteNotFound));
    };
    // Spec 2026-10-08 §4.4: a note in a hidden folder is refused before
    // anything else is decided about it — and so before any provider.
    ai.check_label(account_id, &note.label).map_err(ExtractError::Refused)?;
    let (folders, blockers) = candidates_and_blockers(db, account_id, ai).map_err(db_error)?;
    if folders.is_empty() {
        return Ok(Preflight::Answer(if blockers.is_empty() {
            FolderSuggestionOutcome::NoCandidates
        } else {
            FolderSuggestionOutcome::OnlyBlockedFolders { folders: blockers }
        }));
    }
    Ok(Preflight::Ask { resolved, note, folders })
}

/// The outcomes that need no LLM — note gone, no folders, every folder
/// blocked — read from SQLite only (nothing leaves the device). `None`: the
/// question needs the model. Asked BEFORE the provider is checked
/// (`suggest_note_folder`), so "configure a provider" never hides an answer
/// that needs none.
/// A note in a hidden folder is `Err(Refused)` here too; hidden folders are
/// neither offered nor named as blockers.
pub fn answer_without_llm(
    db: &Db,
    account_id: &str,
    uuid: &str,
    ai: &crate::privacy::ai_scope::AiScope,
) -> Result<Option<FolderSuggestionOutcome>, ExtractError> {
    Ok(match preflight(db, account_id, uuid, ai)? {
        Preflight::Answer(outcome) => Some(outcome),
        Preflight::Ask { .. } => None,
    })
}

fn db_error(e: rusqlite::Error) -> ExtractError {
    ExtractError::Transport(format!("local database: {e}"))
}

/// Propose one EXISTING folder for note `uuid`. The model's answer is never
/// trusted: it must equal a path offered here, and differ from the note's
/// current label (spec Decision 3). A rejected answer is logged. The
/// answers that need no LLM (`answer_without_llm`) come before
/// `provider.check()`.
pub async fn suggest_folder(
    provider: &dyn LlmProvider,
    db: &Db,
    account_id: &str,
    uuid: &str,
    cancel: CancellationToken,
    ai: &crate::privacy::ai_scope::AiScope,
) -> Result<FolderSuggestionOutcome, ExtractError> {
    let (resolved, note, folders) = match preflight(db, account_id, uuid, ai)? {
        Preflight::Answer(outcome) => return Ok(outcome),
        Preflight::Ask { resolved, note, folders } => (resolved, note, folders),
    };
    provider.check()?;
    super::receipts::stage("suggesting_folder");
    super::receipts::source_version(format!("{}:{}:{}", account_id, resolved, note.local_version).as_bytes());

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

    /// A folder whose create or rename the backend refused
    /// (`mark_folder_push_blocked`) is as unusable as one pending deletion: a
    /// note filed there is held by its own push (`label_wait_for_folder_push`
    /// — #151 for a refused create, #147/#148 for a refused rename) until the
    /// folder's block clears, so every capture and extract into it would sit
    /// unsynced instead of landing where it can sync.
    /// The capture sheet names its default with this; it must say what
    /// `resolve_destination` will do, and must not create the Inbox to say it.
    #[test]
    fn default_destination_predicts_resolve_destination_without_creating_anything() {
        let db = temp_db();
        assert_eq!(default_destination(&db, ACCT, false).unwrap(), NOTES_ROOT);
        assert_eq!(default_destination(&db, ACCT, true).unwrap(), INBOX_PATH);
        assert!(folder_row(&db, INBOX_PATH).is_none(), "a preview creates no folder");
        db.create_folder_local_new(ACCT, INBOX_PATH).unwrap();
        db.mark_folder_push_blocked(ACCT, INBOX_PATH, FolderSyncState::DirtyNew, "permanent: refused").unwrap();
        assert_eq!(default_destination(&db, ACCT, true).unwrap(), NOTES_ROOT);
        assert_eq!(resolve_destination(&db, ACCT, true).unwrap(), NOTES_ROOT);
    }

    #[test]
    fn a_blocked_inbox_falls_through_to_the_root() {
        let db = temp_db();
        db.create_folder_local_new(ACCT, INBOX_PATH).unwrap();
        db.mark_folder_push_blocked(ACCT, INBOX_PATH, FolderSyncState::DirtyNew, "permanent: refused").unwrap();
        assert_eq!(resolve_destination(&db, ACCT, true).unwrap(), NOTES_ROOT);

        // "Try again" clears the block: the Inbox is a destination again.
        db.clear_folder_push_block(ACCT, INBOX_PATH).unwrap();
        assert_eq!(resolve_destination(&db, ACCT, true).unwrap(), INBOX_PATH);
    }

    #[test]
    fn an_inbox_whose_rename_was_refused_falls_through_to_the_root() {
        let db = temp_db();
        db.upsert_folder_from_remote(ACCT, "Notes/Old", "Label_1").unwrap();
        db.rename_folder(ACCT, "Notes/Old", INBOX_PATH).unwrap();
        db.mark_folder_push_blocked(ACCT, INBOX_PATH, FolderSyncState::DirtyRenamed, "permanent: refused").unwrap();
        assert_eq!(resolve_destination(&db, ACCT, true).unwrap(), NOTES_ROOT);
    }

    #[test]
    fn re_extract_beside_a_blocked_folder_or_under_one_resolves_the_destination() {
        let db = temp_db();
        db.upsert_folder_from_remote(ACCT, "Notes/Old", "Label_1").unwrap();
        db.rename_subtree(ACCT, "Notes/Old", "Notes/Research").unwrap();
        db.create_folder_local_new(ACCT, "Notes/Research/Sub").unwrap();
        db.mark_folder_push_blocked(ACCT, "Notes/Research", FolderSyncState::DirtyRenamed, "permanent: refused").unwrap();
        assert_eq!(destination_beside(&db, ACCT, "Notes/Research", true).unwrap(), INBOX_PATH);
        assert_eq!(destination_beside(&db, ACCT, "Notes/Research/Sub", true).unwrap(), INBOX_PATH);
        // A sibling sharing the prefix is not under the blocked folder.
        db.create_folder_local_new(ACCT, "Notes/ResearchX").unwrap();
        assert_eq!(destination_beside(&db, ACCT, "Notes/ResearchX", true).unwrap(), "Notes/ResearchX");
    }

    /// Filing agrees with the push: a refused DELETE leaves the folder on the
    /// remote under its name, so a note under it pushes (#151) — it is not a
    /// reason to file elsewhere.
    #[test]
    fn re_extract_under_a_folder_whose_delete_was_refused_stays_beside() {
        let db = temp_db();
        db.upsert_folder_from_remote(ACCT, "Notes/A", "Label_A").unwrap();
        db.upsert_folder_from_remote(ACCT, "Notes/A/B", "Label_B").unwrap();
        db.mark_folder_deleted(ACCT, "Notes/A").unwrap();
        db.mark_folder_push_blocked(ACCT, "Notes/A", FolderSyncState::DeletedPending, "permanent: refused").unwrap();
        assert_eq!(destination_beside(&db, ACCT, "Notes/A/B", true).unwrap(), "Notes/A/B");
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
        /// `check()` fails as an account with no provider (or AI access off) does.
        not_configured: bool,
    }

    impl CountingProvider {
        fn answering(folder: Option<&str>) -> Self {
            CountingProvider { folder: folder.map(String::from), calls: Mutex::new(Vec::new()), not_configured: false }
        }
        fn not_configured() -> Self {
            CountingProvider { not_configured: true, ..Self::answering(Some("Notes/Trading")) }
        }
        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for CountingProvider {
        fn check(&self) -> Result<(), ExtractError> {
            if self.not_configured {
                return Err(ExtractError::NotConfigured("no LLM provider configured for this account".into()));
            }
            Ok(())
        }
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

        assert_eq!(candidate_folders(&db, ACCT, &crate::privacy::ai_scope::AiScope::default()).unwrap(), vec!["Notes/Trading".to_string()]);
    }

    /// Accepting a suggestion moves the note: into a folder whose create or
    /// rename was refused, or under one, its push is then held
    /// (`label_wait_for_folder_push`) — a note that synced fine stops.
    #[test]
    fn candidates_exclude_folders_that_hold_notes_and_their_children() {
        let db = temp_db();
        db.create_folder_local_new(ACCT, "Notes/Trading").unwrap();
        db.upsert_folder_from_remote(ACCT, "Notes/Old", "Label_1").unwrap();
        db.rename_subtree(ACCT, "Notes/Old", "Notes/Research").unwrap();
        db.create_folder_local_new(ACCT, "Notes/Research/Sub").unwrap();
        db.mark_folder_push_blocked(ACCT, "Notes/Research", FolderSyncState::DirtyRenamed, "permanent: refused").unwrap();
        db.create_folder_local_new(ACCT, "Notes/Refused").unwrap();
        db.mark_folder_push_blocked(ACCT, "Notes/Refused", FolderSyncState::DirtyNew, "permanent: refused").unwrap();

        assert_eq!(candidate_folders(&db, ACCT, &crate::privacy::ai_scope::AiScope::default()).unwrap(), vec!["Notes/Trading".to_string()]);
    }

    #[tokio::test]
    async fn a_path_it_offered_is_suggested() {
        let db = db_with_note_in_inbox();
        let p = CountingProvider::answering(Some("Notes/Trading"));
        let out = suggest_folder(&p, &db, ACCT, NOTE_UUID, CancellationToken::new(), &crate::privacy::ai_scope::AiScope::default()).await.unwrap();
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
        let out = suggest_folder(&p, &db, ACCT, NOTE_UUID, CancellationToken::new(), &crate::privacy::ai_scope::AiScope::default()).await.unwrap();
        assert_eq!(out, FolderSuggestionOutcome::NoneFits);
    }

    #[tokio::test]
    async fn the_folder_the_note_is_already_in_is_not_a_suggestion() {
        let db = db_with_note_in_inbox();
        db.move_notes_batch(ACCT, &[NOTE_UUID.to_string()], "Notes/Trading", &crate::provenance::Actor::test()).unwrap();
        let p = CountingProvider::answering(Some("Notes/Trading"));
        let out = suggest_folder(&p, &db, ACCT, NOTE_UUID, CancellationToken::new(), &crate::privacy::ai_scope::AiScope::default()).await.unwrap();
        assert_eq!(out, FolderSuggestionOutcome::NoneFits);
    }

    #[tokio::test]
    async fn a_null_answer_is_none_fits() {
        let db = db_with_note_in_inbox();
        let p = CountingProvider::answering(None);
        let out = suggest_folder(&p, &db, ACCT, NOTE_UUID, CancellationToken::new(), &crate::privacy::ai_scope::AiScope::default()).await.unwrap();
        assert_eq!(out, FolderSuggestionOutcome::NoneFits);
    }

    #[tokio::test]
    async fn no_candidate_folders_means_no_provider_call() {
        let db = temp_db();
        crate::test_support::note(ACCT, NOTE_UUID).insert(&db);
        let p = CountingProvider::answering(Some("Notes/Trading"));
        let out = suggest_folder(&p, &db, ACCT, NOTE_UUID, CancellationToken::new(), &crate::privacy::ai_scope::AiScope::default()).await.unwrap();
        assert_eq!(out, FolderSuggestionOutcome::NoCandidates);
        assert_eq!(p.call_count(), 0, "an empty candidate list must not spend an LLM call");
    }

    /// "No folders yet" is wrong when the account has folders and every one
    /// is blocked (or under a blocked one): say why instead. A blocked Inbox
    /// alone is still `NoCandidates` — the Inbox is never a candidate.
    #[tokio::test]
    async fn only_blocked_folders_is_its_own_outcome_without_a_provider_call() {
        let db = temp_db();
        crate::test_support::note(ACCT, NOTE_UUID).insert(&db);
        db.create_folder_local_new(ACCT, INBOX_PATH).unwrap();
        db.mark_folder_push_blocked(ACCT, INBOX_PATH, FolderSyncState::DirtyNew, "permanent: refused").unwrap();
        let p = CountingProvider::answering(Some("Notes/Work"));
        let out = suggest_folder(&p, &db, ACCT, NOTE_UUID, CancellationToken::new(), &crate::privacy::ai_scope::AiScope::default()).await.unwrap();
        assert_eq!(out, FolderSuggestionOutcome::NoCandidates);

        db.create_folder_local_new(ACCT, "Notes/Work/Sub").unwrap();
        db.mark_folder_push_blocked(ACCT, "Notes/Work", FolderSyncState::DirtyNew, "permanent: refused").unwrap();
        let out = suggest_folder(&p, &db, ACCT, NOTE_UUID, CancellationToken::new(), &crate::privacy::ai_scope::AiScope::default()).await.unwrap();
        // Named: the folder whose block left the others out — the one the
        // sidebar marks "!" — not `Notes/Work/Sub`, which shows no marker.
        // Not the Inbox either: it excluded nothing that was offerable.
        let blocked_work = FolderSuggestionOutcome::OnlyBlockedFolders { folders: vec!["Notes/Work".into()] };
        assert_eq!(out, blocked_work);
        assert_eq!(p.call_count(), 0, "nothing to offer: no LLM call");
        assert_eq!(
            serde_json::to_value(blocked_work).unwrap(),
            serde_json::json!({"kind": "only_blocked_folders", "folders": ["Notes/Work"]})
        );
    }

    /// An answer that needs no LLM must not wait on one: "configure a
    /// provider" sends the user to fix the wrong thing when there is nothing
    /// to suggest anyway. Only a real question reaches `check()`.
    #[tokio::test]
    async fn the_answers_that_need_no_llm_come_before_the_provider_check() {
        let p = CountingProvider::not_configured();
        let cancel = CancellationToken::new;
        let all = crate::privacy::ai_scope::AiScope::default();

        let db = temp_db();
        crate::test_support::note(ACCT, NOTE_UUID).insert(&db);
        assert_eq!(suggest_folder(&p, &db, ACCT, NOTE_UUID, cancel(), &all).await.unwrap(), FolderSuggestionOutcome::NoCandidates);
        assert_eq!(answer_without_llm(&db, ACCT, NOTE_UUID, &all).unwrap(), Some(FolderSuggestionOutcome::NoCandidates));

        db.create_folder_local_new(ACCT, "Notes/Work").unwrap();
        db.mark_folder_push_blocked(ACCT, "Notes/Work", FolderSyncState::DirtyNew, "permanent: refused").unwrap();
        let blocked = FolderSuggestionOutcome::OnlyBlockedFolders { folders: vec!["Notes/Work".into()] };
        assert_eq!(suggest_folder(&p, &db, ACCT, NOTE_UUID, cancel(), &all).await.unwrap(), blocked);
        assert_eq!(
            suggest_folder(&p, &db, ACCT, "no-such-uuid", cancel(), &all).await.unwrap(),
            FolderSuggestionOutcome::NoteNotFound
        );

        // A real question still needs the provider.
        let db = db_with_note_in_inbox();
        assert_eq!(answer_without_llm(&db, ACCT, NOTE_UUID, &all).unwrap(), None);
        assert!(matches!(
            suggest_folder(&p, &db, ACCT, NOTE_UUID, cancel(), &all).await,
            Err(ExtractError::NotConfigured(_))
        ));
        assert_eq!(p.call_count(), 0);
    }

    #[tokio::test]
    async fn a_missing_note_is_reported_without_a_provider_call() {
        let db = db_with_note_in_inbox();
        let p = CountingProvider::answering(Some("Notes/Trading"));
        let out = suggest_folder(&p, &db, ACCT, "no-such-uuid", CancellationToken::new(), &crate::privacy::ai_scope::AiScope::default()).await.unwrap();
        assert_eq!(out, FolderSuggestionOutcome::NoteNotFound);
        assert_eq!(p.call_count(), 0);
    }

    /// Chars, not bytes: a byte cut would split a Thai character and panic.
    #[tokio::test]
    async fn the_note_text_is_cut_at_six_thousand_characters() {
        let db = db_with_note_in_inbox();
        let long = "ก".repeat(10_000);
        db.apply_local_edit(NOTE_UUID, ACCT, "T", &format!("<div>T</div><div>{long}</div>"), INBOX_PATH, &crate::provenance::Actor::test())
            .unwrap();
        let p = CountingProvider::answering(None);
        suggest_folder(&p, &db, ACCT, NOTE_UUID, CancellationToken::new(), &crate::privacy::ai_scope::AiScope::default()).await.unwrap();
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
        db.apply_local_edit(NOTE_UUID, ACCT, "Settrade order types", body, INBOX_PATH, &crate::provenance::Actor::test()).unwrap();
        let p = CountingProvider::answering(Some("Notes/Trading"));
        suggest_folder(&p, &db, ACCT, NOTE_UUID, CancellationToken::new(), &crate::privacy::ai_scope::AiScope::default()).await.unwrap();
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
        let out = suggest_folder(&p, &db, ACCT, NOTE_UUID, CancellationToken::new(), &crate::privacy::ai_scope::AiScope::default()).await.unwrap();
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

        let out = suggest_folder(&p, &db, ACCT, NOTE_UUID, CancellationToken::new(), &crate::privacy::ai_scope::AiScope::default()).await.unwrap();
        assert_eq!(
            out,
            FolderSuggestionOutcome::Suggested {
                uuid: NEW_UUID.into(),
                path: "Notes/Trading".into(),
                reason: Some("fits".into()),
            }
        );
    }

    #[tokio::test]
    async fn hidden_folders_are_never_offered() {
        let db = db_with_note_in_inbox();
        db.create_folder_local_new(ACCT, "Notes/Private").unwrap();
        db.create_folder_local_new(ACCT, "Notes/Private/Bank").unwrap();
        let ai = crate::privacy::ai_scope::AiScope::hiding(ACCT, &["Notes/Private"]);
        let p = CountingProvider::answering(Some("Notes/Trading"));
        suggest_folder(&p, &db, ACCT, NOTE_UUID, CancellationToken::new(), &ai).await.unwrap();
        let (_, folders) = p.calls.lock().unwrap()[0].clone();
        assert_eq!(folders, vec!["Notes/Health".to_string(), "Notes/Trading".to_string()]);
    }

    #[tokio::test]
    async fn only_hidden_folders_left_means_no_candidates_not_blocked() {
        let db = db_with_note_in_inbox();
        let ai = crate::privacy::ai_scope::AiScope::hiding(ACCT, &["Notes/Trading", "Notes/Health"]);
        let p = CountingProvider::answering(Some("Notes/Trading"));
        let out = suggest_folder(&p, &db, ACCT, NOTE_UUID, CancellationToken::new(), &ai).await.unwrap();
        assert_eq!(out, FolderSuggestionOutcome::NoCandidates);
        assert_eq!(p.call_count(), 0);
    }

    #[tokio::test]
    async fn a_hidden_folder_is_never_named_as_a_blocker() {
        // Trading is blocked by a refused push, Health is hidden: the user is
        // told about Trading only — a hidden folder neither counts nor shows.
        let db = db_with_note_in_inbox();
        db.mark_folder_push_blocked(ACCT, "Notes/Trading", FolderSyncState::DirtyNew, "permanent: refused").unwrap();
        let ai = crate::privacy::ai_scope::AiScope::hiding(ACCT, &["Notes/Health"]);
        let p = CountingProvider::answering(None);
        let out = suggest_folder(&p, &db, ACCT, NOTE_UUID, CancellationToken::new(), &ai).await.unwrap();
        assert_eq!(out, FolderSuggestionOutcome::OnlyBlockedFolders { folders: vec!["Notes/Trading".to_string()] });
        assert_eq!(p.call_count(), 0);
    }

    /// I2 (final review), spec §6 canary: nothing from a hidden folder —
    /// its name, a subfolder's name, or a note's title or body inside it —
    /// appears anywhere in what the provider receives.
    #[tokio::test]
    async fn no_canary_from_a_hidden_folder_reaches_the_filing_provider() {
        let db = db_with_note_in_inbox();
        db.create_folder_local_new(ACCT, "Notes/CANARYFOLDER").unwrap();
        db.create_folder_local_new(ACCT, "Notes/CANARYFOLDER/CANARYSUB").unwrap();
        crate::test_support::note(ACCT, "u-canary")
            .title("CANARYTITLE")
            .body("CANARYBODY #CANARYTAG")
            .label("Notes/CANARYFOLDER")
            .insert(&db);
        let ai = crate::privacy::ai_scope::AiScope::hiding(ACCT, &["Notes/CANARYFOLDER"]);
        let p = CountingProvider::answering(Some("Notes/Trading"));
        suggest_folder(&p, &db, ACCT, NOTE_UUID, CancellationToken::new(), &ai).await.unwrap();
        let calls = p.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let (text, folders) = &calls[0];
        let sent = format!("{text}\n{}", folders.join("\n"));
        assert!(!sent.contains("CANARY"), "hidden content reached the provider: {sent}");
        assert!(folders.contains(&"Notes/Trading".to_string()), "the allowed folders are still offered: {folders:?}");
    }

    #[tokio::test]
    async fn a_note_in_a_hidden_folder_is_refused_without_a_provider_call() {
        let db = db_with_note_in_inbox();
        let ai = crate::privacy::ai_scope::AiScope::hiding(ACCT, &[INBOX_PATH]);
        let p = CountingProvider::answering(Some("Notes/Trading"));
        let err = suggest_folder(&p, &db, ACCT, NOTE_UUID, CancellationToken::new(), &ai).await.unwrap_err();
        assert_eq!(err.to_string(), crate::privacy::ai_scope::HIDDEN_NOTE);
        assert_eq!(p.call_count(), 0);
    }

    /// Merge of #165 with the AI boundary: the SQLite-only answer (asked
    /// before any provider) applies the same scope — a hidden note is refused
    /// there, ahead of `check()`, and an account whose only folders are
    /// hidden answers NoCandidates without naming them.
    #[tokio::test]
    async fn the_no_llm_answer_applies_the_ai_scope_before_the_provider_check() {
        let p = CountingProvider::not_configured();

        let db = db_with_note_in_inbox();
        let hidden_note = crate::privacy::ai_scope::AiScope::hiding(ACCT, &[INBOX_PATH]);
        let err = answer_without_llm(&db, ACCT, NOTE_UUID, &hidden_note).unwrap_err();
        assert_eq!(err.to_string(), crate::privacy::ai_scope::HIDDEN_NOTE);
        let err = suggest_folder(&p, &db, ACCT, NOTE_UUID, CancellationToken::new(), &hidden_note).await.unwrap_err();
        assert_eq!(err.to_string(), crate::privacy::ai_scope::HIDDEN_NOTE, "refused before check(), not NotConfigured");

        let only_hidden = crate::privacy::ai_scope::AiScope::hiding(ACCT, &["Notes/Trading", "Notes/Health"]);
        assert_eq!(
            answer_without_llm(&db, ACCT, NOTE_UUID, &only_hidden).unwrap(),
            Some(FolderSuggestionOutcome::NoCandidates)
        );
        assert_eq!(p.call_count(), 0);
    }
}
