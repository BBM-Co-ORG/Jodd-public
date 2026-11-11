//! Page → readable text (spec "ingest/web.rs"). No GitHub special case: a
//! repository page renders its README inside `<article>`.

use markup5ever_rcdom::{Handle, NodeData};
use reqwest::Url;
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

pub(super) fn local_name(node: &Handle) -> Option<String> {
    match &node.data {
        NodeData::Element { name, .. } => Some(name.local.to_string()),
        _ => None,
    }
}

pub(super) fn attr(node: &Handle, key: &str) -> Option<String> {
    match &node.data {
        NodeData::Element { attrs, .. } => attrs.borrow().iter().find(|a| &*a.name.local == key).map(|a| a.value.to_string()),
        _ => None,
    }
}

pub(super) fn find_first(node: &Handle, pred: &dyn Fn(&Handle) -> bool) -> Option<Handle> {
    if pred(node) {
        return Some(node.clone());
    }
    node.children.borrow().iter().find_map(|c| find_first(c, pred))
}

/// The `content` of the first `<meta property="…">` named `property` (`og:title`,
/// …), entities decoded by the HTML parser and trimmed; `None` when the tag is
/// missing or its content is blank.
pub(super) fn og_content(doc: &Handle, property: &str) -> Option<String> {
    find_first(doc, &|n| local_name(n).as_deref() == Some("meta") && attr(n, "property").as_deref() == Some(property))
        .and_then(|n| attr(&n, "content"))
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
}

pub(super) fn collect_text(node: &Handle, out: &mut String) {
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
    parse_page(html, None).0
}

/// [`extract_readable`] plus the links inside the readable content (spec
/// 2026-10-28 §5.1), resolved against `page_url` (or the page's `<base>`).
pub fn extract_readable_with_links(html: &str, page_url: &str) -> (Readable, Vec<PageLink>) {
    let base = Url::parse(page_url).ok();
    parse_page(html, Some(base.as_ref()))
}

/// `links`: `None` = do not collect; `Some(base)` = collect, resolving against
/// `base` (a page with no parseable URL can still use an absolute `<base>`).
fn parse_page(html: &str, links: Option<Option<&Url>>) -> (Readable, Vec<PageLink>) {
    use html5ever::tendril::TendrilSink;
    let dom = html5ever::parse_document(markup5ever_rcdom::RcDom::default(), Default::default()).one(html);
    let doc = dom.document.clone();
    let is = |tag: &'static str| move |n: &Handle| local_name(n).as_deref() == Some(tag);

    let og = find_first(&doc, &|n| local_name(n).as_deref() == Some("meta") && attr(n, "property").as_deref() == Some("og:title"))
        .and_then(|n| attr(&n, "content"))
        .map(|t| collapse(&t))
        .filter(|t| !t.is_empty());
    let structured = json_ld_article(&doc);
    let title = og
        .or_else(|| {
            let t = find_first(&doc, &is("title"))?;
            let mut s = String::new();
            collect_text(&t, &mut s);
            Some(collapse(&s)).filter(|s| !s.is_empty())
        })
        .or_else(|| structured.as_ref().and_then(|a| a.headline.clone()));

    let root = find_first(&doc, &is("article"))
        .or_else(|| find_first(&doc, &is("main")))
        .or_else(|| find_first(&doc, &is("body")))
        .unwrap_or_else(|| doc.clone());
    let mut raw = String::new();
    collect_text(&root, &mut raw);
    let mut text = tidy_lines(&raw);
    // §7.1: a page that renders its article in JavaScript often still ships it
    // as JSON-LD. Used only when it holds MORE than the DOM gave, so a page
    // whose markup already reads well is unchanged.
    if let Some(body) = structured.and_then(|a| a.body) {
        if body.chars().count() > text.chars().count() {
            text = body;
        }
    }
    let found = match links {
        None => Vec::new(),
        Some(page_base) => {
            let base = find_first(&doc, &is("base"))
                .and_then(|b| attr(&b, "href"))
                .and_then(|h| match page_base {
                    Some(p) => p.join(&h).ok(),
                    None => Url::parse(&h).ok(),
                })
                .or_else(|| page_base.cloned());
            match base {
                Some(base) => collect_links(&root, &base),
                None => Vec::new(),
            }
        }
    };
    (Readable { title, text }, found)
}

