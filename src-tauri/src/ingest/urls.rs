//! Find sources in text (spec "ingest/urls.rs").

use reqwest::Url;

use crate::ingest::SourceKind;

/// Decision 9: fetching is pre-selected only below this many alphanumerics.
pub const MOSTLY_URLS_THRESHOLD: usize = 80;

const NOT_A_VIDEO: &str = "not a YouTube video link";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlKind {
    Web,
    YouTube { id: String },
    /// `id` is `None` for the short links (`vt.tiktok.com/<code>`,
    /// `tiktok.com/t/<code>`): the video id only appears once they resolve.
    TikTok { id: Option<String> },
    Instagram { shortcode: String },
    /// `post` is true for a text/photo post, false for a video or reel.
    Facebook { post: bool },
    X { id: String },
    Unsupported(String),
}

/// The ingest kind a classified link is fetched as; `None` for web pages,
/// YouTube and unsupported links.
pub fn social_kind(kind: &UrlKind) -> Option<SourceKind> {
    match kind {
        UrlKind::TikTok { .. } => Some(SourceKind::TikTok),
        UrlKind::Instagram { .. } => Some(SourceKind::Instagram),
        UrlKind::Facebook { .. } => Some(SourceKind::Facebook),
        UrlKind::X { .. } => Some(SourceKind::X),
        UrlKind::Web | UrlKind::YouTube { .. } | UrlKind::Unsupported(_) => None,
    }
}

/// Put a space before an `http://`/`https://` glued onto a letter or digit.
/// Only a letter or digit: `?next=https://` and `/web/2024/https://` are one
/// URL carrying another, and must stay whole.
pub fn split_glued(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    let mut prev: Option<char> = None;
    for (i, c) in text.char_indices() {
        let rest = &text[i..];
        if (rest.starts_with("http://") || rest.starts_with("https://")) && prev.is_some_and(char::is_alphanumeric) {
            out.push(' ');
        }
        out.push(c);
        prev = Some(c);
    }
    out
}

/// Every URL in `text`, deduped, first-seen order, entities decoded.
pub fn detect(text: &str) -> Vec<String> {
    detect_glued(&split_glued(text))
}

/// `detect`'s inner half: takes text that has already been through
/// `split_glued`, so callers holding that result (`context_text`) don't run
/// it twice.
fn detect_glued(glued: &str) -> Vec<String> {
    crate::db::extract_urls(glued)
}

pub fn classify(url: &str) -> UrlKind {
    let Ok(parsed) = Url::parse(url) else {
        return UrlKind::Unsupported("not a valid link".into());
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return UrlKind::Unsupported("only http and https links can be fetched".into());
    }
    let host = parsed.host_str().unwrap_or("");
    let host = host.strip_prefix("www.").or_else(|| host.strip_prefix("m.")).unwrap_or(host);
    let segments: Vec<&str> = parsed.path_segments().map(|s| s.filter(|p| !p.is_empty()).collect()).unwrap_or_default();
    let video = |id: Option<&str>| {
        id.and_then(valid_id).map(|id| UrlKind::YouTube { id }).unwrap_or_else(|| UrlKind::Unsupported(NOT_A_VIDEO.into()))
    };
    if host == "youtu.be" {
        return video(segments.first().copied());
    }
    if matches!(host, "youtube.com" | "music.youtube.com" | "youtube-nocookie.com") {
        return match segments.as_slice() {
            ["watch"] => {
                let v = parsed.query_pairs().find(|(k, _)| k == "v").map(|(_, v)| v.into_owned());
                video(v.as_deref())
            }
            ["shorts" | "embed" | "live", id, ..] => video(Some(id)),
            ["playlist"] => UrlKind::Unsupported("YouTube playlists are not supported yet".into()),
            ["results"] => UrlKind::Unsupported("YouTube search pages are not supported".into()),
            [first, ..] if first.starts_with('@') || matches!(*first, "channel" | "c" | "user") => {
                UrlKind::Unsupported("YouTube channels are not supported yet".into())
            }
            _ => UrlKind::Unsupported(NOT_A_VIDEO.into()),
        };
    }
    classify_social(&parsed, &segments).unwrap_or(UrlKind::Web)
}

/// Query keys that identify nothing — share and tracking decoration (spec
/// §5.1). `s` and `t` are tracking only on X, where `?s=20&t=…` is the share
/// sheet's; on Facebook `permalink.php` the identity is `story_fbid` and `id`.
const TRACKING_KEYS: [&str; 8] = ["srtk", "igsh", "igshid", "is_from_webapp", "sender_device", "mibextid", "rdid", "share_url"];

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

