//! TikTok link → caption, author, auto-captions and a media reference (spec
//! §5.2). The video page, fetched with Jodd's desktop UA, carries everything in
//! `__UNIVERSAL_DATA_FOR_REHYDRATION__`; measured 2026-10-18 on three of the
//! owner's links. UNOFFICIAL, like `youtube.rs`: when TikTok moves a key, the
//! source degrades to "TikTok returned no video data" and the ingest completes.
//!
//! The page's `Set-Cookie`s ride on the returned `MediaRef` (spec D10): the
//! `playAddr` answers 403 without them. The subtitle URL needs none.

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::net::{host_matches_suffixes, read_capped, send_guarded, set_cookies, GuardedRequest, CANCELLED};
use super::social::{vtt_to_text, MediaRef, PlatformMeta, PlatformTranscript};
use super::web::{decode, MAX_BODY_BYTES};
use super::{FetchPolicy, SourceKind};

// ── TikTok constants ───────────────────────────────────────────────────────
// Everything that names TikTok's page shape lives HERE and nowhere else. The
// page UA is `net`'s desktop-Chrome one — `send_guarded` sets it on every
// request. When parsing stops finding a video, re-capture a page and change
// this block, not a call site.
const SCRIPT_ID: &str = "__UNIVERSAL_DATA_FOR_REHYDRATION__";
const SCOPE_KEY: &str = "webapp.video-detail";
/// Spec D11: the final media URL's host must be one of these (or a subdomain).
pub const MEDIA_HOSTS: &[&str] = &["tiktok.com", "tiktokcdn.com", "tiktokcdn-us.com", "tiktokv.com"];
const MEDIA_REFERER: &str = "https://www.tiktok.com/";
const ASR_SOURCE: &str = "TikTok auto-captions";
const CREATOR_SOURCE: &str = "TikTok captions";
const NO_DATA: &str = "TikTok returned no video data";
// ───────────────────────────────────────────────────────────────────────────

/// A caption file is a few KB; this only bounds a hostile answer.
const MAX_VTT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Subtitle {
    url: String,
    language: Option<String>,
    /// `Source: "ASR"` — TikTok's own speech recognition, not the creator's.
    auto: bool,
}

fn text(v: &Value) -> Option<String> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// The JSON inside the rehydration `<script>`, or `None` when the page has none.
fn rehydration_json(html: &str) -> Option<Value> {
    let at = html.find(&format!("id=\"{SCRIPT_ID}\""))?;
    let body = &html[at..];
    let start = body.find('>')? + 1;
    let end = body[start..].find("</script>")? + start;
    serde_json::from_str(&body[start..end]).ok()
}

/// First `webvtt` entry; with none, the first entry (the controller's ruling —
/// no locale matching: a clip has one track in practice).
fn pick_subtitle(infos: &Value) -> Option<Subtitle> {
    let list = infos.as_array()?;
    let chosen = list.iter().find(|s| s["Format"].as_str() == Some("webvtt")).or_else(|| list.first())?;
    Some(Subtitle {
        url: text(&chosen["Url"])?,
        language: text(&chosen["LanguageCodeName"]),
        auto: chosen["Source"].as_str() == Some("ASR"),
    })
}

fn parse(html: &str) -> (PlatformMeta, Option<Subtitle>) {
    let mut meta = PlatformMeta::new(SourceKind::TikTok);
    let json = rehydration_json(html);
    let detail = json.as_ref().map(|j| &j["__DEFAULT_SCOPE__"][SCOPE_KEY]).filter(|d| d.is_object());
    let Some(detail) = detail else {
        meta.unavailable = Some(NO_DATA.into());
        return (meta, None);
    };
    let code = detail["statusCode"].as_i64().unwrap_or(0);
    if code != 0 {
        meta.unavailable = Some(text(&detail["statusMsg"]).unwrap_or_else(|| format!("TikTok answered status {code}")));
        return (meta, None);
    }
    let item = &detail["itemInfo"]["itemStruct"];
    if !item.is_object() {
        meta.unavailable = Some(NO_DATA.into());
        return (meta, None);
    }

    let unique_id = text(&item["author"]["uniqueId"]);
    let nickname = text(&item["author"]["nickname"]);
    meta.author = match (&unique_id, &nickname) {
        (Some(u), Some(n)) => Some(format!("@{u} ({n})")),
        (Some(u), None) => Some(format!("@{u}")),
        (None, Some(n)) => Some(n.clone()),
        (None, None) => None,
    };
    // A short link resolves here: the id and handle name the canonical URL.
    // Both come from page JSON and end up in a stored URL (D12): only the
    // shapes TikTok really uses are interpolated.
    if let (Some(id), Some(user)) = (text(&item["id"]), &unique_id) {
        let id_ok = id.bytes().all(|b| b.is_ascii_digit());
        let user_ok = user.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_');
        if id_ok && user_ok {
            meta.canonical_url = Some(format!("https://www.tiktok.com/@{user}/video/{id}"));
        }
    }
    meta.caption = text(&item["desc"]);
    meta.title = meta.caption.as_deref().and_then(|c| c.lines().map(str::trim).find(|l| !l.is_empty())).map(str::to_string);

    let video = &item["video"];
    meta.duration_secs = video["duration"].as_f64().filter(|d| *d > 0.0);
    meta.media = text(&video["playAddr"]).map(|url| MediaRef {
        url,
        headers: vec![("referer", MEDIA_REFERER.to_string())],
        cookies: Vec::new(),
        allowed_host_suffixes: MEDIA_HOSTS,
    });
    (meta, pick_subtitle(&video["subtitleInfos"]))
}

