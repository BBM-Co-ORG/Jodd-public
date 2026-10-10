//! Which notes and folders may reach an AI provider (spec 2026-10-08 §4.4).
//!
//! One list: the folders hidden from MCP agents in `mcp_write_scope.json` are
//! hidden from Jodd-managed AI too (spec D4, gotcha #33). This module only
//! READS that file; `mcp_scope` owns its format.

use std::path::Path;

use crate::db::Db;
use crate::mcp_scope::{self, ScopeError, ScopeFile};

pub const HIDDEN_NOTE: &str =
    "This note's folder is hidden from AI. Unhide it in Settings → Agent workspace to use AI on it.";
pub const HIDDEN_FOLDER: &str =
    "This folder is hidden from AI. Unhide it in Settings → Agent workspace to ask about it.";

/// Where the text an AI command received came from. Required on every
/// command whose text arrives from the frontend: the backend checks the
/// note's folder itself rather than trusting the UI.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AiSource {
    /// The text is (or was edited from) this note's body.
    Note { uuid: String },
    /// Typed, pasted or captured by the user — no note to check.
    Pasted,
}

/// A snapshot of the hidden-folder list, loaded once per command.
#[derive(Debug, Default)]
pub struct AiScope {
    file: ScopeFile,
}

impl AiScope {
    pub fn load() -> Result<Self, String> {
        Self::load_from(&mcp_scope::scope_path())
    }

    /// Fail closed: only a file that does not exist means "nothing hidden".
    /// `mcp_scope::load_from` folds every read error into `NotConfigured`,
    /// which is right for MCP (deny-by-default) but would make an unreadable
    /// file silently un-hide everything here.
    pub fn load_from(path: &Path) -> Result<Self, String> {
        match mcp_scope::load_from(path) {
            Ok(file) => Ok(Self { file }),
            // `symlink_metadata`, not `metadata`: a dangling symlink exists,
            // and must not read as "nothing hidden".
            Err(ScopeError::NotConfigured) => match std::fs::symlink_metadata(path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
                _ => Err(unreadable("it exists but could not be read")),
            },
            Err(ScopeError::Unparseable(e)) => Err(unreadable(&e)),
        }
    }

    pub fn allows(&self, account_id: &str, label: &str) -> bool {
        !self.file.is_hidden(account_id, label)
    }

    pub fn check_label(&self, account_id: &str, label: &str) -> Result<(), String> {
        if self.allows(account_id, label) { Ok(()) } else { Err(HIDDEN_NOTE.into()) }
    }

    pub fn check_folder(&self, account_id: &str, folder: &str) -> Result<(), String> {
        if self.allows(account_id, folder) { Ok(()) } else { Err(HIDDEN_FOLDER.into()) }
    }

    /// Reads the note's label NOW (after following a rekey alias, gotcha #16):
    /// a note picked in the UI may have moved since.
    pub fn check_source(&self, db: &Db, account_id: &str, source: &AiSource) -> Result<(), String> {
        let AiSource::Note { uuid } = source else { return Ok(()) };
        let uuid = db.resolve_note_uuid(uuid, account_id).map_err(|e| format!("resolve source note: {e}"))?;
        let note = db
            .get(&uuid, account_id)
            .map_err(|e| format!("read source note: {e}"))?
            .ok_or_else(|| "The source note is no longer available.".to_string())?;
        self.check_label(account_id, &note.label)
    }

    /// The account's tag vocabulary as the AI may see it: distinct, sorted
    /// tags that occur on at least one note whose folder is allowed. A tag
    /// carried only by notes in hidden folders is a fact about those notes
    /// (§1), so it is left out. Best-effort, like the unfiltered lookup it
    /// replaced: a db error yields an empty vocabulary, which sends nothing.
    pub fn tags_visible_to_ai(&self, db: &Db, account_id: &str) -> Vec<String> {
        let rows = db.list_tags_with_labels(account_id).unwrap_or_default();
        let tags: std::collections::BTreeSet<String> =
            rows.into_iter().filter(|(_, label)| self.allows(account_id, label)).map(|(tag, _)| tag).collect();
        tags.into_iter().collect()
    }

