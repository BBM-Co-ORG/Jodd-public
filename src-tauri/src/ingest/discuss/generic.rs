//! Comments an ordinary web page publishes in a standard shape (spec
//! 2026-10-28 §7.1) — no site-specific code:
//!
//! 1. **Discourse** forums (`<meta name="generator" content="Discourse …">`):
//!    the topic's public `/t/<id>.json`. A Discourse page is a JavaScript app,
//!    so the first post becomes the page's text and the rest its comments.
//! 2. **JSON-LD** `comment[]` on an article or a forum post, and a `QAPage`'s
//!    answers (the accepted one counts as pinned). A comment already in the
//!    readable text (a page that renders them) is not repeated.
//! 3. The post's own **comment feed** — WordPress's `<link rel="alternate"
//!    type="application/rss+xml" title="… Comments Feed">`, or Atom's
//!    `<link rel="replies">`.
//!
//! The page's author (JSON-LD `author`, else `<meta name="author">`) marks
//! `is_op`. Nothing here is measured against a live site (spec §9).

use markup5ever_rcdom::Handle;
use quick_xml::events::Event;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{get_json, html_text_and_links, Comment, Discussion, MAX_COMMENTS_READ};
use crate::ingest::net::{read_capped, send_guarded, GuardedRequest};
use crate::ingest::web::{attr, find_all, find_first, local_name};
use crate::ingest::FetchPolicy;

const MAX_FEED_BYTES: usize = 2 * 1024 * 1024;
/// How a WordPress post's comment feed is titled, in the languages seen.
const COMMENT_FEED_TITLES: [&str; 3] = ["comments feed", "ฟีดความเห็น", "ความเห็นต่อ"];

/// What the page's markup says about its comments. No network.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hints {
    pub author: Option<String>,
    /// A JSON-LD discussion, already read.
    pub json_ld: Option<Discussion>,
    /// The post's own comment feed (RSS or Atom).
    pub feed: Option<String>,
    /// A Discourse topic's JSON.
    pub discourse: Option<String>,
}

/// Comments read from a page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageComments {
    pub discussion: Discussion,
    /// Discourse: the first post's text, to stand in for an empty page.
    pub main_text: Option<String>,
}

fn json_ld_values(doc: &Handle) -> Vec<Value> {
    let mut scripts = Vec::new();
    find_all(
        doc,
        &|n| local_name(n).as_deref() == Some("script") && attr(n, "type").is_some_and(|t| t.trim().eq_ignore_ascii_case("application/ld+json")),
        &mut scripts,
    );
    scripts
        .iter()
        .filter_map(|s| {
            let mut raw = String::new();
            collect_raw(s, &mut raw);
            serde_json::from_str::<Value>(raw.trim()).ok()
        })
        .collect()
}

fn collect_raw(node: &Handle, out: &mut String) {
    for c in node.children.borrow().iter() {
        if let markup5ever_rcdom::NodeData::Text { contents } = &c.data {
            out.push_str(&contents.borrow());
        }
    }
}

fn name_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        Value::Object(_) => v["name"].as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        Value::Array(a) => a.first().and_then(name_of),
        _ => None,
    }
}

fn types_of(v: &Value) -> Vec<String> {
    match &v["@type"] {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a.iter().filter_map(|t| t.as_str().map(str::to_string)).collect(),
        _ => Vec::new(),
    }
}

