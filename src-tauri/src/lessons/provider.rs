use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

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

#[async_trait::async_trait]
pub trait LessonProvider: Send + Sync {
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
}
