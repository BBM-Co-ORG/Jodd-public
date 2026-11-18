//! Facebook Reel / video / post link → author, text and a media reference (spec
//! §5.2). One page, fetched with a MOBILE Safari UA on the link as given, share
//! links included — with the desktop UA `/reel/<id>` answers HTTP 400
//! (`<title>Error</title>`), and `/share/r/<code>/` needs no redirect: the share
//! page itself carries the Reel's tags (measured 2026-10-18).
//!
//! What the page carries decides what the link is, not the shape of the link:
//!
//! - a **video** has `og:video` (the MP4 URL itself) and `og:title`
//!   `<caption> | <Page name> | Facebook`;
//! - a **post** has no `og:video`; `og:title` is the page name and
//!   `og:description` the post text, which ends in `...` when Facebook cut it
//!   (spec D8). Honest `Partial`: Facebook does not show the video without a
//!   login, so `media` is `None` and `unavailable` stays `None`;
//! - a **video page without `og:video`** (`og:type` `video.*`, e.g. the
//!   `/<page>/videos/<slug>/<id>/` page a `/reel/` link can redirect to) is a
//!   clip whose video Facebook did not show — not a post (final-review ruling).
//!   Its `og:title` is `<views · reactions> | <caption> | <Page>`, no
//!   `| Facebook`. Measured posts are video-typed too, so a page whose own
//!   `og:url` is a post link stays a post.
//!
//! UNOFFICIAL, like `youtube.rs` and `instagram.rs`: when Facebook moves a tag,
//! the source degrades to "Facebook returned no video or post data" and the
//! ingest completes. There is no Facebook transcript, so `transcript` is `None`.

use tokio_util::sync::CancellationToken;
use reqwest::Url;

use super::net::{read_capped, send_guarded, GuardedRequest, CANCELLED};
use super::social::{MediaRef, PlatformMeta};
use super::web::{decode, og_content, MAX_BODY_BYTES};
use super::{urls, FetchPolicy, SourceKind};

// ── Facebook constants ─────────────────────────────────────────────────────
// Everything that names Facebook's page shape lives HERE and nowhere else.
// When parsing stops finding a video or post, re-capture the page and change
// this block, not a call site.
/// Verbatim from the spec (§6). `send_guarded`'s own default is the desktop UA,
/// which answers `/reel/<id>` with HTTP 400.
pub const MOBILE_SAFARI_UA: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_3 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.3 Mobile/15E148 Safari/604.1";
/// Spec D11: the final media URL's host must be one of these (or a subdomain).
pub const MEDIA_HOSTS: &[&str] = &["fbcdn.net"];
const OG_TITLE: &str = "og:title";
const OG_DESCRIPTION: &str = "og:description";
const OG_URL: &str = "og:url";
const OG_VIDEO: [&str; 2] = ["og:video", "og:video:secure_url"];
const OG_TYPE: &str = "og:type";
/// `og:type` of a video page (`video.other`, `video.movie`, …).
const VIDEO_TYPE: &str = "video";
/// `og:title` ends with `| Facebook` on a video page; the site is not the author.
const SITE: &str = "Facebook";
/// Hosts an `og:url` may name to become the note's canonical URL.
const CANONICAL_HOSTS: [&str; 2] = ["www.facebook.com", "facebook.com"];
const NO_DATA: &str = "Facebook returned no video or post data";
// ───────────────────────────────────────────────────────────────────────────

/// `og:title` without the site: `<…> | Facebook` → `<…>`. A title that is only
/// the site ("Facebook", or " | Facebook" with nothing before it) is not a
/// title — it is what the logged-out interstitial says — and comes back blank.
fn without_site(title: &str) -> &str {
    match title.strip_suffix(SITE).map(str::trim_end).and_then(|t| t.strip_suffix('|')) {
        Some(rest) => rest.trim(),
        None if title == SITE => "",
        None => title,
    }
}

/// Only an https URL on Facebook's own host becomes the note's canonical URL
/// (D12): `og:url` is page-served text, and the note links to it.
fn canonical_url(og_url: &str) -> Option<String> {
    let mut u = Url::parse(og_url).ok()?;
    let ok = u.scheme() == "https" && u.port().is_none() && u.username().is_empty() && u.host_str().is_some_and(|h| CANONICAL_HOSTS.contains(&h));
    u.set_fragment(None);
    ok.then(|| u.to_string())
}