fn tidy_lines(raw: &str) -> String {
    raw.lines().map(collapse).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n")
}

// ── JSON-LD (spec 2026-10-28 §7.1) ─────────────────────────────────────────
/// schema.org types whose `articleBody` is the page's main text.
const ARTICLE_TYPES: [&str; 8] =
    ["Article", "NewsArticle", "BlogPosting", "TechArticle", "Report", "ScholarlyArticle", "AnalysisNewsArticle", "SocialMediaPosting"];
/// A script larger than this is not parsed (a page's whole state blob, not metadata).
const MAX_JSON_LD_BYTES: usize = 2 * 1024 * 1024;

struct StructuredArticle {
    headline: Option<String>,
    body: Option<String>,
}

pub(super) fn find_all(node: &Handle, pred: &dyn Fn(&Handle) -> bool, out: &mut Vec<Handle>) {
    if pred(node) {
        out.push(node.clone());
    }
    for c in node.children.borrow().iter() {
        find_all(c, pred, out);
    }
}

fn json_ld_article(doc: &Handle) -> Option<StructuredArticle> {
    let mut scripts = Vec::new();
    find_all(
        doc,
        &|n| local_name(n).as_deref() == Some("script") && attr(n, "type").is_some_and(|t| t.trim().eq_ignore_ascii_case("application/ld+json")),
        &mut scripts,
    );
    scripts.iter().find_map(|s| {
        let mut raw = String::new();
        for c in s.children.borrow().iter() {
            if let NodeData::Text { contents } = &c.data {
                raw.push_str(&contents.borrow());
            }
        }
        if raw.len() > MAX_JSON_LD_BYTES {
            return None;
        }
        let value: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
        find_article(&value, 0)
    })
}

fn is_article_type(v: &serde_json::Value) -> bool {
    let named = |s: &str| ARTICLE_TYPES.contains(&s.trim_start_matches("schema:").trim_start_matches("http://schema.org/").trim_start_matches("https://schema.org/"));
    match v {
        serde_json::Value::String(s) => named(s),
        serde_json::Value::Array(a) => a.iter().any(|t| t.as_str().is_some_and(named)),
        _ => false,
    }
}

/// Depth-first, `@graph` and nested objects included; the first article with
/// a body wins. Depth-limited: JSON-LD is page-served.
fn find_article(v: &serde_json::Value, depth: usize) -> Option<StructuredArticle> {
    if depth > 8 {
        return None;
    }
    match v {
        serde_json::Value::Object(map) => {
            if map.get("@type").is_some_and(is_article_type) {
                let text = |k: &str| map.get(k).and_then(|x| x.as_str()).map(tidy_lines).filter(|s| !s.is_empty());
                let body = text("articleBody").or_else(|| text("text"));
                if body.is_some() {
                    return Some(StructuredArticle { headline: text("headline").map(|h| collapse(&h)), body });
                }
            }
            map.values().find_map(|x| find_article(x, depth + 1))
        }
        serde_json::Value::Array(a) => a.iter().find_map(|x| find_article(x, depth + 1)),
        _ => None,
    }
}

// ── Links inside the content (spec 2026-10-28 §5.1) ────────────────────────
/// A link found inside a page's readable content. Ranking is `links.rs`'s job.
#[derive(Debug, Clone, PartialEq)]
pub struct PageLink {
    /// Absolute, fragment kept (the junk rules decide what a fragment means).
    pub url: String,
    pub anchor: String,
    /// The sentence around the link, ≤ `MAX_CONTEXT_CHARS`.
    pub context: String,
    /// Link text / all text of the nearest container, and how many links it holds.
    pub link_density: f32,
    pub links_in_container: usize,
    /// 0.0 = first link of the content, 1.0 = last.
    pub position: f32,
}

