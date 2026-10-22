//! Instagram Reel link → caption, author, duration and a media reference (spec
//! §5.2). Two pages, both fetched with a MOBILE Safari UA — with Jodd's desktop
//! UA `/reel/<code>/` redirects to `/accounts/login/` (measured 2026-10-18 on
//! four of the owner's links):
//!
//! - `/reel/<code>/` carries `og:title` (`<Name> on Instagram: "<caption>"`) and
//!   `og:description` (`<N> likes, <M> comments - <handle> on <date>: "…"`);
//! - `/reel/<code>/embed/captioned/` carries `video_url` and `video_duration`
//!   inside a script, as JSON inside a JSON string.
//!
//! UNOFFICIAL, like `youtube.rs` and `tiktok.rs`: when Instagram moves a key,
//! the reel page degrades to "Instagram returned no reel data" and the embed to
//! "caption only" — the ingest completes either way. There is no Instagram
//! transcript (none was found in any capture), so `transcript` stays `None`.

use tokio_util::sync::CancellationToken;

use super::net::{read_capped, send_guarded, GuardedRequest, CANCELLED};
use super::social::{MediaRef, PlatformMeta};
use super::urls::is_code;
use super::web::{decode, og_content, MAX_BODY_BYTES};
use super::{FetchPolicy, SourceKind};

// ── Instagram constants ────────────────────────────────────────────────────
// Everything that names Instagram's page shape lives HERE and nowhere else.
// When parsing stops finding a reel, re-capture the two pages and change this
// block, not a call site.
/// Verbatim from the spec (§6). Sent on both page requests via `headers`;
/// `send_guarded`'s own default is the desktop UA, which Instagram bounces to
/// the login page.
pub const MOBILE_SAFARI_UA: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_3 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.3 Mobile/15E148 Safari/604.1";
const ORIGIN: &str = "https://www.instagram.com";
const EMBED_PATH: &str = "embed/captioned/";
/// Spec D11: the final media URL's host must be one of these (or a subdomain).
pub const MEDIA_HOSTS: &[&str] = &["cdninstagram.com", "fbcdn.net"];
const OG_TITLE: &str = "og:title";
const OG_DESCRIPTION: &str = "og:description";
const VIDEO_URL_KEY: &str = "video_url";
const VIDEO_DURATION_KEY: &str = "video_duration";
const NO_DATA: &str = "Instagram returned no reel data";
// ───────────────────────────────────────────────────────────────────────────

/// Only a well-formed shortcode is interpolated into a URL (D12): the fetcher
/// builds both page URLs and the stored canonical URL from it, never from the
/// link as shared.
fn canonical_url(shortcode: &str) -> Option<String> {
    is_code(shortcode).then(|| format!("{ORIGIN}/reel/{shortcode}/"))
}

fn nonblank(s: &str) -> Option<String> {
    Some(s.trim()).filter(|s| !s.is_empty()).map(str::to_string)
}

/// `og:title` and `og:description`, entities decoded by the HTML parser (the
/// caption's Thai arrives as `&#xe21;`).
fn og_contents(html: &str) -> (Option<String>, Option<String>) {
    use html5ever::tendril::TendrilSink;
    // Every handle taken from `dom` is dropped before `dom` is — see
    // `web::extract_readable`.
    let dom = html5ever::parse_document(markup5ever_rcdom::RcDom::default(), Default::default()).one(html);
    let doc = dom.document.clone();
    (og_content(&doc, OG_TITLE), og_content(&doc, OG_DESCRIPTION))
}

/// `<Name> on Instagram: "<caption>"` → (name, caption). A reel without a
/// caption is `<Name> on Instagram`. Anything else is not a reel page.
fn split_title(title: &str) -> Option<(Option<String>, Option<String>)> {
    if let Some((name, rest)) = title.split_once(" on Instagram: \"") {
        return Some((nonblank(name), nonblank(rest.strip_suffix('"').unwrap_or(rest))));
    }
    title.strip_suffix(" on Instagram").map(|name| (nonblank(name), None))
}