pub(super) fn is_code(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Keep the query pairs whose key `keep` accepts. Raw `&`-split, not
/// `query_pairs`, so what is kept is byte-for-byte what was given.
fn retain_query(u: &mut Url, keep: impl Fn(&str) -> bool) {
    let kept: Vec<&str> = u
        .query()
        .unwrap_or("")
        .split('&')
        .filter(|pair| !pair.is_empty() && keep(pair.split('=').next().unwrap_or("")))
        .collect();
    let query = (!kept.is_empty()).then(|| kept.join("&"));
    u.set_query(query.as_deref());
}

/// The query keys that ARE a post's identity on the social platforms
/// (Facebook `permalink.php` and `watch/?v=`); `display_url` keeps these and
/// nothing else.
const IDENTITY_KEYS: [&str; 3] = ["v", "story_fbid", "id"];

/// `canonical_url` of each link, deduped, first-seen order: two shares of one
/// post differ only by tracking, and ingesting it twice is waste.
pub fn canonical_unique(urls: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(urls.len());
    for url in urls {
        let canonical = canonical_url(url);
        if !out.contains(&canonical) {
            out.push(canonical);
        }
    }
    out
}

/// `Some` for the four social platforms' hosts (a recognised shape or an
/// `Unsupported` with the reason), `None` for any other host.
fn classify_social(parsed: &Url, segments: &[&str]) -> Option<UrlKind> {
    let host = parsed.host_str().unwrap_or("");
    let host = host.strip_prefix("www.").or_else(|| host.strip_prefix("mobile.")).or_else(|| host.strip_prefix("m.")).unwrap_or(host);
    let query = |key: &str| parsed.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned());
    let no = |reason: &str| Some(UrlKind::Unsupported(reason.into()));
    match host {
        "tiktok.com" => Some(match segments {
            [user, "video", id] if user.starts_with('@') && is_digits(id) => UrlKind::TikTok { id: Some((*id).into()) },
            ["v", id] if is_digits(id.trim_end_matches(".html")) => UrlKind::TikTok { id: Some(id.trim_end_matches(".html").into()) },
            ["t", code] if is_code(code) => UrlKind::TikTok { id: None },
            _ => return no("not a TikTok video link"),
        }),
        "vt.tiktok.com" | "vm.tiktok.com" => match segments {
            [code] if is_code(code) => Some(UrlKind::TikTok { id: None }),
            _ => no("not a TikTok video link"),
        },
        "instagram.com" => Some(match segments {
            ["reel" | "reels" | "p" | "tv", code, ..] if is_code(code) => UrlKind::Instagram { shortcode: (*code).into() },
            ["stories", ..] => return no("Instagram Stories are not supported"),
            // The app's share sheet scopes a link to its author: `/<user>/reel/<code>/`.
            [_, "reel" | "p", code, ..] if is_code(code) => UrlKind::Instagram { shortcode: (*code).into() },
            _ => return no("not an Instagram post or reel link"),
        }),
        "fb.watch" => match segments {
            [code] if is_code(code) => Some(UrlKind::Facebook { post: false }),
            _ => no("not a Facebook video link"),
        },
        "facebook.com" | "web.facebook.com" => Some(match segments {
            ["reel", id, ..] if is_digits(id) => UrlKind::Facebook { post: false },
            ["share", "r" | "v", code, ..] if is_code(code) => UrlKind::Facebook { post: false },
            ["share", "p", code, ..] if is_code(code) => UrlKind::Facebook { post: true },
            ["watch"] if query("v").is_some_and(|v| is_digits(&v)) => UrlKind::Facebook { post: false },
            // `/<page>/videos/<id>/`, and Facebook's own `og:url` form with a
            // percent-encoded title slug before the id: the id is the LAST segment.
            [_, "videos", .., last] if is_digits(last) => UrlKind::Facebook { post: false },
            // `story.php` is the mobile site's `permalink.php`: the same two keys.
            ["permalink.php" | "story.php"] if query("story_fbid").is_some() => UrlKind::Facebook { post: true },
            // Facebook's own `og:url` puts a percent-encoded slug before the id.
            [_, "posts", id, ..] | [_, "posts", .., id] if is_code(id) => UrlKind::Facebook { post: true },
            ["stories", ..] => return no("Facebook Stories are not supported"),
            _ => return no("not a Facebook video or post link"),
        }),
        "x.com" | "twitter.com" => Some(match segments {
            [_, "status", id, ..] | ["i", "web", "status", id, ..] if is_digits(id) => UrlKind::X { id: (*id).into() },
            _ => return no("not a link to a single post on X"),
        }),
        _ => None,
    }
}