pub const MAX_CONTEXT_CHARS: usize = 300;
/// Links kept per page before ranking — a pathological page is not walked forever.
const MAX_LINKS_PER_PAGE: usize = 400;

/// Where a link's sentence is looked for: the nearest of these around it.
const CONTEXT_BLOCKS: [&str; 16] =
    ["p", "li", "dd", "dt", "td", "th", "blockquote", "h1", "h2", "h3", "h4", "h5", "h6", "figcaption", "pre", "div"];
/// What a link's density is measured over — a list, not one of its items, so
/// a list made only of links reads as one.
const DENSITY_CONTAINERS: [&str; 14] =
    ["p", "ul", "ol", "dl", "table", "blockquote", "div", "section", "figcaption", "pre", "h2", "h3", "h4", "main"];
/// Words inside a `class`/`id` token (split on `-` and `_`) that mark page
/// furniture, not content. Whole words only: `read-more` must not match `ad`.
const BOILERPLATE_WORDS: [&str; 39] = [
    "related", "recommend", "recommended", "recommendations", "share", "sharing", "sharedaddy", "social", "sidebar",
    "newsletter", "subscribe", "subscription", "promo", "promotion", "sponsor", "sponsored", "advert", "advertisement",
    "ads", "breadcrumb", "breadcrumbs", "pagination", "pager", "cookie", "consent", "menu", "navbar", "toc", "comments",
    "comment", "disqus", "outbrain", "taboola", "widget", "byline", "bio", "popular", "trending", "footer",
];
/// Whole `class`/`id` tokens that mark furniture but are too common as words.
/// The last three are MediaWiki's: maintenance banners (`ambox`, marked
/// `metadata`) and section edit links — the same classes in every language.
const BOILERPLATE_TOKENS: [&str; 15] = [
    "tags", "tag-list", "taglist", "tag-cloud", "tagcloud", "post-tags", "entry-tags", "categories", "post-categories", "nav", "navigation",
    "site-header", "ambox", "metadata", "mw-editsection",
];
const BOILERPLATE_ROLES: [&str; 5] = ["navigation", "complementary", "banner", "contentinfo", "search"];
/// A box whose first heading says it is "related" content. English and the
/// Thai headings sites use for the same boxes.
const RELATED_HEADINGS: [&str; 12] = [
    "related", "you may also like", "recommended", "more from", "more stories", "popular", "trending", "read next",
    "บทความที่เกี่ยวข้อง", "ข่าวที่เกี่ยวข้อง", "เรื่องที่เกี่ยวข้อง", "ยอดนิยม",
];

fn is_boilerplate(node: &Handle) -> bool {
    let tokens: Vec<String> = ["class", "id"]
        .iter()
        .filter_map(|k| attr(node, k))
        .flat_map(|v| v.split_whitespace().map(str::to_ascii_lowercase).collect::<Vec<_>>())
        .collect();
    let furniture = tokens.iter().any(|t| {
        BOILERPLATE_TOKENS.contains(&t.as_str()) || t.split(['-', '_']).any(|w| BOILERPLATE_WORDS.contains(&w))
    });
    if furniture {
        return true;
    }
    if attr(node, "role").is_some_and(|r| BOILERPLATE_ROLES.contains(&r.trim().to_ascii_lowercase().as_str())) {
        return true;
    }
    // `<section><h3>Related</h3><ul>…</ul></section>`
    if matches!(local_name(node).as_deref(), Some("section" | "div" | "aside")) {
        let first = node.children.borrow().iter().find(|c| matches!(c.data, NodeData::Element { .. })).cloned();
        if let Some(h) = first.filter(|h| matches!(local_name(h).as_deref(), Some("h2" | "h3" | "h4" | "h5" | "h6" | "strong"))) {
            let mut s = String::new();
            collect_text(&h, &mut s);
            let s = collapse(&s).to_lowercase();
            if s.chars().count() <= 60 && RELATED_HEADINGS.iter().any(|p| s.contains(p)) {
                return true;
            }
        }
    }
    false
}

/// `(text chars, link-text chars, links)` of a container, cached per node.
type DensityCache = std::collections::HashMap<*const markup5ever_rcdom::Node, (usize, usize, usize)>;

