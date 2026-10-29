//! Follow links (spec 2026-10-28-follow-links-and-discussions-design.md §5).
//!
//! A page the user chose is read for the links INSIDE its content; junk is
//! removed by fixed rules (§5.2), the rest ranked by a fixed score (§5.3), and
//! the best few arrive pre-ticked. Nothing here downloads a followed page —
//! discovery reads only the page the user already picked (D2), and the user
//! ticks what is fetched.
//!
//! Everything that names a site, a path word or a phrase lives in the
//! constants block below, the way the platform constants live in one block per
//! platform file: when the rules misjudge real pages, change the block.

use reqwest::{Method, Url};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use super::net::{send_guarded, GuardedRequest};
use super::social::PlatformMeta;
use super::urls::{self, UrlKind};
use super::web::{self, PageLink};
use super::discuss::{self, ThreadEndpoints};
use super::youtube::{self, YoutubeEndpoints};
use super::{facebook, instagram, tiktok, x, FetchPolicy};

/// Pre-ticked candidates per source (D3).
pub const MAX_FOLLOWED_PER_SOURCE: usize = 5;
/// Followed links per ingest, on top of `MAX_URLS_PER_INGEST` (D3: 8 + 8 = 16).
pub const MAX_FOLLOWED_TOTAL: usize = 8;
/// Candidates returned to the list; the rest are only counted.
pub const MAX_CANDIDATES_SHOWN: usize = 20;
/// A candidate below this score is shown but never pre-ticked (§5.3).
pub const PRETICK_MIN_SCORE: i32 = 25;
/// Short links resolved per discovery — each costs one request (§5.2).
pub const MAX_SHORT_LINKS_RESOLVED: usize = 10;

// ── Rules (§5.2) and scoring words (§5.3) ──────────────────────────────────
/// Share buttons: `(host, path prefix)`; an empty prefix is the whole host.
const SHARE_INTENTS: [(&str, &str); 12] = [
    ("facebook.com", "/sharer"),
    ("facebook.com", "/share.php"),
    ("facebook.com", "/dialog/share"),
    ("twitter.com", "/intent"),
    ("twitter.com", "/share"),
    ("x.com", "/intent"),
    ("linkedin.com", "/sharearticle"),
    ("linkedin.com", "/shareArticle"),
    ("line.me", "/R/msg"),
    ("social-plugins.line.me", ""),
    ("api.whatsapp.com", "/send"),
    ("pinterest.com", "/pin/create"),
];
/// Ad and click-tracking hosts (dot-anchored suffix match).
const AD_HOSTS: [&str; 10] = [
    "doubleclick.net", "googleadservices.com", "googlesyndication.com", "adservice.google.com", "outbrain.com",
    "taboola.com", "adnxs.com", "criteo.com", "click.linksynergy.com", "go.skimresources.com",
];
/// Affiliate short hosts — a shop, not a source.
const AFFILIATE_HOSTS: [&str; 9] =
    ["amzn.to", "shope.ee", "s.shopee.co.th", "s.lazada.co.th", "c.lazada.co.th", "invol.co", "lzd.co", "aliexpress.com", "a.co"];
/// Short-link hosts that are resolved, not dropped.
const SHORTENER_HOSTS: [&str; 14] = [
    "bit.ly", "t.co", "lnkd.in", "buff.ly", "ow.ly", "tinyurl.com", "goo.gl", "rebrand.ly", "is.gd", "cutt.ly",
    "shorturl.at", "s.id", "rb.gy", "tiny.cc",
];
/// Hosts where only a post is content; anything else there is a profile or a
/// home page (`urls::classify` already knows the post shapes).
const SOCIAL_HOSTS: [&str; 12] = [
    "facebook.com", "instagram.com", "tiktok.com", "x.com", "twitter.com", "youtube.com", "linkedin.com", "line.me",
    "t.me", "pinterest.com", "threads.net", "threads.com",
];
/// A path segment equal to one of these marks an account or shop page.
const ACCOUNT_SEGMENTS: [&str; 16] = [
    "login", "signin", "sign-in", "log-in", "signup", "sign-up", "register", "account", "my-account", "logout", "cart",
    "checkout", "subscribe", "donate", "membership", "wp-login.php",
];
/// … a listing, not a document.
const LISTING_SEGMENTS: [&str; 9] = ["tag", "tags", "category", "categories", "topics", "archive", "archives", "author", "search"];
/// … the site talking about itself.
const SITE_SEGMENTS: [&str; 14] = [
    "privacy", "privacy-policy", "terms", "terms-of-service", "terms-of-use", "tos", "cookies", "cookie-policy", "legal",
    "about", "about-us", "contact", "contact-us", "careers",
];
/// Files, not pages. PDF is dropped for now (roadmap #16 owns PDF).
const FILE_EXTENSIONS: [&str; 24] = [
    "png", "jpg", "jpeg", "gif", "webp", "svg", "avif", "ico", "mp3", "m4a", "wav", "ogg", "zip", "rar", "7z", "gz",
    "tar", "dmg", "exe", "msi", "apk", "ics", "vcf", "pdf",
];
/// Words in a link's sentence that say WHY it is there (EN + TH).
const WHY_WORDS: [&str; 22] = [
    "source", "study", "paper", "according to", "documentation", "docs", "full video", "full version", "code",
    "repo", "repository", "download", "research", "report", "dataset", "ที่มา", "อ้างอิง", "ดูเพิ่มเติม", "ลิงก์",
    "คลิป", "ฉบับเต็ม", "งานวิจัย",
];
/// Anchors that say nothing about their target.
const GENERIC_ANCHORS: [&str; 12] =
    ["click here", "here", "link", "this", "read more", "more", "learn more", "continue", "คลิก", "คลิกที่นี่", "ที่นี่", "อ่านต่อ"];
/// Sites whose pages are usually the primary source (§5.3 "host class").
const PRIMARY_HOSTS: [&str; 9] =
    ["arxiv.org", "doi.org", "github.com", "gitlab.com", "w3.org", "ietf.org", "readthedocs.io", "developer.mozilla.org", "pubmed.ncbi.nlm.nih.gov"];
