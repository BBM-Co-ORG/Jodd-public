//! Mastodon (and the fediverse servers that speak its API): a status and its
//! replies from the post's own server (spec 2026-10-28 §6.2), via
//! `GET /api/v1/statuses/:id` and `GET /api/v1/statuses/:id/context`.
//!
//! The link only LOOKS like Mastodon (`/@user/<id>` on any host), so a status
//! request that does not answer like Mastodon is `NOT_MASTODON`, and the
//! caller reads the link as a web page instead. A server that hides replies
//! from signed-out readers gets the post and a `Partial`.

use std::collections::HashMap;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{first_line, get_json, html_text_and_links, Comment, Discussion, Thread, MAX_COMMENTS_READ, NOT_MASTODON};
use crate::ingest::FetchPolicy;

const REPLIES_NEED_SIGN_IN: &str = "this server shows replies only to signed-in readers";
const REPLIES_UNREAD: &str = "the replies could not be read";

/// Mentions (`/@user`, `/users/…`) and hashtags (`/tags/…`) are links in a
/// status's HTML but are never sources.
fn is_profile_or_tag(url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url) else { return true };
    let first = u.path_segments().and_then(|mut s| s.next()).unwrap_or("");
    first.starts_with('@') || first == "tags" || first == "users"
}

/// A status's text (content warning first) and its real links.
fn status_text(status: &Value, base: &str) -> (String, Vec<String>) {
    let (mut text, links) = html_text_and_links(status["content"].as_str().unwrap_or(""), Some(base));
    if let Some(cw) = status["spoiler_text"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
        text = format!("CW: {cw}\n{text}");
    }
    (text, links.into_iter().filter(|l| !is_profile_or_tag(l)).collect())
}

/// A reply without the `@handle`s its client put in front of it (D7: a
/// commenter is never named). One inside the sentence was written, and stays.
fn without_leading_handles(text: &str) -> String {
    let mut rest = text.trim_start();
    while rest.starts_with('@') {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        rest = rest[end..].trim_start();
    }
    rest.to_string()
}

fn author(status: &Value) -> Option<String> {
    let acct = status["account"]["acct"].as_str().filter(|a| !a.is_empty());
    let name = status["account"]["display_name"].as_str().map(str::trim).filter(|n| !n.is_empty());
    match (name, acct) {
        (Some(n), Some(a)) => Some(format!("{n} (@{a})")),
        (None, Some(a)) => Some(format!("@{a}")),
        (Some(n), None) => Some(n.to_string()),
        (None, None) => None,
    }
}

/// `context.descendants`, in the server's thread order, as comments.
pub fn parse_descendants(root: &Value, descendants: &[Value], base: &str) -> Vec<Comment> {
    let root_id = root["id"].as_str().unwrap_or("");
    let root_account = root["account"]["id"].as_str().unwrap_or("");
    let mut index: HashMap<String, (usize, u8)> = HashMap::new();
    let mut out = Vec::new();
    for d in descendants {
        if out.len() >= MAX_COMMENTS_READ {
            break;
        }
        let Some(id) = d["id"].as_str() else { continue };
        let reply_to = d["in_reply_to_id"].as_str().unwrap_or("");
        let (parent, depth) = if reply_to == root_id {
            (None, 0)
        } else {
            match index.get(reply_to) {
                Some(&(i, depth)) => (Some(i), depth.saturating_add(1)),
                // A reply to something not in the list: show it top-level.
                None => (None, 0),
            }
        };
        let (text, links) = status_text(d, base);
        let text = without_leading_handles(&text);
        out.push(Comment {
            is_op: !root_account.is_empty() && d["account"]["id"].as_str() == Some(root_account),
            pinned: false,
            likes: d["favourites_count"].as_u64(),
            depth,
            parent,
            text,
            links,
        });
        index.insert(id.to_string(), (out.len() - 1, depth));
    }
    out
}

/// The status JSON, read. `None` when it is not a Mastodon status.
pub fn parse_status(status: &Value, base: &str) -> Option<Thread> {
    status["content"].as_str()?;
    status["id"].as_str()?;
    let (text, links) = status_text(status, base);
    let shared: Vec<String> = status["card"]["url"].as_str().filter(|u| u.starts_with("https://") || u.starts_with("http://")).map(str::to_string).into_iter().collect();
    Some(Thread { title: first_line(&text), author: author(status), text, links, shared, discussion: Discussion::default(), partial: None })
}