fn density_of(node: &Handle, cache: &mut DensityCache) -> (usize, usize, usize) {
    let key = std::rc::Rc::as_ptr(node);
    if let Some(v) = cache.get(&key) {
        return *v;
    }
    fn walk(n: &Handle, in_link: bool, acc: &mut (usize, usize, usize)) {
        match &n.data {
            NodeData::Text { contents } => {
                let c = contents.borrow().chars().filter(|c| !c.is_whitespace()).count();
                acc.0 += c;
                if in_link {
                    acc.1 += c;
                }
            }
            NodeData::Element { name, .. } => {
                let tag = &*name.local;
                if DROPPED.contains(&tag) {
                    return;
                }
                let link = tag == "a" && attr(n, "href").is_some();
                if link {
                    acc.2 += 1;
                }
                for c in n.children.borrow().iter() {
                    walk(c, in_link || link, acc);
                }
            }
            _ => {}
        }
    }
    let mut acc = (0, 0, 0);
    walk(node, false, &mut acc);
    cache.insert(key, acc);
    acc
}

/// The sentence around `anchor` inside `block_text`, at most `MAX_CONTEXT_CHARS`.
fn sentence_around(block_text: &str, anchor: &str) -> String {
    let chars: Vec<char> = block_text.chars().collect();
    if chars.len() <= MAX_CONTEXT_CHARS {
        return block_text.to_string();
    }
    let at_byte = if anchor.is_empty() { None } else { block_text.find(anchor) };
    let at = at_byte.map(|b| block_text[..b].chars().count()).unwrap_or(0);
    let anchor_len = anchor.chars().count();
    let half = (MAX_CONTEXT_CHARS.saturating_sub(anchor_len)) / 2;
    let mut start = at.saturating_sub(half);
    let mut end = (at + anchor_len + half).min(chars.len());
    // Pull the edges in to sentence boundaries when one is inside the window.
    let stop = |c: char| matches!(c, '.' | '!' | '?' | '。' | '\n');
    if let Some(i) = (start..at).rev().find(|&i| stop(chars[i])) {
        start = i + 1;
    }
    if let Some(i) = (at + anchor_len..end).find(|&i| stop(chars[i])) {
        end = i + 1;
    }
    chars[start..end].iter().collect::<String>().trim().to_string()
}