/// The page's metadata, with no network: no transcript text (the VTT is a
/// second request) and no media cookies (they come from the page response).
pub fn parse_page(html: &str) -> PlatformMeta {
    parse(html).0
}

/// The subtitle URL comes from JSON the page served, like the media URL, so it
/// meets the same host allowlist (D11) before anything is requested.
async fn fetch_transcript(sub: &Subtitle, hosts: &[&str], policy: FetchPolicy, cancel: &CancellationToken) -> Option<PlatformTranscript> {
    let host_ok = reqwest::Url::parse(&sub.url).ok().and_then(|u| u.host_str().map(|h| host_matches_suffixes(h, hosts))).unwrap_or(false);
    if !host_ok {
        crate::log!("ingest: TikTok subtitle URL is outside the media host allowlist; not fetched");
        return None;
    }
    let got = async {
        let resp = send_guarded(GuardedRequest::get(&sub.url), policy, cancel).await?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status().as_u16()));
        }
        read_capped(resp, MAX_VTT_BYTES, cancel).await
    }
    .await;
    match got {
        Ok(bytes) => {
            let text = vtt_to_text(&decode(&bytes, Some("text/vtt; charset=utf-8")));
            (!text.is_empty()).then(|| PlatformTranscript {
                text,
                language: sub.language.clone(),
                source: (if sub.auto { ASR_SOURCE } else { CREATOR_SOURCE }).to_string(),
            })
        }
        Err(e) => {
            crate::log!("ingest: TikTok subtitle fetch failed: {e}");
            None
        }
    }
}

/// Never fails the ingest (URL-ingest Decision 2): every outcome is a
/// `PlatformMeta`, and `unavailable` says why there is no video.
pub async fn fetch_meta(url: &str, policy: FetchPolicy, cancel: &CancellationToken) -> PlatformMeta {
    fetch_meta_with(url, MEDIA_HOSTS, policy, cancel).await
}

/// `hosts` is the subtitle URL's allowlist: production passes `MEDIA_HOSTS`,
/// tests pass their mockito host (`allow_loopback` never bypasses it).
pub(crate) async fn fetch_meta_with(url: &str, hosts: &[&str], policy: FetchPolicy, cancel: &CancellationToken) -> PlatformMeta {
    let (mut meta, subtitle) = fetch_page(url, policy, cancel).await;
    if meta.unavailable.is_some() {
        return meta;
    }
    if let Some(sub) = subtitle {
        meta.transcript = fetch_transcript(&sub, hosts, policy, cancel).await;
    }
    meta
}

/// The video page alone — one request, no caption file and no media. For the
/// share sheet's title (spec D9), which must not cost a second round trip.
pub async fn peek_meta(url: &str, policy: FetchPolicy, cancel: &CancellationToken) -> PlatformMeta {
    fetch_page(url, policy, cancel).await.0
}

