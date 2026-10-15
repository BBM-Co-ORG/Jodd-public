//! Share to Jodd — the `jodd://capture` contract (spec
//! docs/superpowers/specs/2026-10-06-share-to-jodd-design.md).
//!
//! Every way something reaches Jodd from another app ends here as one URL:
//! the Android share sheet (rewritten in `MainActivity.kt`), a bookmarklet,
//! Raycast / Shortcuts / PowerToys, or `open jodd://capture?...`. Everything
//! in this module is pure so the whole contract is tested on Linux CI.
//!
//! **Any web page can open a `jodd://` link.** Nothing here writes a note:
//! a capture only waits in [`Queue`] until the person taps Save in the
//! capture sheet. The caps, the dedupe and the escaping in [`body_html`] are
//! what keep a hostile page from flooding the app or planting markup.

use reqwest::Url;
use serde::Serialize;

pub const MAX_TEXT_CHARS: usize = 100_000;
pub const MAX_TITLE_CHARS: usize = 300;
pub const MAX_URL_CHARS: usize = 4_096;
/// A page looping `location = 'jodd://…'` stops here.
pub const MAX_PENDING: usize = 20;
/// An identical payload inside this window is the same share delivered twice
/// (or a page repeating itself), not a second capture.
pub const DEDUPE_WINDOW_MS: i64 = 10_000;
/// How much of the first text line a default title takes.
const DEFAULT_TITLE_CHARS: usize = 120;
/// Jodd is raised to the front at most this often. A real person shares far
/// slower; a page looping `jodd://` links would otherwise steal focus forever.
pub const RAISE_INTERVAL_MS: i64 = 3_000;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct CapturePayload {
    pub url: Option<String>,
    pub text: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureError {
    Empty,
    TooLong { field: &'static str, max: usize },
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureError::Empty => write!(f, "it carried no link and no text"),
            CaptureError::TooLong { field, max } => write!(f, "its {field} is longer than {max} characters"),
        }
    }
}

/// What a deep link is for. OAuth stays exactly as it was; a capture is new.
#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    Capture(Result<CapturePayload, CaptureError>),
    /// `(code, state)` — handed to `complete_oauth` unchanged.
    OAuth(String, String),
    Ignore,
}

/// Route by scheme FIRST: a `jodd:` link is never an OAuth callback, even
/// when a page dresses one up with `code=` and `state=` parameters.
pub fn route(url: &str, parse_oauth: impl Fn(&str) -> Option<(String, String)>) -> Route {
    if is_jodd_scheme(url) {
        return if is_capture_url(url) { Route::Capture(parse_capture_url(url)) } else { Route::Ignore };
    }
    match parse_oauth(url) {
        Some((code, state)) => Route::OAuth(code, state),
        None => Route::Ignore,
    }
}

fn is_jodd_scheme(url: &str) -> bool {
    url.get(..5).is_some_and(|s| s.eq_ignore_ascii_case("jodd:"))
}

/// `jodd://capture?…`, plus the `jodd:///capture` and `jodd:capture`
/// spellings some launchers produce.
pub fn is_capture_url(url: &str) -> bool {
    let Ok(u) = Url::parse(url) else { return false };
    if !u.scheme().eq_ignore_ascii_case("jodd") {
        return false;
    }
    let path = u.path().trim_matches('/');
    match u.host_str().filter(|h| !h.is_empty()) {
        Some(host) => host.eq_ignore_ascii_case("capture") && path.is_empty(),
        None => path.eq_ignore_ascii_case("capture"),
    }
}

/// Parse a capture link. Callers check [`is_capture_url`] first (via [`route`]).
pub fn parse_capture_url(url: &str) -> Result<CapturePayload, CaptureError> {
    let mut p = CapturePayload::default();
    if let Ok(u) = Url::parse(url) {
        // `query_pairs` percent-decodes and reads `+` as a space. The first
        // occurrence of a key wins; unknown keys are ignored.
        for (k, v) in u.query_pairs() {
            let slot = match k.as_ref() {
                "url" => &mut p.url,
                "text" => &mut p.text,
                "title" => &mut p.title,
                _ => continue,
            };
            if slot.is_none() {
                *slot = Some(v.into_owned());
            }
        }
    }
    p.url = p.url.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    p.text = p.text.filter(|s| !s.trim().is_empty());
    p.title = p.title.map(|s| s.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|s| !s.is_empty());
    for (field, value, max) in [
        ("text", &p.text, MAX_TEXT_CHARS),
        ("title", &p.title, MAX_TITLE_CHARS),
        ("link", &p.url, MAX_URL_CHARS),
    ] {
        if value.as_ref().is_some_and(|v| v.chars().count() > max) {
            return Err(CaptureError::TooLong { field, max });
        }
    }
    if p.url.is_none() && p.text.is_none() {
        return Err(CaptureError::Empty);
    }
    Ok(p)
}

