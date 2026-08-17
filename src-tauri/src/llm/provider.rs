use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTurn {
    pub role: ChatRole,
    pub content: String,
}

/// Render a system prompt + conversation as one plain-text transcript, for
/// providers that take a single prompt string rather than a message array
/// (every agent CLI). Kept as a free function so it is unit-testable without
/// spawning a subprocess.
pub fn flatten_turns(system: &str, turns: &[ChatTurn]) -> String {
    let mut out = String::with_capacity(system.len() + 256);
    out.push_str(system);
    out.push_str("\n\n");
    for t in turns {
        let label = match t.role {
            ChatRole::User => "User:",
            ChatRole::Assistant => "Assistant:",
        };
        out.push_str(label);
        out.push('\n');
        out.push_str(&t.content);
        out.push_str("\n\n");
    }
    out.push_str("Assistant:");
    out
}

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("provider not configured: {0}")]
    NotConfigured(String),
    #[error("transport error: {0}")]
    Transport(String),
    #[error("malformed envelope: {reason}")]
    MalformedEnvelope { reason: String, raw: String },
    #[error("upstream error: {0}")]
    UpstreamError(String),
    /// Constructed when the caller cancelled the in-flight extract via a
    /// CancellationToken — for example, the user clicked Cancel on the
    /// extraction modal. The caller should distinguish this from a real
    /// error and NOT create the source-preservation fallback note: the user
    /// actively chose to abort, not "lose" their paste.
    #[error("cancelled")]
    Cancelled,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ExtractEnvelope {
    pub title: Option<String>,
    pub lessons_markdown: String,
    #[serde(default)]
    pub meta_lessons_markdown: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub confidence: Option<String>,
}

/// A candidate related note, summarized for the LLM's relatedness judgment
/// (design spec Step 2) — title + a short snippet, not the full body, to
/// keep the prompt compact when many candidates are involved.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct CandidateSummary {
    pub uuid: String,
    pub title: String,
    pub snippet: String,
}

/// One candidate's relatedness judgment — see LINK_SUGGESTION_SYSTEM_PROMPT
/// for the exact contract the LLM must follow.
#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct LinkSuggestion {
    pub uuid: String,
    pub related: bool,
    #[serde(default)]
    pub should_append: bool,
    #[serde(default)]
    pub addition_text: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct LinkSuggestionsEnvelope {
    #[serde(default)]
    pub suggestions: Vec<LinkSuggestion>,
}

#[async_trait::async_trait]
pub trait LlmProvider: Send + Sync {
    /// Run the extraction. The CancellationToken signals user-initiated
    /// abort — implementations should race their I/O against `cancel.cancelled()`
    /// and return `ExtractError::Cancelled` when the token fires. For the
    /// HTTP provider this means dropping the in-flight reqwest future; for
    /// the subprocess provider it means killing the child process.
    async fn extract(
        &self,
        source: &str,
        cancel: CancellationToken,
    ) -> Result<ExtractEnvelope, ExtractError>;

    /// Judge which of `candidates` are related to `source` and whether each
    /// warrants a one-line addition to that existing note. Same cancellation
    /// contract as `extract`. NOTE: this trait method and the free function
    /// `crate::llm::autolink::suggest_links` share a name but are two
    /// distinct things at two different layers — this is the raw LLM call;
    /// the free function in autolink.rs orchestrates keyword extraction +
    /// candidate search + this call + placeholder substitution. Same
    /// relationship as `extract` (this trait) vs `extract_note` (the
    /// Tauri command that orchestrates around it).
    async fn suggest_links(
        &self,
        source: &str,
        candidates: &[CandidateSummary],
        cancel: CancellationToken,
    ) -> Result<LinkSuggestionsEnvelope, ExtractError>;

    /// Multi-turn free-text completion. Returns the model's raw text — there
    /// is no JSON envelope, because an answer is prose, not a schema. This
    /// also means the single-retry-on-malformed-envelope behavior in the
    /// agent-CLI provider does not apply here: there is no envelope to
    /// malform. Same cancellation contract as `extract`.
    async fn chat(
        &self,
        system: &str,
        turns: &[ChatTurn],
        cancel: CancellationToken,
    ) -> Result<String, ExtractError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_parses_full_response() {
        let json = "{ \"title\": \"Test lesson\", \"lessons_markdown\": \"## Lesson 1\\nbody\", \"meta_lessons_markdown\": \"## Meta\\nbody\", \"tags\": [\"tag-a\", \"tag-b\"], \"confidence\": \"high\" }";
        let env: ExtractEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(env.title.as_deref(), Some("Test lesson"));
        assert_eq!(env.tags.len(), 2);
    }

    #[test]
    fn envelope_parses_minimal_response() {
        // Optional fields all missing — only lessons_markdown required.
        let json = "{ \"lessons_markdown\": \"## L1\\nbody\" }";
        let env: ExtractEnvelope = serde_json::from_str(json).unwrap();
        assert!(env.title.is_none());
        assert!(env.tags.is_empty());
        assert!(env.meta_lessons_markdown.is_none());
    }

    #[test]
    fn flatten_turns_labels_roles_and_keeps_order() {
        let turns = vec![
            ChatTurn { role: ChatRole::User, content: "first question".into() },
            ChatTurn { role: ChatRole::Assistant, content: "an answer".into() },
            ChatTurn { role: ChatRole::User, content: "follow-up".into() },
        ];
        let out = flatten_turns("SYSTEM RULES", &turns);
        let first = out.find("first question").unwrap();
        let second = out.find("an answer").unwrap();
        let third = out.find("follow-up").unwrap();
        assert!(first < second && second < third, "turn order must be preserved");
        assert!(out.starts_with("SYSTEM RULES"), "system prompt leads the transcript");
        assert!(out.contains("User:") && out.contains("Assistant:"));
    }

    #[test]
    fn flatten_turns_handles_a_single_turn() {
        let turns = vec![ChatTurn { role: ChatRole::User, content: "only".into() }];
        let out = flatten_turns("S", &turns);
        assert!(out.contains("only"));
        // The trailing "Assistant:" cues the model to respond. A single user turn
        // produces no assistant TURN in the conversation, but the cue still appears.
        assert!(out.trim_end().ends_with("Assistant:"));
        assert_eq!(out.matches("Assistant:").count(), 1);
    }
}
