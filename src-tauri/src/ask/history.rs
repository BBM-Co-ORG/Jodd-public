//! Ask Jodd conversation history (spec 2026-10-14). Types and pure logic
//! only — the SQL lives in `db.rs` (its connection is private), the Tauri
//! commands in `ask_history_commands.rs`. Local to this device: never synced,
//! never read by jodd-mcp, and never a source of masked values on its own —
//! see `resume_known` (R2 + R4).

use serde::{Deserialize, Serialize};

use crate::accounts::Account;
use crate::ask::{AskScope, CitedNote};
use crate::db::Db;
use crate::llm::provider::{ChatRole, ChatTurn};
use crate::privacy::ai_scope::AiScope;
use crate::privacy::vault::{Known, Vault};

/// Migration 28. `IF NOT EXISTS` throughout, so a vault that ran this DDL
/// under its pre-merge number re-runs it harmlessly. No foreign key: `db.rs` never enables `PRAGMA
/// foreign_keys`, so `ON DELETE CASCADE` would be inert — every delete path
/// removes the turns itself, in the same transaction.
pub const ASK_HISTORY_DDL: &str = "
CREATE TABLE IF NOT EXISTS ask_conversations (
    id          TEXT PRIMARY KEY,
    title       TEXT NOT NULL,
    scope_json  TEXT NOT NULL,
    destination TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    archived_at INTEGER
);
CREATE INDEX IF NOT EXISTS ask_conversations_list ON ask_conversations (archived_at, updated_at);
CREATE TABLE IF NOT EXISTS ask_turns (
    conversation_id TEXT NOT NULL,
    seq             INTEGER NOT NULL,
    question        TEXT NOT NULL,
    answer_markdown TEXT NOT NULL,
    cited_json      TEXT NOT NULL,
    read_json       TEXT NOT NULL,
    stats_json      TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    PRIMARY KEY (conversation_id, seq)
);";