fn is_http(url: &str) -> bool {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// The capture's links: `url` first, then every `http(s)` URL in the text.
/// Deduped, first-seen order. Only http(s) — the same rule as linkifying.
pub fn links(p: &CapturePayload) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let candidates = p.url.iter().cloned().chain(p.text.iter().flat_map(|t| crate::ingest::urls::detect(t)));
    for u in candidates {
        if is_http(&u) && !out.contains(&u) {
            out.push(u);
        }
    }
    out
}

/// Title shown (and editable) in the sheet: explicit title → first text line
/// that says something besides a link → the first link's host → `Shared <today>`.
pub fn default_title(p: &CapturePayload, today: &str) -> String {
    if let Some(t) = &p.title {
        return t.clone();
    }
    if let Some(text) = &p.text {
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let without_links: String = split_links(line)
                .into_iter()
                .filter_map(|seg| match seg {
                    Seg::Text(s) => Some(s),
                    Seg::Link(_) => None,
                })
                .collect();
            if without_links.chars().any(char::is_alphanumeric) {
                // The links are listed under "What was shared"; the title keeps the words.
                let words = without_links.split_whitespace().collect::<Vec<_>>().join(" ");
                return words.chars().take(DEFAULT_TITLE_CHARS).collect::<String>().trim().to_string();
            }
        }
    }
    if let Some(host) = links(p).first().and_then(|u| crate::ingest::urls::host_of(u)) {
        return host.strip_prefix("www.").unwrap_or(&host).to_string();
    }
    format!("Shared {today}")
}

#[derive(Debug, PartialEq)]
enum Seg<'a> {
    Text(&'a str),
    Link(&'a str),
}

/// Split a line into text and `http(s)` link segments. A link runs to
/// whitespace or a character that cannot end one unescaped in HTML, minus
/// trailing sentence punctuation.
fn split_links(line: &str) -> Vec<Seg<'_>> {
    let mut out = Vec::new();
    let mut rest = line;
    loop {
        let lower = rest.to_ascii_lowercase();
        let start = match (lower.find("http://"), lower.find("https://")) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) | (None, Some(a)) => a,
            (None, None) => break,
        };
        let tail = &rest[start..];
        let mut end = tail.find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '`')).unwrap_or(tail.len());
        while end > 0 && tail[..end].ends_with(['.', ',', ';', ':', '!', '?', ')', ']', '}']) {
            end -= 1;
        }
        let scheme_len = if tail.to_ascii_lowercase().starts_with("https://") { 8 } else { 7 };
        if end <= scheme_len {
            // A bare "https://" is not a link; keep it as text and move on.
            out.push(Seg::Text(&rest[..start + scheme_len]));
            rest = &rest[start + scheme_len..];
            continue;
        }
        if start > 0 {
            out.push(Seg::Text(&rest[..start]));
        }
        out.push(Seg::Link(&tail[..end]));
        rest = &tail[end..];
    }
    if !rest.is_empty() {
        out.push(Seg::Text(rest));
    }
    out
}

fn linkify_line(line: &str) -> String {
    use crate::llm::markdown::escape_html;
    split_links(line)
        .into_iter()
        .map(|seg| match seg {
            Seg::Text(s) => escape_html(s),
            Seg::Link(u) => format!("<a href=\"{0}\">{0}</a>", escape_html(u)),
        })
        .collect()
}

