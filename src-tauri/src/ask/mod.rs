//! Ask Jodd — read-only question answering over the SQLite cache. It never
//! writes notes, folders, edges or sidecars; answered turns are kept in its
//! own history tables (history.rs, spec 2026-10-14) unless the owner turns
//! that off.
//!
//! Pipeline per turn (spec §5):
//!   1. pool.rs     SQL pre-filter  → ≤ CANDIDATE_POOL_MAX candidates
//!   2. catalog.rs  compact catalog → one line per candidate
//!   3. LLM call 1  the model picks uuid8s from the catalog
//!   4. LLM call 2  answer from the selected bodies, with citations
//!
//! Nothing in the answer pipeline writes notes, folders, edges or sidecars;
//! history is the one exception (see above).
//! See docs/superpowers/specs/2026-07-29-ask-jodd-design.md

pub mod catalog;
pub mod context;
pub mod history;
pub mod pool;
pub mod prompt;
pub mod related;
pub mod run;
pub mod terms;

use serde::{Deserialize, Serialize};

/// Hard cap on the candidate pool. The catalog is built only from the pool,
/// so this is what bounds prompt size regardless of vault size (spec F1: a
/// full catalog of the live 6,655-note account would be ~150k tokens).
pub const CANDIDATE_POOL_MAX: usize = 400;
/// How many most-recently-modified notes join the pool as a prior.
pub const RECENCY_K: usize = 150;
/// Most notes whose bodies go into the answer call.
pub const MAX_SELECTED_NOTES: usize = 12;
/// Per-note character cap, applied BEFORE the total. The largest live note is
/// 1,037,880 chars — without this, one note evicts every other selection.
pub const MAX_NOTE_CHARS: usize = 20_000;
/// Total stripped-body characters across all selected notes.
pub const MAX_CONTEXT_CHARS: usize = 120_000;

/// Which notes a question may draw on. Folder scope is recursive — see
/// Db::list_notes_in_subtree and spec §5.6.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AskScope {
    AllAccounts,
    Account { account_id: String },
    Folder { account_id: String, label: String },
    /// One note, optionally widened to its linked and same-tag notes up to
    /// `depth` steps (spec 2026-10-23). Reached only from the note context
    /// menu.
    Note {
        account_id: String,
        uuid: String,
        /// Display only — the title when the scope was chosen, for the
        /// history list. Never used for retrieval.
        #[serde(default)]
        title: String,
        #[serde(default)]
        links: bool,
        #[serde(default)]
        tags: bool,
        /// Read through `note_depth()`, which clamps to 1..=NOTE_DEPTH_MAX.
        #[serde(default = "default_note_depth")]
        depth: u8,
    },
}

/// The deepest a note scope's related-notes walk may go.
pub const NOTE_DEPTH_MAX: u8 = 3;

/// Refusal when a note scope's note was deleted or never existed.
pub const NOTE_GONE: &str = "This note is no longer available.";

fn default_note_depth() -> u8 {
    1
}

impl AskScope {
    pub fn account_id(&self) -> Option<&str> {
        match self {
            AskScope::AllAccounts => None,
            AskScope::Account { account_id } => Some(account_id),
            AskScope::Folder { account_id, .. } => Some(account_id),
            AskScope::Note { account_id, .. } => Some(account_id),
        }
    }

    /// The walk depth, clamped to 1..=NOTE_DEPTH_MAX; 0 for any other scope.
    pub fn note_depth(&self) -> u8 {
        match self {
            AskScope::Note { depth, .. } => (*depth).clamp(1, NOTE_DEPTH_MAX),
            _ => 0,
        }
    }

    pub fn label(&self) -> Option<&str> {
        match self {
            AskScope::Folder { label, .. } => Some(label),
            _ => None,
        }
    }
}

/// One note in the candidate pool — enough to write a catalog line, never the
/// body. Bodies are fetched only for the ≤12 notes the model actually selects.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub uuid: String,
    pub account_id: String,
    /// First 8 hex chars of the uuid — the id the model cites and selects by.
    pub uuid8: String,
    pub title: String,
    pub label: String,
    pub tags: Vec<String>,
    /// Epoch ms, from last_remote_modified_at (never from `date`, spec F4).
    pub date_ms: i64,
    /// True when this note came from the FTS source. Drives fill order when
    /// the pool cap binds, and trim order in the answer stage: it is the only
    /// source with evidence of relevance to *this* question.
    pub from_fts: bool,
}

/// One note's text as it goes into the answer call.
#[derive(Debug, Clone)]
pub struct SelectedNote {
    pub uuid: String,
    pub account_id: String,
    pub uuid8: String,
    pub title: String,
    /// `<title-slug>-<uuid8>` — the form the model must cite (db::note_slug).
    pub slug: String,
    /// HTML-stripped body, already capped at MAX_NOTE_CHARS.
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, Default)]
pub struct AnswerContext {
    pub notes: Vec<SelectedNote>,
    /// True when a cap dropped at least one selected note, so the UI can say so.
    pub trimmed: bool,
}