fn collect_links(root: &Handle, base: &Url) -> Vec<PageLink> {
    struct Walk<'a> {
        base: &'a Url,
        context_stack: Vec<Handle>,
        density_stack: Vec<Handle>,
        cache: DensityCache,
        text_cache: std::collections::HashMap<*const markup5ever_rcdom::Node, String>,
        out: Vec<PageLink>,
    }
    fn visit(w: &mut Walk, node: &Handle, is_root: bool) {
        if w.out.len() >= MAX_LINKS_PER_PAGE {
            return;
        }
        let NodeData::Element { name, .. } = &node.data else {
            if matches!(node.data, NodeData::Document) {
                for c in node.children.borrow().iter() {
                    visit(w, c, false);
                }
            }
            return;
        };
        let tag = name.local.to_string();
        if DROPPED.contains(&tag.as_str()) || (!is_root && is_boilerplate(node)) {
            return;
        }
        if tag == "a" {
            // `#section` is a jump inside this page, never another source.
            if let Some(href) = attr(node, "href").filter(|h| !h.trim().is_empty() && !h.trim().starts_with('#')) {
                record(w, node, href.trim());
            }
            return;
        }
        let ctx = CONTEXT_BLOCKS.contains(&tag.as_str());
        let dens = DENSITY_CONTAINERS.contains(&tag.as_str());
        if ctx {
            w.context_stack.push(node.clone());
        }
        if dens {
            w.density_stack.push(node.clone());
        }
        for c in node.children.borrow().iter() {
            visit(w, c, false);
        }
        if ctx {
            w.context_stack.pop();
        }
        if dens {
            w.density_stack.pop();
        }
    }
    fn record(w: &mut Walk, a: &Handle, href: &str) {
        let Ok(url) = w.base.join(href) else { return };
        let mut s = String::new();
        collect_text(a, &mut s);
        let mut anchor = collapse(&s);
        if anchor.is_empty() {
            anchor = ["aria-label", "title"]
                .iter()
                .find_map(|k| attr(a, k).map(|v| collapse(&v)).filter(|v| !v.is_empty()))
                .or_else(|| find_first(a, &|n| local_name(n).as_deref() == Some("img")).and_then(|i| attr(&i, "alt")).map(|v| collapse(&v)))
                .unwrap_or_default();
        }
        let context = match w.context_stack.last().cloned() {
            Some(block) => {
                let key = std::rc::Rc::as_ptr(&block);
                let text = w.text_cache.entry(key).or_insert_with(|| {
                    let mut t = String::new();
                    collect_text(&block, &mut t);
                    collapse(&t)
                });
                sentence_around(text, &anchor)
            }
            None => anchor.clone(),
        };
        let (link_density, links_in_container) = match w.density_stack.last().cloned() {
            Some(c) => {
                let (text, link, n) = density_of(&c, &mut w.cache);
                (if text == 0 { 1.0 } else { link as f32 / text as f32 }, n)
            }
            None => (0.0, 1),
        };
        w.out.push(PageLink { url: url.to_string(), anchor, context, link_density, links_in_container, position: 0.0 });
    }
    let mut w = Walk {
        base,
        context_stack: Vec::new(),
        density_stack: Vec::new(),
        cache: DensityCache::new(),
        text_cache: std::collections::HashMap::new(),
        out: Vec::new(),
    };
    visit(&mut w, root, true);
    let n = w.out.len();
    for (i, l) in w.out.iter_mut().enumerate() {
        l.position = if n <= 1 { 0.0 } else { i as f32 / (n - 1) as f32 };
    }
    w.out
}

/// What a fetched document is, before it is read.
struct Document {
    final_url: String,
    mime: String,
    decoded: String,
}

async fn fetch_document(url: &str, policy: FetchPolicy, cancel: &CancellationToken) -> Result<Document, String> {
    let resp = send_guarded(GuardedRequest::get(url), policy, cancel).await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    let final_url = resp.url().to_string();
    let content_type = resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_string);
    let mime = content_type.as_deref().unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    if mime != "text/html" && mime != "text/plain" {
        return Err(format!("unsupported content type ({})", if mime.is_empty() { "none" } else { mime.as_str() }));
    }
    let bytes = read_capped(resp, MAX_BODY_BYTES, cancel).await?;
    let decoded = decode(&bytes, content_type.as_deref());
    Ok(Document { final_url, mime, decoded })
}

const NO_READABLE_TEXT: &str = "no readable text (the page may need JavaScript)";

pub async fn fetch_web(url: &str, policy: FetchPolicy, cancel: &CancellationToken) -> FetchedSource {
    match fetch_document(url, policy, cancel).await {
        Ok(doc) => read_document(url, doc, policy, cancel).await,
        Err(e) => FetchedSource::failed(url, SourceKind::Web, e),
    }
}

/// A page the private reader rendered (spec 2026-10-28 §7.3), read exactly
/// like a fetched one — JSON-LD, readable text, published comments — and
/// marked so the note says it was rendered.
pub async fn read_rendered(page: &crate::ingest::render::RenderedPage, policy: FetchPolicy, cancel: &CancellationToken) -> FetchedSource {
    let doc = Document { final_url: page.final_url.clone(), mime: "text/html".into(), decoded: page.html.clone() };
    let mut s = read_document(&page.url, doc, policy, cancel).await;
    match &s.status {
        FetchStatus::Failed(r) if r == NO_READABLE_TEXT => {
            s.status = FetchStatus::Failed("no readable text, even rendered in a private reader (the page may need a sign-in)".into());
        }
        FetchStatus::Failed(_) => {}
        _ => s.text = format!("{}\n\n{}", crate::ingest::render::RENDERED_MARK, s.text),
    }
    s
}