fn is_cut(text: &str) -> bool {
    text.ends_with("...") || text.ends_with('\u{2026}')
}

/// The mobile page (`og:` tags only), no network. `og:video` makes it a video,
/// whatever the link looked like; so does a video `og:type` whose `og:url` is
/// not a post, with no `media`. Otherwise it is a post — page name and text,
/// no `media`, and not `unavailable`: Facebook does not show the video without
/// a login, so the source is a Partial, not a failure (spec D8).
pub fn parse_mobile_page(html: &str) -> PlatformMeta {
    use html5ever::tendril::TendrilSink;
    let mut meta = PlatformMeta::new(SourceKind::Facebook);
    // Every handle taken from `dom` is dropped before `dom` is — see
    // `web::extract_readable`.
    let dom = html5ever::parse_document(markup5ever_rcdom::RcDom::default(), Default::default()).one(html);
    let doc = dom.document.clone();
    let title = og_content(&doc, OG_TITLE).filter(|t| !without_site(t).is_empty());
    let Some(title) = title else {
        meta.unavailable = Some(NO_DATA.into());
        return meta;
    };
    let shown = without_site(&title);
    let caption = og_content(&doc, OG_DESCRIPTION);
    // Page-served text: only an https URL is a candidate
    // (the host allowlist, D11, is applied where it is downloaded).
    // The https filter is inside `find_map`: an http `og:video` must not hide
    // an https `og:video:secure_url`.
    let video = OG_VIDEO.iter().find_map(|p| og_content(&doc, p).filter(|u| u.starts_with("https://")));
    let og_url = og_content(&doc, OG_URL);
    let names_a_post = og_url.as_deref().is_some_and(|u| urls::classify(u) == urls::UrlKind::Facebook { post: true });
    let video_page = !names_a_post && og_content(&doc, OG_TYPE).is_some_and(|t| t.starts_with(VIDEO_TYPE));
    // `<…> | <Page name>`: the page name is the LAST segment, so a caption that
    // itself contains " | " does not move it.
    let page_name = shown.rsplit_once(" | ").map(|(_, name)| name.trim()).filter(|n| !n.is_empty()).map(str::to_string);
    let first_line = caption.as_deref().and_then(|c| c.lines().map(str::trim).find(|l| !l.is_empty())).map(str::to_string);

    if video.is_some() || video_page {
        meta.author = page_name;
        meta.title = first_line;
        meta.media = video.map(|url| MediaRef { url, headers: Vec::new(), cookies: Vec::new(), allowed_host_suffixes: MEDIA_HOSTS });
    } else if page_name.is_some() {
        meta.author = page_name;
        meta.title = first_line;
        meta.is_post = true;
    } else {
        // A real post's og:title is the bare page name.
        meta.author = Some(shown.to_string());
        meta.title = Some(shown.to_string());
        meta.is_post = true;
    }
    meta.caption_maybe_cut = caption.as_deref().is_some_and(is_cut);
    meta.caption = caption;
    meta.canonical_url = og_url.as_deref().and_then(canonical_url);
    meta
}

/// The page, as text. `Err` is the reason to show, already naming Facebook.
async fn get_page(url: &str, policy: FetchPolicy, cancel: &CancellationToken) -> Result<String, String> {
    let req = GuardedRequest { headers: vec![("user-agent", MOBILE_SAFARI_UA.into())], ..GuardedRequest::get(url) };
    let resp = match send_guarded(req, policy, cancel).await {
        Ok(r) => r,
        Err(e) if e == CANCELLED => return Err(e),
        Err(e) => return Err(format!("Facebook could not be reached ({e})")),
    };
    if !resp.status().is_success() {
        return Err(format!("Facebook answered HTTP {}", resp.status().as_u16()));
    }
    match read_capped(resp, MAX_BODY_BYTES, cancel).await {
        Ok(bytes) => Ok(decode(&bytes, Some("text/html; charset=utf-8"))),
        Err(e) if e == CANCELLED => Err(e),
        Err(e) => Err(format!("Facebook could not be read ({e})")),
    }
}