/// Every object in the JSON-LD, `@graph` and nesting included (depth-limited).
fn objects<'a>(v: &'a Value, depth: usize, out: &mut Vec<&'a Value>) {
    if depth > 8 {
        return;
    }
    match v {
        Value::Object(map) => {
            out.push(v);
            for x in map.values() {
                objects(x, depth + 1, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| objects(x, depth + 1, out)),
        _ => {}
    }
}

fn likes_of(v: &Value) -> Option<u64> {
    v["upvoteCount"].as_u64().or_else(|| {
        // `interactionStatistic: {interactionType: LikeAction, userInteractionCount: n}`
        let stats = match &v["interactionStatistic"] {
            Value::Array(a) => a.clone(),
            o @ Value::Object(_) => vec![o.clone()],
            _ => Vec::new(),
        };
        stats.iter().find(|s| s["interactionType"].to_string().contains("LikeAction")).and_then(|s| s["userInteractionCount"].as_u64())
    })
}

fn push_ld_comment(v: &Value, depth: u8, parent: Option<usize>, author: Option<&str>, pinned: bool, readable: &str, out: &mut Vec<Comment>) {
    if out.len() >= MAX_COMMENTS_READ {
        return;
    }
    let Some(raw) = v["text"].as_str().or_else(|| v["articleBody"].as_str()) else { return };
    let (text, links) = html_text_and_links(raw, None);
    let probe: String = text.chars().take(80).collect();
    // A page that renders its comments already has them in its text.
    let shown = probe.chars().count() >= 20 && readable.contains(probe.as_str());
    let me = if shown {
        parent
    } else {
        out.push(Comment {
            is_op: author.is_some() && name_of(&v["author"]).as_deref() == author,
            pinned,
            likes: likes_of(v),
            depth,
            parent,
            text,
            links,
        });
        Some(out.len() - 1)
    };
    let next = if shown { depth } else { depth.saturating_add(1) };
    for child in v["comment"].as_array().into_iter().flatten() {
        push_ld_comment(child, next, me, author, false, readable, out);
    }
}

/// JSON-LD comments and answers, minus the ones already in `readable`.
pub fn json_ld_discussion(values: &[Value], author: Option<&str>, readable: &str) -> Option<Discussion> {
    let mut all = Vec::new();
    for v in values {
        objects(v, 0, &mut all);
    }
    let mut comments = Vec::new();
    let mut total: Option<u64> = None;
    for o in &all {
        let types = types_of(o);
        // A QAPage's `mainEntity` is a Question, which `objects` also visits
        // on its own: read Questions only, once.
        if types.iter().any(|t| t == "Question") {
            let q = *o;
            if !q["acceptedAnswer"].is_null() {
                push_ld_comment(&q["acceptedAnswer"], 0, None, author, true, readable, &mut comments);
            }
            for a in q["suggestedAnswer"].as_array().into_iter().flatten() {
                push_ld_comment(a, 0, None, author, false, readable, &mut comments);
            }
            total = q["answerCount"].as_u64().or(total);
            continue;
        }
        if let Some(list) = o["comment"].as_array() {
            if types.iter().any(|t| t == "Comment") {
                continue; // a nested comment — reached from its post
            }
            for c in list {
                push_ld_comment(c, 0, None, author, false, readable, &mut comments);
            }
            total = o["commentCount"].as_u64().or(total);
        }
    }
    (!comments.is_empty()).then_some(Discussion { total, comments })
}

/// Read the page's markup for comment hints. No network.
pub fn hints(html: &str, page_url: &str, readable: &str) -> Hints {
    use html5ever::tendril::TendrilSink;
    let dom = html5ever::parse_document(markup5ever_rcdom::RcDom::default(), Default::default()).one(html);
    let doc = dom.document.clone();
    let values = json_ld_values(&doc);
    let mut objs = Vec::new();
    for v in &values {
        objects(v, 0, &mut objs);
    }
    let author = objs
        .iter()
        .find(|o| !types_of(o).iter().any(|t| t == "Comment") && !o["author"].is_null() && (!o["articleBody"].is_null() || !o["headline"].is_null() || !o["comment"].is_null()))
        .and_then(|o| name_of(&o["author"]))
        .or_else(|| {
            find_first(&doc, &|n| local_name(n).as_deref() == Some("meta") && attr(n, "name").is_some_and(|v| v.eq_ignore_ascii_case("author")))
                .and_then(|m| attr(&m, "content"))
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });
    let json_ld = json_ld_discussion(&values, author.as_deref(), readable);
    let base = reqwest::Url::parse(page_url).ok();
    let resolve = |href: &str| base.as_ref().and_then(|b| b.join(href).ok()).filter(|u| matches!(u.scheme(), "http" | "https")).map(|u| u.to_string());
    let mut links = Vec::new();
    find_all(&doc, &|n| local_name(n).as_deref() == Some("link"), &mut links);
    let feed = links.iter().find_map(|l| {
        let rel = attr(l, "rel").unwrap_or_default().to_ascii_lowercase();
        let ty = attr(l, "type").unwrap_or_default().to_ascii_lowercase();
        let title = attr(l, "title").unwrap_or_default().to_lowercase();
        let href = attr(l, "href")?;
        let replies = rel.split_whitespace().any(|r| r == "replies") && ty.contains("atom");
        let wp = rel.split_whitespace().any(|r| r == "alternate") && ty.contains("rss") && COMMENT_FEED_TITLES.iter().any(|t| title.contains(t));
        let url = resolve(&href)?;
        // The site-wide comments feed is not this post's.
        let site_wide = reqwest::Url::parse(&url).map(|u| u.path().trim_end_matches('/') == "/comments/feed").unwrap_or(true);
        ((replies || wp) && !site_wide).then_some(url)
    });
    let generator = find_first(&doc, &|n| local_name(n).as_deref() == Some("meta") && attr(n, "name").is_some_and(|v| v.eq_ignore_ascii_case("generator")))
        .and_then(|m| attr(&m, "content"))
        .unwrap_or_default();
    let discourse = generator.to_ascii_lowercase().starts_with("discourse").then(|| discourse_json_url(page_url)).flatten();
    drop(links);
    drop(doc);
    Hints { author, json_ld, feed, discourse }
}

/// `…/t/<slug>/<id>[/<post>]` → `…/t/<id>.json`.
pub fn discourse_json_url(page_url: &str) -> Option<String> {
    let u = reqwest::Url::parse(page_url).ok()?;
    let segs: Vec<&str> = u.path_segments()?.filter(|s| !s.is_empty()).collect();
    let at = segs.iter().position(|s| *s == "t")?;
    let id = segs[at + 1..].iter().find(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))?;
    let prefix = segs[..at].join("/");
    let origin = u.origin().ascii_serialization();
    Some(if prefix.is_empty() { format!("{origin}/t/{id}.json") } else { format!("{origin}/{prefix}/t/{id}.json") })
}