/// The handle from `<N> likes, <M> comments - <handle> on <date>: "<caption>"`.
/// Only the part BEFORE the caption opens is searched, so a " - x on " inside
/// a caption cannot pass for it, and the handle is interpolated into the
/// author line only in the shape Instagram handles really have.
fn description_handle(description: &str) -> Option<String> {
    let head = &description[..description.find(": \"")?];
    let (_, rest) = head.split_once(" - ")?;
    let (handle, _date) = rest.split_once(" on ")?;
    let ok = !handle.is_empty() && handle.len() <= 30 && handle.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_');
    ok.then(|| handle.to_string())
}

/// The reel page (`/reel/<code>/`, mobile UA): author and caption, no network.
/// The video comes from the embed page — see `parse_embed`.
pub fn parse_reel_page(html: &str) -> PlatformMeta {
    let mut meta = PlatformMeta::new(SourceKind::Instagram);
    let (title, description) = og_contents(html);
    let Some((name, caption)) = title.as_deref().and_then(split_title) else {
        meta.unavailable = Some(NO_DATA.into());
        return meta;
    };
    let handle = description.as_deref().and_then(description_handle);
    meta.author = match (name, handle) {
        (Some(n), Some(h)) => Some(format!("{n} (@{h})")),
        (Some(n), None) => Some(n),
        (None, Some(h)) => Some(format!("@{h}")),
        (None, None) => None,
    };
    meta.title = caption.as_deref().and_then(|c| c.lines().map(str::trim).find(|l| !l.is_empty())).map(str::to_string);
    meta.caption = caption;
    meta
}

/// The text right after `"<key>":` in JSON that sits under any number of
/// string-escape layers (`\"key\":` once, `\\\"key\\\":` twice…). The key must
/// be a whole key: a `"` immediately before it.
fn after_key<'a>(html: &'a str, key: &str) -> Option<&'a str> {
    let mut from = 0;
    while let Some(i) = html[from..].find(key) {
        let start = from + i;
        let end = start + key.len();
        from = end;
        if !html[..start].ends_with('"') {
            continue;
        }
        let Some(rest) = html[end..].trim_start_matches('\\').strip_prefix('"') else { continue };
        let Some(rest) = rest.trim_start().strip_prefix(':') else { continue };
        return Some(rest.trim_start());
    }
    None
}

/// Layers of JSON-in-JSON to peel. Measured: two; the bound only stops a
/// hostile page from asking for more.
const MAX_ESCAPE_LAYERS: usize = 4;

/// Decodes `encoded` as a JSON string body once per escape layer it carries,
/// until no backslash is left. `None` if a layer is not valid JSON (or a
/// backslash survives every layer).
fn unescape(encoded: &str) -> Option<String> {
    let mut s = encoded.to_string();
    for _ in 0..MAX_ESCAPE_LAYERS {
        if !s.contains('\\') {
            return Some(s);
        }
        s = serde_json::from_str::<String>(&format!("\"{s}\"")).ok()?;
    }
    (!s.contains('\\')).then_some(s)
}

fn video_url(html: &str) -> Option<String> {
    // The value is the text between its quotes; the escape backslashes that
    // precede the closing quote belong to the layer, not to the URL.
    let value = after_key(html, VIDEO_URL_KEY)?.trim_start_matches('\\').strip_prefix('"')?;
    let url = unescape(value[..value.find('"')?].trim_end_matches('\\'))?;
    // It comes from page JSON: only an https URL is a candidate (the host
    // allowlist, D11, is applied where it is downloaded).
    url.starts_with("https://").then_some(url)
}

fn video_duration(html: &str) -> Option<f64> {
    let rest = after_key(html, VIDEO_DURATION_KEY)?;
    let number = &rest[..rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len())];
    number.parse::<f64>().ok().filter(|d| d.is_finite() && *d > 0.0)
}

/// The embed page (`/reel/<code>/embed/captioned/`, mobile UA): the media URL
/// and the duration. Adds only what it finds; a page without them leaves
/// `meta` as it was — the reel is then caption-only, not failed.
pub fn parse_embed(html: &str, meta: &mut PlatformMeta) {
    if let Some(secs) = video_duration(html) {
        meta.duration_secs = Some(secs);
    }
    if let Some(url) = video_url(html) {
        // The one measured download (302 -> 200 MP4) used the mobile UA; the
        // download's own default is the desktop one Instagram bounces to login.
        let headers = vec![("user-agent", MOBILE_SAFARI_UA.to_string())];
        meta.media = Some(MediaRef { url, headers, cookies: Vec::new(), allowed_host_suffixes: MEDIA_HOSTS });
    }
}