/// Host suffixes for government and academic sites.
const PRIMARY_SUFFIXES: [&str; 5] = [".gov", ".go.th", ".ac.th", ".edu", ".ac.uk"];
/// Second-level labels that make the registrable site three labels long.
const SECOND_LEVEL: [&str; 9] = ["co", "ac", "go", "or", "in", "com", "net", "org", "gov"];
/// `docs.` hosts that hold people's documents, not documentation.
const NOT_DOCS_HOSTS: [&str; 1] = ["docs.google.com"];
/// Query keys that only say where a click came from (`utm_*` too). Medium
/// puts `?source=…` on every link, its blog's home page included.
const TRACKING_QUERY_KEYS: [&str; 9] = ["source", "ref", "ref_src", "fbclid", "gclid", "mc_cid", "mc_eid", "igshid", "si"];
/// A single path segment with this many hyphen-separated words is an article
/// slug (Medium's `/title-words-1a2b3c`), not a section of the site.
const ARTICLE_SLUG_WORDS: usize = 4;
// ───────────────────────────────────────────────────────────────────────────

/// Where in the source a link was found (§5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Region {
    /// A web page's content.
    Body,
    /// A YouTube video's description.
    Description,
    /// A social post's caption or text.
    Caption,
    /// The link a post exists to share: a Hacker News story, a Bluesky link
    /// card, a Mastodon preview card (spec 2026-10-28 §6.2).
    Shared,
    /// A comment by the post's own author — "full video in the comments" (D6).
    AuthorComment,
    /// Text the user pasted from comments Jodd cannot read (D8).
    Pasted,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LinkCandidate {
    /// Canonical, fragment removed — what the ingest fetches and stores.
    pub url: String,
    pub anchor: String,
    pub context: String,
    pub region: Region,
    /// As `analyze_ingest_sources` spells it: `web`, `youtube`, `tiktok`, …
    pub kind: &'static str,
    pub score: i32,
    /// Shown in the list, e.g. "cited in a sentence", "code".
    pub reasons: Vec<&'static str>,
    pub preticked: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SkippedCount {
    pub reason: String,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Discovery {
    pub url: String,
    pub title: Option<String>,
    /// Best first, at most `MAX_CANDIDATES_SHOWN`.
    pub candidates: Vec<LinkCandidate>,
    /// Kept by the rules but beyond `MAX_CANDIDATES_SHOWN`.
    pub not_shown: usize,
    /// What the rules removed, most common first — said, not hidden (§2).
    pub skipped: Vec<SkippedCount>,
}

fn host_matches(host: &str, suffixes: &[&str]) -> bool {
    super::net::host_matches_suffixes(host, suffixes)
}

fn bare_host(u: &Url) -> String {
    let h = u.host_str().unwrap_or("").to_ascii_lowercase();
    h.strip_prefix("www.").or_else(|| h.strip_prefix("m.")).or_else(|| h.strip_prefix("mobile.")).map(str::to_string).unwrap_or(h)
}

/// `docs.example.co.th` → `example.co.th`; `a.b.example.com` → `example.com`.
pub fn site_of(host: &str) -> String {
    let labels: Vec<&str> = host.trim_end_matches('.').split('.').collect();
    let n = labels.len();
    if n <= 2 {
        return host.to_ascii_lowercase();
    }
    let take = if SECOND_LEVEL.contains(&labels[n - 2]) && labels[n - 1].len() == 2 { 3 } else { 2 };
    labels[n.saturating_sub(take)..].join(".").to_ascii_lowercase()
}

fn segments(u: &Url) -> Vec<String> {
    u.path_segments().map(|s| s.filter(|p| !p.is_empty()).map(|p| p.to_ascii_lowercase()).collect()).unwrap_or_default()
}


pub fn is_short_link(u: &Url) -> bool {
    host_matches(&u.host_str().unwrap_or("").to_ascii_lowercase(), &SHORTENER_HOSTS)
}

/// No query, or one that only tracks the click.
fn no_real_query(u: &Url) -> bool {
    u.query_pairs().all(|(k, _)| k.starts_with("utm_") || TRACKING_QUERY_KEYS.contains(&k.as_ref()))
}

/// The anchor is the link's own URL, maybe bracketed (`<www.example.com/x>`)
/// or without its scheme — not words describing it.
fn anchor_is_the_url(anchor: &str, link: &Url) -> bool {
    let a = anchor.trim().trim_matches(['<', '>']);
    if a.starts_with("http://") || a.starts_with("https://") {
        return true;
    }
    let host = link.host_str().unwrap_or("");
    !a.is_empty() && !a.contains(char::is_whitespace) && !host.is_empty() && (a.starts_with(host) || a.starts_with(host.trim_start_matches("www.")))
}

/// Why `link` is junk, or `None` to keep it (§5.2). `page` is the page it was
/// found on — `None` for a caption or a description.
pub fn junk_reason(link: &Url, page: Option<&Url>) -> Option<String> {
    let says = |s: &str| Some(s.to_string());
    if !matches!(link.scheme(), "http" | "https") {
        return says("not a web link");
    }
    if let Some(page) = page {
        let strip = |u: &Url| {
            let mut u = u.clone();
            u.set_fragment(None);
            u.to_string().trim_end_matches('/').to_string()
        };
        if strip(link) == strip(page) {
            return says("a link to the same page");
        }
    }
    let raw_host = link.host_str().unwrap_or("").to_ascii_lowercase();
    let host = bare_host(link);
    let path = link.path();
    if SHARE_INTENTS.iter().any(|(h, prefix)| host_matches(&host, &[h]) && (prefix.is_empty() || path.to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase()))) {
        return says("a share button");
    }
    if host_matches(&raw_host, &AD_HOSTS) {
        return says("an ad or tracking link");
    }
    if host_matches(&raw_host, &AFFILIATE_HOSTS) || path.to_ascii_lowercase().contains("/aff/") {
        return says("a shop or affiliate link");
    }
    let segs = segments(link);
    let ext = segs.last().and_then(|l| l.rsplit_once('.')).map(|(_, e)| e.to_string());
    if ext.is_some_and(|e| FILE_EXTENSIONS.contains(&e.as_str())) {
        return says("a file, not a page");
    }
    if segs.iter().any(|s| ACCOUNT_SEGMENTS.contains(&s.as_str())) {
        return says("a sign-in, account or shop page");
    }
    let numbered_page = segs.windows(2).any(|w| w[0] == "page" && w[1].chars().all(|c| c.is_ascii_digit()))
        || link.query_pairs().any(|(k, v)| (k == "page" || k == "paged") && v.chars().all(|c| c.is_ascii_digit()));
    if numbered_page || segs.first().is_some_and(|s| LISTING_SEGMENTS.contains(&s.as_str())) || segs.iter().any(|s| s == "tag" || s == "category") {
        return says("a tag, category, archive or search page");
    }
    if link.query_pairs().any(|(k, _)| k == "share" || k == "replytocom") {
        return says("a share button");
    }
    if segs.len() <= 2 && segs.last().is_some_and(|s| SITE_SEGMENTS.contains(&s.as_str())) {
        return says("a page about the site (about, contact, privacy, terms)");
    }
    let kind = urls::classify(link.as_str());
    if host_matches(&host, &SOCIAL_HOSTS) && matches!(kind, UrlKind::Web | UrlKind::Unsupported(_)) {
        return says("a profile or home page, not a post");
    }
    if let UrlKind::Unsupported(_) = kind {
        return says("a link Jodd cannot read");
    }
    if segs.is_empty() && no_real_query(link) && !is_short_link(link) {
        return says("a site's home page");
    }
    None
}

/// The §5.3 score and the reasons shown for it. `page_site` is the registrable
/// site of the page the link was found on, if any.
pub fn score(link: &Url, l: &PageLink, region: Region, page_site: Option<&str>) -> (i32, Vec<&'static str>) {
    let mut s = 0i32;
    let mut why = Vec::new();
    match region {
        Region::AuthorComment => {
            s += 40;
            why.push("from the author's own comment");
        }
        Region::Pasted => {
            s += 40;
            why.push("you pasted it");
        }
        Region::Shared => {
            s += 30;
            why.push("the link this post shares");
        }
        Region::Body | Region::Description | Region::Caption => {}
    }
    let anchor_lower = l.anchor.to_lowercase();
    let context_lower = l.context.to_lowercase();
    let anchor_is_url = anchor_is_the_url(&l.anchor, link);
    // The sentence minus the anchor: is there prose around the link?
    let around = l.context.chars().count().saturating_sub(l.anchor.chars().count());
    if region == Region::Body && around >= 40 {
        s += 15;
        why.push("cited in a sentence");
    }
    if WHY_WORDS.iter().any(|w| context_lower.contains(w)) {
        s += 20;
        why.push("the text says why it is linked");
    }
    let words = l.anchor.split_whitespace().count();
    let generic = GENERIC_ANCHORS.iter().any(|g| anchor_lower.trim_end_matches(['.', '!', ':', '…']) == *g);
    if generic {
        s -= 10;
        why.push("vague link text");
    } else if !anchor_is_url && (words >= 3 || (words >= 1 && l.anchor.chars().count() >= 12)) {
        s += 10;
        why.push("descriptive link text");
    }
    let host = bare_host(link);
    let link_site = site_of(&host);
    let other_site = page_site.is_some_and(|p| p != link_site);
    if other_site {
        s += 10;
        why.push("another site");
    }
    let docs_host = host.starts_with("docs.") && !host_matches(&host, &NOT_DOCS_HOSTS);
    if host_matches(&host, &PRIMARY_HOSTS) || docs_host || PRIMARY_SUFFIXES.iter().any(|x| host.ends_with(x)) {
        s += 15;
        why.push(if host_matches(&host, &["github.com", "gitlab.com"]) { "code" } else { "docs, paper or official site" });
    }
    let kind = urls::classify(link.as_str());
    if !matches!(kind, UrlKind::Web) {
        s += 10;
        why.push("a video or post Jodd can read");
    }
    if l.position <= 0.5 && region == Region::Body {
        s += 5;
    }
    let segs = segments(link);
    let article_slug = segs.len() == 1 && segs[0].split('-').filter(|w| !w.is_empty()).count() >= ARTICLE_SLUG_WORDS;
    if page_site.is_some() && !other_site && segs.len() <= 1 && !article_slug {
        s -= 15;
        why.push("a top-level page of the same site");
    }
    if l.link_density > 0.6 && l.links_in_container >= 3 {
        s -= 10;
        why.push("in a list of links");
    }
    (s, why)
}

/// Junk-filter, score, dedupe, cap and pre-tick (§5.2–5.3). `exclude`: links
/// already in this ingest (any form; compared canonically).
pub fn rank(links: Vec<(PageLink, Region)>, page_url: Option<&str>, exclude: &[String]) -> (Vec<LinkCandidate>, Vec<SkippedCount>) {
    let page = page_url.and_then(|p| Url::parse(p).ok());
    let page_site = page.as_ref().map(|p| site_of(&bare_host(p)));
    let excluded: Vec<String> = exclude.iter().map(|u| urls::canonical_url(&strip_fragment(u))).collect();
    let mut skipped: Vec<SkippedCount> = Vec::new();
    let mut skip = |reason: String| match skipped.iter_mut().find(|s| s.reason == reason) {
        Some(s) => s.count += 1,
        None => skipped.push(SkippedCount { reason, count: 1 }),
    };
    let mut out: Vec<LinkCandidate> = Vec::new();
    for (l, region) in links {
        let Ok(u) = Url::parse(&l.url) else {
            skip("not a web link".into());
            continue;
        };
        if let Some(reason) = junk_reason(&u, page.as_ref()) {
            skip(reason);
            continue;
        }
        let url = urls::canonical_url(&strip_fragment(u.as_str()));
        if excluded.contains(&url) {
            skip("already a source".into());
            continue;
        }
        let (score, reasons) = score(&u, &l, region, page_site.as_deref());
        let kind = urls::kind_name(&urls::classify(&url));
        let cand = LinkCandidate { url: url.clone(), anchor: l.anchor, context: l.context, region, kind, score, reasons, preticked: false };
        match out.iter_mut().find(|c| c.url == url) {
            Some(prev) if prev.score >= cand.score => {}
            Some(prev) => *prev = cand,
            None => out.push(cand),
        }
    }
    // Stable: equal scores keep page order.
    out.sort_by(|a, b| b.score.cmp(&a.score));
    for c in out.iter_mut().take(MAX_FOLLOWED_PER_SOURCE) {
        c.preticked = c.score >= PRETICK_MIN_SCORE;
    }
    skipped.sort_by(|a, b| b.count.cmp(&a.count));
    (out, skipped)
}

fn strip_fragment(u: &str) -> String {
    match Url::parse(u) {
        Ok(mut p) => {
            p.set_fragment(None);
            p.to_string()
        }
        Err(_) => u.split('#').next().unwrap_or(u).to_string(),
    }
}

/// A link the user ticked under a source (`from_url`), as the frontend sends it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct FollowedLink {
    pub url: String,
    pub from_url: String,
}

