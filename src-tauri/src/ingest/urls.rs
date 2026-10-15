//! Find sources in text (spec "ingest/urls.rs").

use reqwest::Url;

/// Decision 9: fetching is pre-selected only below this many alphanumerics.
pub const MOSTLY_URLS_THRESHOLD: usize = 80;

const NOT_A_VIDEO: &str = "not a YouTube video link";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlKind {
    Web,
    YouTube { id: String },
    Unsupported(String),
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
    UrlKind::Web
}

fn valid_id(s: &str) -> Option<String> {
    (s.len() == 11 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')).then(|| s.to_string())
}

/// What a provider sees: no query, no fragment, no credentials — signed-URL
/// tokens never leave Jodd (spec "URLs that carry secrets"). YouTube becomes
/// `https://youtu.be/<id>`, identifiable without any other parameter.
pub fn display_url(url: &str) -> String {
    if let UrlKind::YouTube { id } = classify(url) {
        return format!("https://youtu.be/{id}");
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
    match Url::parse(url) {
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