/// The note body for Save as-is: a body fragment (Extract's shape), every
/// character of the shared content escaped, only http(s) URLs linked. The
/// title is NOT in the body — like Extract, the backend injects it on push.
pub fn body_html(p: &CapturePayload) -> String {
    let mut out = String::new();
    let text = p.text.as_deref().unwrap_or("");
    if let Some(url) = &p.url {
        if !text.contains(url.as_str()) {
            let line = if is_http(url) { linkify_line(url) } else { crate::llm::markdown::escape_html(url) };
            out.push_str(&format!("<div>{line}</div>"));
            if !text.is_empty() {
                out.push_str("<div><br></div>");
            }
        }
    }
    for line in text.trim_matches(['\n', '\r']).lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            out.push_str("<div><br></div>");
        } else {
            out.push_str(&format!("<div>{}</div>", linkify_line(line)));
        }
    }
    out
}

/// What "Add to existing note" appends: a blank line, a dated divider so a
/// note that collects shares says when each arrived, then exactly the body
/// Save as-is would have written. `stamp` is local time, `YYYY-MM-DD HH:MM`.
pub fn appended_html(p: &CapturePayload, stamp: &str) -> String {
    format!(
        "<div><br></div><div>— {} —</div>{}",
        crate::llm::markdown::escape_html(stamp),
        body_html(p)
    )
}

/// One queued capture as the sheet sees it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PendingCapture {
    pub id: String,
    pub payload: CapturePayload,
    pub links: Vec<String>,
    pub default_title: String,
    pub received_at_ms: i64,
}

/// Why [`Queue::push`] did not queue a capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dropped {
    /// The same payload arrived within [`DEDUPE_WINDOW_MS`] — even if the
    /// earlier copy was already saved or discarded.
    Duplicate,
    /// [`MAX_PENDING`] captures are already waiting.
    Full,
}

/// What the caller should do after a push: queue it or not, and whether
/// this one may raise the window / tell the person the queue is full.
#[derive(Debug, PartialEq, Eq)]
pub struct Pushed {
    pub result: Result<String, Dropped>,
    /// Raise Jodd and show the sheet. Only for a queued capture, and at most
    /// once per [`RAISE_INTERVAL_MS`].
    pub raise: bool,
    /// Say "too many waiting" — at most once per [`DEDUPE_WINDOW_MS`].
    pub notify_full: bool,
}

/// Captures waiting for the person. In memory on purpose (spec D7): a
/// capture is untrusted input, and it only becomes data once someone taps Save.
#[derive(Default)]
pub struct Queue {
    items: Vec<PendingCapture>,
    /// Every payload seen inside the dedupe window, queued or not — so a
    /// Discard does not reopen the door to the same link a page is looping.
    recent: Vec<(CapturePayload, i64)>,
    last_raise_ms: Option<i64>,
    last_full_notice_ms: Option<i64>,
}

impl Queue {
    pub fn push(&mut self, payload: CapturePayload, now_ms: i64, today: &str) -> Pushed {
        self.recent.retain(|(_, t)| now_ms - t < DEDUPE_WINDOW_MS);
        let dropped = |result, notify_full| Pushed { result: Err(result), raise: false, notify_full };
        if self.recent.iter().any(|(p, _)| *p == payload) {
            return dropped(Dropped::Duplicate, false);
        }
        self.recent.push((payload.clone(), now_ms));
        if self.items.len() >= MAX_PENDING {
            let notify = self.last_full_notice_ms.is_none_or(|t| now_ms - t >= DEDUPE_WINDOW_MS);
            if notify {
                self.last_full_notice_ms = Some(now_ms);
            }
            return dropped(Dropped::Full, notify);
        }
        let id = uuid::Uuid::new_v4().to_string();
        self.items.push(PendingCapture {
            id: id.clone(),
            links: links(&payload),
            default_title: default_title(&payload, today),
            payload,
            received_at_ms: now_ms,
        });
        let raise = self.last_raise_ms.is_none_or(|t| now_ms - t >= RAISE_INTERVAL_MS);
        if raise {
            self.last_raise_ms = Some(now_ms);
        }
        Pushed { result: Ok(id), raise, notify_full: false }
    }

    pub fn pending(&self) -> Vec<PendingCapture> {
        self.items.clone()
    }

    pub fn get(&self, id: &str) -> Option<PendingCapture> {
        self.items.iter().find(|c| c.id == id).cloned()
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|c| c.id != id);
        self.items.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(s: &str) -> String {
        urlencoding::encode(s).into_owned()
    }