/// A Discourse topic: the first post's text, the rest as comments.
pub fn parse_discourse(topic: &Value, page_url: &str) -> Option<PageComments> {
    let posts = topic["post_stream"]["posts"].as_array()?;
    let first = posts.iter().find(|p| p["post_number"].as_u64() == Some(1))?;
    let op = first["username"].as_str().unwrap_or("");
    let (main, _) = html_text_and_links(first["cooked"].as_str().unwrap_or(""), Some(page_url));
    let title = topic["title"].as_str().map(str::trim).filter(|t| !t.is_empty());
    let main_text = match title {
        Some(t) => format!("{t}\n{main}"),
        None => main,
    };
    let mut comments: Vec<Comment> = Vec::new();
    let mut index: std::collections::HashMap<u64, (usize, u8)> = std::collections::HashMap::new();
    for p in posts.iter().filter(|p| p["post_number"].as_u64() != Some(1)) {
        if comments.len() >= MAX_COMMENTS_READ {
            break;
        }
        let (text, links) = html_text_and_links(p["cooked"].as_str().unwrap_or(""), Some(page_url));
        let reply_to = p["reply_to_post_number"].as_u64();
        let (parent, depth) = match reply_to.and_then(|n| index.get(&n)) {
            Some(&(i, d)) => (Some(i), d.saturating_add(1)),
            None => (None, 0),
        };
        let likes = p["like_count"].as_u64().or_else(|| {
            p["actions_summary"].as_array()?.iter().find(|a| a["id"].as_u64() == Some(2)).and_then(|a| a["count"].as_u64())
        });
        comments.push(Comment { is_op: !op.is_empty() && p["username"].as_str() == Some(op), pinned: false, likes, depth, parent, text, links });
        if let Some(n) = p["post_number"].as_u64() {
            index.insert(n, (comments.len() - 1, depth));
        }
    }
    let total = topic["posts_count"].as_u64().map(|n| n.saturating_sub(1));
    Some(PageComments { discussion: Discussion { total, comments }, main_text: Some(main_text) })
}