/// Whether a source failed only because its page builds itself in JavaScript —
/// the one failure the private reader is offered for.
pub fn needs_javascript(status: &FetchStatus) -> bool {
    matches!(status, FetchStatus::Failed(r) if r == NO_READABLE_TEXT)
}

async fn read_document(url: &str, doc: Document, policy: FetchPolicy, cancel: &CancellationToken) -> FetchedSource {
    let failed = |reason: String| FetchedSource::failed(url, SourceKind::Web, reason);
    let (title, mut text) = if doc.mime == "text/html" {
        let r = extract_readable(&doc.decoded);
        (r.title, r.text)
    } else {
        (None, doc.decoded.trim().to_string())
    };
    // Spec 2026-10-28 §6/§7.1: comments the page publishes in a standard shape.
    if doc.mime == "text/html" {
        if let Some(pc) = crate::ingest::discuss::generic::read(&doc.decoded, &doc.final_url, &text, policy, cancel).await {
            // Discourse: the first post stands in for a JavaScript-only page.
            if let Some(main) = pc.main_text.filter(|m| m.chars().count() > text.chars().count()) {
                text = main;
            }
            text = crate::ingest::discuss::append_to(&text, &pc.discussion);
        }
    }
    if text.trim().is_empty() {
        return failed(NO_READABLE_TEXT.into());
    }
    FetchedSource { url: url.to_string(), kind: SourceKind::Web, title, text, status: FetchStatus::Ok }
}

/// A page read for its links (spec 2026-10-28 §5): the readable text too, so
/// the caller can tell the user what the page was.
pub struct LinkedPage {
    pub title: Option<String>,
    pub text: String,
    pub links: Vec<PageLink>,
    /// After redirects — what relative links were resolved against.
    pub final_url: String,
    /// Comments the page publishes (spec §7.1) — their author's links are
    /// candidates too.
    pub discussion: Option<crate::ingest::discuss::Discussion>,
}

pub async fn fetch_page_links(url: &str, policy: FetchPolicy, cancel: &CancellationToken) -> Result<LinkedPage, String> {
    let doc = fetch_document(url, policy, cancel).await?;
    if doc.mime == "text/html" {
        let (r, links) = extract_readable_with_links(&doc.decoded, &doc.final_url);
        let discussion = crate::ingest::discuss::generic::read(&doc.decoded, &doc.final_url, &r.text, policy, cancel).await.map(|pc| pc.discussion);
        return Ok(LinkedPage { title: r.title, text: r.text, links, final_url: doc.final_url, discussion });
    }
    // Plain text: its bare URLs are its links.
    let text = doc.decoded.trim().to_string();
    let links = text_links(&text);
    Ok(LinkedPage { title: None, text, links, final_url: doc.final_url, discussion: None })
}