const TITLE_MAX_CHARS: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NoteRef {
    pub uuid: String,
    pub account_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnStats {
    pub notes_in_scope: usize,
    pub notes_considered: usize,
    pub notes_used: usize,
    pub trimmed: bool,
    pub dropped_citations: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredTurn {
    pub seq: i64,
    pub question: String,
    pub answer_markdown: String,
    pub cited: Vec<CitedNote>,
    /// Every note whose text went into this turn's answer call — cited or
    /// not. R2 and R4 need all of them: an uncited note still shaped the answer.
    pub read: Vec<NoteRef>,
    pub stats: TurnStats,
    pub created_at: i64,
}

pub struct NewTurn<'a> {
    pub question: &'a str,
    pub answer_markdown: &'a str,
    pub cited: &'a [CitedNote],
    pub read: &'a [NoteRef],
    pub stats: &'a TurnStats,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConversationSummary {
    pub id: String,
    pub title: String,
    pub scope: AskScope,
    pub updated_at: i64,
    pub archived: bool,
    pub turn_count: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StoredConversation {
    pub id: String,
    pub title: String,
    pub scope: AskScope,
    pub destination: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub archived: bool,
    pub turns: Vec<StoredTurn>,
}

/// Whitespace collapsed, cut to 80 chars by `char`, never by byte (Thai).
pub fn conversation_title(question: &str) -> String {
    let collapsed = question.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(TITLE_MAX_CHARS).collect()
}

// ── The on/off setting ────────────────────────────────────────────────────
// Its own file, NOT a field on AppLlmConfig: `ai_stamp` hashes
// `app_llm_config::load()`, so a field there would reset every live Ask
// session whenever the owner toggled history.

#[derive(Serialize, Deserialize)]
struct Settings {
    enabled: bool,
}

/// Missing or unreadable means the default, on.
pub fn parse_enabled(raw: Option<&str>) -> bool {
    raw.and_then(|t| serde_json::from_str::<Settings>(t).ok())
        .map(|s| s.enabled)
        .unwrap_or(true)
}

fn settings_path() -> Result<std::path::PathBuf, String> {
    let base = crate::paths::config_base().ok_or("no config dir on this OS")?;
    let dir = base.join("jodd");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    Ok(dir.join("ask_history.json"))
}

pub fn enabled() -> bool {
    let raw = settings_path().ok().and_then(|p| std::fs::read_to_string(p).ok());
    parse_enabled(raw.as_deref())
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    let p = settings_path()?;
    let txt = serde_json::to_string(&Settings { enabled }).map_err(|e| e.to_string())?;
    std::fs::write(&p, txt).map_err(|e| format!("write {}: {e}", p.display()))
}

pub const READ_NOTE_ACCOUNT: &str =
    "A note this conversation used belongs to an account that no longer allows AI.";
pub const READ_NOTE_GONE: &str = "A note this conversation used was deleted.";
pub const READ_NOTE_HIDDEN: &str = "A note this conversation used is now in a hidden folder.";

/// Stored turns as the provider history `Session.turns` holds.
pub fn chat_turns(turns: &[StoredTurn]) -> Vec<ChatTurn> {
    turns
        .iter()
        .flat_map(|t| {
            [
                ChatTurn { role: ChatRole::User, content: t.question.clone() },
                ChatTurn { role: ChatRole::Assistant, content: t.answer_markdown.clone() },
            ]
        })
        .collect()
}

fn distinct_reads(turns: &[StoredTurn]) -> Vec<&NoteRef> {
    let mut seen = std::collections::HashSet::new();
    turns.iter().flat_map(|t| &t.read).filter(|r| seen.insert(*r)).collect()
}

/// R2 + R4 in one pass so a note cannot vanish between the check and the
/// rebuild (fail-closed). R2 (spec §4): every note any turn READ must still
/// be eligible — account allows AI, note exists, folder not hidden — or the
/// history cannot be resent. R4: the values masked by context in earlier turns
/// are re-derived from the current bodies of those notes into a fresh `Known`,
/// so the resent history masks them again. Nothing here is persisted. The
/// rebuild uses each note's full text (`note_text_for_ai`), a conservative
/// superset of what Ask sent, since Ask truncates to `MAX_NOTE_CHARS`.
pub fn resume_known(db: &Db, accounts: &[Account], ai: &AiScope, turns: &[StoredTurn]) -> Result<Known, String> {
    let mut texts: Vec<String> = Vec::new();
    for r in distinct_reads(turns) {
        if !accounts.iter().any(|a| a.id == r.account_id && crate::llm::policy::account_allowed(a)) {
            return Err(READ_NOTE_ACCOUNT.into());
        }
        // Gotcha #16: an iCloud rekey must not look like a deletion.
        let uuid = db.resolve_note_uuid(&r.uuid, &r.account_id).map_err(|e| e.to_string())?;
        let note = db
            .note_by_uuid(&r.account_id, &uuid)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| READ_NOTE_GONE.to_string())?;
        if !ai.allows(&r.account_id, &note.label) {
            return Err(READ_NOTE_HIDDEN.into());
        }
        texts.push(crate::ask::context::note_text_for_ai(&note.body_html));
    }
    let mut known = Known::default();
    Vault::reserving(texts.iter().map(String::as_str)).remember(&mut known);
    Ok(known)
}

/// R3: proceed when the destination is unchanged, or the caller accepted
/// exactly the destination in force now.
pub fn destination_accepted(stored: &str, current: &str, accepted: Option<&str>) -> bool {
    stored == current || accepted == Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_db;

    const A: &str = "gmail:a@x";

    fn stats() -> TurnStats {
        TurnStats { notes_in_scope: 10, notes_considered: 5, notes_used: 2, trimmed: false, dropped_citations: 0 }
    }

    fn turn<'a>(q: &'a str, a: &'a str, read: &'a [NoteRef], s: &'a TurnStats) -> NewTurn<'a> {
        NewTurn { question: q, answer_markdown: a, cited: &[], read, stats: s }
    }

    fn account_scope() -> AskScope {
        AskScope::Account { account_id: A.into() }
    }

    #[test]
    fn title_collapses_whitespace() {
        assert_eq!(conversation_title("  why\n keep   both? "), "why keep both?");
    }

    #[test]
    fn title_cuts_thai_on_a_char_boundary() {
        let q = "ทำไม".repeat(30); // 120 chars, 360 bytes
        let t = conversation_title(&q);
        assert_eq!(t.chars().count(), 80);
        assert!(q.starts_with(&t));
    }

    #[test]
    fn enabled_defaults_on_and_reads_the_file() {
        assert!(parse_enabled(None));
        assert!(parse_enabled(Some("not json")));
        assert!(parse_enabled(Some(r#"{"enabled":true}"#)));
        assert!(!parse_enabled(Some(r#"{"enabled":false}"#)));
    }

    #[test]
    fn first_answer_creates_the_conversation_and_later_ones_append() {
        let db = temp_db();
        let s = stats();
        let read = [NoteRef { uuid: "n1".into(), account_id: A.into() }];
        let id = db.ask_append_turn(None, &account_scope(), "HTTP x", &turn("first q", "first a", &read, &s), 1_000).unwrap();
        let again = db.ask_append_turn(Some(&id), &account_scope(), "HTTP y", &turn("second q", "second a", &[], &s), 2_000).unwrap();
        assert_eq!(again, id);

        let c = db.ask_load_conversation(&id).unwrap().unwrap();
        assert_eq!(c.title, "first q");
        assert_eq!(c.scope, account_scope());
        assert_eq!(c.destination, "HTTP y", "destination follows the last turn");
        assert_eq!((c.created_at, c.updated_at), (1_000, 2_000));
        assert!(!c.archived);
        assert_eq!(c.turns.iter().map(|t| t.seq).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(c.turns[0].read, read);
        assert_eq!(c.turns[1].answer_markdown, "second a");
        assert_eq!(c.turns[0].stats, s);
    }

    #[test]
    fn append_to_a_deleted_conversation_errors_instead_of_recreating() {
        let db = temp_db();
        let s = stats();
        let id = db.ask_append_turn(None, &account_scope(), "d", &turn("q", "a", &[], &s), 1).unwrap();
        assert!(db.ask_delete_conversation(&id).unwrap());
        assert!(db.ask_append_turn(Some(&id), &account_scope(), "d", &turn("q2", "a2", &[], &s), 2).is_err());
        assert!(db.ask_list_conversations(false).unwrap().is_empty());
        let orphans: i64 = db.ask_turn_count_for_tests(&id);
        assert_eq!(orphans, 0, "no orphan turn rows");
    }

    #[test]
    fn list_is_newest_first_and_split_by_archived() {
        let db = temp_db();
        let s = stats();
        let old = db.ask_append_turn(None, &account_scope(), "d", &turn("old", "a", &[], &s), 1).unwrap();
        let new = db.ask_append_turn(None, &AskScope::AllAccounts, "d", &turn("new", "a", &[], &s), 5).unwrap();
        let active = db.ask_list_conversations(false).unwrap();
        assert_eq!(active.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), [new.as_str(), old.as_str()]);
        assert_eq!(active[0].turn_count, 1);

        assert!(db.ask_set_archived(&old, Some(9)).unwrap());
        assert_eq!(db.ask_list_conversations(false).unwrap().len(), 1);
        let archived = db.ask_list_conversations(true).unwrap();
        assert_eq!(archived.len(), 1);
        assert!(archived[0].archived);
        assert!(db.ask_set_archived(&old, None).unwrap());
        assert_eq!(db.ask_list_conversations(false).unwrap().len(), 2);
    }

    #[test]
    fn rename_delete_and_clear() {
        let db = temp_db();
        let s = stats();
        let a = db.ask_append_turn(None, &account_scope(), "d", &turn("a", "a", &[], &s), 1).unwrap();
        let b = db.ask_append_turn(None, &account_scope(), "d", &turn("b", "b", &[], &s), 2).unwrap();
        assert!(db.ask_rename_conversation(&a, "Renamed").unwrap());
        assert_eq!(db.ask_load_conversation(&a).unwrap().unwrap().title, "Renamed");
        assert!(!db.ask_rename_conversation("missing", "x").unwrap());

        assert!(db.ask_delete_conversation(&a).unwrap());
        assert!(db.ask_load_conversation(&a).unwrap().is_none());
        assert_eq!(db.ask_turn_count_for_tests(&a), 0);

        assert_eq!(db.ask_clear_history().unwrap(), 1);
        assert!(db.ask_load_conversation(&b).unwrap().is_none());
        assert_eq!(db.ask_turn_count_for_tests(&b), 0);
    }

    #[test]
    fn removing_an_account_deletes_its_scoped_conversations_only() {
        let db = temp_db();
        let s = stats();
        let mine = db.ask_append_turn(None, &account_scope(), "d", &turn("m", "a", &[], &s), 1).unwrap();
        let folder = db.ask_append_turn(None, &AskScope::Folder { account_id: A.into(), label: "Notes/X".into() }, "d", &turn("f", "a", &[], &s), 2).unwrap();
        let other = db.ask_append_turn(None, &AskScope::Account { account_id: "gmail:b@x".into() }, "d", &turn("o", "a", &[], &s), 3).unwrap();
        let all = db.ask_append_turn(None, &AskScope::AllAccounts, "d", &turn("all", "a", &[], &s), 4).unwrap();

        db.delete_account(A).unwrap();

        assert!(db.ask_load_conversation(&mine).unwrap().is_none());
        assert!(db.ask_load_conversation(&folder).unwrap().is_none());
        assert_eq!(db.ask_turn_count_for_tests(&mine), 0);
        assert_eq!(db.ask_turn_count_for_tests(&folder), 0);
        assert!(db.ask_load_conversation(&other).unwrap().is_some());
        assert!(db.ask_load_conversation(&all).unwrap().is_some(), "all-accounts history stays");
    }

    #[test]
    fn migration_28_runs_on_an_existing_vault() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        {
            let db = crate::db::Db::open_unencrypted(&path).unwrap();
            db.ask_drop_tables_for_tests(); // back to a version-27 vault
        }
        let db = crate::db::Db::open_unencrypted(&path).unwrap();
        let s = stats();
        db.ask_append_turn(None, &AskScope::AllAccounts, "d", &turn("q", "a", &[], &s), 1).unwrap();
        assert_eq!(db.ask_list_conversations(false).unwrap().len(), 1);
    }

    use crate::llm::policy::CheckedProvider;
    use crate::llm::provider::*;
    use crate::test_support::note;
    use std::sync::{Arc, Mutex};
    use tokio_util::sync::CancellationToken;

    /// Same shape as `llm::policy`'s test `account(id, "none", Some(true))`,
    /// which `account_allowed` accepts.
    fn allowed(id: &str) -> Account {
        serde_json::from_value(serde_json::json!({
            "id": id, "email": "synthetic@example.test", "added_at": "2026-10-14",
            "llm": { "provider": "none", "data_allowed": true }
        })).unwrap()
    }

    fn stored(read: Vec<NoteRef>, answer: &str) -> StoredTurn {
        StoredTurn {
            seq: 1, question: "office wifi?".into(), answer_markdown: answer.into(),
            cited: vec![], read, stats: stats(), created_at: 1,
        }
    }

    fn wifi_note(db: &crate::db::Db) -> NoteRef {
        note(A, "aabbccdd-0000").title("Office wifi").body("Office wifi pass: Tr0ub4dor").label("Notes/Office").insert(db);
        NoteRef { uuid: "aabbccdd-0000".into(), account_id: A.into() }
    }

    #[test]
    fn chat_turns_alternate_question_and_answer() {
        let t = chat_turns(&[stored(vec![], "ans")]);
        assert_eq!(t.len(), 2);
        assert_eq!((t[0].role, t[0].content.as_str()), (ChatRole::User, "office wifi?"));
        assert_eq!((t[1].role, t[1].content.as_str()), (ChatRole::Assistant, "ans"));
    }

    #[test]
    fn r2_passes_for_an_eligible_note() {
        let db = temp_db();
        let r = wifi_note(&db);
        assert!(resume_known(&db, &[allowed(A)], &AiScope::default(), &[stored(vec![r], "a")]).is_ok());
    }

    #[test]
    fn r2_refuses_a_note_whose_account_is_gone() {
        let db = temp_db();
        let r = wifi_note(&db);
        assert_eq!(resume_known(&db, &[], &AiScope::default(), &[stored(vec![r], "a")]).map(|_| ()), Err(READ_NOTE_ACCOUNT.into()));
    }

    #[test]
    fn r2_refuses_a_deleted_note() {
        let db = temp_db();
        let r = wifi_note(&db);
        db.mark_deleted(&r.uuid, A).unwrap();
        assert_eq!(resume_known(&db, &[allowed(A)], &AiScope::default(), &[stored(vec![r], "a")]).map(|_| ()), Err(READ_NOTE_GONE.into()));
    }

    #[test]
    fn r2_refuses_a_note_now_in_a_hidden_folder() {
        let db = temp_db();
        let r = wifi_note(&db);
        let ai = AiScope::hiding(A, &["Notes/Office"]);
        assert_eq!(resume_known(&db, &[allowed(A)], &ai, &[stored(vec![r], "a")]).map(|_| ()), Err(READ_NOTE_HIDDEN.into()));
    }

    #[test]
    fn r2_refuses_a_note_whose_account_no_longer_allows_ai() {
        let db = temp_db();
        let r = wifi_note(&db);
        let mut acct = allowed(A);
        acct.llm = serde_json::from_value(serde_json::json!({ "provider": "none", "data_allowed": false })).unwrap();
        assert_eq!(resume_known(&db, &[acct], &AiScope::default(), &[stored(vec![r], "a")]).map(|_| ()), Err(READ_NOTE_ACCOUNT.into()));
    }

    /// Gotcha #16: a note rekeyed after it was read is not a deleted note, and
    /// its values are still masked from the rekeyed row's body.
    #[tokio::test]
    async fn a_rekeyed_note_still_resumes_and_still_masks() {
        let db = temp_db();
        let r = wifi_note(&db);
        db.rekey_note_uuid(&r.uuid, "11223344-new", A).unwrap();
        let history = [stored(vec![r], "Use Tr0ub4dor to join the wifi.")];

        let known = resume_known(&db, &[allowed(A)], &AiScope::default(), &history).expect("an alias is not a deletion");
        let sent = resend(known, &history).await;
        assert!(!sent.contains("Tr0ub4dor"), "rekeyed note must still mask: {sent}");
    }

    #[test]
    fn r3_destination_gate() {
        assert!(destination_accepted("HTTP a", "HTTP a", None));
        assert!(!destination_accepted("HTTP a", "HTTP b", None));
        assert!(destination_accepted("HTTP a", "HTTP b", Some("HTTP b")));
        assert!(!destination_accepted("HTTP a", "HTTP c", Some("HTTP b")), "an acceptance of an older destination is stale");
    }

    struct Recorder {
        seen: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait::async_trait]
    impl LlmProvider for Recorder {
        async fn extract(&self, _: &str, _: &[String], _: CancellationToken) -> Result<ExtractEnvelope, ExtractError> { unreachable!() }
        async fn run_workflow(&self, _: WorkflowKind, _: &str, _: &[String], _: CancellationToken) -> Result<ExtractEnvelope, ExtractError> { unreachable!() }
        async fn suggest_links(&self, _: &str, _: &[CandidateSummary], _: CancellationToken) -> Result<LinkSuggestionsEnvelope, ExtractError> { unreachable!() }
        async fn suggest_folder(&self, _: &str, _: &[String], _: CancellationToken) -> Result<FolderSuggestionEnvelope, ExtractError> { unreachable!() }
        async fn synthesize(&self, _: &[SourceDigest], _: &str, _: CancellationToken) -> Result<ExtractEnvelope, ExtractError> { unreachable!() }
        async fn chat(&self, s: &str, t: &[ChatTurn], _: CancellationToken) -> Result<String, ExtractError> {
            self.seen.lock().unwrap().push(format!("{s} {t:?}"));
            Ok("ok".into())
        }
    }

    /// Resend history after a restart: what the provider receives.
    async fn resend(known: crate::privacy::vault::Known, history: &[StoredTurn]) -> String {
        let gate = Mutex::new(());
        let seen = Arc::new(Mutex::new(vec![]));
        let p = CheckedProvider {
            cancel: Mutex::new(None),
            gate: &gate,
            inner: Box::new(Recorder { seen: seen.clone() }),
            valid: Box::new(|| true),
            known: Arc::new(Mutex::new(known)),
        };
        let mut turns = chat_turns(history);
        turns.push(ChatTurn { role: ChatRole::User, content: "and again?".into() });
        p.chat("system", &turns, CancellationToken::new()).await.unwrap();
        let out = seen.lock().unwrap().join("\n");
        out
    }

    /// R4. The stored answer repeats the password bare — no `pass:` context
    /// to re-detect it — so only the rebuilt `Known` can mask it.
    #[tokio::test]
    async fn r4_rebuilt_known_masks_a_context_detected_value_in_resent_history() {
        let db = temp_db();
        let r = wifi_note(&db);
        let history = [stored(vec![r], "Use Tr0ub4dor to join the wifi.")];

        let sent = resend(resume_known(&db, &[allowed(A)], &AiScope::default(), &history).unwrap(), &history).await;
        assert!(!sent.contains("Tr0ub4dor"), "rebuilt Known must mask it: {sent}");
    }

    /// The control. Without R4 the value must leak — otherwise the test
    /// above proves nothing (the detector would be catching it on its own,
    /// and this file's fixture would need a value it cannot detect bare).
    #[tokio::test]
    async fn r4_control_without_rebuild_the_value_leaks() {
        let db = temp_db();
        let r = wifi_note(&db);
        let history = [stored(vec![r], "Use Tr0ub4dor to join the wifi.")];

        let sent = resend(Default::default(), &history).await;
        assert!(sent.contains("Tr0ub4dor"), "control: an empty Known must not mask it");
    }
}
