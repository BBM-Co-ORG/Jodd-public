//! A link's caption and author, for the share sheet's title (spec D9, §5.6).
//!
//! **Metadata only, one page per link.** Never media, never a transcript, never
//! a caption file: the sheet fills a title field while the user is still
//! deciding whether to save, so the request has to be cheap and has no business
//! sending anything to speech-to-text. It goes through the same guarded
//! fetchers as ingest — the SSRF guard and the platform constants are not
//! duplicated here.

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::social::PlatformMeta;
use super::urls::{self, UrlKind};
use super::youtube::{self, Playability, YoutubeEndpoints};
use super::{facebook, instagram, tiktok, x, FetchPolicy};

/// The whole peek, all of its requests together. It is fire-and-forget on the
/// sheet's side, so nothing cancels it; this is what stops it lingering.
pub const PEEK_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkMeta {
    pub caption: Option<String>,
    pub author: Option<String>,
}

/// `None`: not a recognised video link, or nothing usable came back. A failure
/// is silent by design — the host title stays.
pub async fn peek(url: &str) -> Option<LinkMeta> {
    let endpoints = YoutubeEndpoints::default();
    let work = peek_with(url, &endpoints, FetchPolicy::default());
    tokio::time::timeout(PEEK_TIMEOUT, work).await.ok().flatten()
}

async fn peek_with(url: &str, youtube_endpoints: &YoutubeEndpoints, policy: FetchPolicy) -> Option<LinkMeta> {
    // Nothing cancels a peek (see PEEK_TIMEOUT), so the token is never tripped.
    let cancel = CancellationToken::new();
    match urls::classify(url) {
        UrlKind::YouTube { id } => peek_youtube(&id, youtube_endpoints, policy, &cancel).await,
        UrlKind::TikTok { .. } => from_platform(tiktok::peek_meta(url, policy, &cancel).await),
        UrlKind::Instagram { shortcode } => from_platform(instagram::peek_meta(&shortcode, policy, &cancel).await),
        UrlKind::Facebook { .. } => from_platform(facebook::fetch_meta(url, policy, &cancel).await),
        UrlKind::X { id } => from_platform(x::fetch_meta(&id, policy, &cancel).await),
        UrlKind::Web | UrlKind::Unsupported(_) => None,
    }
}

/// The player request alone: its title and channel. `fetch_youtube` always
/// goes on to request captions, so it is not reused.
async fn peek_youtube(id: &str, endpoints: &YoutubeEndpoints, policy: FetchPolicy, cancel: &CancellationToken) -> Option<LinkMeta> {
    let info = youtube::request_player(id, endpoints, policy, cancel).await.ok()?;
    (info.playability == Playability::Ok).then_some(LinkMeta { caption: info.title, author: info.author })
}