/// Bare URLs in plain text (a caption, a description, a `text/plain` page),
/// each with its line as context.
pub fn text_links(text: &str) -> Vec<PageLink> {
    let mut out: Vec<PageLink> = Vec::new();
    for line in text.lines() {
        for url in crate::ingest::urls::detect(line) {
            let context: String = collapse(line).chars().take(MAX_CONTEXT_CHARS).collect();
            out.push(PageLink { url: url.clone(), anchor: url, context, link_density: 0.0, links_in_container: 1, position: 0.0 });
        }
    }
    let n = out.len();
    for (i, l) in out.iter_mut().enumerate() {
        l.position = if n <= 1 { 0.0 } else { i as f32 / (n - 1) as f32 };
    }
    out
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

    const LINKED: &str = include_str!("fixtures/article_with_links.html");

    fn urls_of(links: &[PageLink]) -> Vec<&str> {
        links.iter().map(|l| l.url.as_str()).collect()
    }

    #[test]
    fn links_come_from_the_content_only_and_resolve_against_base() {
        let (r, links) = extract_readable_with_links(LINKED, "https://blog.example.com/posts/rust");
        assert!(r.text.contains("every value has a single owner"), "{}", r.text);
        let got = urls_of(&links);
        for want in [
            "https://doc.rust-lang.org/book/ch04-01-what-is-ownership.html",
            "https://arxiv.org/abs/1902.00001",
            "https://github.com/example/ownership-demo",
            "https://bit.ly/3abcDEF",
            // `<base href>` wins over the page URL for relative links.
            "https://blog.example.com/posts/next-post",
            "https://doi.org/10.1145/3158154",
        ] {
            assert!(got.contains(&want), "missing {want}: {got:?}");
        }
        // Page furniture never reaches the ranking: nav, header, breadcrumbs,
        // share box, tag footer, author bio, a "related" box with a Thai
        // heading, comments, sidebar, site footer.
        for gone in [
            "/login", "/category/rust/", "sharer.php", "intent/tweet", "line.me", "/tag/rust/", "/author/jane/",
            "twitter.com/jane", "/posts/lifetimes", "casino", "/newsletter", "/privacy", "/about",
        ] {
            assert!(!got.iter().any(|u| u.contains(gone)), "`{gone}` leaked: {got:?}");
        }
    }

    #[test]
    fn a_link_carries_its_sentence_and_its_anchor() {
        let (_, links) = extract_readable_with_links(LINKED, "https://blog.example.com/posts/rust");
        let book = links.iter().find(|l| l.url.contains("doc.rust-lang.org")).unwrap();
        assert_eq!(book.anchor, "the official Rust book chapter on ownership");
        assert!(book.context.starts_with("Ownership is Rust's most unusual feature."), "{}", book.context);
        let thai = links.iter().find(|l| l.url.contains("github.com")).unwrap();
        assert!(thai.context.contains("ฉบับเต็ม"), "{}", thai.context);
        assert!(links.first().unwrap().position == 0.0 && links.last().unwrap().position == 1.0);
    }

    #[test]
    fn a_list_made_of_links_reads_as_dense() {
        let (_, links) = extract_readable_with_links(LINKED, "https://blog.example.com/posts/rust");
        let reference = links.iter().find(|l| l.url.contains("doi.org")).unwrap();
        assert!(reference.link_density > 0.9 && reference.links_in_container == 2, "{reference:?}");
        let prose = links.iter().find(|l| l.url.contains("arxiv")).unwrap();
        assert!(prose.link_density < 0.5, "{prose:?}");
    }

    #[test]
    fn an_image_only_link_takes_its_alt_text_and_class_words_match_whole() {
        let page = r#"<body><article><p class="read-more"><a href="https://a.example/x"><img alt="Chart of results"></a></p></article></body>"#;
        let (_, links) = extract_readable_with_links(page, "https://a.example/");
        // `read-more` contains "ad" as letters, not as a word: kept.
        assert_eq!(links.len(), 1, "{links:?}");
        assert_eq!(links[0].anchor, "Chart of results");
    }

    /// Live pass 2026-10-29 (§19): a MediaWiki maintenance banner ("this
    /// section needs more sources — please help improve it") is furniture in
    /// every language edition; its policy and edit links were pre-ticked.
    #[test]
    fn a_wiki_maintenance_banner_and_edit_links_are_furniture() {
        let page = r#"<body><main>
            <table class="plainlinks metadata ambox ambox-content" role="presentation"><tr><td>
              ส่วนนี้ต้องการ<a href="/wiki/วิกิพีเดีย:การอ้างอิงแหล่งที่มา">เอกสารอ้างอิงหรือแหล่งข้อมูล</a>เพิ่มเติมเพื่อการพิสูจน์ยืนยัน กรุณาช่วย<a href="/w/index.php?title=x&amp;action=edit">พัฒนาส่วนนี้</a>
            </td></tr></table>
            <h2>History <span class="mw-editsection"><a href="/w/index.php?title=x&amp;action=edit&amp;section=1">edit</a></span></h2>
            <p>Thai is a tonal language; see <a href="https://doi.org/10.1/abc">the 2017 survey of Tai tones</a> for the details of the six tones.</p>
        </main></body>"#;
        let (_, links) = extract_readable_with_links(page, "https://th.wikipedia.org/wiki/x");
        let urls: Vec<&str> = links.iter().map(|l| l.url.as_str()).collect();
        assert_eq!(urls, vec!["https://doi.org/10.1/abc"], "{links:?}");
    }

    #[test]
    fn plain_text_links_carry_their_line() {
        let links = text_links("Intro line\nCode: https://github.com/a/b and slides https://bit.ly/x\n");
        assert_eq!(urls_of(&links), vec!["https://github.com/a/b", "https://bit.ly/x"]);
        assert!(links[0].context.starts_with("Code:"));
    }

    #[test]
    fn json_ld_article_body_is_used_when_the_markup_is_empty() {
        let page = r#"<html><head><script type="application/ld+json">
            {"@context":"https://schema.org","@graph":[{"@type":"WebPage"},
             {"@type":["NewsArticle"],"headline":"Rendered later","articleBody":"First paragraph of the story.\n\nSecond paragraph."}]}
            </script></head><body><div id="app"></div></body></html>"#;
        let r = extract_readable(page);
        assert_eq!(r.text, "First paragraph of the story.\nSecond paragraph.");
        assert_eq!(r.title.as_deref(), Some("Rendered later"));
    }

    #[test]
    fn json_ld_never_replaces_a_fuller_page_and_bad_json_is_ignored() {
        let page = r#"<head><script type="application/ld+json">{"@type":"Article","articleBody":"short"}</script>
            <script type="application/ld+json">{not json</script></head>
            <body><article><p>The page itself says a great deal more than the summary does.</p></article></body>"#;
        assert_eq!(extract_readable(page).text, "The page itself says a great deal more than the summary does.");
    }

    fn policy() -> FetchPolicy {
        FetchPolicy { allow_loopback: true }
    }

    #[tokio::test]
    async fn page_links_resolve_against_the_url_after_redirects() {
        let mut server = mockito::Server::new_async().await;
        let _r = server.mock("GET", "/old").with_status(301).with_header("location", "/new/page").create_async().await;
        let _p = server
            .mock("GET", "/new/page")
            .with_status(200)
            .with_header("content-type", "text/html")
            .with_body("<body><article><p>See <a href=\"next\">the next part</a> now.</p></article></body>")
            .create_async()
            .await;
        let page = fetch_page_links(&format!("{}/old", server.url()), policy(), &CancellationToken::new()).await.unwrap();
        assert_eq!(page.final_url, format!("{}/new/page", server.url()));
        assert_eq!(page.links[0].url, format!("{}/new/next", server.url()));
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

    /// Spec 2026-10-28 §7.3: a rendered page is read like a fetched one, and
    /// marked; a page that is still empty says rendering did not help.
    #[tokio::test]
    async fn a_rendered_page_is_read_like_a_fetched_one_and_marked() {
        use crate::ingest::render::{RenderedPage, RENDERED_MARK};
        let cancel = CancellationToken::new();
        let shell = "<html><head><title>App</title></head><body><div id=root></div><script>build()</script></body></html>";
        let fetched = parse_page(shell, None).0;
        assert!(fetched.text.is_empty(), "the shell must have no text, or this pins nothing");
        assert!(needs_javascript(&FetchStatus::Failed(NO_READABLE_TEXT.into())));
        assert!(!needs_javascript(&FetchStatus::Failed("HTTP 404".into())));

        let built = "<html><head><title>App</title></head><body><div id=root><article><p>Built by JavaScript.</p></article></div></body></html>";
        let page = RenderedPage::new("https://app.example/a", "https://app.example/a", built.into());
        let s = read_rendered(&page, FetchPolicy::default(), &cancel).await;
        assert_eq!(s.status, FetchStatus::Ok);
        assert_eq!(s.url, "https://app.example/a");
        assert!(s.text.starts_with(RENDERED_MARK) && s.text.contains("Built by JavaScript."), "{}", s.text);

        let still_empty = RenderedPage::new("https://app.example/b", "https://app.example/b", shell.into());
        let s = read_rendered(&still_empty, FetchPolicy::default(), &cancel).await;
        assert!(matches!(&s.status, FetchStatus::Failed(r) if r.contains("even rendered")), "{:?}", s.status);
        assert!(!needs_javascript(&s.status), "rendering again would not help");
    }
}