/// Never fails the ingest (URL-ingest Decision 2): every outcome is a
/// `PlatformMeta`, and `unavailable` says why there is nothing. The link is
/// fetched as given — a `/share/r/<code>/` page carries the Reel's own tags —
/// and the real URL comes back as `canonical_url` (`og:url`), never the share
/// code.
pub async fn fetch_meta(url: &str, policy: FetchPolicy, cancel: &CancellationToken) -> PlatformMeta {
    match get_page(url, policy, cancel).await {
        Ok(html) => parse_mobile_page(&html),
        Err(reason) => PlatformMeta { unavailable: Some(reason), ..PlatformMeta::new(SourceKind::Facebook) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::net::host_matches_suffixes;

    const REEL: &str = include_str!("fixtures/facebook_reel_mobile.html");
    const POST: &str = include_str!("fixtures/facebook_post_mobile.html");
    const REEL_URL: &str = "https://www.facebook.com/reel/1120404190712037/";
    const NO_DATA_REASON: &str = "Facebook returned no video or post data";

    fn page(tags: &[(&str, &str)]) -> String {
        tags.iter().map(|(p, c)| format!("<meta property=\"{p}\" content=\"{c}\">")).collect()
    }

    #[test]
    fn a_reel_page_yields_its_video_and_page_name() {
        let meta = parse_mobile_page(REEL);
        assert_eq!(meta.platform, SourceKind::Facebook);
        assert_eq!(meta.unavailable, None);
        assert_eq!(meta.author.as_deref(), Some("Panutat Tejasen"));
        assert_eq!(meta.caption.as_deref(), Some("ดูวิดีโอฉบับเต็มจากลิงค์ในคอมเม้นท์"));
        assert_eq!(meta.title, meta.caption, "a one-line caption is its own title");
        assert!(!meta.caption_maybe_cut);
        assert!(!meta.is_post);
        assert_eq!(meta.canonical_url.as_deref(), Some(REEL_URL));
        assert_eq!((meta.transcript, meta.duration_secs), (None, None));

        let media = meta.media.expect("og:video");
        assert!(media.url.starts_with("https://video.fbkk22-2.fna.fbcdn.net/o1/v/t2/f2/m412/AQNfHqU0"), "{}", media.url);
        assert!(media.url.ends_with(".mp4?redacted=1"), "{}", media.url);
        assert_eq!(media.allowed_host_suffixes, MEDIA_HOSTS);
        assert!(host_matches_suffixes("video.fbkk22-2.fna.fbcdn.net", media.allowed_host_suffixes));
        assert!(media.headers.is_empty() && media.cookies.is_empty(), "the CDN answered 206 to the default download request");
    }

    #[test]
    fn the_video_url_is_entity_decoded_and_the_author_is_the_last_segment() {
        // The capture's `&amp;`-separated query is redacted out of the fixture, so it is asserted here.
        let html = page(&[
            ("og:title", "Tips | part 1 | The Page | Facebook"),
            ("og:description", "Tips | part 1"),
            ("og:video", "https://video.x.fbcdn.net/v.mp4?a=1&amp;b=2&amp;c=3"),
        ]);
        let meta = parse_mobile_page(&html);
        assert_eq!(meta.media.unwrap().url, "https://video.x.fbcdn.net/v.mp4?a=1&b=2&c=3");
        assert_eq!(meta.author.as_deref(), Some("The Page"), "a caption containing ' | ' does not move the page name");

        // `og:video:secure_url` alone is enough.
        let secure = parse_mobile_page(&page(&[("og:title", "c | P | Facebook"), ("og:video:secure_url", "https://v.fbcdn.net/a.mp4")]));
        assert_eq!(secure.media.map(|m| m.url).as_deref(), Some("https://v.fbcdn.net/a.mp4"));
        assert_eq!(secure.author.as_deref(), Some("P"));
    }

    #[test]
    fn a_post_yields_its_text_marked_as_maybe_cut() {
        let meta = parse_mobile_page(POST);
        assert_eq!(meta.unavailable, None, "a post is a Partial, not a failure");
        assert_eq!(meta.media, None);
        assert!(meta.is_post);
        assert_eq!(meta.author.as_deref(), Some("ติด Ai ไปกับลุงจืด"));
        assert_eq!(meta.title.as_deref(), Some("ติด Ai ไปกับลุงจืด"));
        let caption = meta.caption.as_deref().unwrap();
        assert!(caption.starts_with("ตอนนี้ทำแบบนี้"), "{caption}");
        assert!(caption.ends_with("..."), "{caption}");
        assert!(meta.caption_maybe_cut);
        assert!(meta.canonical_url.as_deref().unwrap().starts_with("https://www.facebook.com/61574743782103/posts/"));

        // The other ellipsis, and a text that was not cut.
        let cut = |text: &str| parse_mobile_page(&page(&[("og:title", "P | Facebook"), ("og:description", text)])).caption_maybe_cut;
        assert!(cut("hello…"));
        assert!(!cut("hello"));
        assert!(!cut("hello... world"), "only a trailing ellipsis marks a cut");
    }

    #[test]
    fn a_video_page_without_og_video_is_a_clip_facebook_did_not_show() {
        // A /reel/ or /share/r/ page that lost its video (removed, or login-gated):
        // the page says it is a video, so it is not a post (final-review ruling).
        let meta = parse_mobile_page(&page(&[("og:type", "video.other"), ("og:title", "caption | The Page | Facebook"), ("og:description", "caption")]));
        assert_eq!((meta.unavailable, meta.media), (None, None));
        assert!(!meta.is_post, "a video page is a clip whose video Facebook did not show");
        assert_eq!(meta.author.as_deref(), Some("The Page"));
        assert_eq!((meta.title.as_deref(), meta.caption.as_deref()), (Some("caption"), Some("caption")));
        // A page that is not a video (no og:type, or another one) and has no og:video is a post.
        for tags in [vec![], vec![("og:type", "article")]] {
            let mut tags = tags;
            tags.extend([("og:title", "The Page | Facebook"), ("og:description", "text")]);
            let meta = parse_mobile_page(&page(&tags));
            assert!(meta.is_post, "{tags:?}");
            assert_eq!((meta.author.as_deref(), meta.title.as_deref()), (Some("The Page"), Some("The Page")), "a bare page name is both");
        }
        // An og:video that is not an https URL is no video.
        for bad in ["javascript:alert(1)", "http://v.fbcdn.net/a.mp4", "", "/relative.mp4"] {
            let meta = parse_mobile_page(&page(&[("og:title", "c | P | Facebook"), ("og:video", bad)]));
            assert_eq!((meta.media, meta.unavailable), (None, None), "{bad:?}");
        }
    }

    /// `/reel/2601500666993021` (live probe, 2026-10-18) redirects to a regular
    /// video page: `og:type` video, no `og:video`, and an `og:title` of
    /// `<views · reactions> | <caption> | <Page>` with no `| Facebook`.
    #[test]
    fn the_probed_video_page_without_og_video_names_its_page_not_its_view_count() {
        let caption = "ประธานสาวเผลอนอนกับหนุ่มเพิ่งพ้นคุก! ไม่รู้เลยว่าเขาคือท่านอ๋องที่ทหารหมื่นนายต้องคุกเข่า";
        let og_url = "https://www.facebook.com/61594930971517/videos/%E0%B8%9B%E0%B8%A3%E0%B8%B0/2601500666993021/";
        let title = format!("ยอดดู 2.8 แสน ครั้ง · 8.9 พัน ความรู้สึก | {caption} | Nang AI");
        let meta = parse_mobile_page(&page(&[("og:type", "video.other"), ("og:title", &title), ("og:description", caption), ("og:url", og_url)]));
        assert_eq!((meta.unavailable, meta.media.clone()), (None, None));
        assert!(!meta.is_post);
        assert_eq!(meta.author.as_deref(), Some("Nang AI"));
        assert_eq!(meta.title.as_deref(), Some(caption));
        assert_eq!(meta.canonical_url.as_deref(), Some(og_url));
    }

    /// The measured posts are video-typed too (`og:type` `video.other`, see the
    /// post fixture): their own `og:url` naming a post keeps them posts (D8).
    #[test]
    fn a_video_typed_page_whose_og_url_is_a_post_stays_a_post() {
        let post_url = "https://www.facebook.com/61574743782103/posts/122133207056824792/";
        let meta = parse_mobile_page(&page(&[("og:type", "video.other"), ("og:title", "Page"), ("og:description", "text"), ("og:url", post_url)]));
        assert!(meta.is_post);
        // A post whose og:title has segments: the page is the last, the title the text's first line.
        let meta = parse_mobile_page(&page(&[("og:type", "video.other"), ("og:title", "1K views | the text | The Page"), ("og:description", "the text\nmore"), ("og:url", post_url)]));
        assert!(meta.is_post);
        assert_eq!((meta.author.as_deref(), meta.title.as_deref()), (Some("The Page"), Some("the text")));
    }

    #[test]
    fn an_http_og_video_does_not_hide_an_https_secure_url() {
        let meta = parse_mobile_page(&page(&[
            ("og:title", "c | P | Facebook"),
            ("og:video", "http://v.fbcdn.net/a.mp4"),
            ("og:video:secure_url", "https://v.fbcdn.net/a.mp4"),
        ]));
        assert_eq!(meta.media.map(|m| m.url).as_deref(), Some("https://v.fbcdn.net/a.mp4"));
    }

    #[test]
    fn facebook_tolerates_a_page_without_its_data() {
        for html in [
            // What the desktop UA gets for /reel/<id> (HTTP 400, kept in case a proxy turns it into a 200).
            "<!DOCTYPE html><html lang=\"en\" id=\"facebook\"><head><title>Error</title><meta name=\"robots\" content=\"noindex,nofollow\" /></head><body><h1>Sorry, something went wrong.</h1></body></html>",
            // The logged-out interstitial: a title, no og: tags.
            "<!DOCTYPE html><html><head><title>Facebook</title></head><body></body></html>",
            "",
            "<meta property=\"og:title\" content=\"\">",
            "<meta property=\"og:title\" content=\" | Facebook\">",
            "<meta property=\"og:title\" content=\"Facebook\">",
            "<meta property=\"og:description\" content=\"text but no title\">",
            "<meta property=\"og:video\" content=\"https://v.fbcdn.net/a.mp4\">",
        ] {
            let meta = parse_mobile_page(html);
            assert_eq!(meta.unavailable.as_deref(), Some(NO_DATA_REASON), "{html}");
            assert!(meta.media.is_none() && meta.caption.is_none() && meta.author.is_none() && meta.title.is_none() && meta.canonical_url.is_none());
        }
    }

    #[test]
    fn og_url_becomes_the_canonical_url_for_a_share_link() {
        // Only an https facebook.com URL may become the note's canonical URL (D12).
        let canonical = |url: &str| parse_mobile_page(&page(&[("og:title", "P | Facebook"), ("og:url", url)])).canonical_url;
        assert_eq!(canonical(REEL_URL).as_deref(), Some(REEL_URL));
        assert_eq!(canonical("https://facebook.com/reel/1/").as_deref(), Some("https://facebook.com/reel/1/"));
        for bad in [
            "http://www.facebook.com/reel/1/",
            "https://www.facebook.com.evil.example/reel/1/",
            "https://evil.example/https://www.facebook.com/reel/1/",
            "https://m.facebook.com/reel/1/",
            "https://user@www.facebook.com@evil.example/",
            "javascript:alert(1)",
            "not a url",
            "",
        ] {
            assert_eq!(canonical(bad), None, "{bad:?}");
        }
    }

    #[tokio::test]
    async fn a_share_link_is_fetched_as_given_with_the_mobile_ua_and_keeps_the_real_url() {
        let mut server = mockito::Server::new_async().await;
        let share = server
            .mock("GET", "/share/r/1FVsvkbtSt/")
            .match_header("user-agent", MOBILE_SAFARI_UA)
            .with_status(200)
            .with_body(REEL)
            .create_async()
            .await;
        let url = format!("{}/share/r/1FVsvkbtSt/", server.url());
        let meta = fetch_meta(&url, FetchPolicy { allow_loopback: true }, &CancellationToken::new()).await;
        share.assert_async().await;
        assert_eq!(meta.unavailable, None);
        assert_eq!(meta.canonical_url.as_deref(), Some(REEL_URL), "og:url, never the share code");
        assert_eq!(meta.author.as_deref(), Some("Panutat Tejasen"));
        assert!(meta.media.is_some());
    }

    #[tokio::test]
    async fn a_refused_page_names_facebook() {
        let mut server = mockito::Server::new_async().await;
        let _m = server.mock("GET", "/reel/1/").with_status(400).with_body("<title>Error</title>").create_async().await;
        let policy = FetchPolicy { allow_loopback: true };
        let meta = fetch_meta(&format!("{}/reel/1/", server.url()), policy, &CancellationToken::new()).await;
        assert_eq!(meta.unavailable.as_deref(), Some("Facebook answered HTTP 400"));
        assert_eq!(meta.platform, SourceKind::Facebook);

        let cancel = CancellationToken::new();
        cancel.cancel();
        let meta = fetch_meta(&format!("{}/reel/1/", server.url()), policy, &cancel).await;
        assert_eq!(meta.unavailable.as_deref(), Some(CANCELLED));
    }
}
