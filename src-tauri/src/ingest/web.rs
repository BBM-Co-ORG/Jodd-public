//! Page → readable text (spec "ingest/web.rs"). No GitHub special case: a
//! repository page renders its README inside `<article>`.

use markup5ever_rcdom::{Handle, NodeData};
use tokio_util::sync::CancellationToken;

use crate::ingest::net::{read_capped, send_guarded, GuardedRequest};
use crate::ingest::{FetchPolicy, FetchStatus, FetchedSource, SourceKind};

pub const MAX_BODY_BYTES: usize = 5 * 1024 * 1024;

const DROPPED: [&str; 9] = ["script", "style", "noscript", "nav", "header", "footer", "aside", "form", "svg"];
const BLOCKS: [&str; 28] = [
    "address", "article", "blockquote", "br", "dd", "div", "dl", "dt", "figcaption", "figure", "h1", "h2", "h3",
    "h4", "h5", "h6", "hr", "li", "main", "ol", "p", "pre", "section", "table", "td", "th", "tr", "ul",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readable {
    pub title: Option<String>,
    pub text: String,
}

fn charset_from_content_type(ct: &str) -> Option<String> {
    ct.split(';').skip(1).find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim().eq_ignore_ascii_case("charset").then(|| v.trim().trim_matches('"').to_string())
    })
}

/// Covers both `<meta charset="tis-620">` and
/// `<meta http-equiv="Content-Type" content="text/html; charset=windows-874">`.
fn charset_from_meta(head: &str) -> Option<String> {
    let lower = head.to_ascii_lowercase();
    let at = lower.find("charset=")?;
    let rest = lower[at + "charset=".len()..].trim_start_matches(['"', '\'']);
    let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_')).unwrap_or(rest.len());
    (end > 0).then(|| rest[..end].to_string())
}

/// Header charset, else `<meta>` in the first 4 KB, else UTF-8.
/// `Encoding::decode` also honours a BOM and replaces malformed sequences.
pub fn decode(bytes: &[u8], content_type: Option<&str>) -> String {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]);
    let label = content_type.and_then(charset_from_content_type).or_else(|| charset_from_meta(&head));
    let encoding = label.and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes())).unwrap_or(encoding_rs::UTF_8);
    encoding.decode(bytes).0.into_owned()
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn local_name(node: &Handle) -> Option<String> {
    match &node.data {
        NodeData::Element { name, .. } => Some(name.local.to_string()),
        _ => None,
    }
}

fn attr(node: &Handle, key: &str) -> Option<String> {
    match &node.data {
        NodeData::Element { attrs, .. } => attrs.borrow().iter().find(|a| &*a.name.local == key).map(|a| a.value.to_string()),
        _ => None,
    }
}

fn find_first(node: &Handle, pred: &dyn Fn(&Handle) -> bool) -> Option<Handle> {
    if pred(node) {
        return Some(node.clone());
    }
    node.children.borrow().iter().find_map(|c| find_first(c, pred))
}

fn collect_text(node: &Handle, out: &mut String) {
    match &node.data {
        NodeData::Text { contents } => out.push_str(&contents.borrow()),
        NodeData::Element { name, .. } => {
            let tag = &*name.local;
            if DROPPED.contains(&tag) {
                return;
            }
            let block = BLOCKS.contains(&tag);
            if block {
                out.push('\n');
            }
            for child in node.children.borrow().iter() {
                collect_text(child, out);
            }
            if block {
                out.push('\n');
            }
        }
        NodeData::Document => {
            for child in node.children.borrow().iter() {
                collect_text(child, out);
            }
        }
        _ => {}
    }
}

/// Every handle taken from `dom` is dropped before `dom` is — rcdom's
/// `Drop for Node` empties the tree (see `llm::markdown::with_fragment_root`).
pub fn extract_readable(html: &str) -> Readable {
    use html5ever::tendril::TendrilSink;
    let dom = html5ever::parse_document(markup5ever_rcdom::RcDom::default(), Default::default()).one(html);
    let doc = dom.document.clone();
    let is = |tag: &'static str| move |n: &Handle| local_name(n).as_deref() == Some(tag);

    let og = find_first(&doc, &|n| local_name(n).as_deref() == Some("meta") && attr(n, "property").as_deref() == Some("og:title"))
        .and_then(|n| attr(&n, "content"))
        .map(|t| collapse(&t))
        .filter(|t| !t.is_empty());
    let title = og.or_else(|| {
        let t = find_first(&doc, &is("title"))?;
        let mut s = String::new();
        collect_text(&t, &mut s);
        Some(collapse(&s)).filter(|s| !s.is_empty())
    });

    let root = find_first(&doc, &is("article"))
        .or_else(|| find_first(&doc, &is("main")))
        .or_else(|| find_first(&doc, &is("body")))
        .unwrap_or_else(|| doc.clone());
    let mut raw = String::new();
    collect_text(&root, &mut raw);
    let text = raw.lines().map(collapse).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
    Readable { title, text }
}