/// The link with its share and tracking decoration removed, for the four
/// social kinds only (D12); every other link, YouTube included, comes back
/// unchanged. Offline: share codes in the path stay (`classify_social` accepts
/// them) and are replaced by the fetch once the platform reveals the post.
pub fn canonical_url(url: &str) -> String {
    let kind = classify(url);
    if social_kind(&kind).is_none() {
        return url.to_string();
    }
    let Ok(mut u) = Url::parse(url) else { return url.to_string() };
    let on_x = matches!(kind, UrlKind::X { .. });
    retain_query(&mut u, |key| !TRACKING_KEYS.contains(&key) && !(on_x && matches!(key, "s" | "t")));
    u.set_fragment(None);
    if matches!(kind, UrlKind::Instagram { .. }) {
        let path = instagram_path(u.path());
        u.set_path(&path);
    }
    u.to_string()
}

/// An Instagram post's path without its decoration: `/reels/` is `/reel/`,
/// and an author-scoped `/<user>/reel/<code>/` (or `/p/`) loses the author, so
/// it is the same link as the unscoped one.
fn instagram_path(path: &str) -> String {
    let segments: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    match segments.as_slice() {
        [first, kind @ ("reel" | "p"), code, ..] if !matches!(*first, "reel" | "reels" | "p" | "tv") => format!("/{kind}/{code}/"),
        _ => path.replacen("/reels/", "/reel/", 1),
    }
}

/// The forms a note may cite `url` by: its `canonical_url`, then the same
/// link with the trailing slash toggled — `/reel/<id>` and `/reel/<id>/` are
/// one post. A link with a query or fragment has one form: a slash before
/// `?` is a different path to toggle, and not worth guessing.
pub fn citation_forms(url: &str) -> Vec<String> {
    let canonical = canonical_url(url);
    if canonical.contains(['?', '#']) || !canonical.contains("://") {
        return vec![canonical];
    }
    let other = match canonical.strip_suffix('/') {
        Some(without) => without.to_string(),
        None => format!("{canonical}/"),
    };
    vec![canonical, other]
}