fn from_platform(meta: PlatformMeta) -> Option<LinkMeta> {
    if meta.unavailable.is_some() {
        return None;
    }
    let caption = meta.caption.or(meta.title);
    (caption.is_some() || meta.author.is_some()).then_some(LinkMeta { caption, author: meta.author })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Matcher;

    const LOOPBACK: FetchPolicy = FetchPolicy { allow_loopback: true };
    const TIKTOK_PAGE: &str = include_str!("fixtures/tiktok_video.html");
    const REEL_PAGE: &str = include_str!("fixtures/instagram_reel_mobile.html");
    const YT_PLAYER: &str = include_str!("fixtures/yt_player_ios_en.json");

    #[tokio::test]
    async fn tiktok_is_one_page_request_and_never_the_caption_file() {
        let mut server = mockito::Server::new_async().await;
        // The fixture's subtitle URL points at tiktok.com; aim it at the mock so
        // a request for it WOULD be seen.
        let page = TIKTOK_PAGE.replace("https://v16-webapp.tiktok.com", &format!("{}/subs", server.url()));
        let page_mock = server.mock("GET", "/video").with_status(200).with_body(page).expect(1).create_async().await;
        let subtitle = server.mock("GET", Matcher::Regex("^/subs".into())).expect(0).create_async().await;
        let meta = tiktok::peek_meta(&format!("{}/video", server.url()), LOOPBACK, &CancellationToken::new()).await;
        page_mock.assert_async().await;
        subtitle.assert_async().await;
        let got = from_platform(meta).expect("a caption");
        assert_eq!(got.author.as_deref(), Some("@tonsr_ (Lazy_Productive)"));
        assert!(got.caption.unwrap().starts_with("5 skill ใน claude code"));
    }

    #[tokio::test]
    async fn youtube_is_the_player_request_and_never_captions() {
        let mut server = mockito::Server::new_async().await;
        // A player response with a caption track whose URL points at the mock,
        // and a channel — the trimmed fixture carries neither.
        let local = YT_PLAYER.replace("https://www.youtube.com/api/timedtext", &format!("{}/api/timedtext", server.url()));
        let mut v: serde_json::Value = serde_json::from_str(&local).unwrap();
        v["videoDetails"]["author"] = "The Channel".into();
        let player = server.mock("POST", "/youtubei/v1/player").match_query(Matcher::Any).with_status(200).with_body(v.to_string()).expect(1).create_async().await;
        let captions = server.mock("GET", Matcher::Regex("^/api/timedtext".into())).expect(0).create_async().await;
        let endpoints = YoutubeEndpoints { player_url: format!("{}/youtubei/v1/player?prettyPrint=false", server.url()), watch_url_prefix: format!("{}/watch?v=", server.url()) };
        let got = peek_youtube("jXtnhyro-QE", &endpoints, LOOPBACK, &CancellationToken::new()).await.expect("a title");
        player.assert_async().await;
        captions.assert_async().await;
        assert_eq!(got.author.as_deref(), Some("The Channel"));
        assert!(got.caption.is_some_and(|t| !t.is_empty()));
    }

    #[tokio::test]
    async fn an_unplayable_video_offers_no_title() {
        let mut server = mockito::Server::new_async().await;
        let gone = include_str!("fixtures/yt_player_ios_unavailable.json");
        let _p = server.mock("POST", "/youtubei/v1/player").match_query(Matcher::Any).with_status(200).with_body(gone).create_async().await;
        let endpoints = YoutubeEndpoints { player_url: format!("{}/youtubei/v1/player", server.url()), watch_url_prefix: String::new() };
        assert_eq!(peek_youtube("jXtnhyro-QE", &endpoints, LOOPBACK, &CancellationToken::new()).await, None);
    }

    #[tokio::test]
    async fn a_web_link_or_a_non_link_makes_no_request() {
        // The default policy would refuse loopback anyway; the point is `None`
        // without ever reaching a fetcher.
        let endpoints = YoutubeEndpoints::default();
        for url in ["https://example.com/article", "not a url", "ftp://x/y"] {
            assert_eq!(peek_with(url, &endpoints, FetchPolicy::default()).await, None, "{url}");
        }
    }

    #[test]
    fn an_unavailable_post_offers_nothing() {
        let mut meta = PlatformMeta::new(crate::ingest::SourceKind::TikTok);
        meta.unavailable = Some("item is dark post".into());
        meta.caption = Some("kept?".into());
        assert_eq!(from_platform(meta), None);
        assert_eq!(from_platform(PlatformMeta::new(crate::ingest::SourceKind::X)), None, "nothing found is nothing");
    }

    #[test]
    fn the_instagram_reel_page_alone_gives_a_title() {
        // `instagram::peek_meta` fetches exactly this page; its parse is covered
        // there, this pins that the page alone is enough for D9.
        let got = from_platform(instagram::parse_reel_page(REEL_PAGE)).expect("caption");
        assert_eq!(got.author.as_deref(), Some("Chanisara Wongdeeprasith (@thechanisara)"));
    }
}