    fn no_oauth(_: &str) -> Option<(String, String)> {
        None
    }

    #[test]
    fn an_appended_capture_is_a_blank_line_a_dated_divider_then_the_saved_body() {
        let p = CapturePayload { url: Some("https://a.example/".into()), text: Some("<b>hi</b>".into()), title: None };
        let html = appended_html(&p, "2026-10-09 14:32");
        assert_eq!(html, format!("<div><br></div><div>— 2026-10-09 14:32 —</div>{}", body_html(&p)));
        assert!(!html.contains("<b>"), "the shared text stays escaped: {html}");
    }

    #[test]
    fn parses_url_text_and_title() {
        let u = format!("jodd://capture?url={}&text={}&title={}", enc("https://a.example/x?y=1"), enc("hello\nworld"), enc("A page"));
        let p = parse_capture_url(&u).unwrap();
        assert_eq!(p.url.as_deref(), Some("https://a.example/x?y=1"));
        assert_eq!(p.text.as_deref(), Some("hello\nworld"));
        assert_eq!(p.title.as_deref(), Some("A page"));
    }

    #[test]
    fn plus_is_a_space_and_thai_round_trips() {
        let u = format!("jodd://capture?text=a+b%20c&title={}", enc("จดบันทึก"));
        let p = parse_capture_url(&u).unwrap();
        assert_eq!(p.text.as_deref(), Some("a b c"));
        assert_eq!(p.title.as_deref(), Some("จดบันทึก"));
    }

    #[test]
    fn needs_a_url_or_text() {
        assert_eq!(parse_capture_url("jodd://capture"), Err(CaptureError::Empty));
        assert_eq!(parse_capture_url("jodd://capture?title=only"), Err(CaptureError::Empty));
        assert_eq!(parse_capture_url("jodd://capture?text=%20%0A&url=%20"), Err(CaptureError::Empty));
    }

    #[test]
    fn over_cap_fields_are_refused_not_truncated() {
        let long = "a".repeat(MAX_TEXT_CHARS + 1);
        assert_eq!(
            parse_capture_url(&format!("jodd://capture?text={long}")),
            Err(CaptureError::TooLong { field: "text", max: MAX_TEXT_CHARS })
        );
        let title = "t".repeat(MAX_TITLE_CHARS + 1);
        assert!(matches!(parse_capture_url(&format!("jodd://capture?text=x&title={title}")), Err(CaptureError::TooLong { field: "title", .. })));
        let url = format!("https://a.example/{}", "p".repeat(MAX_URL_CHARS));
        assert!(matches!(parse_capture_url(&format!("jodd://capture?url={}", enc(&url))), Err(CaptureError::TooLong { field: "link", .. })));
        // Exactly at the cap is fine — counted in characters, not bytes.
        let thai = "ก".repeat(MAX_TITLE_CHARS);
        assert!(parse_capture_url(&format!("jodd://capture?text=x&title={}", enc(&thai))).is_ok());
    }

    #[test]
    fn first_duplicate_key_wins_and_unknown_keys_are_ignored() {
        let p = parse_capture_url("jodd://capture?text=one&text=two&evil=1").unwrap();
        assert_eq!(p.text.as_deref(), Some("one"));
    }

    #[test]
    fn title_whitespace_collapses() {
        let p = parse_capture_url("jodd://capture?text=x&title=%20a%0A%0Ab%20").unwrap();
        assert_eq!(p.title.as_deref(), Some("a b"));
    }

    #[test]
    fn capture_url_spellings() {
        assert!(is_capture_url("jodd://capture?text=x"));
        assert!(is_capture_url("JODD://Capture?text=x"));
        assert!(is_capture_url("jodd:///capture?text=x"));
        assert!(is_capture_url("jodd:capture?text=x"));
        assert!(!is_capture_url("jodd://settings?text=x"));
        assert!(!is_capture_url("jodd://capture/more?text=x"));
        assert!(!is_capture_url("https://capture?text=x"));
    }

