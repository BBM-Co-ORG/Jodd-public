//! Ask Jodd — read-only, ephemeral question answering over the SQLite cache.
//!
//! Pipeline per turn (spec §5):
//!   1. pool.rs     SQL pre-filter  → ≤ CANDIDATE_POOL_MAX candidates
//!   2. catalog.rs  compact catalog → one line per candidate
//!   3. LLM call 1  the model picks uuid8s from the catalog
//!   4. LLM call 2  answer from the selected bodies, with citations
//!
//! Nothing here writes: no notes, no folders, no edges, no sidecars.
//! See docs/superpowers/specs/2026-07-29-ask-jodd-design.md

pub mod catalog;
pub mod context;
pub mod pool;
pub mod prompt;
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AskScope {
    AllAccounts,
    Account { account_id: String },
    Folder { account_id: String, label: String },
}

impl AskScope {
    pub fn account_id(&self) -> Option<&str> {
        match self {
            AskScope::AllAccounts => None,
            AskScope::Account { account_id } => Some(account_id),
            AskScope::Folder { account_id, .. } => Some(account_id),
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

#[cfg(test)]
mod tests {
    use super::*;
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

        // AskAnswer — all seven fields, with CitedNote's four nested inside.
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
            })
        );
    }
}