/// Text of the current element, entity references resolved.
fn push_text(ev: &Event, buf: &mut String) {
    match ev {
        Event::Text(t) => buf.push_str(&t.decode().unwrap_or_default()),
        Event::CData(t) => buf.push_str(&t.decode().unwrap_or_default()),
        Event::GeneralRef(r) => {
            if let Ok(Some(c)) = r.resolve_char_ref() {
                buf.push(c);
            } else {
                match r.decode().unwrap_or_default().as_ref() {
                    "amp" => buf.push('&'),
                    "lt" => buf.push('<'),
                    "gt" => buf.push('>'),
                    "quot" => buf.push('"'),
                    "apos" => buf.push('\''),
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// A WordPress comment body without its opening "In reply to <a
/// href=…#comment-N>Name</a>." paragraph. Told by the link, not the words —
/// the words are in the site's language. The name is a commenter's (D7).
fn without_reply_line(body: &str) -> &str {
    let trimmed = body.trim_start();
    if !trimmed.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("<p>")) {
        return body;
    }
    let Some(end) = trimmed.find("</p>") else { return body };
    let first = &trimmed[..end];
    let one_link = first.matches("<a ").count() == 1;
    let to_a_comment = first.contains("#comment-");
    if one_link && to_a_comment && first.chars().count() < 200 {
        &trimmed[end + "</p>".len()..]
    } else {
        body
    }
}

/// An RSS `<item>` or Atom `<entry>` list → comments (no threading: a feed
/// has none). `author` marks the page author's own replies.
pub fn parse_feed(xml: &str, author: Option<&str>) -> Discussion {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut comments = Vec::new();
    // Inside an item: (who, best body so far, body is from content:encoded/content)
    let mut item: Option<(String, String, bool)> = None;
    let mut field: Option<String> = None;
    let mut buf = String::new();
    loop {
        let ev = match reader.read_event() {
            Ok(Event::Eof) | Err(_) => break,
            Ok(ev) => ev,
        };
        match &ev {
            Event::Start(s) => {
                let name = String::from_utf8_lossy(s.name().as_ref()).to_string();
                if name == "item" || name == "entry" {
                    item = Some((String::new(), String::new(), false));
                } else if item.is_some() {
                    field = Some(name);
                    buf.clear();
                }
            }
            Event::End(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name == "item" || name == "entry" {
                    if let Some((who, body, _)) = item.take() {
                        if comments.len() < MAX_COMMENTS_READ && !body.trim().is_empty() {
                            let (text, links) = html_text_and_links(without_reply_line(&body), None);
                            let who = who.trim().trim_start_matches("By:").trim().to_string();
                            comments.push(Comment {
                                is_op: author.is_some_and(|a| !who.is_empty() && who == a),
                                pinned: false,
                                likes: None,
                                depth: 0,
                                parent: None,
                                text,
                                links,
                            });
                        }
                    }
                } else if let (Some(f), Some((who, body, rich))) = (field.as_deref(), item.as_mut()) {
                    if f == name.as_str() {
                        match f {
                            "dc:creator" | "name" => *who = buf.clone(),
                            "title" if who.is_empty() => *who = buf.clone(),
                            "content:encoded" | "content" => {
                                *body = buf.clone();
                                *rich = true;
                            }
                            "description" | "summary" if !*rich => *body = buf.clone(),
                            _ => {}
                        }
                        field = None;
                    }
                }
            }
            other => {
                if field.is_some() {
                    push_text(other, &mut buf);
                }
            }
        }
    }
    let total = Some(comments.len() as u64);
    Discussion { total, comments }
}

async fn get_text(url: &str, policy: FetchPolicy, cancel: &CancellationToken) -> Result<String, String> {
    let resp = send_guarded(GuardedRequest::get(url), policy, cancel).await?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let bytes = read_capped(resp, MAX_FEED_BYTES, cancel).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The page's comments, by the first route its markup offers. Best effort: a
/// route that fails reads as "no comments", never as a failed page.
pub async fn read(html: &str, page_url: &str, readable: &str, policy: FetchPolicy, cancel: &CancellationToken) -> Option<PageComments> {
    let h = hints(html, page_url, readable);
    if let Some(url) = &h.discourse {
        if let Ok(topic) = get_json(url, policy, cancel).await {
            if let Some(pc) = parse_discourse(&topic, page_url) {
                return Some(pc);
            }
        }
    }
    if let Some(d) = h.json_ld {
        return Some(PageComments { discussion: d, main_text: None });
    }
    if let Some(feed) = &h.feed {
        if let Ok(xml) = get_text(feed, policy, cancel).await {
            let d = parse_feed(&xml, h.author.as_deref());
            if !d.comments.is_empty() {
                return Some(PageComments { discussion: d, main_text: None });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_ld_comments_nest_mark_the_author_and_skip_what_the_page_shows() {
        let page = json!({"@context": "https://schema.org", "@type": "DiscussionForumPosting", "author": {"name": "Ann"},
            "headline": "Post", "commentCount": 9, "comment": [
                {"@type": "Comment", "text": "A question that the page itself never renders anywhere", "author": {"name": "Bob"}, "upvoteCount": 4,
                 "comment": [{"@type": "Comment", "text": "<p>The author answers with <a href=\"https://example.com/x\">a link</a></p>", "author": "Ann"}]},
                {"@type": "Comment", "text": "This comment is already rendered in the page text", "author": {"name": "Cy"}}
            ]});
        let d = json_ld_discussion(&[page], Some("Ann"), "… This comment is already rendered in the page text …").unwrap();
        assert_eq!(d.total, Some(9));
        assert_eq!(d.comments.len(), 2);
        assert_eq!(d.comments[0].likes, Some(4));
        assert!(d.comments[1].is_op && d.comments[1].depth == 1 && d.comments[1].parent == Some(0));
        assert_eq!(d.author_links(), vec!["https://example.com/x".to_string()]);
    }

    #[test]
    fn a_qa_page_pins_the_accepted_answer() {
        let qa = json!({"@type": "QAPage", "mainEntity": {"@type": "Question", "answerCount": 2,
            "acceptedAnswer": {"@type": "Answer", "text": "Use the borrow checker this way, as the docs explain", "upvoteCount": 50},
            "suggestedAnswer": [{"@type": "Answer", "text": "Another approach that also works fine for most cases", "upvoteCount": 3}]}});
        let d = json_ld_discussion(&[qa], None, "").unwrap();
        assert!(d.comments[0].pinned && d.comments[0].likes == Some(50));
        assert_eq!(d.comments.len(), 2);
    }

    #[test]
    fn hints_find_the_posts_own_feed_and_not_the_site_wide_one() {
        let html = r#"<html><head>
            <meta name="author" content="Ann">
            <link rel="alternate" type="application/rss+xml" title="Blog » Feed" href="/feed/">
            <link rel="alternate" type="application/rss+xml" title="Blog » Comments Feed" href="/comments/feed/">
            <link rel="alternate" type="application/rss+xml" title="Blog » My post Comments Feed" href="/my-post/feed/">
            </head><body><article><p>x</p></article></body></html>"#;
        let h = hints(html, "https://blog.example/my-post/", "x");
        assert_eq!(h.feed.as_deref(), Some("https://blog.example/my-post/feed/"));
        assert_eq!(h.author.as_deref(), Some("Ann"));
        assert_eq!(h.discourse, None);
    }

    #[test]
    fn rss_and_atom_comment_feeds_read() {
        let rss = r#"<?xml version="1.0"?><rss xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:content="http://purl.org/rss/1.0/modules/content/"><channel>
            <title>Comments on: My post</title>
            <item><title>By: Bob</title><dc:creator><![CDATA[Bob]]></dc:creator>
              <description>Short &amp; plain</description>
              <content:encoded><![CDATA[<p>A full comment from Bob with <a href="https://example.com/b">a link</a></p>]]></content:encoded></item>
            <item><title>By: Ann</title><dc:creator>Ann</dc:creator><description>&lt;p&gt;The author replies here, at length&lt;/p&gt;</description></item>
            </channel></rss>"#;
        let d = parse_feed(rss, Some("Ann"));
        assert_eq!(d.comments.len(), 2);
        assert_eq!(d.comments[0].text, "A full comment from Bob with a link");
        assert_eq!(d.comments[0].links, vec!["https://example.com/b".to_string()]);
        assert!(d.comments[1].is_op);
        assert_eq!(d.comments[1].text, "The author replies here, at length");
        let atom = r#"<feed xmlns="http://www.w3.org/2005/Atom"><entry><author><name>Cy</name></author><content type="html">&lt;p&gt;Atom comment body text&lt;/p&gt;</content></entry></feed>"#;
        let d = parse_feed(atom, None);
        assert_eq!(d.comments[0].text, "Atom comment body text");
    }

    /// Live pass 2026-10-29 (§19): WordPress opens a threaded comment with
    /// "In reply to <a href=…#comment-N>Name</a>." in the site's language —
    /// a commenter's name (D7) and a link back to the page itself.
    #[test]
    fn a_wordpress_reply_line_names_nobody() {
        let rss = r#"<rss xmlns:content="http://purl.org/rss/1.0/modules/content/"><channel>
            <item><title>By: Ann</title><content:encoded><![CDATA[<p>In reply to <a href="https://blog.example/post/#comment-607604">Daniel Kam</a>.</p>
<p>You can post as much as you want to the main blog.</p>]]></content:encoded></item>
            <item><title>By: Bob</title><content:encoded><![CDATA[<p>ตอบกลับ <a href="https://blog.example/post/#comment-12">Somchai</a></p><p>ขอบคุณมากครับ อธิบายได้ชัดเจนดี</p>]]></content:encoded></item>
            <item><title>By: Cy</title><content:encoded><![CDATA[<p>See <a href="https://example.com/paper">the paper</a> on this.</p>]]></content:encoded></item>
            </channel></rss>"#;
        let d = parse_feed(rss, Some("Ann"));
        assert_eq!(d.comments[0].text, "You can post as much as you want to the main blog.");
        assert!(d.comments[0].links.is_empty(), "{:?}", d.comments[0].links);
        assert_eq!(d.comments[1].text, "ขอบคุณมากครับ อธิบายได้ชัดเจนดี");
        assert_eq!(d.comments[2].text, "See the paper on this.", "an ordinary first paragraph stays");
    }

    #[test]
    fn discourse_topics_read_the_first_post_as_the_page() {
        assert_eq!(discourse_json_url("https://forum.example/t/some-topic/1234/5").as_deref(), Some("https://forum.example/t/1234.json"));
        assert_eq!(discourse_json_url("https://example.com/community/t/x/77").as_deref(), Some("https://example.com/community/t/77.json"));
        let topic = json!({"title": "How do I X?", "posts_count": 3, "post_stream": {"posts": [
            {"post_number": 1, "username": "ann", "cooked": "<p>The question body</p>"},
            {"post_number": 2, "username": "bob", "cooked": "<p>An answer with real detail in it</p>", "like_count": 7},
            {"post_number": 3, "username": "ann", "cooked": "<p>Thanks, that worked: https://example.com/fix</p>", "reply_to_post_number": 2,
             "actions_summary": [{"id": 2, "count": 1}]}
        ]}});
        let pc = parse_discourse(&topic, "https://forum.example/t/x/1").unwrap();
        assert_eq!(pc.main_text.as_deref(), Some("How do I X?\nThe question body"));
        assert_eq!(pc.discussion.total, Some(2));
        let c = &pc.discussion.comments;
        assert_eq!((c[0].likes, c[1].likes), (Some(7), Some(1)));
        assert!(c[1].is_op && c[1].parent == Some(0));
    }

    const LOOPBACK: FetchPolicy = FetchPolicy { allow_loopback: true };

    #[tokio::test]
    async fn read_follows_the_feed_through_the_guard() {
        let mut server = mockito::Server::new_async().await;
        let feed = server
            .mock("GET", "/post/feed/")
            .with_body(r#"<rss><channel><item><title>By: Bob</title><description>A comment with enough letters in it</description></item></channel></rss>"#)
            .create_async()
            .await;
        let html = r#"<head><link rel="alternate" type="application/rss+xml" title="My post Comments Feed" href="/post/feed/"></head><body><article>x</article></body>"#;
        let pc = read(html, &format!("{}/post/", server.url()), "x", LOOPBACK, &CancellationToken::new()).await.unwrap();
        feed.assert_async().await;
        assert_eq!(pc.discussion.comments.len(), 1);
        // The default policy never reaches a local feed.
        assert_eq!(read(html, &format!("{}/post/", server.url()), "x", FetchPolicy::default(), &CancellationToken::new()).await, None);
    }
}