pub async fn read(origin: &str, id: &str, page_url: &str, policy: FetchPolicy, cancel: &CancellationToken) -> Result<Thread, String> {
    let status = match get_json(&format!("{origin}/api/v1/statuses/{id}"), policy, cancel).await {
        Ok(v) => v,
        Err(e) if e == crate::ingest::net::CANCELLED => return Err(e),
        Err(_) => return Err(NOT_MASTODON.into()),
    };
    let mut thread = parse_status(&status, page_url).ok_or_else(|| NOT_MASTODON.to_string())?;
    match get_json(&format!("{origin}/api/v1/statuses/{id}/context"), policy, cancel).await {
        Ok(ctx) => {
            let descendants = ctx["descendants"].as_array().cloned().unwrap_or_default();
            let comments = parse_descendants(&status, &descendants, page_url);
            thread.discussion = Discussion { total: Some(descendants.len() as u64), comments };
        }
        Err(e) if e == "HTTP 401" || e == "HTTP 403" => thread.partial = Some(REPLIES_NEED_SIGN_IN.into()),
        Err(e) if e == crate::ingest::net::CANCELLED => return Err(e),
        Err(_) => thread.partial = Some(REPLIES_UNREAD.into()),
    }
    Ok(thread)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn status(id: &str, account: &str, reply_to: Option<&str>, html: &str, favs: u64) -> Value {
        json!({"id": id, "in_reply_to_id": reply_to, "content": html, "spoiler_text": "", "favourites_count": favs,
               "account": {"id": account, "acct": "ann@example.social", "display_name": "Ann"}})
    }

    const BASE: &str = "https://example.social/@ann/1100000000";

    #[test]
    fn mentions_and_hashtags_are_not_links_but_cards_are_shared() {
        let mut s = status("1100000000", "1", None, r#"<p>Read <a href="https://example.com/post">this</a> <a href="https://example.social/@bob">@bob</a> <a href="https://example.social/tags/rust">#rust</a></p>"#, 0);
        s["card"] = json!({"url": "https://example.com/post"});
        let t = parse_status(&s, BASE).unwrap();
        assert_eq!(t.links, vec!["https://example.com/post".to_string()]);
        assert_eq!(t.shared, vec!["https://example.com/post".to_string()]);
        assert_eq!(t.author.as_deref(), Some("Ann (@ann@example.social)"));
        assert!(parse_status(&json!({"title": "not mastodon"}), BASE).is_none());
    }

    #[test]
    fn replies_nest_by_in_reply_to_and_the_author_is_marked() {
        let root = status("1100000000", "1", None, "<p>Root</p>", 0);
        let d = vec![
            status("1100000001", "2", Some("1100000000"), "<p>First reply with a real point to make</p>", 4),
            status("1100000002", "1", Some("1100000001"), "<p>The author answers the first reply</p>", 1),
            status("1100000003", "3", Some("9999"), "<p>Orphan reply to something unseen here</p>", 0),
        ];
        let c = parse_descendants(&root, &d, BASE);
        assert_eq!((c[0].depth, c[0].parent, c[0].is_op), (0, None, false));
        assert_eq!((c[1].depth, c[1].parent, c[1].is_op), (1, Some(0), true));
        assert_eq!((c[2].depth, c[2].parent), (0, None));
        assert_eq!(c[0].likes, Some(4));
    }

    /// Live pass 2026-10-29 (§19): every reply opens with the handles it
    /// answers — the client adds them, the commenter did not write them (D7).
    #[test]
    fn a_reply_loses_its_leading_handles_but_keeps_one_inside_the_text() {
        let root = status("1100000000", "1", None, "<p>Root</p>", 0);
        let mention = |who: &str| format!(r#"<span class="h-card"><a href="https://example.social/@{who}" class="u-url mention">@<span>{who}</span></a></span>"#);
        let d = vec![
            status("1100000001", "2", Some("1100000000"), &format!("<p>{} {} Number one is superb</p>", mention("ann"), mention("bob@other.example")), 1),
            status("1100000002", "3", Some("1100000000"), &format!("<p>Agreed, and cc {} who asked</p>", mention("cy")), 0),
        ];
        let c = parse_descendants(&root, &d, BASE);
        assert_eq!(c[0].text, "Number one is superb");
        assert_eq!(c[1].text, "Agreed, and cc @cy who asked");
    }

    const LOOPBACK: FetchPolicy = FetchPolicy { allow_loopback: true };

    #[tokio::test]
    async fn not_mastodon_and_signed_in_only_replies_are_told_apart() {
        let mut server = mockito::Server::new_async().await;
        let _no = server.mock("GET", "/api/v1/statuses/1200000000").with_status(404).with_body("<html>nope</html>").create_async().await;
        let err = read(&server.url(), "1200000000", BASE, LOOPBACK, &CancellationToken::new()).await.unwrap_err();
        assert_eq!(err, NOT_MASTODON);
        let body = status("1100000000", "1", None, "<p>Root post text</p>", 0).to_string();
        let _s = server.mock("GET", "/api/v1/statuses/1100000000").with_body(body).create_async().await;
        let _c = server.mock("GET", "/api/v1/statuses/1100000000/context").with_status(401).create_async().await;
        let t = read(&server.url(), "1100000000", BASE, LOOPBACK, &CancellationToken::new()).await.unwrap();
        assert_eq!(t.text, "Root post text");
        assert_eq!(t.partial.as_deref(), Some(REPLIES_NEED_SIGN_IN));
    }
}