    /// For callers that still take a `ScopeFile` (curate).
    pub fn file(&self) -> &ScopeFile {
        &self.file
    }

    #[cfg(test)]
    pub fn hiding(account_id: &str, folders: &[&str]) -> Self {
        let file: ScopeFile = serde_json::from_value(serde_json::json!({
            "accounts": { account_id: { "hidden_folders": folders } }
        }))
        .unwrap();
        Self { file }
    }
}

fn unreadable(detail: &str) -> String {
    format!("mcp_write_scope.json can't be read ({detail}); fix it before using AI.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{note, temp_db};

    const A: &str = "gmail:a@x.com";

    #[test]
    fn a_missing_scope_file_hides_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let ai = AiScope::load_from(&dir.path().join("mcp_write_scope.json")).unwrap();
        assert!(ai.allows(A, "Notes/Private"));
    }

    #[test]
    fn hidden_folders_and_their_subfolders_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("mcp_write_scope.json");
        std::fs::write(&p, r#"{"accounts":{"gmail:a@x.com":{"hidden_folders":["Notes/Private"]}}}"#).unwrap();
        let ai = AiScope::load_from(&p).unwrap();
        assert!(!ai.allows(A, "Notes/Private"));
        assert!(!ai.allows(A, "Notes/Private/Bank"));
        assert!(ai.allows(A, "Notes/PrivateX"), "a prefix sibling is not inside the hidden folder");
        assert!(ai.allows("gmail:other@x.com", "Notes/Private"), "hiding is per account");
        assert_eq!(ai.check_label(A, "Notes/Private/Bank").unwrap_err(), HIDDEN_NOTE);
        assert_eq!(ai.check_folder(A, "Notes/Private").unwrap_err(), HIDDEN_FOLDER);
    }

    #[test]
    fn a_legacy_bare_email_key_still_hides() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("mcp_write_scope.json");
        std::fs::write(&p, r#"{"accounts":{"a@x.com":{"hidden_folders":["Notes/Private"]}}}"#).unwrap();
        assert!(!AiScope::load_from(&p).unwrap().allows(A, "Notes/Private"));
    }

    #[test]
    fn an_unparseable_scope_file_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("mcp_write_scope.json");
        std::fs::write(&p, "{ not json").unwrap();
        let err = AiScope::load_from(&p).unwrap_err();
        assert!(err.starts_with("mcp_write_scope.json can't be read ("), "{err}");
        assert!(err.ends_with("fix it before using AI."), "{err}");
    }

    #[test]
    fn a_scope_path_that_exists_but_cannot_be_read_fails_closed() {
        // A directory where the file should be: read_to_string fails, so
        // mcp_scope::load_from says NotConfigured — which must NOT read as
        // "nothing hidden" here. Portable stand-in for a permission error.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("mcp_write_scope.json");
        std::fs::create_dir(&p).unwrap();
        assert!(AiScope::load_from(&p).is_err());
    }

    /// M3 (final review): `metadata` follows the link, so a dangling symlink
    /// read as NotFound — "nothing hidden". The path exists; refuse.
    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_at_the_scope_path_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("mcp_write_scope.json");
        std::os::unix::fs::symlink(dir.path().join("gone.json"), &p).unwrap();
        assert!(AiScope::load_from(&p).is_err());
    }

    #[test]
    fn ai_source_has_the_documented_wire_shape() {
        let n: AiSource = serde_json::from_str(r#"{"kind":"note","uuid":"u1"}"#).unwrap();
        assert!(matches!(n, AiSource::Note { uuid } if uuid == "u1"));
        let p: AiSource = serde_json::from_str(r#"{"kind":"pasted"}"#).unwrap();
        assert!(matches!(p, AiSource::Pasted));
        assert!(serde_json::from_str::<AiSource>(r#"{}"#).is_err(), "no silent default");
    }

    #[test]
    fn check_source_allows_pasted_text_and_notes_outside_hidden_folders() {
        let db = temp_db();
        note(A, "u-open").label("Notes/Work").insert(&db);
        let ai = AiScope::hiding(A, &["Notes/Private"]);
        assert!(ai.check_source(&db, A, &AiSource::Pasted).is_ok());
        assert!(ai.check_source(&db, A, &AiSource::Note { uuid: "u-open".into() }).is_ok());
    }

    #[test]
    fn check_source_refuses_a_note_in_a_hidden_folder_and_a_missing_note() {
        let db = temp_db();
        note(A, "u-hidden").label("Notes/Private/Bank").insert(&db);
        let ai = AiScope::hiding(A, &["Notes/Private"]);
        assert_eq!(
            ai.check_source(&db, A, &AiSource::Note { uuid: "u-hidden".into() }).unwrap_err(),
            HIDDEN_NOTE
        );
        assert!(ai.check_source(&db, A, &AiSource::Note { uuid: "nope".into() }).is_err());
    }

    #[test]
    fn check_source_reads_the_label_now_not_when_picked() {
        // The modal picked the note while it sat in Notes/Work; it was then
        // moved into the hidden folder. The command-time label decides.
        let db = temp_db();
        note(A, "u-moved").label("Notes/Work").insert(&db);
        let ai = AiScope::hiding(A, &["Notes/Private"]);
        let src = AiSource::Note { uuid: "u-moved".into() };
        assert!(ai.check_source(&db, A, &src).is_ok());
        db.apply_local_edit(
            "u-moved",
            A,
            "u-moved",
            "<div>u-moved</div><div>body of u-moved</div>",
            "Notes/Private",
            &crate::provenance::Actor::test(),
        )
        .unwrap();
        assert_eq!(ai.check_source(&db, A, &src).unwrap_err(), HIDDEN_NOTE);
    }

    /// C1 (final review): the Extract/workflow tag vocabulary went to the
    /// provider from `list_all_tags`, unfiltered — a tag that exists only on
    /// a hidden note leaked its name even for pasted text.
    #[test]
    fn tags_visible_to_ai_omits_tags_that_occur_only_in_hidden_folders() {
        let db = temp_db();
        note(A, "u-hidden").label("Notes/Private/Bank").body("statement #CANARYTAG #shared").insert(&db);
        note(A, "u-open").label("Notes/Work").body("standup #opentag #shared").insert(&db);
        note("gmail:other@x.com", "u-elsewhere").label("Notes/Work").body("#otheraccount").insert(&db);
        let ai = AiScope::hiding(A, &["Notes/Private"]);
        let tags = ai.tags_visible_to_ai(&db, A);
        assert_eq!(tags, vec!["opentag".to_string(), "shared".to_string()], "a tag on any allowed note stays; hidden-only goes");
        assert!(tags.iter().all(|t| !t.to_lowercase().contains("canary")), "{tags:?}");
        // Nothing hidden: every tag of this account is offered, as before.
        let all = AiScope::default().tags_visible_to_ai(&db, A);
        assert_eq!(all.len(), 3, "{all:?}");
    }

    #[test]
    fn the_tag_vocabulary_in_every_prompt_never_names_a_hidden_only_tag() {
        use crate::llm::provider::WorkflowKind;
        let db = temp_db();
        note(A, "u-hidden").label("Notes/Private").body("#CANARYTAG").insert(&db);
        note(A, "u-open").label("Notes/Work").body("#opentag").insert(&db);
        let ai = AiScope::hiding(A, &["Notes/Private"]);
        let vocab = ai.tags_visible_to_ai(&db, A);
        let mut prompts = vec![crate::llm::prompt::extract_system_prompt(&vocab)];
        for w in [WorkflowKind::Summarize, WorkflowKind::ActionItems, WorkflowKind::ExpandBullets, WorkflowKind::CleanTranscript] {
            prompts.push(crate::llm::prompt::workflow_system_prompt(w, &vocab));
        }
        for p in &prompts {
            assert!(!p.to_lowercase().contains("canarytag"), "hidden tag in prompt: {p}");
        }
        assert!(prompts[0].contains("opentag"), "the allowed vocabulary is still offered");
    }

    #[test]
    fn refused_displays_verbatim() {
        let e = crate::llm::provider::ExtractError::Refused(HIDDEN_NOTE.into());
        assert_eq!(e.to_string(), HIDDEN_NOTE);
    }
}