fn valid_id(s: &str) -> Option<String> {
    (s.len() == 11 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')).then(|| s.to_string())
}

/// What a provider sees: no query, no fragment, no credentials — signed-URL
/// tokens never leave Jodd (spec "URLs that carry secrets"). YouTube becomes
/// `https://youtu.be/<id>`, identifiable without any other parameter. The one
/// exception is a TikTok/Instagram/Facebook/X link, which keeps the query keys
/// that are the post's identity (`v`, `story_fbid`, `id` — `IDENTITY_KEYS`)
/// and nothing else: dropping them would leave `permalink.php` naming no post.
pub fn display_url(url: &str) -> String {
    let canonical = canonical_url(url);
    let kind = classify(&canonical);
    if let UrlKind::YouTube { id } = kind {
        return format!("https://youtu.be/{id}");
    }
    if social_kind(&kind).is_some() {
        if let Ok(mut u) = Url::parse(&canonical) {
            retain_query(&mut u, |key| IDENTITY_KEYS.contains(&key));
            u.set_fragment(None);
            let _ = u.set_username("");
            let _ = u.set_password(None);
            return u.to_string();
        }
    }
    match Url::parse(url) {
        Ok(mut u) => {
            u.set_query(None);
            u.set_fragment(None);
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u.to_string()
        }
        Err(_) => url.split(['?', '#']).next().unwrap_or(url).to_string(),
    }
}

pub fn host_of(url: &str) -> Option<String> {
    Url::parse(url).ok()?.host_str().map(str::to_string)
}

/// What `applog` may record: host + path, never a query string.
pub fn log_form(url: &str) -> String {
    match Url::parse(&canonical_url(url)) {
        Ok(u) => format!("{}{}", u.host_str().unwrap_or(""), u.path()),
        Err(_) => "<unparseable url>".to_string(),
    }
}

/// The input with its URLs removed — the user's own statement of why these
/// sources were collected (Decision 11). Whitespace collapsed.
pub fn context_text(text: &str) -> String {
    let mut rest = split_glued(text);
    let mut urls = detect_glued(&rest);
    // Longest first: a longer URL is never a substring of a shorter one, so
    // removing longest-first means no removal can eat part of another URL
    // still waiting to be removed (e.g. "https://a.com" is a prefix of
    // "https://a.com/blog" — removing the short one first would leave "/blog"
    // behind looking like user prose).
    urls.sort_unstable_by_key(|u| std::cmp::Reverse(u.len()));
    for url in urls {
        rest = rest.replace(&url, " ");
    }
    rest.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `text` with every URL that carries a query string, a fragment or
/// credentials rewritten to its `display_url`, so signed-URL tokens stay out
/// of text bound for an LLM provider that is not an ingest map call (Ask
/// Jodd reads a note's stored Source block, whose `URL:` lines keep the full
/// link). Everything else — including URLs with no such part — is left
/// byte-for-byte.
///
/// Only secret-bearing URLs are rewritten, longest first: `display_url`
/// normalises a bare host to `https://a.com/`, so rewriting `https://a.com`
/// would corrupt a later `https://a.com/blog` — `context_text`'s prefix trap.
pub fn redact_url_secrets(text: &str) -> String {
    let mut secret_urls: Vec<String> = detect(text).into_iter().filter(|u| carries_secret(u)).collect();
    secret_urls.sort_unstable_by_key(|u| std::cmp::Reverse(u.len()));
    let mut out = text.to_string();
    for url in secret_urls {
        out = out.replace(&url, &display_url(&url));
    }
    out
}

fn carries_secret(url: &str) -> bool {
    match Url::parse(url) {
        Ok(u) => u.query().is_some() || u.fragment().is_some() || !u.username().is_empty() || u.password().is_some(),
        Err(_) => url.contains(['?', '#']),
    }
}

pub fn is_mostly_urls(text: &str) -> bool {
    context_text(text).chars().filter(|c| c.is_alphanumeric()).count() < MOSTLY_URLS_THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yt(id: &str) -> UrlKind {
        UrlKind::YouTube { id: id.to_string() }
    }

    /// Measured in the vault (spec "Problem"): links glued onto the text
    /// before them, which `db::extract_urls` alone reads as one URL.
    #[test]
    fn glued_urls_split_into_two() {
        assert_eq!(
            detect("…Silfrainhttps://www.youtube.com/watch?v=jXtnhyro-QE"),
            vec!["https://www.youtube.com/watch?v=jXtnhyro-QE".to_string()]
        );
        assert_eq!(
            detect("https://www.youtube.com/watch?v=jXtnhyro-QE…EtsNhttps://www.youtube.com/shorts/ve4f7oz-UPs"),
            vec![
                "https://www.youtube.com/watch?v=jXtnhyro-QE…EtsN".to_string(),
                "https://www.youtube.com/shorts/ve4f7oz-UPs".to_string(),
            ]
        );
        assert_eq!(
            detect("https://www.youtube.com/watch?v=jXtnhyro-QEhttps://youtu.be/ve4f7oz-UPs"),
            vec!["https://www.youtube.com/watch?v=jXtnhyro-QE".to_string(), "https://youtu.be/ve4f7oz-UPs".to_string()]
        );
    }

    #[test]
    fn a_url_nested_inside_a_url_is_not_split() {
        for one in ["https://web.archive.org/web/2024/https://example.com/", "https://x.example/?next=https://y.example/"] {
            assert_eq!(detect(one), vec![one.to_string()], "{one}");
        }
    }

    #[test]
    fn youtube_ids_come_from_every_video_shape_ignoring_t_si_and_list() {
        assert_eq!(classify("https://www.youtube.com/watch?v=jXtnhyro-QE&t=42s&list=PLx"), yt("jXtnhyro-QE"));
        assert_eq!(classify("https://youtu.be/jXtnhyro-QE?si=abc&t=3"), yt("jXtnhyro-QE"));
        assert_eq!(classify("https://youtube.com/shorts/ve4f7oz-UPs?si=x"), yt("ve4f7oz-UPs"));
        assert_eq!(classify("https://www.youtube.com/embed/jXtnhyro-QE"), yt("jXtnhyro-QE"));
        assert_eq!(classify("https://m.youtube.com/live/jXtnhyro-QE?feature=share"), yt("jXtnhyro-QE"));
    }

    #[test]
    fn playlists_channels_and_search_pages_are_unsupported() {
        for url in [
            "https://www.youtube.com/playlist?list=PL123",
            "https://www.youtube.com/@somechannel",
            "https://www.youtube.com/channel/UC123",
            "https://www.youtube.com/results?search_query=rust",
        ] {
            assert!(matches!(classify(url), UrlKind::Unsupported(_)), "{url}");
        }
    }

    #[test]
    fn ordinary_links_are_web_and_other_schemes_are_unsupported() {
        assert_eq!(classify("https://github.com/BBM-Co-ORG/Jodd"), UrlKind::Web);
        assert!(matches!(classify("ftp://example.com/x"), UrlKind::Unsupported(_)));
    }

    fn tiktok(id: &str) -> UrlKind {
        UrlKind::TikTok { id: Some(id.to_string()) }
    }

    #[test]
    fn the_owners_links_classify_as_their_platform() {
        assert!(matches!(classify("https://www.tiktok.com/@tonsr_/video/7696048386981645569?is_from_webapp=1&sender_device=pc"), UrlKind::TikTok { id: Some(ref i) } if i == "7696048386981645569"));
        assert!(matches!(classify("https://vt.tiktok.com/ZSabc123/"), UrlKind::TikTok { id: None }));
        assert_eq!(classify("https://www.instagram.com/reels/DWy2pKFjXKC/"), UrlKind::Instagram { shortcode: "DWy2pKFjXKC".into() });
        assert_eq!(classify("https://www.facebook.com/share/r/1FVsvkbtSt/"), UrlKind::Facebook { post: false });
        assert_eq!(classify("https://www.facebook.com/permalink.php?story_fbid=pfbid02bt&id=61574743782103"), UrlKind::Facebook { post: true });
        assert_eq!(classify("https://x.com/deepseek_ai/status/2111375646223933650"), UrlKind::X { id: "2111375646223933650".into() });
        assert!(matches!(classify("https://www.instagram.com/thechanisara/"), UrlKind::Unsupported(_)));
    }

    /// One assertion per shape in spec §5.1's table.
    #[test]
    fn every_recognised_shape_classifies() {
        assert_eq!(classify("https://tiktok.com/@u/video/123"), tiktok("123"));
        assert_eq!(classify("https://m.tiktok.com/v/123.html"), tiktok("123"));
        assert_eq!(classify("https://vm.tiktok.com/ZSabc123/"), UrlKind::TikTok { id: None });
        assert_eq!(classify("https://www.tiktok.com/t/ZTabc123/"), UrlKind::TikTok { id: None });
        for p in ["reel", "reels", "p", "tv"] {
            assert_eq!(classify(&format!("https://www.instagram.com/{p}/Abc_-123/?igsh=x")), UrlKind::Instagram { shortcode: "Abc_-123".into() }, "{p}");
        }
        let video = UrlKind::Facebook { post: false };
        let post = UrlKind::Facebook { post: true };
        assert_eq!(classify("https://www.facebook.com/reel/1234567890"), video);
        assert_eq!(classify("https://m.facebook.com/share/v/1AbC/"), video);
        assert_eq!(classify("https://www.facebook.com/somepage/videos/1234567890/"), video);
        assert_eq!(classify("https://www.facebook.com/watch/?v=1234567890"), video);
        assert_eq!(classify("https://fb.watch/abcDEF12/"), video);
        assert_eq!(classify("https://www.facebook.com/somepage/posts/pfbid02bt"), post);
        assert_eq!(classify("https://www.facebook.com/share/p/1AbC/"), post);
        for host in ["x.com", "twitter.com", "mobile.twitter.com", "mobile.x.com", "www.x.com"] {
            assert_eq!(classify(&format!("https://{host}/a/status/42?s=20")), UrlKind::X { id: "42".into() }, "{host}");
        }
    }

    /// Facebook's own canonical video URL, as `og:url` named it for
    /// `/reel/2601500666993021` (live probe, 2026-10-18): a percent-encoded
    /// title slug sits between `videos` and the id.
    const FB_VIDEO_WITH_SLUG: &str = "https://www.facebook.com/61594930971517/videos/%E0%B8%9B%E0%B8%A3%E0%B8%B0%E0%B8%98%E0%B8%B2%E0%B8%99%E0%B8%AA%E0%B8%B2%E0%B8%A7%E0%B9%80%E0%B8%9C%E0%B8%A5%E0%B8%AD%E0%B8%99%E0%B8%AD%E0%B8%99%E0%B8%81%E0%B8%B1%E0%B8%9A%E0%B8%AB%E0%B8%99%E0%B8%B8%E0%B9%88%E0%B8%A1%E0%B9%80%E0%B8%9E%E0%B8%B4%E0%B9%88%E0%B8%87%E0%B8%9E%E0%B9%89%E0%B8%99%E0%B8%84%E0%B8%B8%E0%B8%81-%E0%B9%84%E0%B8%A1%E0%B9%88%E0%B8%A3%E0%B8%B9%E0%B9%89%E0%B9%80%E0%B8%A5%E0%B8%A2%E0%B8%A7%E0%B9%88%E0%B8%B2%E0%B9%80%E0%B8%82%E0%B8%B2%E0%B8%84%E0%B8%B7%E0%B8%AD%E0%B8%97%E0%B9%88%E0%B8%B2%E0%B8%99%E0%B8%AD%E0%B9%8B%E0%B8%AD%E0%B8%87%E0%B8%97%E0%B8%B5%E0%B9%88%E0%B8%97%E0%B8%AB%E0%B8%B2%E0%B8%A3%E0%B8%AB%E0%B8%A1%E0%B8%B7%E0%B9%88%E0%B8%99%E0%B8%99%E0%B8%B2%E0%B8%A2%E0%B8%95%E0%B9%89%E0%B8%AD/2601500666993021/";

    #[test]
    fn facebooks_own_video_url_with_a_title_slug_is_a_video() {
        assert_eq!(classify(FB_VIDEO_WITH_SLUG), UrlKind::Facebook { post: false });
        assert_eq!(canonical_url(FB_VIDEO_WITH_SLUG), FB_VIDEO_WITH_SLUG, "already canonical: it round-trips unchanged");
        assert_eq!(classify("https://www.facebook.com/somepage/videos/some-title/1234567890"), UrlKind::Facebook { post: false });
        // The same form for a post, as the post fixture's og:url has it.
        assert_eq!(classify("https://www.facebook.com/61574743782103/posts/%E0%B8%95%E0%B8%AD-ai-%E0%B9%84/122133160232824792/"), UrlKind::Facebook { post: true });
        // The id is the LAST segment: a slug alone, or a slug after the id, is no video.
        for url in ["https://www.facebook.com/somepage/videos/some-title/", "https://www.facebook.com/somepage/videos/1234567890/some-title/"] {
            assert!(matches!(classify(url), UrlKind::Unsupported(_)), "{url}");
        }
    }

    /// Shapes the platforms' own apps and mobile sites share (final review).
    #[test]
    fn mobile_and_profile_scoped_shapes_classify() {
        let post = UrlKind::Facebook { post: true };
        assert_eq!(classify("https://m.facebook.com/story.php?story_fbid=pfbid02bt&id=61574743782103"), post);
        assert!(matches!(classify("https://m.facebook.com/story.php"), UrlKind::Unsupported(_)), "story.php names no post without story_fbid");
        assert_eq!(
            canonical_url("https://m.facebook.com/story.php?story_fbid=pfbid02bt&id=61574743782103&mibextid=x&rdid=y"),
            "https://m.facebook.com/story.php?story_fbid=pfbid02bt&id=61574743782103"
        );
        for p in ["reel", "p"] {
            let url = format!("https://www.instagram.com/thechanisara/{p}/DeeyrDqyckk/?igsh=x");
            assert_eq!(classify(&url), UrlKind::Instagram { shortcode: "DeeyrDqyckk".into() }, "{url}");
            // The author goes; the kind stays, so it dedupes with the unscoped link.
            assert_eq!(canonical_url(&url), format!("https://www.instagram.com/{p}/DeeyrDqyckk/"), "{url}");
        }
        assert!(matches!(classify("https://www.instagram.com/thechanisara/tagged/"), UrlKind::Unsupported(_)));
        assert_eq!(classify("https://x.com/i/web/status/2111113436314648758?s=20"), UrlKind::X { id: "2111113436314648758".into() });
        assert_eq!(canonical_url("https://x.com/i/web/status/2111113436314648758?s=20"), "https://x.com/i/web/status/2111113436314648758");
    }

    #[test]
    fn the_unsupported_platform_shapes_say_why() {
        for url in [
            "https://www.tiktok.com/@tonsr_",
            "https://www.tiktok.com/tag/rust",
            "https://www.instagram.com/stories/someone/123/",
            "https://www.instagram.com/explore/tags/rust/",
            "https://www.facebook.com/somepage",
            "https://www.facebook.com/watch/",
            "https://www.facebook.com/stories/123",
            "https://www.facebook.com/permalink.php",
            "https://x.com/deepseek_ai",
            "https://x.com/hashtag/rust",
            "https://x.com/home",
        ] {
            let UrlKind::Unsupported(reason) = classify(url) else { panic!("{url} should be unsupported") };
            assert!(!reason.is_empty(), "{url}");
        }
    }

    #[test]
    fn lookalike_hosts_stay_web() {
        for url in ["https://nottiktok.com/@u/video/1", "https://instagram.com.evil.example/reel/abc/", "https://max.com/a/status/1"] {
            assert_eq!(classify(url), UrlKind::Web, "{url}");
        }
    }

    #[test]
    fn social_kind_maps_only_the_four_platforms() {
        assert_eq!(social_kind(&tiktok("1")), Some(SourceKind::TikTok));
        assert_eq!(social_kind(&UrlKind::Instagram { shortcode: "a".into() }), Some(SourceKind::Instagram));
        assert_eq!(social_kind(&UrlKind::Facebook { post: true }), Some(SourceKind::Facebook));
        assert_eq!(social_kind(&UrlKind::X { id: "1".into() }), Some(SourceKind::X));
        assert_eq!(social_kind(&yt("jXtnhyro-QE")), None);
        assert_eq!(social_kind(&UrlKind::Web), None);
        assert_eq!(social_kind(&UrlKind::Unsupported("x".into())), None);
    }

    #[test]
    fn canonical_url_strips_tracking_and_keeps_identity() {
        assert_eq!(canonical_url("https://www.instagram.com/reel/DeeyrDqyckk/?srtk=cnlldmM0ajl6d21m"), "https://www.instagram.com/reel/DeeyrDqyckk/");
        assert_eq!(canonical_url("https://www.instagram.com/reels/DWy2pKFjXKC/?igsh=abc"), "https://www.instagram.com/reel/DWy2pKFjXKC/");
        assert_eq!(canonical_url("https://www.tiktok.com/@tonsr_/video/7696048386981645569?is_from_webapp=1&sender_device=pc"), "https://www.tiktok.com/@tonsr_/video/7696048386981645569");
        assert_eq!(canonical_url("https://www.facebook.com/permalink.php?story_fbid=pfbid02bt&id=61574743782103&mibextid=x"), "https://www.facebook.com/permalink.php?story_fbid=pfbid02bt&id=61574743782103");
        assert_eq!(canonical_url("https://x.com/a/status/1?s=20&t=abc"), "https://x.com/a/status/1");
        assert_eq!(canonical_url("https://youtu.be/J9NQTYUP6k0?si=Fk4s"), "https://youtu.be/J9NQTYUP6k0?si=Fk4s"); // YouTube out of D12
    }

    #[test]
    fn canonical_url_edge_cases() {
        // `watch/?v=` keeps its identity; the fragment goes.
        assert_eq!(canonical_url("https://www.facebook.com/watch/?v=123&rdid=zz&share_url=https%3A%2F%2Ff#frag"), "https://www.facebook.com/watch/?v=123");
        // `s` and `t` are tracking on X only.
        assert_eq!(canonical_url("https://www.facebook.com/permalink.php?story_fbid=1&id=2&s=3&t=4"), "https://www.facebook.com/permalink.php?story_fbid=1&id=2&s=3&t=4");
        assert_eq!(canonical_url("https://twitter.com/a/status/1?t=abc&s=20"), "https://twitter.com/a/status/1");
        // Share codes in the path cannot be canonicalised offline; short links lose only their query.
        assert_eq!(canonical_url("https://vt.tiktok.com/ZSabc123/?_r=1&is_from_webapp=1"), "https://vt.tiktok.com/ZSabc123/?_r=1");
        assert_eq!(canonical_url("https://www.facebook.com/share/r/1FVsvkbtSt/?mibextid=wwXIfr"), "https://www.facebook.com/share/r/1FVsvkbtSt/");
        // Already canonical, unsupported and ordinary links come back untouched.
        assert_eq!(canonical_url("https://www.instagram.com/reel/DeeyrDqyckk/"), "https://www.instagram.com/reel/DeeyrDqyckk/");
        assert_eq!(canonical_url("https://www.instagram.com/thechanisara/?igsh=x"), "https://www.instagram.com/thechanisara/?igsh=x");
        assert_eq!(canonical_url("https://example.com/a?s=1&igsh=2"), "https://example.com/a?s=1&igsh=2");
        assert_eq!(canonical_url("not a url"), "not a url");
    }

    #[test]
    fn display_url_keeps_only_a_social_posts_identity_keys() {
        assert_eq!(
            display_url("https://www.facebook.com/permalink.php?story_fbid=1&id=2&fbclid=SECRET&utm_source=x#frag"),
            "https://www.facebook.com/permalink.php?story_fbid=1&id=2"
        );
        assert_eq!(display_url("https://www.facebook.com/watch/?v=123&token=SECRET&_r=1"), "https://www.facebook.com/watch/?v=123");
        assert_eq!(display_url("https://u:pass@www.instagram.com/reel/DeeyrDqyckk/?igsh=abc&token=SECRET"), "https://www.instagram.com/reel/DeeyrDqyckk/");
        assert_eq!(display_url("https://user:pw@x.com/a/status/1?s=20&foo=SECRET"), "https://x.com/a/status/1");
        for out in [
            display_url("https://www.tiktok.com/@u/video/1?_r=1&token=SECRET#f"),
            display_url("https://p:SECRET@vt.tiktok.com/ZSabc123/?utm_x=SECRET"),
        ] {
            assert!(!out.contains("SECRET") && !out.contains('?') && !out.contains('#'), "{out}");
        }
    }

    #[test]
    fn canonical_unique_collapses_shares_of_one_post_in_first_seen_order() {
        let urls: Vec<String> = [
            "https://www.instagram.com/reels/DWy2pKFjXKC/?igsh=abc",
            "https://x.com/a/status/1?s=20",
            "https://www.instagram.com/reel/DWy2pKFjXKC/",
            "https://x.com/a/status/1?t=zz",
            "https://example.com/p",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            canonical_unique(&urls),
            ["https://www.instagram.com/reel/DWy2pKFjXKC/", "https://x.com/a/status/1", "https://example.com/p"]
        );
    }

    #[test]
    fn citation_forms_toggle_the_trailing_slash_of_the_canonical_link() {
        assert_eq!(citation_forms("https://www.facebook.com/reel/1/?mibextid=x"), ["https://www.facebook.com/reel/1/", "https://www.facebook.com/reel/1"]);
        assert_eq!(citation_forms("https://www.facebook.com/reel/1"), ["https://www.facebook.com/reel/1", "https://www.facebook.com/reel/1/"]);
        // A query that is the post's identity stays, and is the only form.
        assert_eq!(citation_forms("https://www.facebook.com/permalink.php?story_fbid=1&id=2"), ["https://www.facebook.com/permalink.php?story_fbid=1&id=2"]);
        assert_eq!(citation_forms("not a url"), ["not a url"]);
    }

    #[test]
    fn display_url_and_log_form_use_the_canonical_form() {
        assert_eq!(display_url("https://www.instagram.com/reels/DWy2pKFjXKC/?igsh=abc"), "https://www.instagram.com/reel/DWy2pKFjXKC/");
        // The query that IS the post's identity survives display; tracking does not.
        assert_eq!(
            display_url("https://www.facebook.com/permalink.php?story_fbid=pfbid02bt&id=61574743782103&mibextid=x"),
            "https://www.facebook.com/permalink.php?story_fbid=pfbid02bt&id=61574743782103"
        );
        assert_eq!(log_form("https://www.instagram.com/reels/DWy2pKFjXKC/?igsh=abc"), "www.instagram.com/reel/DWy2pKFjXKC/");
        assert_eq!(log_form("https://x.com/a/status/1?s=20"), "x.com/a/status/1");
    }

    #[test]
    fn display_url_drops_query_fragment_and_credentials_and_canonicalises_youtube() {
        assert_eq!(display_url("https://u:p@example.com/a/b?token=SECRET#frag"), "https://example.com/a/b");
        assert_eq!(display_url("https://www.youtube.com/watch?v=jXtnhyro-QE&t=9"), "https://youtu.be/jXtnhyro-QE");
    }

    #[test]
    fn log_form_is_host_and_path_only() {
        assert_eq!(log_form("https://example.com/a?token=SECRET"), "example.com/a");
        assert_eq!(host_of("https://www.youtube.com/watch?v=x").as_deref(), Some("www.youtube.com"));
    }

    #[test]
    fn redact_url_secrets_strips_queries_fragments_and_credentials() {
        let text = "see https://e.example/p?token=SECRET&x=1 and https://u:pw@h.example/a#frag \
                    and https://www.youtube.com/watch?v=jXtnhyro-QE&si=SECRET2 but https://plain.example/doc stays";
        let out = redact_url_secrets(text);
        for gone in ["SECRET", "SECRET2", "pw@", "#frag", "token="] {
            assert!(!out.contains(gone), "`{gone}` survived: {out}");
        }
        assert!(out.contains("https://e.example/p "), "{out}");
        assert!(out.contains("https://h.example/a "), "{out}");
        assert!(out.contains("https://youtu.be/jXtnhyro-QE "), "{out}");
        assert!(out.contains("https://plain.example/doc stays"), "a URL with no secret is left byte-for-byte: {out}");
    }

    /// `display_url` adds a trailing `/` to a bare host, so rewriting a URL
    /// that has no secret would corrupt a longer URL that starts with it —
    /// the same prefix trap `context_text` fell into.
    #[test]
    fn redact_url_secrets_leaves_a_secretless_prefix_url_alone() {
        let text = "https://a.com and https://a.com/blog?t=1";
        assert_eq!(redact_url_secrets(text), "https://a.com and https://a.com/blog");
    }

    #[test]
    fn is_mostly_urls_counts_alphanumerics_left_after_removing_urls() {
        let with = |n: usize| format!("https://example.com/a {}", "a".repeat(n));
        assert!(is_mostly_urls(&with(79)));
        assert!(!is_mostly_urls(&with(80)));
        assert!(!is_mostly_urls(&with(81)));
    }

    #[test]
    fn context_text_keeps_the_users_words() {
        assert_eq!(
            context_text("Two talks on ownership:\nhttps://youtu.be/jXtnhyro-QE and https://example.com/post"),
            "Two talks on ownership: and"
        );
    }

    /// A shorter detected URL that is a literal prefix of a later, longer one
    /// must not eat the longer one's prefix via `String::replace` matching
    /// every occurrence — removal must go longest-first.
    #[test]
    fn context_text_removes_a_url_that_extends_an_earlier_one() {
        assert_eq!(
            context_text("See https://a.com and also https://a.com/blog for more."),
            "See and also for more."
        );
    }
}