/// The page request and its parse; the subtitle URL is returned, not fetched.
async fn fetch_page(url: &str, policy: FetchPolicy, cancel: &CancellationToken) -> (PlatformMeta, Option<Subtitle>) {
    let unavailable = |reason: String| (PlatformMeta { unavailable: Some(reason), ..PlatformMeta::new(SourceKind::TikTok) }, None);
    let resp = match send_guarded(GuardedRequest::get(url), policy, cancel).await {
        Ok(r) => r,
        Err(e) if e == CANCELLED => return unavailable(e),
        Err(e) => return unavailable(format!("TikTok could not be reached ({e})")),
    };
    if !resp.status().is_success() {
        return unavailable(format!("TikTok answered HTTP {}", resp.status().as_u16()));
    }
    let cookies = set_cookies(&resp);
    let html = match read_capped(resp, MAX_BODY_BYTES, cancel).await {
        Ok(bytes) => decode(&bytes, Some("text/html; charset=utf-8")),
        Err(e) => return unavailable(format!("TikTok could not be read ({e})")),
    };

    let (mut meta, subtitle) = parse(&html);
    if meta.unavailable.is_some() {
        return (meta, None);
    }
    if let Some(media) = meta.media.as_mut() {
        media.cookies = cookies;
    }
    (meta, subtitle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Matcher;

    const VIDEO: &str = include_str!("fixtures/tiktok_video.html");
    const DARK: &str = include_str!("fixtures/tiktok_dark_post.html");
    const VTT: &str = include_str!("fixtures/tiktok_subtitle.vtt");
    const ID: &str = "7696048386981645569";
    const CANONICAL: &str = "https://www.tiktok.com/@tonsr_/video/7696048386981645569";

    /// mockito's host, standing in for `MEDIA_HOSTS` in the fetch tests.
    const LOCAL_HOSTS: &[&str] = &["127.0.0.1"];

    /// The fixture's redacted subtitle URL points at tiktok.com; aim it at mockito.
    fn local(html: &str, server: &mockito::ServerGuard) -> String {
        html.replace("https://v16-webapp.tiktok.com", &server.url())
    }

    #[test]
    fn the_video_page_yields_author_caption_duration_and_asr() {
        let (meta, sub) = parse(VIDEO);
        assert_eq!(meta.platform, SourceKind::TikTok);
        assert_eq!(meta.unavailable, None);
        assert_eq!(meta.author.as_deref(), Some("@tonsr_ (Lazy_Productive)"));
        assert!(meta.caption.as_deref().unwrap().starts_with("5 skill ใน claude code"));
        assert_eq!(meta.title.as_deref(), meta.caption.as_deref());
        assert_eq!(meta.duration_secs, Some(100.0));
        assert_eq!(meta.canonical_url.as_deref(), Some(CANONICAL));
        let media = meta.media.unwrap();
        assert!(media.url.starts_with("https://v16-webapp-prime.tiktok.com/video/tos/"), "{}", media.url);
        assert!(media.url.ends_with("?redacted=1"));
        assert_eq!(media.allowed_host_suffixes, MEDIA_HOSTS);
        assert!(media.allowed_host_suffixes.contains(&"tiktok.com"));
        assert_eq!(media.headers, vec![("referer", "https://www.tiktok.com/".to_string())]);
        assert!(media.cookies.is_empty(), "cookies come from the page RESPONSE, not its body");
        let sub = sub.expect("subtitleInfos");
        assert_eq!((sub.language.as_deref(), sub.auto), (Some("tha-TH"), true));
        assert!(sub.url.starts_with("https://v16-webapp.tiktok.com/"));
        assert_eq!(parse_page(VIDEO).transcript, None, "the VTT is a second request");
    }

    #[test]
    fn a_dark_post_is_unavailable_with_tiktoks_message() {
        let meta = parse_page(DARK);
        assert_eq!(meta.unavailable.as_deref(), Some("item is dark post"));
        assert!(meta.media.is_none() && meta.caption.is_none() && meta.author.is_none());
    }

    #[test]
    fn tiktok_tolerates_a_page_without_its_data() {
        for html in [
            "<html><body>Please log in</body></html>",
            "",
            // The script is there but is not JSON, or the scope key moved.
            "<script id=\"__UNIVERSAL_DATA_FOR_REHYDRATION__\">not json</script>",
            "<script id=\"__UNIVERSAL_DATA_FOR_REHYDRATION__\">{\"__DEFAULT_SCOPE__\":{\"webapp.video-detail-v2\":{}}}</script>",
            // Status ok but the item moved.
            "<script id=\"__UNIVERSAL_DATA_FOR_REHYDRATION__\">{\"__DEFAULT_SCOPE__\":{\"webapp.video-detail\":{\"statusCode\":0}}}</script>",
        ] {
            let meta = parse_page(html);
            assert_eq!(meta.unavailable.as_deref(), Some("TikTok returned no video data"), "{html}");
            assert!(meta.media.is_none() && meta.transcript.is_none());
        }
        // A refusal without a message still says something.
        let meta = parse_page("<script id=\"__UNIVERSAL_DATA_FOR_REHYDRATION__\">{\"__DEFAULT_SCOPE__\":{\"webapp.video-detail\":{\"statusCode\":10204}}}</script>");
        assert_eq!(meta.unavailable.as_deref(), Some("TikTok answered status 10204"));
    }

    #[test]
    fn the_first_webvtt_track_wins_and_a_missing_handle_degrades_the_author() {
        let page = |item: &str| {
            format!("<script id=\"__UNIVERSAL_DATA_FOR_REHYDRATION__\">{{\"__DEFAULT_SCOPE__\":{{\"webapp.video-detail\":{{\"statusCode\":0,\"itemInfo\":{{\"itemStruct\":{item}}}}}}}}}</script>")
        };
        let (meta, sub) = parse(&page(
            r#"{"id":"1","desc":"line one\nline two","author":{"nickname":"Nick"},"video":{"duration":0,"playAddr":"","subtitleInfos":[
                {"Url":"https://x/srt","LanguageCodeName":"eng-US","Format":"srt","Source":"MT"},
                {"Url":"https://x/a.vtt","LanguageCodeName":"tha-TH","Format":"webvtt","Source":"ASR"},
                {"Url":"https://x/b.vtt","LanguageCodeName":"eng-US","Format":"webvtt","Source":"ASR"}]}}"#,
        ));
        assert_eq!(meta.author.as_deref(), Some("Nick"));
        assert_eq!(meta.title.as_deref(), Some("line one"));
        assert_eq!(meta.caption.as_deref(), Some("line one\nline two"));
        assert_eq!(meta.canonical_url, None, "no handle, no canonical URL");
        assert_eq!((meta.duration_secs, meta.media), (None, None), "zero duration and an empty playAddr are absent");
        assert_eq!(sub, Some(Subtitle { url: "https://x/a.vtt".into(), language: Some("tha-TH".into()), auto: true }));

        // No webvtt entry: the first entry, as TikTok gave it.
        let (_, sub) = parse(&page(r#"{"video":{"subtitleInfos":[{"Url":"https://x/srt","Format":"srt"}]}}"#));
        assert_eq!(sub, Some(Subtitle { url: "https://x/srt".into(), language: None, auto: false }));
        let (_, sub) = parse(&page(r#"{"video":{"subtitleInfos":[]}}"#));
        assert_eq!(sub, None);
    }

    #[tokio::test]
    async fn fetch_meta_carries_the_page_cookies_to_the_media_ref_and_fetches_the_vtt_without_them() {
        let mut server = mockito::Server::new_async().await;
        let page = server
            .mock("GET", format!("/@tonsr_/video/{ID}").as_str())
            .match_header("user-agent", Matcher::Regex("Chrome/".into()))
            .with_status(200)
            .with_header("set-cookie", "ttwid=TW; Max-Age=31536000; Domain=.tiktok.com; Path=/; HttpOnly")
            .with_header("set-cookie", "tt_chain_token=CT; Path=/")
            .with_header("set-cookie", "msToken=MS; Path=/")
            .with_body(local(VIDEO, &server))
            .create_async()
            .await;
        let vtt = server
            .mock("GET", Matcher::Regex("/redacted/redacted/video/tos/.*".into()))
            .match_header("cookie", Matcher::Missing)
            .with_status(200)
            .with_header("content-type", "text/vtt")
            .with_body(VTT)
            .create_async()
            .await;

        let url = format!("{}/@tonsr_/video/{ID}", server.url());
        let meta = fetch_meta_with(&url, LOCAL_HOSTS, FetchPolicy { allow_loopback: true }, &CancellationToken::new()).await;
        page.assert_async().await;
        vtt.assert_async().await;

        assert_eq!(meta.unavailable, None);
        assert_eq!(meta.canonical_url.as_deref(), Some(CANONICAL));
        let t = meta.transcript.expect("the VTT was served");
        assert_eq!((t.source.as_str(), t.language.as_deref()), ("TikTok auto-captions", Some("tha-TH")));
        assert_eq!(t.text, vtt_to_text(VTT));
        assert!(t.text.starts_with("ห้าสกิลในคลอสโค้ด"));
        let media = meta.media.expect("playAddr");
        assert_eq!(
            media.cookies,
            vec![("ttwid".to_string(), "TW".to_string()), ("tt_chain_token".into(), "CT".into()), ("msToken".into(), "MS".into())]
        );
        assert_eq!(media.headers, vec![("referer", "https://www.tiktok.com/".to_string())]);
    }

    #[tokio::test]
    async fn a_failing_vtt_keeps_the_media_and_a_failing_page_says_why() {
        let mut server = mockito::Server::new_async().await;
        let _p = server
            .mock("GET", format!("/@tonsr_/video/{ID}").as_str())
            .with_status(200)
            .with_body(local(VIDEO, &server))
            .create_async()
            .await;
        let _v = server.mock("GET", Matcher::Regex("/redacted/.*".into())).with_status(404).create_async().await;
        let _g = server.mock("GET", "/gone").with_status(404).create_async().await;
        let _d = server.mock("GET", "/dark").with_status(200).with_body(DARK).create_async().await;
        let policy = FetchPolicy { allow_loopback: true };
        let cancel = CancellationToken::new();

        let meta = fetch_meta_with(&format!("{}/@tonsr_/video/{ID}", server.url()), LOCAL_HOSTS, policy, &cancel).await;
        assert_eq!((meta.unavailable, meta.transcript), (None, None));
        assert!(meta.media.is_some() && meta.caption.is_some());

        let meta = fetch_meta(&format!("{}/gone", server.url()), policy, &cancel).await;
        assert_eq!(meta.unavailable.as_deref(), Some("TikTok answered HTTP 404"));
        let meta = fetch_meta(&format!("{}/dark", server.url()), policy, &cancel).await;
        assert_eq!(meta.unavailable.as_deref(), Some("item is dark post"));
        // The default policy refuses the loopback mock: the guard is in the path.
        let meta = fetch_meta(&format!("{}/dark", server.url()), FetchPolicy::default(), &cancel).await;
        assert!(meta.unavailable.unwrap().starts_with("TikTok could not be reached"));
    }

    #[tokio::test]
    async fn a_subtitle_url_outside_the_media_hosts_is_never_requested() {
        let mut server = mockito::Server::new_async().await;
        let _p = server
            .mock("GET", format!("/@tonsr_/video/{ID}").as_str())
            .with_status(200)
            .with_body(local(VIDEO, &server))
            .create_async()
            .await;
        let vtt = server.mock("GET", Matcher::Regex("/redacted/.*".into())).expect(0).with_body(VTT).create_async().await;
        let url = format!("{}/@tonsr_/video/{ID}", server.url());
        let policy = FetchPolicy { allow_loopback: true };

        // Loopback is permitted by the policy, yet not by the allowlist.
        let meta = fetch_meta_with(&url, MEDIA_HOSTS, policy, &CancellationToken::new()).await;
        vtt.assert_async().await;
        assert_eq!(meta.unavailable, None);
        assert_eq!(meta.transcript, None);
        assert!(meta.media.is_some(), "the rest of the page still counts");

        // A look-alike host fails the dot-anchored suffix match too.
        let sub = Subtitle { url: "https://evil-tiktok.com/x.vtt".into(), language: None, auto: true };
        assert!(fetch_transcript(&sub, MEDIA_HOSTS, policy, &CancellationToken::new()).await.is_none());
    }

    #[test]
    fn canonical_url_is_built_only_from_well_formed_ids_and_handles() {
        let page = |id: &str, user: &str| {
            parse_page(&format!(
                "<script id=\"__UNIVERSAL_DATA_FOR_REHYDRATION__\">{{\"__DEFAULT_SCOPE__\":{{\"webapp.video-detail\":{{\"statusCode\":0,\"itemInfo\":{{\"itemStruct\":{{\"id\":\"{id}\",\"author\":{{\"uniqueId\":\"{user}\"}}}}}}}}}}}}</script>"
            ))
            .canonical_url
        };
        assert_eq!(page("123", "a.b_C9").as_deref(), Some("https://www.tiktok.com/@a.b_C9/video/123"));
        for (id, user) in [("12a", "u"), ("", "u"), ("1", "u/../x"), ("1", "u?x=1"), ("1", "u#f"), ("1", "u v"), ("1", "ü")] {
            assert_eq!(page(id, user), None, "{id:?} {user:?}");
        }
    }
}