    #[test]
    fn routing_keeps_oauth_untouched_and_never_reads_a_jodd_link_as_oauth() {
        let oauth = |u: &str| crate::parse_oauth_callback(u);
        assert_eq!(
            route("https://jodd.bbmedia.co.th/oauth2redirect?code=C&state=S", oauth),
            Route::OAuth("C".into(), "S".into())
        );
        // A page dressing a jodd link up as a callback gets nowhere near OAuth.
        assert!(matches!(route("jodd://capture?code=C&state=S&text=hi", oauth), Route::Capture(Ok(_))));
        assert_eq!(route("jodd://oauth2redirect?code=C&state=S", oauth), Route::Ignore);
        assert_eq!(route("https://example.com/", no_oauth), Route::Ignore);
        assert_eq!(route("jodd://capture", no_oauth), Route::Capture(Err(CaptureError::Empty)));
    }

    #[test]
    fn links_are_url_then_text_links_deduped_http_only() {
        let p = CapturePayload {
            url: Some("https://a.example/".into()),
            text: Some("see https://b.example/x and https://a.example/ and javascript:alert(1)".into()),
            title: None,
        };
        assert_eq!(links(&p), vec!["https://a.example/".to_string(), "https://b.example/x".to_string()]);
        let js = CapturePayload { url: Some("javascript:alert(1)".into()), ..Default::default() };
        assert!(links(&js).is_empty());
    }

    #[test]
    fn default_title_order() {
        let today = "2026-10-06";
        let titled = CapturePayload { title: Some("T".into()), text: Some("line".into()), ..Default::default() };
        assert_eq!(default_title(&titled, today), "T");
        let text = CapturePayload { text: Some("\nhttps://x.example/a\nA real line here\n".into()), ..Default::default() };
        assert_eq!(default_title(&text, today), "A real line here");
        let only_link = CapturePayload { text: Some("https://www.youtube.com/watch?v=abc".into()), ..Default::default() };
        assert_eq!(default_title(&only_link, today), "youtube.com");
        let nothing = CapturePayload { url: Some("ftp://x".into()), ..Default::default() };
        assert_eq!(default_title(&nothing, today), "Shared 2026-10-06");
        // A link inside the line is listed under "What was shared" already;
        // the title keeps only the words (measured: a Services share of
        // "… please Discard https://github.com/mcp" titled itself with the URL).
        let mixed = CapturePayload { text: Some("Worth reading https://github.com/mcp today".into()), ..Default::default() };
        assert_eq!(default_title(&mixed, today), "Worth reading today");
        let trailing = CapturePayload { text: Some("Services test - please Discard https://github.com/mcp".into()), ..Default::default() };
        assert_eq!(default_title(&trailing, today), "Services test - please Discard");
        let long = CapturePayload { text: Some("w".repeat(500)), ..Default::default() };
        assert_eq!(default_title(&long, today).chars().count(), 120);
    }

    #[test]
    fn body_escapes_everything_and_links_only_http() {
        let corpus = [
            "<script>alert(1)</script>",
            "<img src=x onerror=alert(1)>",
            "</div></body><h1>x",
            "&lt;script&gt; stays literal",
            "\"><svg onload=alert(1)>",
            "javascript:alert(1)",
            "data:text/html,<b>x</b>",
            "https://ok.example/?q=\"><script>alert(1)</script>",
        ];
        for s in corpus {
            let body = body_html(&CapturePayload { text: Some(s.into()), ..Default::default() });
            assert!(!body.contains("<script"), "unescaped script from {s:?}: {body}");
            assert!(!body.contains("<img"), "unescaped img from {s:?}: {body}");
            assert!(!body.contains("<svg"), "unescaped svg from {s:?}: {body}");
            assert!(!body.contains("<h1"), "unescaped h1 from {s:?}: {body}");
            assert!(!body.contains("</body"), "unescaped close tag from {s:?}: {body}");
            assert!(!body.contains("href=\"javascript"), "javascript link from {s:?}: {body}");
            assert!(!body.contains("href=\"data"), "data link from {s:?}: {body}");
        }
        let ent = body_html(&CapturePayload { text: Some("&lt;b&gt;".into()), ..Default::default() });
        assert_eq!(ent, "<div>&amp;lt;b&amp;gt;</div>");
    }