// ── AI pick (§5.4, D4) ─────────────────────────────────────────────────────
/// The person's own words sent with the list, at most.
const PICK_CONTEXT_CHARS: usize = 2_000;
/// One pick's reason, at most.
const PICK_WHY_CHARS: usize = 200;

/// A listed candidate as the AI pick sees it (and as the frontend sends it).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct PickInput {
    pub url: String,
    pub anchor: String,
    pub context: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pick {
    pub url: String,
    pub why: String,
}

/// The one user message: the page, the person's words, the numbered links —
/// each URL as its `display_url`, and every URL inside the TEXT fields
/// redacted the same way (a bare-URL anchor is the URL, token and all), so a
/// token in a link never reaches a provider.
pub fn pick_message(page_title: Option<&str>, page_url: &str, why_saved: &str, candidates: &[PickInput]) -> String {
    let clean = urls::redact_url_secrets;
    let links: Vec<serde_json::Value> = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| serde_json::json!({"index": i, "text": clean(&c.anchor), "sentence": clean(&c.context), "url": urls::display_url(&c.url)}))
        .collect();
    serde_json::json!({
        "page": {"title": clean(page_title.unwrap_or("")), "url": urls::display_url(page_url)},
        "why_saved": clean(&why_saved.chars().take(PICK_CONTEXT_CHARS).collect::<String>()),
        "links": links,
    })
    .to_string()
}