/// One page, as text. `Err` is the reason to show, already naming Instagram.
async fn get_page(url: &str, policy: FetchPolicy, cancel: &CancellationToken) -> Result<String, String> {
    let req = GuardedRequest { headers: vec![("user-agent", MOBILE_SAFARI_UA.into())], ..GuardedRequest::get(url) };
    let resp = match send_guarded(req, policy, cancel).await {
        Ok(r) => r,
        Err(e) if e == CANCELLED => return Err(e),
        Err(e) => return Err(format!("Instagram could not be reached ({e})")),
    };
    if !resp.status().is_success() {
        return Err(format!("Instagram answered HTTP {}", resp.status().as_u16()));
    }
    match read_capped(resp, MAX_BODY_BYTES, cancel).await {
        Ok(bytes) => Ok(decode(&bytes, Some("text/html; charset=utf-8"))),
        Err(e) if e == CANCELLED => Err(e),
        Err(e) => Err(format!("Instagram could not be read ({e})")),
    }
}

/// Never fails the ingest (URL-ingest Decision 2): every outcome is a
/// `PlatformMeta`, and `unavailable` says why there is no reel. A reel whose
/// embed page gives no video keeps its caption, with no `media`.
///
/// Fetched by shortcode alone (D12): the link as shared never reaches Instagram.
pub async fn fetch_meta(shortcode: &str, policy: FetchPolicy, cancel: &CancellationToken) -> PlatformMeta {
    fetch_meta_with(ORIGIN, shortcode, policy, cancel).await
}

/// `origin` is where the two pages are fetched from: production passes
/// `ORIGIN`, tests pass their mockito server. The canonical URL is always
/// built on `ORIGIN`.
async fn fetch_meta_with(origin: &str, shortcode: &str, policy: FetchPolicy, cancel: &CancellationToken) -> PlatformMeta {
    let mut meta = fetch_reel_with(origin, shortcode, policy, cancel).await;
    if meta.unavailable.is_some() {
        return meta;
    }
    let page_url = format!("{origin}/reel/{shortcode}/");
    match get_page(&format!("{page_url}{EMBED_PATH}"), policy, cancel).await {
        Ok(html) => parse_embed(&html, &mut meta),
        Err(e) if e == CANCELLED => return PlatformMeta { unavailable: Some(e), ..PlatformMeta::new(SourceKind::Instagram) },
        Err(e) => crate::log!("ingest: Instagram embed page failed ({e}); keeping the caption only"),
    }
    meta
}

/// The reel page alone — author and caption, one request, no embed page (so
/// no media URL or duration). For the share sheet's title (spec D9).
pub async fn peek_meta(shortcode: &str, policy: FetchPolicy, cancel: &CancellationToken) -> PlatformMeta {
    fetch_reel_with(ORIGIN, shortcode, policy, cancel).await
}

