//! Read discussions (spec 2026-10-28-follow-links-and-discussions-design.md §6).
//!
//! A post's comments become part of its source: the post's author first, then
//! the best few by likes, junk dropped (D6). Third parties are never named in
//! the note (D7): a comment reads `Author` or `Commenter`, never a handle.
//! Only the author's comments contribute link candidates — anyone can post a
//! link under someone else's post.
//!
//! Readers return the comments AS READ, in thread order; this module decides
//! which are kept and how they read.
//!
//! UNMEASURED: the session that built these readers reached none of the hosts
//! (spec §9). Every shape below is from the platform's own documentation or
//! the spec's sources; the first live pass is what proves them.

pub mod bluesky;
pub mod generic;
pub mod hn;
pub mod mastodon;
pub mod youtube;

use tokio_util::sync::CancellationToken;

use super::net::{read_capped, send_guarded, GuardedRequest};
use super::urls::{self, UrlKind};
use super::{FetchPolicy, FetchStatus, FetchedSource, SourceKind};

/// Where the thread readers send requests. Tests point these at a mock server.
#[derive(Debug, Clone)]
pub struct ThreadEndpoints {
    /// Bluesky's public AppView.
    pub bluesky: String,
    /// Hacker News' Algolia API.
    pub hn: String,
}

impl Default for ThreadEndpoints {
    fn default() -> Self {
        ThreadEndpoints { bluesky: bluesky::APPVIEW.to_string(), hn: hn::ALGOLIA.to_string() }
    }
}

/// A post and its replies, as one reader read them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Thread {
    pub title: Option<String>,
    /// The post's author — public, and already the note's source (D7 is about
    /// commenters).
    pub author: Option<String>,
    pub text: String,
    /// Links inside the post's text.
    pub links: Vec<String>,
    /// The link the post exists to share: a Hacker News story, a Bluesky
    /// link card, a Mastodon preview card.
    pub shared: Vec<String>,
    pub discussion: Discussion,
    /// Why the replies are incomplete, when they are.
    pub partial: Option<String>,
}

/// Mastodon's API did not answer like Mastodon: the link is read as a page.
pub const NOT_MASTODON: &str = "not a Mastodon post";
/// A JSON answer larger than this is not a thread.
const MAX_JSON_BYTES: usize = 4 * 1024 * 1024;

/// One guarded GET, JSON back. Errors read `HTTP <code>` for a status, so a
/// caller can tell "sign in to see replies" (401/403) apart.
pub(crate) async fn get_json(url: &str, policy: FetchPolicy, cancel: &CancellationToken) -> Result<serde_json::Value, String> {
    let req = GuardedRequest { headers: vec![("accept", "application/json".to_string())], ..GuardedRequest::get(url) };
    let resp = send_guarded(req, policy, cancel).await?;
    let status = resp.status();
    let bytes = read_capped(resp, MAX_JSON_BYTES, cancel).await?;
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    serde_json::from_slice(&bytes).map_err(|_| "the answer was not JSON".to_string())
}

/// Read a thread link with its reader.
pub async fn read_thread(url: &str, ep: &ThreadEndpoints, policy: FetchPolicy, cancel: &CancellationToken) -> Result<Thread, String> {
    match urls::classify(url) {
        UrlKind::Bluesky { actor, rkey } => bluesky::read(&ep.bluesky, &actor, &rkey, policy, cancel).await,
        UrlKind::Mastodon { origin, id } => mastodon::read(&origin, &id, url, policy, cancel).await,
        UrlKind::HackerNews { id } => hn::read(&ep.hn, &id, policy, cancel).await,
        _ => Err("not a thread link".into()),
    }
}

/// A thread as an ingest source: the post, what it shares, then its comments.
pub async fn fetch_thread(url: &str, kind: SourceKind, ep: &ThreadEndpoints, policy: FetchPolicy, cancel: &CancellationToken) -> FetchedSource {
    match read_thread(url, ep, policy, cancel).await {
        Ok(t) => thread_source(url, kind, t),
        // A `/@user/123` page that is not Mastodon is still a page.
        Err(e) if kind == SourceKind::Mastodon && e == NOT_MASTODON => super::web::fetch_web(url, policy, cancel).await,
        Err(e) => FetchedSource::failed(url, kind, e),
    }
}