/// The model's answer → picks among `candidates`. Lenient about shape
/// (gotcha #7b), strict about content: an index outside the list, a repeat,
/// or anything past `MAX_FOLLOWED_PER_SOURCE` is dropped — the model can only
/// re-tick what Jodd listed, never add a URL.
pub fn parse_picks(reply: &str, candidates: &[PickInput]) -> Result<Vec<Pick>, String> {
    let v: serde_json::Value = crate::llm::provider::parse_envelope_lenient(reply)?;
    let items = v["pick"].as_array().cloned().unwrap_or_default();
    let mut out: Vec<Pick> = Vec::new();
    let mut seen: Vec<usize> = Vec::new();
    for item in items {
        // `{"index": 3}`, `{"index": "3"}`, or a bare `3`.
        let idx = match (&item["index"], &item) {
            (serde_json::Value::Number(n), _) => n.as_u64().map(|n| n as usize),
            (serde_json::Value::String(t), _) => t.trim().parse().ok(),
            (_, serde_json::Value::Number(n)) => n.as_u64().map(|n| n as usize),
            _ => None,
        };
        let Some(i) = idx.filter(|i| *i < candidates.len() && !seen.contains(i)) else { continue };
        seen.push(i);
        let why: String = item["why"].as_str().unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ").chars().take(PICK_WHY_CHARS).collect();
        out.push(Pick { url: candidates[i].url.clone(), why });
        if out.len() >= MAX_FOLLOWED_PER_SOURCE {
            break;
        }
    }
    Ok(out)
}

/// "Let AI choose": one chat call, its answer validated (§5.4).
pub async fn ai_pick(
    provider: &dyn crate::llm::provider::LlmProvider,
    page_title: Option<&str>,
    page_url: &str,
    why_saved: &str,
    candidates: &[PickInput],
    cancel: CancellationToken,
) -> Result<Vec<Pick>, crate::llm::provider::ExtractError> {
    use crate::llm::provider::{ChatRole, ChatTurn, ExtractError};
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let turn = ChatTurn { role: ChatRole::User, content: pick_message(page_title, page_url, why_saved, candidates) };
    let reply = provider.chat(crate::llm::prompt::LINK_PICK_PROMPT, &[turn], cancel).await?;
    parse_picks(&reply, candidates).map_err(|reason| ExtractError::MalformedEnvelope { reason, raw: reply.chars().take(200).collect() })
}

/// What the user pasted from one of their links' comments (D8).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct PastedComments {
    pub url: String,
    pub text: String,
}

/// Pasted text per link, keyed by canonical URL. Each must belong to a link
/// in `user_urls` (canonical already); blank pastes are dropped.
pub fn plan_pasted(user_urls: &[String], pasted: &[PastedComments]) -> Result<std::collections::HashMap<String, String>, String> {
    let mut out = std::collections::HashMap::new();
    for p in pasted {
        if p.text.trim().is_empty() {
            continue;
        }
        let url = urls::canonical_url(&p.url);
        if !user_urls.contains(&url) {
            return Err("Pasted comments must belong to a link in this ingest.".into());
        }
        out.insert(url, p.text.chars().take(discuss::MAX_PASTED_CHARS).collect());
    }
    Ok(out)
}

/// The links in pasted text, ranked (D8). No network — short links stay as
/// they are and are resolved when fetched.
pub fn pasted_candidates(text: &str, exclude: &[String]) -> Discovery {
    let links: Vec<(PageLink, Region)> = web::text_links(text).into_iter().map(|l| (l, Region::Pasted)).collect();
    let (mut candidates, skipped) = rank(links, None, exclude);
    let not_shown = candidates.len().saturating_sub(MAX_CANDIDATES_SHOWN);
    candidates.truncate(MAX_CANDIDATES_SHOWN);
    Discovery { url: String::new(), title: None, candidates, not_shown, skipped }
}

/// The ingest's fetch list — the user's links, then the followed ones — and
/// each followed link's parent, all in canonical form. `user_urls` must
/// already be canonical (`urls::canonical_unique`). Errors are shown to the
/// user. A followed link that duplicates another link is dropped, not refused.
pub fn plan_followed(user_urls: &[String], followed: &[FollowedLink]) -> Result<(Vec<String>, std::collections::HashMap<String, String>), String> {
    let mut all: Vec<String> = user_urls.to_vec();
    let mut from_of: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for f in followed {
        let url = urls::canonical_url(&f.url);
        let from = urls::canonical_url(&f.from_url);
        if all.contains(&url) {
            continue;
        }
        if !all.contains(&from) {
            return Err("A followed link must come from a link in this ingest.".into());
        }
        // D3: depth 2 at most — the parent may itself be followed, its parent may not.
        if from_of.get(&from).is_some_and(|grand| from_of.contains_key(grand)) {
            return Err("Links can be followed at most two levels deep.".into());
        }
        all.push(url.clone());
        from_of.insert(url, from);
    }
    if from_of.len() > MAX_FOLLOWED_TOTAL {
        return Err(format!("At most {MAX_FOLLOWED_TOTAL} followed links can be ingested at once."));
    }
    Ok((all, from_of))
}

