//! URL ingest — fetch the content behind links (web pages, YouTube
//! transcripts) so Extract condenses the source, not its URL.
//!
//! Jodd fetches; the LLM never holds a tool (spec Decision 1). One failing
//! source never fails the ingest (Decision 2), which is why fetching has no
//! error type: every outcome is a `FetchedSource` with a status.
//!
//! See docs/superpowers/specs/2026-09-15-url-ingest-design.md.

use serde::Serialize;
use tokio_util::sync::CancellationToken;

pub mod urls;

pub mod net;
pub mod run;
pub mod stored;
pub mod web;
pub mod youtube;

pub use net::FetchPolicy;

/// Measured link-collection notes hold 3–6+ links (spec "Size budget").
pub const MAX_URLS_PER_INGEST: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SourceKind {
    #[serde(rename = "web")]
    Web,
    /// Spelled out: `rename_all = "snake_case"` would say `you_tube`.
    #[serde(rename = "youtube")]
    YouTube,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchStatus {
    Ok,
    /// Usable, but not the whole source — e.g. a video's title and
    /// description without its transcript.
    Partial(String),
    Failed(String),
}

impl FetchStatus {
    pub fn is_usable(&self) -> bool {
        !matches!(self, FetchStatus::Failed(_))
    }

    /// The one spelling used in the stored block, the `## Sources` list and
    /// logs. `parse_label` is its inverse.
    pub fn label(&self) -> String {
        match self {
            FetchStatus::Ok => "ok".to_string(),
            FetchStatus::Partial(r) => format!("partial: {r}"),
            FetchStatus::Failed(r) => format!("failed: {r}"),
        }
    }

    pub fn parse_label(s: &str) -> Option<Self> {
        if s == "ok" {
            return Some(FetchStatus::Ok);
        }
        if let Some(r) = s.strip_prefix("partial: ") {
            return Some(FetchStatus::Partial(r.to_string()));
        }
        s.strip_prefix("failed: ").map(|r| FetchStatus::Failed(r.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedSource {
    pub url: String,
    pub kind: SourceKind,
    pub title: Option<String>,
    pub text: String,
    pub status: FetchStatus,
}

impl FetchedSource {
    pub fn failed(url: &str, kind: SourceKind, reason: impl Into<String>) -> Self {
        FetchedSource { url: url.to_string(), kind, title: None, text: String::new(), status: FetchStatus::Failed(reason.into()) }
    }

    /// Worth an LLM call: not failed, and carrying text.
    pub fn is_usable(&self) -> bool {
        self.status.is_usable() && !self.text.trim().is_empty()
    }
}

/// One real implementation (`HttpFetcher`, Task 6); fakes in tests.
#[async_trait::async_trait]
pub trait SourceFetcher: Send + Sync {
    /// Never errors — see the module comment. Must race `cancel`; a cancelled
    /// fetch may return anything, because the caller checks the token next.
    async fn fetch(&self, url: &str, kind: SourceKind, cancel: CancellationToken) -> FetchedSource;
}

/// The real fetcher: web pages through `web`, videos through `youtube`, both
/// behind the SSRF guard.
#[derive(Debug, Clone, Default)]
pub struct HttpFetcher {
    pub policy: FetchPolicy,
    pub youtube: youtube::YoutubeEndpoints,
}

#[async_trait::async_trait]
impl SourceFetcher for HttpFetcher {
    async fn fetch(&self, url: &str, kind: SourceKind, cancel: CancellationToken) -> FetchedSource {
        match (kind, urls::classify(url)) {
            (SourceKind::YouTube, urls::UrlKind::YouTube { id }) => {
                youtube::fetch_youtube(url, &id, &self.youtube, self.policy, &cancel).await
            }
            (_, urls::UrlKind::Unsupported(reason)) => FetchedSource::failed(url, kind, reason),
            _ => web::fetch_web(url, self.policy, &cancel).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_labels_round_trip() {
        for s in [FetchStatus::Ok, FetchStatus::Partial("no captions".into()), FetchStatus::Failed("HTTP 404".into())] {
            assert_eq!(FetchStatus::parse_label(&s.label()), Some(s));
        }
        assert_eq!(FetchStatus::parse_label("weird"), None);
    }

    #[test]
    fn source_kind_serializes_as_the_frontend_spells_it() {
        assert_eq!(serde_json::to_string(&SourceKind::YouTube).unwrap(), "\"youtube\"");
        assert_eq!(serde_json::to_string(&SourceKind::Web).unwrap(), "\"web\"");
    }
}