/// A note the answer cited, resolved back to something the UI can open.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CitedNote {
    pub uuid: String,
    pub account_id: String,
    pub title: String,
    pub slug: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskAnswer {
    pub markdown: String,
    pub cited: Vec<CitedNote>,
    /// Total notes the scope contains — the denominator in
    /// "6,655 in scope → 400 considered → 9 read".
    pub notes_in_scope: usize,
    /// Size of the candidate pool. Together with notes_in_scope this makes
    /// the pre-filter's recall ceiling visible instead of leaving the user to
    /// infer it from a weak answer (spec §5.1).
    pub notes_considered: usize,
    pub notes_used: usize,
    /// A budget cap dropped at least one selected note.
    pub trimmed: bool,
    /// Citations the model produced for ids that were never in context. They
    /// are stripped from `markdown`; this is the count.
    pub dropped_citations: usize,
    /// Every note whose text went into the answer call. Kept for history
    /// (resume's R2/R4) and never sent to the frontend.
    #[serde(skip)]
    pub read: Vec<crate::ask::history::NoteRef>,
    /// The saved conversation this turn belongs to; `None` when history is
    /// off or the save failed. Set by `ask_jodd`, never by `run_ask`.
    #[serde(default)]
    pub conversation_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ask::history::NoteRef;
    use crate::llm::provider::{ChatRole, ChatTurn};
    use serde_json::json;

    /// Pins the exact JSON that crosses the `ask_jodd` IPC boundary.
    ///
    /// The frontend consumes these shapes through hand-written TypeScript
    /// (AskJoddModal.svelte's type block, askScope.ts) that nothing else
    /// cross-checks: rename a field, drop `#[serde(tag = "kind")]`, or add a
    /// `rename_all` and every other test in the repo stays green while the UI
    /// silently renders "undefined in scope" or `invoke` rejects into a red
    /// chat bubble. Comparing whole values (not field-by-field) makes this
    /// fail on additions and removals too — if you add a field on purpose,
    /// update this test AND the TS type in the same commit.
    fn note(depth: u8) -> AskScope {
        AskScope::Note {
            account_id: "gmail:a@x".into(),
            uuid: "u1".into(),
            title: "T".into(),
            links: true,
            tags: false,
            depth,
        }
    }

    #[test]
    fn note_scope_round_trips() {
        let v = serde_json::to_value(note(2)).unwrap();
        assert_eq!(
            v,
            json!({ "kind": "note", "account_id": "gmail:a@x", "uuid": "u1",
                    "title": "T", "links": true, "tags": false, "depth": 2 })
        );
        assert_eq!(serde_json::from_value::<AskScope>(v).unwrap(), note(2));
    }

    #[test]
    fn note_scope_defaults_missing_fields() {
        let s: AskScope =
            serde_json::from_value(json!({ "kind": "note", "account_id": "a", "uuid": "u" })).unwrap();
        assert_eq!(
            s,
            AskScope::Note { account_id: "a".into(), uuid: "u".into(), title: String::new(),
                             links: false, tags: false, depth: 1 }
        );
    }

    #[test]
    fn note_depth_is_clamped() {
        assert_eq!(note(0).note_depth(), 1);
        assert_eq!(note(2).note_depth(), 2);
        assert_eq!(note(9).note_depth(), NOTE_DEPTH_MAX);
        assert_eq!(AskScope::AllAccounts.note_depth(), 0);
    }

    #[test]
    fn note_scope_account_id() {
        assert_eq!(note(1).account_id(), Some("gmail:a@x"));
        assert_eq!(note(1).label(), None);
    }

    #[test]
    fn ask_wire_shapes_are_stable() {
        // AskScope — internally tagged, snake_case variant names.
        assert_eq!(
            serde_json::to_value(AskScope::AllAccounts).unwrap(),
            json!({ "kind": "all_accounts" })
        );
        assert_eq!(
            serde_json::to_value(AskScope::Account {
                account_id: "a@x".into()
            })
            .unwrap(),
            json!({ "kind": "account", "account_id": "a@x" })
        );
        assert_eq!(
            serde_json::to_value(AskScope::Folder {
                account_id: "a@x".into(),
                label: "Notes/A".into(),
            })
            .unwrap(),
            json!({ "kind": "folder", "account_id": "a@x", "label": "Notes/A" })
        );

        // ChatTurn — role is snake_case, matching the TS `'user' | 'assistant'`.
        assert_eq!(
            serde_json::to_value(ChatTurn {
                role: ChatRole::User,
                content: "hi".into(),
            })
            .unwrap(),
            json!({ "role": "user", "content": "hi" })
        );
        assert_eq!(
            serde_json::to_value(ChatTurn {
                role: ChatRole::Assistant,
                content: "yo".into(),
            })
            .unwrap(),
            json!({ "role": "assistant", "content": "yo" })
        );

        // AskAnswer — all eight wire fields (`read` is `serde(skip)` and must NOT
        // appear), with CitedNote's four nested inside.
        assert_eq!(
            serde_json::to_value(AskAnswer {
                markdown: "We chose keep-both. [[sync-conflicts-aabbccdd]]".into(),
                cited: vec![CitedNote {
                    uuid: "aabbccdd-0000-0000-0000-000000000000".into(),
                    account_id: "a@x".into(),
                    title: "Sync conflicts".into(),
                    slug: "sync-conflicts-aabbccdd".into(),
                }],
                notes_in_scope: 6655,
                notes_considered: 400,
                notes_used: 9,
                trimmed: true,
                dropped_citations: 2,
                read: vec![NoteRef {
                    uuid: "x".into(),
                    account_id: "a@x".into(),
                }],
                conversation_id: Some("conv-1".into()),
            })
            .unwrap(),
            json!({
                "markdown": "We chose keep-both. [[sync-conflicts-aabbccdd]]",
                "cited": [{
                    "uuid": "aabbccdd-0000-0000-0000-000000000000",
                    "account_id": "a@x",
                    "title": "Sync conflicts",
                    "slug": "sync-conflicts-aabbccdd",
                }],
                "notes_in_scope": 6655,
                "notes_considered": 400,
                "notes_used": 9,
                "trimmed": true,
                "dropped_citations": 2,
                "conversation_id": "conv-1",
            })
        );
    }
}