pub async fn fetch_web(url: &str, policy: FetchPolicy, cancel: &CancellationToken) -> FetchedSource {
    let failed = |reason: String| FetchedSource::failed(url, SourceKind::Web, reason);
    let resp = match send_guarded(GuardedRequest::get(url), policy, cancel).await {
        Ok(r) => r,
        Err(e) => return failed(e),
    };
    let status = resp.status();
    if !status.is_success() {
        return failed(format!("HTTP {}", status.as_u16()));
    }
    let content_type = resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_string);
    let mime = content_type.as_deref().unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    if mime != "text/html" && mime != "text/plain" {
        return failed(format!("unsupported content type ({})", if mime.is_empty() { "none" } else { mime.as_str() }));
    }
    let bytes = match read_capped(resp, MAX_BODY_BYTES, cancel).await {
        Ok(b) => b,
        Err(e) => return failed(e),
    };
    let decoded = decode(&bytes, content_type.as_deref());
    let (title, text) = if mime == "text/html" {
        let r = extract_readable(&decoded);
        (r.title, r.text)
    } else {
        (None, decoded.trim().to_string())
    };
    if text.trim().is_empty() {
        return failed("no readable text (the page may need JavaScript)".into());
    }
    FetchedSource { url: url.to_string(), kind: SourceKind::Web, title, text, status: FetchStatus::Ok }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::{FetchStatus, SourceKind};

    const HOSTILE: &str = include_str!("fixtures/hostile_page.html");

    #[test]
    fn scripts_styles_and_page_chrome_are_dropped() {
        let r = extract_readable(HOSTILE);
        for gone in ["stolen", "background", "Home", "Site banner", "Related links", "© Example", "svg text"] {
            assert!(!r.text.contains(gone), "`{gone}` leaked into readable text: {}", r.text);
        }
        assert!(r.text.contains("Ownership is a set of rules."), "{}", r.text);
        assert!(r.text.contains("Borrowing lets you refer to a value."), "{}", r.text);
    }

    #[test]
    fn og_title_wins_over_title() {
        assert_eq!(extract_readable(HOSTILE).title.as_deref(), Some("A perfectly normal article"));
        let plain = extract_readable("<html><head><title>Just &amp; title</title></head><body><p>x</p></body></html>");
        assert_eq!(plain.title.as_deref(), Some("Just & title"));
    }

    #[test]
    fn article_beats_main_beats_body() {
        let both = "<body><p>outside</p><main><p>in main</p><article><p>in article</p></article></main></body>";
        assert_eq!(extract_readable(both).text, "in article");
        let main_only = "<body><p>outside</p><main><p>in main</p></main></body>";
        assert_eq!(extract_readable(main_only).text, "in main");
        assert_eq!(extract_readable("<body><p>just body</p></body>").text, "just body");
    }

    #[test]
    fn block_elements_become_line_breaks() {
        assert_eq!(extract_readable("<body><h2>One</h2><p>two <b>bold</b></p><ul><li>a</li><li>b</li></ul></body>").text, "One\ntwo bold\na\nb");
    }

    #[test]
    fn a_tis620_page_declaring_its_charset_only_in_meta_decodes_to_thai() {
        let page = "<html><head><meta charset=\"tis-620\"><title>ข่าว</title></head><body><p>ยกตัวอย่าง</p></body></html>";
        let (bytes, _, had_errors) = encoding_rs::WINDOWS_874.encode(page);
        assert!(!had_errors);
        assert!(std::str::from_utf8(&bytes).is_err(), "the fixture must not be UTF-8, or this pins nothing");
        let r = extract_readable(&decode(&bytes, Some("text/html")));
        assert_eq!(r.title.as_deref(), Some("ข่าว"));
        assert_eq!(r.text, "ยกตัวอย่าง");
    }

    #[test]
    fn the_content_type_charset_wins_over_meta() {
        let (bytes, _, _) = encoding_rs::WINDOWS_874.encode("<meta charset=\"utf-8\"><p>ไทย</p>");
        assert!(decode(&bytes, Some("text/html; charset=windows-874")).contains("ไทย"));
    }

    fn policy() -> FetchPolicy {
        FetchPolicy { allow_loopback: true }
    }

    #[tokio::test]
    async fn fetches_a_page_into_readable_text() {
        let mut server = mockito::Server::new_async().await;
        let _m = server.mock("GET", "/a").with_status(200).with_header("content-type", "text/html; charset=utf-8").with_body(HOSTILE).create_async().await;
        let s = fetch_web(&format!("{}/a", server.url()), policy(), &CancellationToken::new()).await;
        assert_eq!(s.status, FetchStatus::Ok);
        assert_eq!(s.kind, SourceKind::Web);
        assert_eq!(s.title.as_deref(), Some("A perfectly normal article"));
        assert!(s.text.contains("Ownership is a set of rules."));
    }

    #[tokio::test]
    async fn not_found_pdf_oversize_and_empty_pages_fail_with_a_reason() {
        let mut server = mockito::Server::new_async().await;
        let _a = server.mock("GET", "/404").with_status(404).create_async().await;
        let _b = server.mock("GET", "/pdf").with_status(200).with_header("content-type", "application/pdf").with_body("%PDF-1.7").create_async().await;
        let _c = server.mock("GET", "/big").with_status(200).with_header("content-type", "text/html").with_body("a".repeat(MAX_BODY_BYTES + 1)).create_async().await;
        let _d = server.mock("GET", "/js").with_status(200).with_header("content-type", "text/html").with_body("<html><body><div id=app></div><script>render()</script></body></html>").create_async().await;
        for (path, needle) in [("/404", "HTTP 404"), ("/pdf", "application/pdf"), ("/big", "larger than 5 MB"), ("/js", "no readable text")] {
            let s = fetch_web(&format!("{}{path}", server.url()), policy(), &CancellationToken::new()).await;
            match &s.status {
                FetchStatus::Failed(reason) => assert!(reason.contains(needle), "{path}: {reason}"),
                other => panic!("{path}: expected Failed, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn the_default_policy_turns_a_local_page_into_a_failed_source() {
        let mut server = mockito::Server::new_async().await;
        let _m = server.mock("GET", "/").with_status(200).with_header("content-type", "text/html").with_body("<p>x</p>").create_async().await;
        let s = fetch_web(&server.url(), FetchPolicy::default(), &CancellationToken::new()).await;
        assert_eq!(s.status, FetchStatus::Failed(crate::ingest::net::REFUSED.into()));
    }
}