    #[test]
    fn body_links_http_urls_and_keeps_lines() {
        let p = CapturePayload {
            url: Some("https://a.example/page".into()),
            text: Some("Read this: https://b.example/x.\n\nsecond".into()),
            title: None,
        };
        assert_eq!(
            body_html(&p),
            "<div><a href=\"https://a.example/page\">https://a.example/page</a></div><div><br></div>\
             <div>Read this: <a href=\"https://b.example/x\">https://b.example/x</a>.</div><div><br></div><div>second</div>"
        );
        // The url already inside the text is not repeated.
        let inside = CapturePayload { url: Some("https://a.example/".into()), text: Some("x https://a.example/".into()), title: None };
        assert_eq!(body_html(&inside), "<div>x <a href=\"https://a.example/\">https://a.example/</a></div>");
        // A non-http url is kept, inert.
        let js = CapturePayload { url: Some("javascript:alert(1)".into()), ..Default::default() };
        assert_eq!(body_html(&js), "<div>javascript:alert(1)</div>");
    }

    #[test]
    fn split_links_edges() {
        assert_eq!(split_links("https://"), vec![Seg::Text("https://")]);
        assert_eq!(split_links("(https://a.example/x)"), vec![Seg::Text("("), Seg::Link("https://a.example/x"), Seg::Text(")")]);
        assert_eq!(split_links("HTTPS://A.example"), vec![Seg::Link("HTTPS://A.example")]);
    }

    #[test]
    fn queue_caps_dedupes_and_removes() {
        let mut q = Queue::default();
        let p = |t: &str| CapturePayload { text: Some(t.into()), ..Default::default() };
        let id = q.push(p("a"), 0, "d").result.unwrap();
        assert_eq!(q.push(p("a"), DEDUPE_WINDOW_MS - 1, "d").result, Err(Dropped::Duplicate));
        assert!(q.push(p("a"), 2 * DEDUPE_WINDOW_MS, "d").result.is_ok(), "after the window it is a new share");
        for i in 0..MAX_PENDING {
            q.push(p(&format!("x{i}")), 2 * DEDUPE_WINDOW_MS, "d");
        }
        assert_eq!(q.pending().len(), MAX_PENDING);
        assert_eq!(q.push(p("overflow"), 2 * DEDUPE_WINDOW_MS, "d").result, Err(Dropped::Full));
        assert_eq!(q.get(&id).unwrap().payload, p("a"));
        assert!(q.remove(&id));
        assert!(!q.remove(&id));
        assert!(q.get(&id).is_none());
    }

    #[test]
    fn a_discarded_payload_stays_deduped_for_the_window() {
        let mut q = Queue::default();
        let p = CapturePayload { text: Some("loop".into()), ..Default::default() };
        let id = q.push(p.clone(), 0, "d").result.unwrap();
        q.remove(&id);
        let again = q.push(p, 1_000, "d");
        assert_eq!(again.result, Err(Dropped::Duplicate), "a page looping one link cannot re-open the sheet");
        assert!(!again.raise);
    }

    #[test]
    fn raising_the_window_is_rate_limited_and_never_for_a_drop() {
        let mut q = Queue::default();
        let p = |t: &str| CapturePayload { text: Some(t.into()), ..Default::default() };
        assert!(q.push(p("a"), 0, "d").raise);
        assert!(!q.push(p("b"), RAISE_INTERVAL_MS - 1, "d").raise, "queued, but no second focus-steal");
        assert!(q.push(p("c"), RAISE_INTERVAL_MS, "d").raise);
        assert!(!q.push(p("c"), RAISE_INTERVAL_MS * 2, "d").raise, "a duplicate never raises");
    }

    #[test]
    fn a_full_queue_says_so_once_per_window() {
        let mut q = Queue::default();
        let p = |t: &str| CapturePayload { text: Some(t.into()), ..Default::default() };
        for i in 0..MAX_PENDING {
            q.push(p(&format!("x{i}")), 0, "d");
        }
        let first = q.push(p("y1"), 0, "d");
        assert_eq!(first.result, Err(Dropped::Full));
        assert!(first.notify_full && !first.raise);
        assert!(!q.push(p("y2"), 1, "d").notify_full);
        assert!(q.push(p("y3"), DEDUPE_WINDOW_MS, "d").notify_full);
    }
}
