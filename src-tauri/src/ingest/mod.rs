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

pub mod audio;
pub mod discuss;
pub mod facebook;
pub mod instagram;
pub mod links;
pub mod net;
pub mod peek;
pub mod quality;
pub mod render;
pub mod robots;
pub mod run;
pub mod social;
pub mod social_fetch;
pub mod stored;
pub mod tiktok;
pub mod web;
pub mod x;
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
    #[serde(rename = "tiktok")]
    TikTok,
    #[serde(rename = "instagram")]
    Instagram,
    #[serde(rename = "facebook")]
    Facebook,
    #[serde(rename = "x")]
    X,
    /// Threads whose replies are the content (spec 2026-10-28 §6.2).
    #[serde(rename = "bluesky")]
    Bluesky,
    #[serde(rename = "mastodon")]
    Mastodon,
    #[serde(rename = "hackernews")]
    HackerNews,
}

impl SourceKind {
    /// The four platforms whose text opens with D14's provenance header.
    pub fn is_social(self) -> bool {
        matches!(self, SourceKind::TikTok | SourceKind::Instagram | SourceKind::Facebook | SourceKind::X)
    }

    /// A post plus its replies, read by `discuss` (spec 2026-10-28 §6.2).
    pub fn is_thread(self) -> bool {
        matches!(self, SourceKind::Bluesky | SourceKind::Mastodon | SourceKind::HackerNews)
    }
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

    /// `fetch`, telling `on_stage` when a long step inside it begins
    /// (`Downloading`, `Transcribing`) — the caller turns each into progress
    /// with its own counts. A fetcher with no such steps keeps this default.
    async fn fetch_reporting(
        &self,
        url: &str,
        kind: SourceKind,
        cancel: CancellationToken,
        _on_stage: &(dyn Fn(run::IngestStage) + Send + Sync),
    ) -> FetchedSource {
        self.fetch(url, kind, cancel).await
    }

    /// Whether `url`'s robots.txt lets Jodd open it — asked only for a link
    /// Jodd followed (spec 2026-10-28 D11). Fakes allow everything.
    async fn robots_allow(&self, _url: &str, _cancel: CancellationToken) -> bool {
        true
    }
}

/// The real fetcher: web pages through `web`, videos through `youtube`, the
/// four social platforms through `social_fetch` — all behind the SSRF guard.
/// No `Debug`: `stt` carries the speech-to-text key.
#[derive(Clone, Default)]
pub struct HttpFetcher {
    pub policy: FetchPolicy,
    pub youtube: youtube::YoutubeEndpoints,
    /// Read once per ingest (`stt_config::ready()` reads the keychain);
    /// `None` is "speech-to-text is not set up" (D6).
    pub stt: Option<crate::stt_config::SttReady>,
    /// "Transcribe the audio myself" (D2): speech-to-text even where the
    /// platform has its own transcript.
    pub prefer_own_transcription: bool,
    /// robots.txt per origin, read once per ingest (one `HttpFetcher` is
    /// built per ingest command).
    pub robots: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<String, robots::Rules>>>,
    /// Where the thread readers send their requests (tests aim them at a mock).
    pub threads: discuss::ThreadEndpoints,
    /// Set when the user saved a YouTube Data API key (`youtube_key::ready`):
    /// a video's public comments are read with it. No `Debug` anywhere on the
    /// way — the key is in it.
    pub youtube_comments: Option<discuss::youtube::CommentsApi>,
    /// Pages the private reader rendered for this ingest (spec 2026-10-28
    /// §7.3), by source URL: read from here instead of fetched again.
    pub rendered: std::sync::Arc<std::collections::HashMap<String, render::RenderedPage>>,
    /// Tests only: mockito's host, standing in for every platform's media
    /// (and TikTok caption) host allowlist, which `allow_loopback` never
    /// opens. Absent from every non-test build.
    #[cfg(test)]
    pub test_media_hosts: Option<&'static [&'static str]>,
}

#[async_trait::async_trait]
impl SourceFetcher for HttpFetcher {
    async fn fetch(&self, url: &str, kind: SourceKind, cancel: CancellationToken) -> FetchedSource {
        self.fetch_reporting(url, kind, cancel, &|_| {}).await
    }