async fn fetch_reel_with(origin: &str, shortcode: &str, policy: FetchPolicy, cancel: &CancellationToken) -> PlatformMeta {
    let unavailable = |reason: String| PlatformMeta { unavailable: Some(reason), ..PlatformMeta::new(SourceKind::Instagram) };
    let Some(canonical) = canonical_url(shortcode) else {
        return unavailable("not an Instagram post link".into());
    };
    let page_url = format!("{origin}/reel/{shortcode}/");

    let mut meta = match get_page(&page_url, policy, cancel).await {
        Ok(html) => parse_reel_page(&html),
        Err(reason) => return unavailable(reason),
    };
    if meta.unavailable.is_none() {
        meta.canonical_url = Some(canonical);
    }
    meta
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Matcher;

    const REEL: &str = include_str!("fixtures/instagram_reel_mobile.html");
    const EMBED: &str = include_str!("fixtures/instagram_embed.html");
    const CODE: &str = "DeeyrDqyckk";
    const CANONICAL: &str = "https://www.instagram.com/reel/DeeyrDqyckk/";

    /// A script as Instagram serves it: the JSON is a string inside another
    /// JSON, so every `"` is `\"` and every `\` is `\\`.
    fn embed(tail: &str) -> String {
        format!("<script>{{\"define\":[[\"x\",{{\"context\":\"{{\\\"video_duration\\\":12.5,{tail}}}\"}}]]}}</script>")
    }

    #[test]
    fn the_reel_page_yields_author_and_caption() {
        let meta = parse_reel_page(REEL);
        assert_eq!(meta.platform, SourceKind::Instagram);
        assert_eq!(meta.unavailable, None);
        assert_eq!(meta.author.as_deref(), Some("Chanisara Wongdeeprasith (@thechanisara)"));
        let caption = meta.caption.as_deref().unwrap();
        assert!(caption.starts_with("Fastwork.com มาแล้วจ้า"), "{caption}");
        assert!(caption.contains("@ckfastwork"), "numeric character references are decoded: {caption}");
        assert!(!caption.contains("&#") && !caption.contains("&quot;"), "{caption}");
        assert!(!caption.starts_with('"') && !caption.ends_with('"'), "the quotes around the caption are not part of it");
        assert_eq!(meta.title.as_deref(), Some(caption), "a one-line caption is its own title");
        assert_eq!((meta.media, meta.transcript, meta.duration_secs), (None, None, None), "those come from the embed page");
        assert_eq!(meta.canonical_url, None, "the shortcode is the fetcher's, not the page's");
    }

    #[test]
    fn the_title_is_the_first_line_and_the_handle_is_optional() {
        let page = |title: &str, desc: &str| {
            parse_reel_page(&format!("<meta property=\"og:title\" content=\"{title}\"><meta property=\"og:description\" content=\"{desc}\">"))
        };
        let m = page("Ty on Instagram: &quot;line one\nline two&quot;", "1 likes, 2 comments - ty.builds on May 1, 2026: &quot;x&quot;. ");
        assert_eq!(m.author.as_deref(), Some("Ty (@ty.builds)"));
        assert_eq!(m.title.as_deref(), Some("line one"));
        assert_eq!(m.caption.as_deref(), Some("line one\nline two"));
        // No description, or one whose handle has an unexpected shape: Name only.
        assert_eq!(page("Ty on Instagram: &quot;hi&quot;", "").author.as_deref(), Some("Ty"));
        assert_eq!(page("Ty on Instagram: &quot;hi&quot;", "5 likes - a b/c on May 1, 2026: &quot;x&quot;").author.as_deref(), Some("Ty"));
        // A caption that itself contains " - x on " is not mistaken for the handle.
        assert_eq!(page("Ty on Instagram: &quot;hi&quot;", "May 1, 2026: &quot;a - evil on b&quot;").author.as_deref(), Some("Ty"));
        // A reel with no caption is still a reel.
        let m = page("Ty on Instagram", "");
        assert_eq!((m.unavailable, m.author.as_deref(), m.caption, m.title), (None, Some("Ty"), None, None));
    }

    #[test]
    fn the_embed_yields_an_unescaped_video_url_and_duration() {
        let mut meta = PlatformMeta::new(SourceKind::Instagram);
        parse_embed(EMBED, &mut meta);
        let media = meta.media.expect("video_url");
        assert!(media.url.starts_with("https://instagram.fbkk22-4.fna.fbcdn.net/o1/v/t2/f2/m86/AQPiLWbnb2__"), "{}", media.url);
        assert!(media.url.ends_with(".mp4?redacted=1"), "{}", media.url);
        assert!(!media.url.contains('\\'), "{}", media.url);
        assert_eq!(media.allowed_host_suffixes, MEDIA_HOSTS);
        assert_eq!(media.headers, vec![("user-agent", MOBILE_SAFARI_UA.to_string())], "the download must carry the UA the URL was measured with");
        assert!(media.cookies.is_empty());
        assert!((meta.duration_secs.unwrap() - 143.233).abs() < 0.01, "{:?}", meta.duration_secs);
        assert_eq!(meta.unavailable, None);
    }

    #[test]
    fn a_video_url_is_unescaped_as_deep_as_it_is_nested() {
        // Two layers of escaping, with `&` for `&` and `%3D` for `%3D`.
        let page = embed(r#"\"video_url\":\"https:\\\/\\\/scontent.cdninstagram.com\\\/v\\\/a.mp4?x=1\\u0026y=2\\u00253D\""#);
        let mut meta = PlatformMeta::new(SourceKind::Instagram);
        parse_embed(&page, &mut meta);
        assert_eq!(meta.media.unwrap().url, "https://scontent.cdninstagram.com/v/a.mp4?x=1&y=2%3D");
        assert_eq!(meta.duration_secs, Some(12.5));

        // One layer (the JSON sits directly in the page), and none at all.
        for (json, expect) in [
            (r#"{"video_url":"https:\/\/a.fbcdn.net\/v.mp4?x=1&y=2","video_duration":3}"#, "https://a.fbcdn.net/v.mp4?x=1&y=2"),
            (r#"{"video_url": "https://a.fbcdn.net/v.mp4?x=1&y=2"}"#, "https://a.fbcdn.net/v.mp4?x=1&y=2"),
        ] {
            let mut meta = PlatformMeta::new(SourceKind::Instagram);
            parse_embed(&format!("<script>{json}</script>"), &mut meta);
            assert_eq!(meta.media.unwrap().url, expect);
        }
    }

    #[test]
    fn the_embed_ignores_what_is_not_a_video_url() {
        for tail in [
            // Not https, not a string, a different key that merely ends the same way, undecodable.
            r#"\"video_url\":\"javascript:alert(1)\""#,
            r#"\"video_url\":null"#,
            r#"\"not_video_url\":\"https:\\\/\\\/a.fbcdn.net\\\/v.mp4\""#,
            r#"\"video_url\":\"https:\\\/\\\/a.fbcdn.net\\\/\\x\""#,
            r#"\"video_url\":\"\""#,
        ] {
            let mut meta = PlatformMeta::new(SourceKind::Instagram);
            parse_embed(&embed(tail), &mut meta);
            assert!(meta.media.is_none(), "{tail}");
            assert_eq!(meta.duration_secs, Some(12.5), "the duration is independent: {tail}");
            assert_eq!(meta.unavailable, None, "a missing video is not a failed source: {tail}");
        }
        // A zero or garbled duration is absent.
        for d in ["0", "-3", "\"x\""] {
            let mut meta = PlatformMeta::new(SourceKind::Instagram);
            parse_embed(&format!("<script>{{\"video_duration\":{d}}}</script>"), &mut meta);
            assert_eq!(meta.duration_secs, None, "{d}");
        }
    }

    #[test]
    fn instagram_tolerates_a_page_without_its_data() {
        for html in [
            // What the desktop UA gets: the login page, 200 after the redirect.
            "<!DOCTYPE html><html><head><title>Instagram</title></head><body><div id=\"react-root\"></div></body></html>",
            "",
            "<meta property=\"og:title\" content=\"Instagram\">",
            "<meta property=\"og:title\" content=\"\">",
            "<meta property=\"og:description\" content=\"33K likes - x on May 1, 2026: &quot;hi&quot;\">",
        ] {
            let meta = parse_reel_page(html);
            assert_eq!(meta.unavailable.as_deref(), Some("Instagram returned no reel data"), "{html}");
            assert!(meta.media.is_none() && meta.caption.is_none() && meta.author.is_none() && meta.title.is_none());
        }
        let mut meta = PlatformMeta::new(SourceKind::Instagram);
        parse_embed("<html><body>Log in</body></html>", &mut meta);
        assert_eq!(meta, PlatformMeta::new(SourceKind::Instagram));
    }

    #[tokio::test]
    async fn fetch_meta_reads_both_pages_with_the_mobile_ua_and_the_shortcode_alone() {
        let mut server = mockito::Server::new_async().await;
        let reel = server
            .mock("GET", format!("/reel/{CODE}/").as_str())
            .match_header("user-agent", MOBILE_SAFARI_UA)
            .with_status(200)
            .with_body(REEL)
            .create_async()
            .await;
        let embed = server
            .mock("GET", format!("/reel/{CODE}/embed/captioned/").as_str())
            .match_header("user-agent", MOBILE_SAFARI_UA)
            .match_query(Matcher::Missing)
            .with_status(200)
            .with_body(EMBED)
            .create_async()
            .await;

        let meta = fetch_meta_with(&server.url(), CODE, FetchPolicy { allow_loopback: true }, &CancellationToken::new()).await;
        reel.assert_async().await;
        embed.assert_async().await;
        assert_eq!(meta.unavailable, None);
        assert_eq!(meta.canonical_url.as_deref(), Some(CANONICAL), "the production URL, whatever origin the test aims at");
        assert_eq!(meta.author.as_deref(), Some("Chanisara Wongdeeprasith (@thechanisara)"));
        assert!(meta.media.is_some() && meta.duration_secs.is_some());
        assert_eq!(meta.transcript, None);
    }

    #[tokio::test]
    async fn the_peek_reads_the_reel_page_and_never_the_embed_page() {
        let mut server = mockito::Server::new_async().await;
        let reel = server.mock("GET", format!("/reel/{CODE}/").as_str()).with_status(200).with_body(REEL).expect(1).create_async().await;
        let embed = server.mock("GET", format!("/reel/{CODE}/embed/captioned/").as_str()).expect(0).create_async().await;
        let meta = fetch_reel_with(&server.url(), CODE, FetchPolicy { allow_loopback: true }, &CancellationToken::new()).await;
        reel.assert_async().await;
        embed.assert_async().await;
        assert_eq!(meta.author.as_deref(), Some("Chanisara Wongdeeprasith (@thechanisara)"));
        assert!(meta.caption.is_some() && meta.media.is_none());
    }

    #[test]
    fn the_canonical_url_is_the_production_reel_url() {
        assert_eq!(canonical_url(CODE).as_deref(), Some(CANONICAL));
        assert_eq!(canonical_url("a-B_9").as_deref(), Some("https://www.instagram.com/reel/a-B_9/"));
        for bad in ["", "a/b", "a?b=1", "a#b", "a b", "ü", "..", "a.b"] {
            assert_eq!(canonical_url(bad), None, "{bad:?}");
        }
    }

    #[tokio::test]
    async fn a_login_wall_a_failing_page_and_a_bad_shortcode_say_why() {
        let mut server = mockito::Server::new_async().await;
        let _l = server.mock("GET", "/reel/LOGIN/").with_status(200).with_body("<title>Instagram</title>").create_async().await;
        let _g = server.mock("GET", "/reel/GONE/").with_status(404).create_async().await;
        let never = server.mock("GET", "/reel/LOGIN/embed/captioned/").expect(0).create_async().await;
        let policy = FetchPolicy { allow_loopback: true };
        let cancel = CancellationToken::new();

        let meta = fetch_meta_with(&server.url(), "LOGIN", policy, &cancel).await;
        assert_eq!(meta.unavailable.as_deref(), Some("Instagram returned no reel data"));
        never.assert_async().await;
        let meta = fetch_meta_with(&server.url(), "GONE", policy, &cancel).await;
        assert_eq!(meta.unavailable.as_deref(), Some("Instagram answered HTTP 404"));
        // A shortcode with a path in it is refused before any request is built.
        let meta = fetch_meta_with(&server.url(), "../x", policy, &cancel).await;
        assert_eq!(meta.unavailable.as_deref(), Some("not an Instagram post link"));
        // The default policy refuses the loopback mock: the guard is in the path.
        let meta = fetch_meta_with(&server.url(), "LOGIN", FetchPolicy::default(), &cancel).await;
        assert!(meta.unavailable.unwrap().starts_with("Instagram could not be reached"));
        // A cancelled fetch says so.
        let c = CancellationToken::new();
        c.cancel();
        let meta = fetch_meta_with(&server.url(), "LOGIN", policy, &c).await;
        assert_eq!(meta.unavailable.as_deref(), Some("cancelled"));
    }

    #[tokio::test]
    async fn a_failing_embed_keeps_the_caption() {
        let mut server = mockito::Server::new_async().await;
        let _r = server.mock("GET", format!("/reel/{CODE}/").as_str()).with_status(200).with_body(REEL).create_async().await;
        let _e = server.mock("GET", format!("/reel/{CODE}/embed/captioned/").as_str()).with_status(500).create_async().await;
        let meta = fetch_meta_with(&server.url(), CODE, FetchPolicy { allow_loopback: true }, &CancellationToken::new()).await;
        assert_eq!(meta.unavailable, None);
        assert!(meta.caption.is_some() && meta.canonical_url.is_some());
        assert_eq!((meta.media, meta.duration_secs), (None, None));
    }
}