/// One guarded hop: where a short link points, or `None` when it would not
/// say. HEAD first (a shortener answers it with its redirect); a server that
/// refuses HEAD gets a GET, whose redirects `send_guarded` follows.
pub async fn resolve_short(url: &str, policy: FetchPolicy, cancel: &CancellationToken) -> Option<String> {
    let head = GuardedRequest { method: Method::HEAD, ..GuardedRequest::get(url) };
    let resp = send_guarded(head, policy, cancel).await.ok()?;
    if resp.status().is_redirection() {
        let loc = resp.headers().get(reqwest::header::LOCATION)?.to_str().ok()?;
        return Url::parse(url).ok()?.join(loc).ok().map(|u| u.to_string());
    }
    if resp.status().is_success() {
        return None;
    }
    let resp = send_guarded(GuardedRequest::get(url), policy, cancel).await.ok()?;
    let fin = resp.url().to_string();
    (fin != url && resp.status().is_success()).then_some(fin)
}

/// Replace each short link the platform already expanded (X) with its target.
fn apply_expansions(links: &mut [PageLink], expanded: &[(String, String)]) {
    for l in links.iter_mut() {
        if let Some((_, long)) = expanded.iter().find(|(short, _)| *short == l.url) {
            if l.anchor == l.url {
                l.anchor = long.clone();
            }
            l.url = long.clone();
        }
    }
}

fn caption_links(meta: PlatformMeta) -> Result<(Option<String>, Vec<(PageLink, Region)>), String> {
    if let Some(reason) = meta.unavailable {
        return Err(reason);
    }
    let mut links = meta.caption.as_deref().map(web::text_links).unwrap_or_default();
    apply_expansions(&mut links, &meta.expanded_links);
    let title = meta.title.or(meta.author);
    Ok((title, links.into_iter().map(|l| (l, Region::Caption)).collect()))
}

/// Where discovery reads from. Tests point these at a mock server.
#[derive(Debug, Clone, Default)]
pub struct Readers {
    pub youtube: YoutubeEndpoints,
    pub threads: ThreadEndpoints,
    /// Set when the user saved a YouTube Data API key: the author's comments
    /// under a video are read for links (spec §6.2).
    pub youtube_comments: Option<discuss::youtube::CommentsApi>,
}

/// URLs with no anchor of their own, each with the sentence it came from.
fn plain_links(urls: &[String], context: &str, region: Region) -> Vec<(PageLink, Region)> {
    let context: String = context.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(web::MAX_CONTEXT_CHARS).collect();
    urls.iter()
        .map(|u| {
            (PageLink { url: u.clone(), anchor: u.clone(), context: context.clone(), link_density: 0.0, links_in_container: 1, position: 0.0 }, region)
        })
        .collect()
}

/// Links in the author's own comments, each with that comment as context (D6).
pub fn author_comment_links(d: &discuss::Discussion) -> Vec<(PageLink, Region)> {
    d.comments.iter().filter(|c| c.is_op).flat_map(|c| plain_links(&c.links, &c.text, Region::AuthorComment)).collect()
}

/// Read `url` for its links and rank them (spec §5). Network: the page itself,
/// and up to `MAX_SHORT_LINKS_RESOLVED` short links. Never a followed page.
pub async fn discover(url: &str, exclude: &[String], readers: &Readers, policy: FetchPolicy, cancel: &CancellationToken) -> Result<Discovery, String> {
    let (title, page_url, links) = match urls::classify(url) {
        UrlKind::Web => {
            let page = web::fetch_page_links(url, policy, cancel).await?;
            let mut links: Vec<(PageLink, Region)> = page.links.into_iter().map(|l| (l, Region::Body)).collect();
            if let Some(d) = &page.discussion {
                links.extend(author_comment_links(d));
            }
            (page.title, Some(page.final_url), links)
        }
        UrlKind::YouTube { id } => {
            let info = youtube::request_player(&id, &readers.youtube, policy, cancel).await?;
            let mut links: Vec<(PageLink, Region)> =
                info.description.as_deref().map(web::text_links).unwrap_or_default().into_iter().map(|l| (l, Region::Description)).collect();
            if let Some(api) = &readers.youtube_comments {
                // Best effort: a video with comments off still has its description.
                if let Ok(d) = discuss::youtube::read(api, &id, policy, cancel).await {
                    links.extend(author_comment_links(&d));
                }
            }
            (info.title, None, links)
        }
        UrlKind::Bluesky { .. } | UrlKind::Mastodon { .. } | UrlKind::HackerNews { .. } => {
            let t = discuss::read_thread(url, &readers.threads, policy, cancel).await?;
            let mut links = plain_links(&t.shared, t.title.as_deref().unwrap_or(""), Region::Shared);
            let inline: Vec<String> = t.links.iter().filter(|l| !t.shared.contains(l)).cloned().collect();
            links.extend(plain_links(&inline, &t.text, Region::Caption));
            links.extend(author_comment_links(&t.discussion));
            (t.title, None, links)
        }
        UrlKind::TikTok { .. } => {
            let (t, l) = caption_links(tiktok::peek_meta(url, policy, cancel).await)?;
            (t, None, l)
        }
        UrlKind::Instagram { shortcode } => {
            let (t, l) = caption_links(instagram::peek_meta(&shortcode, policy, cancel).await)?;
            (t, None, l)
        }
        UrlKind::Facebook { .. } => {
            let (t, l) = caption_links(facebook::fetch_meta(url, policy, cancel).await)?;
            (t, None, l)
        }
        UrlKind::X { id } => {
            let (t, l) = caption_links(x::fetch_meta(&id, policy, cancel).await)?;
            (t, None, l)
        }
        UrlKind::Unsupported(reason) => return Err(reason),
    };
    // The link itself is never its own candidate.
    let mut exclude = exclude.to_vec();
    exclude.push(url.to_string());
    if let Some(p) = &page_url {
        exclude.push(p.clone());
    }
    let links = resolve_short_links(links, policy, cancel).await;
    let (mut candidates, skipped) = rank(links, page_url.as_deref(), &exclude);
    let not_shown = candidates.len().saturating_sub(MAX_CANDIDATES_SHOWN);
    candidates.truncate(MAX_CANDIDATES_SHOWN);
    Ok(Discovery { url: url.to_string(), title, candidates, not_shown, skipped })
}