    async fn fetch_reporting(
        &self,
        url: &str,
        kind: SourceKind,
        cancel: CancellationToken,
        on_stage: &(dyn Fn(run::IngestStage) + Send + Sync),
    ) -> FetchedSource {
        if let (SourceKind::Web, Some(page)) = (kind, self.rendered.get(url)) {
            return web::read_rendered(page, self.policy, &cancel).await;
        }
        match (kind, urls::classify(url)) {
            (SourceKind::YouTube, urls::UrlKind::YouTube { id }) => {
                let mut s = youtube::fetch_youtube(url, &id, &self.youtube, self.policy, &cancel).await;
                // Spec 2026-10-28 §6.2: comments ride along when a key is saved.
                // Best effort — comments turned off, a used-up quota or a bad key
                // leave the video as it was, and the log says which.
                if let (Some(api), true) = (&self.youtube_comments, s.is_usable()) {
                    match discuss::youtube::read(api, &id, self.policy, &cancel).await {
                        Ok(d) => s.text = discuss::append_to(&s.text, &d),
                        Err(e) => crate::log!("ingest: YouTube comments not read — {e}"),
                    }
                }
                s
            }
            (_, urls::UrlKind::Unsupported(reason)) => FetchedSource::failed(url, kind, reason),
            (k, _) if k.is_social() => social_fetch::fetch(self, url, kind, &cancel, on_stage).await,
            (k, _) if k.is_thread() => discuss::fetch_thread(url, k, &self.threads, self.policy, &cancel).await,
            _ => web::fetch_web(url, self.policy, &cancel).await,
        }
    }

    async fn robots_allow(&self, url: &str, cancel: CancellationToken) -> bool {
        let Ok(parsed) = reqwest::Url::parse(url) else { return true };
        let Some(origin) = robots::origin_of(&parsed) else { return true };
        // Held across the fetch: two followed links on one site read it once.
        let mut cache = self.robots.lock().await;
        if !cache.contains_key(&origin) {
            let rules = robots::fetch(&origin, self.policy, &cancel).await;
            cache.insert(origin.clone(), rules);
        }
        cache[&origin].allows(&robots::path_of(&parsed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec 2026-10-28 §6.2: with a saved key, a video's comments ride along;
    /// with none, the video reads exactly as before.
    #[tokio::test]
    async fn a_youtube_fetch_carries_comments_only_with_a_key() {
        let mut server = mockito::Server::new_async().await;
        let mut player: serde_json::Value = serde_json::from_str(include_str!("fixtures/yt_player_ios_en.json")).unwrap();
        player["captions"] = serde_json::Value::Null;
        let _p = server.mock("POST", "/youtubei/v1/player").match_query(mockito::Matcher::Any).with_body(player.to_string()).create_async().await;
        let threads = serde_json::json!({"items": [{"snippet": {"channelId": "UCc", "topLevelComment": {"snippet": {
            "textOriginal": "Full code: https://github.com/a/b", "authorChannelId": {"value": "UCc"}, "likeCount": 9}}}}]});
        let comments = server.mock("GET", "/commentThreads").match_query(mockito::Matcher::Any).with_body(threads.to_string()).expect(1).create_async().await;
        let youtube = youtube::YoutubeEndpoints {
            player_url: format!("{}/youtubei/v1/player?prettyPrint=false", server.url()),
            watch_url_prefix: format!("{}/watch?v=", server.url()),
        };
        let url = "https://www.youtube.com/watch?v=jXtnhyro-QE";
        let plain = HttpFetcher { policy: FetchPolicy { allow_loopback: true }, youtube: youtube.clone(), ..HttpFetcher::default() };
        let before = plain.fetch(url, SourceKind::YouTube, CancellationToken::new()).await;
        assert!(!before.text.contains(discuss::COMMENTS_HEADER));
        let keyed = HttpFetcher {
            youtube_comments: Some(discuss::youtube::CommentsApi { base: server.url(), key: "K".into() }),
            ..plain
        };
        let after = keyed.fetch(url, SourceKind::YouTube, CancellationToken::new()).await;
        comments.assert_async().await;
        assert!(after.text.starts_with(&before.text), "the video's own text is unchanged");
        assert!(after.text.contains("\nAuthor (9 likes): Full code: https://github.com/a/b"), "{}", after.text);
        assert_eq!(discuss::without_discussion(&after.text), before.text);
    }

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
        for (kind, spelled) in [
            (SourceKind::TikTok, "tiktok"),
            (SourceKind::Instagram, "instagram"),
            (SourceKind::Facebook, "facebook"),
            (SourceKind::X, "x"),
        ] {
            assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{spelled}\""));
        }
    }
}