pub fn thread_source(url: &str, kind: SourceKind, t: Thread) -> FetchedSource {
    let mut text = String::new();
    let platform = super::social::platform_label(kind);
    match &t.author {
        Some(a) => text.push_str(&format!("{platform} post by {a}\n")),
        None => text.push_str(&format!("{platform} post\n")),
    }
    if kind == SourceKind::HackerNews {
        if let Some(title) = &t.title {
            text.push_str(&format!("{title}\n"));
        }
    }
    if !t.text.trim().is_empty() {
        text.push_str(t.text.trim());
        text.push('\n');
    }
    for s in &t.shared {
        text.push_str(&format!("Shared link: {s}\n"));
    }
    let text = append_to(text.trim_end(), &t.discussion);
    let status = match t.partial {
        Some(why) => FetchStatus::Partial(why),
        None => FetchStatus::Ok,
    };
    FetchedSource { url: url.to_string(), kind, title: t.title, text, status }
}

/// The first non-empty line, at most 100 characters — a post has no title.
pub fn first_line(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let cut: String = line.chars().take(100).collect();
    Some(if line.chars().count() > 100 { format!("{}…", cut.trim_end()) } else { cut })
}

/// Comments read per discussion, at most (≤ 2 pages of most APIs).
pub const MAX_COMMENTS_READ: usize = 200;
/// Comments kept in the source.
pub const MAX_COMMENTS_KEPT: usize = 30;
/// Characters of comments kept in the source.
pub const MAX_COMMENT_CHARS: usize = 12_000;
/// One comment longer than this is cut, with "…".
const MAX_ONE_COMMENT_CHARS: usize = 1_500;
/// A comment with fewer letters and digits than this (after removing links and
/// @mentions) says nothing: "🔥🔥🔥", "+1", "first!".
const MIN_COMMENT_LETTERS: usize = 15;
/// The header a discussion section opens with inside a source's text.
pub const COMMENTS_HEADER: &str = "--- Comments";
/// What the user pasted from comments Jodd cannot read (D8).
pub const PASTED_HEADER: &str = "--- Pasted by you from the comments ---";
/// Pasted text kept per source.
pub const MAX_PASTED_CHARS: usize = 20_000;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Comment {
    /// Written by the post's own author — only when the platform says so.
    pub is_op: bool,
    pub pinned: bool,
    pub likes: Option<u64>,
    /// 0 = top level.
    pub depth: u8,
    /// Index in the read list of the comment this replies to.
    pub parent: Option<usize>,
    /// Plain text; HTML already stripped.
    pub text: String,
    /// Full URLs this comment links to (facets, `href`s, bare URLs).
    pub links: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Discussion {
    /// As the platform reports it; `None` when it does not.
    pub total: Option<u64>,
    /// As read, thread order.
    pub comments: Vec<Comment>,
}

impl Discussion {
    /// The links the post's author put in their own comments — the "full
    /// video in the comments" case. All of them, not only the kept ones.
    pub fn author_links(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for c in self.comments.iter().filter(|c| c.is_op) {
            for l in &c.links {
                if !out.contains(l) {
                    out.push(l.clone());
                }
            }
        }
        out
    }

    /// The kept comments as a section of a source's text, or `None` when
    /// nothing is worth keeping.
    pub fn render(&self) -> Option<String> {
        let kept = select(&self.comments);
        if kept.is_empty() {
            return None;
        }
        let total = self.total.map(|t| t as usize).unwrap_or(self.comments.len()).max(self.comments.len());
        let author_first = kept.iter().any(|&i| self.comments[i].is_op);
        let mut out = format!(
            "{COMMENTS_HEADER} ({} of {} {}{}) ---",
            kept.len(),
            group_thousands(total),
            if total == 1 { "comment" } else { "comments" },
            if author_first { "; the author's first" } else { "" },
        );
        let mut prev: Option<usize> = None;
        for &i in &kept {
            let c = &self.comments[i];
            let who = if c.is_op { "Author" } else { "Commenter" };
            // An arrow says "answers the line above" — only true when it does.
            // The author's replies are listed first, far from what they answer.
            let under_parent = c.depth > 0 && c.parent.is_some() && c.parent == prev;
            prev = Some(i);
            let mut tags: Vec<String> = Vec::new();
            if c.depth > 0 && !under_parent {
                tags.push("a reply".into());
            }
            if c.pinned {
                tags.push("pinned".into());
            }
            if let Some(n) = c.likes.filter(|n| *n > 0) {
                tags.push(format!("{} {}", group_thousands(n as usize), if n == 1 { "like" } else { "likes" }));
            }
            let label = if tags.is_empty() { who.to_string() } else { format!("{who} ({})", tags.join(", ")) };
            let indent = if under_parent { "  ↳ " } else { "" };
            out.push_str(&format!("\n{indent}{label}: {}", one_line(&c.text)));
        }
        Some(out)
    }
}

/// `1240` → `1 240` (a thin, unambiguous separator in any locale).
fn group_thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

fn one_line(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= MAX_ONE_COMMENT_CHARS {
        return flat;
    }
    let cut: String = flat.chars().take(MAX_ONE_COMMENT_CHARS).collect();
    format!("{}…", cut.trim_end())
}

/// Letters and digits once links and @mentions are gone. Thai vowel and tone
/// marks are not `alphanumeric`, so the Thai block counts whole.
fn letters(text: &str) -> usize {
    text.split_whitespace()
        .filter(|w| !w.starts_with('@') && !w.starts_with("http://") && !w.starts_with("https://"))
        .flat_map(str::chars)
        .filter(|c| c.is_alphanumeric() || ('\u{0E00}'..='\u{0E7F}').contains(c))
        .count()
}

/// Mostly a link: the text is little more than its URLs.
fn mostly_link(text: &str) -> bool {
    let total: usize = text.split_whitespace().map(|w| w.chars().count()).sum();
    let urls: usize = text.split_whitespace().filter(|w| w.starts_with("http://") || w.starts_with("https://")).map(|w| w.chars().count()).sum();
    total > 0 && urls * 2 > total
}

fn is_junk(c: &Comment) -> bool {
    if c.is_op {
        // The author's own words are never junk — "link 👇 https://…" is the point.
        return c.text.trim().is_empty() && c.links.is_empty();
    }
    letters(&c.text) < MIN_COMMENT_LETTERS || mostly_link(&c.text)
}

/// D6 / §6.3: indices into `comments`, in the order they are shown.
/// 1. The author's comments and pinned ones, thread order.
/// 2. Top-level comments by likes (ties: thread order), each with its best
///    reply.
/// Junk and exact duplicates are dropped; stops at the count and size caps.
pub fn select(comments: &[Comment]) -> Vec<usize> {
    let mut kept: Vec<usize> = Vec::new();
    let mut seen_texts: Vec<String> = Vec::new();
    let mut chars = 0usize;
    let mut take = |i: usize, kept: &mut Vec<usize>| -> bool {
        if kept.len() >= MAX_COMMENTS_KEPT || kept.contains(&i) || is_junk(&comments[i]) {
            return false;
        }
        let norm = one_line(&comments[i].text).to_lowercase();
        if seen_texts.contains(&norm) {
            return false;
        }
        let len = norm.chars().count();
        if chars + len > MAX_COMMENT_CHARS && !kept.is_empty() {
            return false;
        }
        chars += len;
        seen_texts.push(norm);
        kept.push(i);
        true
    };
    for (i, c) in comments.iter().enumerate() {
        if c.is_op || c.pinned {
            take(i, &mut kept);
        }
    }
    let mut top: Vec<usize> = (0..comments.len()).filter(|&i| comments[i].depth == 0 && !comments[i].is_op && !comments[i].pinned).collect();
    top.sort_by(|&a, &b| comments[b].likes.unwrap_or(0).cmp(&comments[a].likes.unwrap_or(0)));
    for i in top {
        if kept.len() >= MAX_COMMENTS_KEPT {
            break;
        }
        if !take(i, &mut kept) {
            continue;
        }
        let best_reply = (0..comments.len())
            .filter(|&j| comments[j].parent == Some(i) && !comments[j].is_op && !is_junk(&comments[j]))
            .max_by(|&a, &b| comments[a].likes.unwrap_or(0).cmp(&comments[b].likes.unwrap_or(0)).then(b.cmp(&a)));
        if let Some(j) = best_reply {
            take(j, &mut kept);
        }
    }
    kept
}

/// The source text with its discussion appended (spec §6.1). A source whose
/// discussion kept nothing is returned as it was.
pub fn append_to(text: &str, discussion: &Discussion) -> String {
    match discussion.render() {
        Some(section) if text.trim().is_empty() => section,
        Some(section) => format!("{}\n\n{section}", text.trim_end()),
        None => text.to_string(),
    }
}

/// Text the user pasted from comments Jodd cannot read (D8), appended under
/// its own header, capped.
pub fn append_pasted(text: &str, pasted: &str) -> String {
    let pasted = pasted.trim();
    if pasted.is_empty() {
        return text.to_string();
    }
    let kept: String = pasted.chars().take(MAX_PASTED_CHARS).collect();
    if text.trim().is_empty() {
        format!("{PASTED_HEADER}\n{kept}")
    } else {
        format!("{}\n\n{PASTED_HEADER}\n{kept}", text.trim_end())
    }
}

/// A source's text without its discussion and pasted sections — what was
/// said, for Transcript mode.
pub fn without_discussion(text: &str) -> &str {
    let cut = [format!("\n\n{COMMENTS_HEADER} ("), format!("\n\n{PASTED_HEADER}")]
        .iter()
        .filter_map(|h| text.find(h.as_str()))
        .min();
    let starts_with = text.starts_with(&format!("{COMMENTS_HEADER} (")) || text.starts_with(PASTED_HEADER);
    match (starts_with, cut) {
        (true, _) => "",
        (false, Some(i)) => &text[..i],
        (false, None) => text,
    }
}

/// Plain text of a comment body that arrives as HTML, and the links in it.
pub fn html_text_and_links(html: &str, base: Option<&str>) -> (String, Vec<String>) {
    let wrapped = format!("<html><body><article>{html}</article></body></html>");
    let base = base.unwrap_or("https://invalid.example/");
    let (r, links) = super::web::extract_readable_with_links(&wrapped, base);
    let mut out: Vec<String> = Vec::new();
    for l in links {
        if (l.url.starts_with("https://") || l.url.starts_with("http://")) && !l.url.starts_with("https://invalid.example/") && !out.contains(&l.url) {
            out.push(l.url);
        }
    }
    // Bare URLs in the text too (a comment that pasted one without a link).
    for u in super::urls::detect(&r.text) {
        if !out.contains(&u) {
            out.push(u);
        }
    }
    (r.text, out)
}

/// Bare URLs in plain text.
pub fn text_links(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for u in super::urls::detect(text) {
        if !out.contains(&u) {
            out.push(u);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(text: &str, likes: u64) -> Comment {
        Comment { text: text.into(), likes: Some(likes), ..Comment::default() }
    }

    #[test]
    fn the_author_comes_first_then_the_best_by_likes_with_one_reply_each() {
        let comments = vec![
            c("A useful question about the second half of the video", 5),
            Comment { parent: Some(0), depth: 1, ..c("A good answer that explains the second half in detail", 9) },
            Comment { parent: Some(0), depth: 1, ..c("A weaker answer that still has enough letters in it", 1) },
            c("The most liked comment on this post, by a long way", 300),
            Comment { is_op: true, links: vec!["https://example.com/full".into()], ..c("Full video 👇 https://example.com/full", 0) },
        ];
        let kept = select(&comments);
        assert_eq!(kept, vec![4, 3, 0, 1]);
        let d = Discussion { total: Some(1240), comments };
        let text = d.render().unwrap();
        assert!(text.starts_with("--- Comments (4 of 1 240 comments; the author's first) ---"), "{text}");
        assert!(text.contains("\nAuthor: Full video 👇 https://example.com/full"), "{text}");
        assert!(text.contains("\nCommenter (300 likes): The most liked"), "{text}");
        assert!(text.contains("\n  ↳ Commenter (9 likes): A good answer"), "{text}");
        assert_eq!(d.author_links(), vec!["https://example.com/full".to_string()]);
    }

    /// Live pass 2026-10-29 (§19): the author's replies are listed first, away
    /// from what they answer — an arrow there would point at the wrong line.
    #[test]
    fn an_arrow_only_follows_the_comment_it_answers() {
        let comments = vec![
            c("A commenter asks something with plenty of words", 15),
            Comment { is_op: true, parent: Some(0), depth: 1, ..c("The author answers that commenter here", 13) },
            Comment { is_op: true, ..c("The author adds a top-level follow-up", 105) },
            Comment { parent: Some(0), depth: 1, ..c("Another commenter replies to the question", 8) },
        ];
        let text = Discussion { total: None, comments }.render().unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[1], "Author (a reply, 13 likes): The author answers that commenter here", "{text}");
        assert_eq!(lines[2], "Author (105 likes): The author adds a top-level follow-up", "{text}");
        assert_eq!(lines[3], "Commenter (15 likes): A commenter asks something with plenty of words", "{text}");
        assert_eq!(lines[4], "  ↳ Commenter (8 likes): Another commenter replies to the question", "{text}");
    }

    #[test]
    fn junk_duplicates_and_link_drops_are_dropped_but_never_the_author() {
        let comments = vec![
            c("🔥🔥🔥🔥", 900),
            c("@someone +1 first!", 800),
            c("https://spam.example/win-a-prize-now-click", 700),
            c("This explains the setup step better than the docs do", 10),
            c("this explains the setup step better than the docs do", 9),
            c("ขอบคุณสำหรับคลิปนี้ครับ อธิบายเข้าใจง่ายมาก", 3),
            Comment { is_op: true, ..c("👇", 0) },
        ];
        let kept = select(&comments);
        assert_eq!(kept, vec![6, 3, 5]);
    }

    #[test]
    fn the_caps_hold() {
        let many: Vec<Comment> = (0..100).map(|i| c(&format!("Comment number {i} with enough words to be kept around"), i)).collect();
        assert_eq!(select(&many).len(), MAX_COMMENTS_KEPT);
        let long: Vec<Comment> = (0..20).map(|i| c(&format!("{i} {}", "word ".repeat(400)), 1)).collect();
        let kept = select(&long);
        let chars: usize = kept.iter().map(|&i| one_line(&long[i].text).chars().count()).sum();
        assert!(chars <= MAX_COMMENT_CHARS, "{chars}");
        assert!(!kept.is_empty());
    }

    #[test]
    fn nothing_worth_keeping_leaves_the_source_alone() {
        let d = Discussion { total: Some(3), comments: vec![c("👍", 1), c("lol", 2)] };
        assert_eq!(d.render(), None);
        assert_eq!(append_to("The post.", &d), "The post.");
        let good = Discussion { total: None, comments: vec![c("A comment that adds a real point to the post", 2)] };
        assert!(append_to("The post.", &good).starts_with("The post.\n\n--- Comments (1 of 1 comment) ---"));
    }

    #[tokio::test]
    async fn a_mastodon_shaped_page_that_is_not_mastodon_is_read_as_a_page() {
        let mut server = mockito::Server::new_async().await;
        let _api = server.mock("GET", "/api/v1/statuses/112233445566").with_status(404).create_async().await;
        let _page = server
            .mock("GET", "/@ann/112233445566")
            .with_status(200)
            .with_header("content-type", "text/html")
            .with_body("<body><article><p>An ordinary page that happens to have this shape.</p></article></body>")
            .create_async()
            .await;
        let url = format!("{}/@ann/112233445566", server.url());
        let policy = FetchPolicy { allow_loopback: true };
        let s = fetch_thread(&url, SourceKind::Mastodon, &ThreadEndpoints::default(), policy, &CancellationToken::new()).await;
        assert_eq!(s.kind, SourceKind::Web);
        assert!(s.text.contains("An ordinary page"), "{s:?}");
    }

    #[test]
    fn a_thread_source_reads_post_link_and_comments_in_that_order() {
        let t = Thread {
            title: Some("A story".into()),
            author: Some("pg".into()),
            text: "Body of the post.".into(),
            links: vec![],
            shared: vec!["https://example.com/story".into()],
            discussion: Discussion { total: Some(1), comments: vec![c("A comment that adds a real point to the post", 2)] },
            partial: Some("the replies could not be read".into()),
        };
        let s = thread_source("https://news.ycombinator.com/item?id=1", SourceKind::HackerNews, t);
        assert!(s.text.starts_with("Hacker News post by pg\nA story\nBody of the post.\nShared link: https://example.com/story\n\n--- Comments"), "{}", s.text);
        assert_eq!(s.status, FetchStatus::Partial("the replies could not be read".into()));
    }

    #[test]
    fn transcript_mode_never_reads_comments_or_pastes() {
        let d = Discussion { total: None, comments: vec![c("A comment that adds a real point to the post", 2)] };
        let with = append_pasted(&append_to("What was said.", &d), "pasted https://a.example/x");
        assert_eq!(without_discussion(&with), "What was said.");
        assert_eq!(without_discussion("Only speech."), "Only speech.");
        assert_eq!(without_discussion(&append_pasted("", "just a paste")), "");
    }

    #[test]
    fn pasted_text_is_capped_and_headed() {
        assert_eq!(append_pasted("Post", "  "), "Post");
        assert_eq!(append_pasted("", "the link https://a.example/x"), format!("{PASTED_HEADER}\nthe link https://a.example/x"));
        let long = append_pasted("Post", &"x".repeat(MAX_PASTED_CHARS + 50));
        assert_eq!(long.chars().filter(|c| *c == 'x').count(), MAX_PASTED_CHARS);
    }

    #[test]
    fn html_comment_bodies_give_text_and_links() {
        let (text, links) = html_text_and_links(r#"<p>See <a href="https://example.com/a">this</a> and https://example.com/b</p>"#, None);
        assert_eq!(text, "See this and https://example.com/b");
        assert_eq!(links, vec!["https://example.com/a".to_string(), "https://example.com/b".to_string()]);
        let (_, rel) = html_text_and_links(r#"<a href="/relative">x</a>"#, None);
        assert!(rel.is_empty(), "a relative link with no base is not a link: {rel:?}");
    }
}