/// Swap up to `MAX_SHORT_LINKS_RESOLVED` short links for their targets, in
/// page order (the ranking needs the target to judge them). An unresolved one
/// stays as it is and is judged as a short link.
async fn resolve_short_links(mut links: Vec<(PageLink, Region)>, policy: FetchPolicy, cancel: &CancellationToken) -> Vec<(PageLink, Region)> {
    let mut budget = MAX_SHORT_LINKS_RESOLVED;
    for (l, _) in links.iter_mut() {
        if budget == 0 || cancel.is_cancelled() {
            break;
        }
        let Ok(u) = Url::parse(&l.url) else { continue };
        if !is_short_link(&u) {
            continue;
        }
        budget -= 1;
        if let Some(target) = resolve_short(&l.url, policy, cancel).await {
            if l.anchor == l.url {
                l.anchor = target.clone();
            }
            l.url = target;
        }
    }
    links
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINKED: &str = include_str!("fixtures/article_with_links.html");
    const PAGE: &str = "https://blog.example.com/posts/rust";

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn junk(s: &str) -> Option<String> {
        junk_reason(&u(s), Some(&u(PAGE)))
    }

    #[test]
    fn each_rule_drops_its_kind_and_spares_its_lookalike() {
        let cases: [(&str, &str, &str); 12] = [
            ("mailto:a@b.c", "not a web link", "https://a.example/mail"),
            ("https://blog.example.com/posts/rust#intro", "same page", "https://blog.example.com/posts/rust-2"),
            ("https://www.facebook.com/sharer/sharer.php?u=x", "share button", "https://www.facebook.com/reel/123"),
            ("https://ad.doubleclick.net/x", "ad or tracking", "https://doubleclick.example/x"),
            ("https://amzn.to/abc", "affiliate", "https://amazon.example/abc"),
            ("https://site.example/login", "sign-in", "https://site.example/login-flows-explained"),
            ("https://site.example/tag/rust/", "tag, category", "https://site.example/posts/tagging-in-rust"),
            ("https://site.example/blog/page/2", "tag, category", "https://site.example/blog/page-two-notes"),
            ("https://site.example/privacy", "about the site", "https://site.example/docs/privacy/guide/chapter-1"),
            ("https://site.example/a/b.zip", "a file", "https://site.example/a/zip-files"),
            ("https://www.instagram.com/someone/", "profile or home page", "https://www.instagram.com/reel/ABC123xyz/"),
            ("https://site.example/", "home page", "https://site.example/one-post"),
        ];
        for (bad, needle, good) in cases {
            let got = junk(bad);
            assert!(got.as_deref().is_some_and(|r| r.contains(needle)), "{bad}: {got:?}");
            assert_eq!(junk(good), None, "{good} must be kept");
        }
        // A short link is resolved, not dropped as a home page.
        assert_eq!(junk("https://bit.ly/3abc"), None);
    }

    /// Live pass 2026-10-29 (§19): four rules that met real pages and lost.
    #[test]
    fn live_pass_pages_rank_their_articles_not_their_furniture() {
        let link = |url: &str, anchor: &str, context: &str| PageLink {
            url: url.into(),
            anchor: anchor.into(),
            context: context.into(),
            link_density: 0.1,
            links_in_container: 1,
            position: 0.3,
        };
        // Medium tags every link `?source=…`; a home page is still a home page.
        let medium = u("https://netflixtechblog.com/making-failure-diagnosis-reliable-6b0811a1c015");
        let blog_home = u("https://netflixtechblog.medium.com/?source=post_page---byline--6b0811a1c015---");
        assert!(junk_reason(&blog_home, Some(&medium)).is_some_and(|r| r.contains("home page")));
        assert!(junk_reason(&u("https://site.example/?utm_source=x&utm_medium=y"), Some(&u(PAGE))).is_some());
        assert_eq!(junk_reason(&u("https://site.example/?p=123"), Some(&u(PAGE))), None, "a real query is a page");

        // A Medium / Substack article lives one segment deep on the same site.
        let sentence = "We built Pensive to diagnose failed jobs automatically, as described in an earlier post.";
        let article = link("https://netflixtechblog.com/auto-diagnosis-and-remediation-in-netflix-data-platform-5bcc52d853d1", "Pensive", sentence);
        let (s, why) = score(&u(&article.url), &article, Region::Body, Some("netflixtechblog.com"));
        assert!(!why.contains(&"a top-level page of the same site"), "{why:?}");
        let nested = link("https://netflixtechblog.com/p/auto-diagnosis-and-remediation-in-netflix-data-platform", "Pensive", sentence);
        assert_eq!(s, score(&u(&nested.url), &nested, Region::Body, Some("netflixtechblog.com")).0, "scored like any article on the site");
        let profile = link("https://github.com/akryum", "Guillaume Chau", "Maintained by Guillaume Chau and many contributors over the years.");
        let (_, why) = score(&u(&profile.url), &profile, Region::Body, Some("github.com"));
        assert!(why.contains(&"a top-level page of the same site"), "a profile is still penalised: {why:?}");

        // docs.google.com is someone's document, not documentation.
        let gdoc = link("https://docs.google.com/document/d/1abc/edit", "Atlanta", sentence);
        let (_, why) = score(&u(&gdoc.url), &gdoc, Region::Body, Some("astralcodexten.com"));
        assert!(!why.contains(&"docs, paper or official site"), "{why:?}");

        // `<https://…>` is a bare URL, not descriptive link text.
        let bracketed = link("https://www.law.cornell.edu/uscode/text/1/204", "<www.law.cornell.edu/uscode/text/1/204>", sentence);
        let (_, why) = score(&u(&bracketed.url), &bracketed, Region::Body, Some("lexifina.com"));
        assert!(!why.contains(&"descriptive link text"), "{why:?}");
    }

    #[test]
    fn site_of_keeps_country_second_level_domains() {
        assert_eq!(site_of("docs.example.co.th"), "example.co.th");
        assert_eq!(site_of("a.b.example.com"), "example.com");
        assert_eq!(site_of("example.com"), "example.com");
        assert_eq!(site_of("www.bbc.co.uk"), "bbc.co.uk");
    }

    fn ranked() -> (Vec<LinkCandidate>, Vec<SkippedCount>) {
        let (_, links) = web::extract_readable_with_links(LINKED, PAGE);
        rank(links.into_iter().map(|l| (l, Region::Body)).collect(), Some(PAGE), &[])
    }

    #[test]
    fn the_fixture_page_pre_ticks_its_cited_sources_and_not_its_junk() {
        let (c, skipped) = ranked();
        let ticked: Vec<&str> = c.iter().filter(|c| c.preticked).map(|c| c.url.as_str()).collect();
        for want in ["doc.rust-lang.org", "arxiv.org/abs", "github.com/example/ownership-demo"] {
            assert!(ticked.iter().any(|u| u.contains(want)), "{want} not pre-ticked: {ticked:?}\n{c:#?}");
        }
        assert!(ticked.len() <= MAX_FOLLOWED_PER_SOURCE);
        let all: Vec<&str> = c.iter().map(|c| c.url.as_str()).collect();
        for gone in ["amzn.to", "#summary", "diagram-large.png"] {
            assert!(!all.iter().any(|u| u.contains(gone)), "`{gone}` should be junk: {all:?}");
        }
        assert!(skipped.iter().any(|s| s.reason.contains("affiliate")), "{skipped:?}");
        // "click here" to the same site is kept but never pre-ticked.
        let vague = c.iter().find(|c| c.url.ends_with("/posts/next-post")).expect("kept");
        assert!(!vague.preticked && vague.reasons.contains(&"vague link text"), "{vague:?}");
    }

    #[test]
    fn a_link_already_in_the_ingest_is_not_offered_twice() {
        let (_, links) = web::extract_readable_with_links(LINKED, PAGE);
        let (c, skipped) = rank(
            links.into_iter().map(|l| (l, Region::Body)).collect(),
            Some(PAGE),
            &["https://arxiv.org/abs/1902.00001#top".to_string()],
        );
        assert!(!c.iter().any(|c| c.url.contains("arxiv")));
        assert!(skipped.iter().any(|s| s.reason == "already a source"));
    }

    #[test]
    fn duplicates_collapse_to_their_best_occurrence() {
        let l = |anchor: &str, context: &str| PageLink {
            url: "https://other.example/paper".into(),
            anchor: anchor.into(),
            context: context.into(),
            link_density: 0.1,
            links_in_container: 1,
            position: 0.2,
        };
        let (c, _) = rank(
            vec![
                (l("here", "here"), Region::Body),
                (l("the full research paper", "Our numbers come from the full research paper published last year."), Region::Body),
            ],
            Some(PAGE),
            &[],
        );
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].anchor, "the full research paper");
    }

    #[test]
    fn caption_links_use_the_platforms_own_expansions() {
        let mut meta = PlatformMeta::new(crate::ingest::SourceKind::X);
        meta.caption = Some("Code: https://t.co/aaa".into());
        meta.expanded_links = vec![("https://t.co/aaa".into(), "https://github.com/a/b".into())];
        let (_, links) = caption_links(meta).unwrap();
        assert_eq!(links[0].0.url, "https://github.com/a/b");
        let mut gone = PlatformMeta::new(crate::ingest::SourceKind::X);
        gone.unavailable = Some("X returned no post data".into());
        assert_eq!(caption_links(gone).unwrap_err(), "X returned no post data");
    }

    #[test]
    fn pasted_text_belongs_to_a_user_link_and_its_links_arrive_ticked() {
        let user = vec!["https://www.facebook.com/reel/123".to_string()];
        let p = |url: &str, text: &str| PastedComments { url: url.into(), text: text.into() };
        let plan = plan_pasted(&user, &[p("https://www.facebook.com/reel/123", "full video https://www.youtube.com/watch?v=jXtnhyro-QE"), p("https://www.facebook.com/reel/123", " ")]).unwrap();
        assert_eq!(plan.len(), 1);
        assert!(plan_pasted(&user, &[p("https://elsewhere.example/", "x")]).unwrap_err().contains("must belong"));
        let d = pasted_candidates("ดูคลิปเต็ม https://www.youtube.com/watch?v=jXtnhyro-QE และ https://www.facebook.com/sharer/sharer.php?u=x", &user);
        assert_eq!(d.candidates.len(), 1, "{d:?}");
        assert!(d.candidates[0].preticked && d.candidates[0].region == Region::Pasted && d.candidates[0].kind == "youtube");
        assert!(d.skipped.iter().any(|s| s.reason.contains("share")));
    }

    fn pin(url: &str) -> PickInput {
        PickInput { url: url.into(), anchor: format!("text of {url}"), context: "a sentence".into() }
    }

    #[test]
    fn picks_are_validated_capped_and_never_new_urls() {
        let c: Vec<PickInput> = (0..8).map(|i| pin(&format!("https://e.example/{i}"))).collect();
        let reply = r#"Sure! ```json
            {"pick": [{"index": 2, "why": "  the  study  "}, {"index": "5", "why": "code"}, {"index": 2}, {"index": 99},
                      {"index": 0}, {"index": 1}, {"index": 3}, {"index": 4}, {"url": "https://evil.example/"}]}
            ```"#;
        let picks = parse_picks(reply, &c).unwrap();
        assert_eq!(picks.iter().map(|p| p.url.as_str()).collect::<Vec<_>>(), vec![
            "https://e.example/2", "https://e.example/5", "https://e.example/0", "https://e.example/1", "https://e.example/3",
        ]);
        assert_eq!(picks[0].why, "the study");
        assert!(parse_picks(r#"{"pick": []}"#, &c).unwrap().is_empty());
        assert!(parse_picks("no json here", &c).is_err());
    }

    #[test]
    fn the_message_never_carries_a_query_or_fragment() {
        let msg = pick_message(
            Some("T https://t.example/?k=SECRET3"),
            "https://a.example/post?session=SECRET1",
            "my reason, see https://w.example/?q=SECRET4",
            &[PickInput { context: "in https://c.example/?s=SECRET5".into(), ..pin("https://b.example/x?token=SECRET2#frag") }],
        );
        assert!(!msg.contains("SECRET") && !msg.contains("frag"), "{msg}");
        assert!(msg.contains("https://b.example/x") && msg.contains("my reason"));
    }

    struct ChatOnly(String, std::sync::Mutex<Option<(String, String)>>);

    #[async_trait::async_trait]
    impl crate::llm::provider::LlmProvider for ChatOnly {
        async fn extract(&self, _: &str, _: &[String], _: CancellationToken) -> Result<crate::llm::provider::ExtractEnvelope, crate::llm::provider::ExtractError> {
            unreachable!()
        }
        async fn run_workflow(&self, _: crate::llm::provider::WorkflowKind, _: &str, _: &[String], _: CancellationToken) -> Result<crate::llm::provider::ExtractEnvelope, crate::llm::provider::ExtractError> {
            unreachable!()
        }
        async fn suggest_links(&self, _: &str, _: &[crate::llm::provider::CandidateSummary], _: CancellationToken) -> Result<crate::llm::provider::LinkSuggestionsEnvelope, crate::llm::provider::ExtractError> {
            unreachable!()
        }
        async fn suggest_folder(&self, _: &str, _: &[String], _: CancellationToken) -> Result<crate::llm::provider::FolderSuggestionEnvelope, crate::llm::provider::ExtractError> {
            unreachable!()
        }
        async fn synthesize(&self, _: &[crate::llm::provider::SourceDigest], _: &str, _: CancellationToken) -> Result<crate::llm::provider::ExtractEnvelope, crate::llm::provider::ExtractError> {
            unreachable!()
        }
        async fn chat(&self, system: &str, turns: &[crate::llm::provider::ChatTurn], _: CancellationToken) -> Result<String, crate::llm::provider::ExtractError> {
            *self.1.lock().unwrap() = Some((system.to_string(), turns[0].content.clone()));
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn ai_pick_asks_once_with_the_pick_prompt_and_an_empty_list_asks_nothing() {
        let p = ChatOnly(r#"{"pick":[{"index":1,"why":"the paper"}]}"#.into(), Default::default());
        let c = vec![pin("https://a.example/1"), pin("https://a.example/2")];
        let picks = ai_pick(&p, Some("Post"), "https://a.example/post", "why", &c, CancellationToken::new()).await.unwrap();
        assert_eq!(picks, vec![Pick { url: "https://a.example/2".into(), why: "the paper".into() }]);
        let (system, user) = p.1.lock().unwrap().clone().unwrap();
        assert_eq!(system, crate::llm::prompt::LINK_PICK_PROMPT);
        assert!(user.contains("\"index\":1"));
        let none = ChatOnly(String::new(), Default::default());
        assert!(ai_pick(&none, None, "https://a.example/", "", &[], CancellationToken::new()).await.unwrap().is_empty());
        assert!(none.1.lock().unwrap().is_none(), "no candidates, no call");
    }

    fn f(url: &str, from: &str) -> FollowedLink {
        FollowedLink { url: url.into(), from_url: from.into() }
    }

    #[test]
    fn followed_links_need_a_parent_in_the_ingest_and_stop_at_depth_two() {
        let user = vec!["https://a.example/post".to_string()];
        let (all, from) = plan_followed(
            &user,
            &[f("https://b.example/paper", "https://a.example/post"), f("https://c.example/data", "https://b.example/paper")],
        )
        .unwrap();
        assert_eq!(all, vec!["https://a.example/post", "https://b.example/paper", "https://c.example/data"]);
        assert_eq!(from["https://c.example/data"], "https://b.example/paper");
        // A duplicate of a user link is dropped, not refused.
        let (all, from) = plan_followed(&user, &[f("https://a.example/post", "https://a.example/post")]).unwrap();
        assert_eq!((all.len(), from.len()), (1, 0));
        assert!(plan_followed(&user, &[f("https://b.example/x", "https://elsewhere.example/")]).unwrap_err().contains("must come from"));
        let chain = [
            f("https://b.example/1", "https://a.example/post"),
            f("https://b.example/2", "https://b.example/1"),
            f("https://b.example/3", "https://b.example/2"),
        ];
        assert!(plan_followed(&user, &chain).unwrap_err().contains("two levels"));
        let many: Vec<FollowedLink> = (0..=MAX_FOLLOWED_TOTAL).map(|i| f(&format!("https://b.example/{i}"), "https://a.example/post")).collect();
        assert!(plan_followed(&user, &many).unwrap_err().contains("At most"));
    }

    const LOOPBACK: FetchPolicy = FetchPolicy { allow_loopback: true };

    #[tokio::test]
    async fn a_short_link_resolves_through_head_and_falls_back_to_get() {
        let mut server = mockito::Server::new_async().await;
        let _h = server.mock("HEAD", "/s1").with_status(301).with_header("location", "https://target.example/post").create_async().await;
        let _h2 = server.mock("HEAD", "/s2").with_status(405).create_async().await;
        let _g2 = server.mock("GET", "/s2").with_status(302).with_header("location", "/landed").create_async().await;
        let _l = server.mock("GET", "/landed").with_status(200).with_body("ok").create_async().await;
        let c = CancellationToken::new();
        assert_eq!(resolve_short(&format!("{}/s1", server.url()), LOOPBACK, &c).await.as_deref(), Some("https://target.example/post"));
        assert_eq!(resolve_short(&format!("{}/s2", server.url()), LOOPBACK, &c).await, Some(format!("{}/landed", server.url())));
    }

    #[tokio::test]
    async fn discovery_reads_one_page_and_never_a_candidate() {
        let mut server = mockito::Server::new_async().await;
        let page = format!(
            "<body><article><p>Our numbers come from <a href=\"{0}/paper\">the full research paper on the method</a>, published last year.</p></article></body>",
            server.url()
        );
        let p = server.mock("GET", "/post").with_status(200).with_header("content-type", "text/html").with_body(page).expect(1).create_async().await;
        let paper = server.mock("GET", "/paper").expect(0).create_async().await;
        let d = discover(&format!("{}/post", server.url()), &[], &Readers::default(), LOOPBACK, &CancellationToken::new()).await.unwrap();
        p.assert_async().await;
        paper.assert_async().await;
        assert_eq!(d.candidates.len(), 1);
        assert!(d.candidates[0].url.ends_with("/paper"));
    }

    #[tokio::test]
    async fn the_authors_comment_link_arrives_ticked() {
        let mut server = mockito::Server::new_async().await;
        let page = r#"<html><head><script type="application/ld+json">
            {"@type":"BlogPosting","author":{"name":"Ann"},"headline":"Teaser","comment":[
              {"@type":"Comment","author":{"name":"Ann"},"text":"Full version here https://full.example/video"},
              {"@type":"Comment","author":{"name":"Spam"},"text":"Win now https://spam.example/prize"}]}
            </script></head><body><article><p>Just a teaser; see the comments.</p></article></body></html>"#;
        let _m = server.mock("GET", "/post").with_status(200).with_header("content-type", "text/html").with_body(page).create_async().await;
        let d = discover(&format!("{}/post", server.url()), &[], &Readers::default(), LOOPBACK, &CancellationToken::new()).await.unwrap();
        let full = d.candidates.iter().find(|c| c.url == "https://full.example/video").expect("the author's link is a candidate");
        assert!(full.preticked && full.region == Region::AuthorComment, "{full:?}");
        assert!(!d.candidates.iter().any(|c| c.url.contains("spam.example")), "only the author's comments give links");
    }

    #[tokio::test]
    async fn the_default_policy_refuses_a_local_page() {
        let mut server = mockito::Server::new_async().await;
        let _m = server.mock("GET", "/").with_status(200).with_header("content-type", "text/html").with_body("<p>x</p>").create_async().await;
        let err = discover(&server.url(), &[], &Readers::default(), FetchPolicy::default(), &CancellationToken::new()).await.unwrap_err();
        assert_eq!(err, crate::ingest::net::REFUSED);
    }
}
