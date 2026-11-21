//! Hacker News: a story (or comment) and its whole reply tree in one request
//! to Algolia's public API, `GET /api/v1/items/:id` (spec 2026-10-28 §6.2).
//!
//! Algolia gives no score for comments, so `likes` is `None` and the
//! selection keeps thread order among them. The story's own link is the
//! thread's shared link — usually the thing worth following.

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{first_line, get_json, html_text_and_links, Comment, Discussion, Thread, MAX_COMMENTS_READ};
use crate::ingest::FetchPolicy;

pub const ALGOLIA: &str = "https://hn.algolia.com";

fn walk(node: &Value, depth: u8, parent: Option<usize>, op: &str, out: &mut Vec<Comment>) {
    for child in node["children"].as_array().into_iter().flatten() {
        if out.len() >= MAX_COMMENTS_READ {
            return;
        }
        // A deleted comment has no text and no author; its replies still count.
        let Some(html) = child["text"].as_str() else {
            walk(child, depth, parent, op, out);
            continue;
        };
        let (text, links) = html_text_and_links(html, Some("https://news.ycombinator.com/"));
        out.push(Comment {
            is_op: !op.is_empty() && child["author"].as_str() == Some(op),
            pinned: false,
            likes: None,
            depth,
            parent,
            text,
            links,
        });
        let me = out.len() - 1;
        walk(child, depth.saturating_add(1), Some(me), op, out);
    }
}

fn count(node: &Value) -> u64 {
    node["children"].as_array().map(|c| c.iter().map(|x| 1 + count(x)).sum()).unwrap_or(0)
}

/// The Algolia item, read. No network.
pub fn parse_item(item: &Value) -> Result<Thread, String> {
    if item["id"].is_null() {
        return Err("Hacker News did not return this item".into());
    }
    let op = item["author"].as_str().unwrap_or("");
    let (text, links) = match item["text"].as_str() {
        Some(html) => html_text_and_links(html, Some("https://news.ycombinator.com/")),
        None => (String::new(), Vec::new()),
    };
    let title = item["title"].as_str().map(str::to_string).or_else(|| first_line(&text));
    let shared: Vec<String> = item["url"].as_str().filter(|u| u.starts_with("https://") || u.starts_with("http://")).map(str::to_string).into_iter().collect();
    let mut comments = Vec::new();
    walk(item, 0, None, op, &mut comments);
    Ok(Thread {
        title,
        author: (!op.is_empty()).then(|| op.to_string()),
        text,
        links,
        shared,
        discussion: Discussion { total: Some(count(item)), comments },
        partial: None,
    })
}

pub async fn read(base: &str, id: &str, policy: FetchPolicy, cancel: &CancellationToken) -> Result<Thread, String> {
    let v = get_json(&format!("{base}/api/v1/items/{id}"), policy, cancel).await.map_err(|e| format!("Hacker News did not return the item ({e})"))?;
    parse_item(&v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item() -> Value {
        json!({
            "id": 1, "type": "story", "author": "pg", "title": "A story worth reading", "url": "https://example.com/story", "text": null,
            "children": [
                {"id": 2, "author": "alice", "text": "<p>A long and thoughtful top-level comment</p>", "children": [
                    {"id": 3, "author": "pg", "text": "<p>The submitter replies: see <a href=\"https://example.com/data\">the data</a></p>", "children": []}
                ]},
                {"id": 4, "author": null, "text": null, "children": [
                    {"id": 5, "author": "bob", "text": "<p>A reply under a deleted comment still reads fine</p>", "children": []}
                ]}
            ]
        })
    }

    #[test]
    fn a_story_shares_its_link_and_reads_its_tree() {
        let t = parse_item(&item()).unwrap();
        assert_eq!(t.title.as_deref(), Some("A story worth reading"));
        assert_eq!(t.shared, vec!["https://example.com/story".to_string()]);
        assert_eq!(t.discussion.total, Some(4));
        let c = &t.discussion.comments;
        assert_eq!(c.len(), 3);
        assert!(c[1].is_op && c[1].depth == 1 && c[1].parent == Some(0));
        assert_eq!(c[1].text, "The submitter replies: see the data");
        assert_eq!(t.discussion.author_links(), vec!["https://example.com/data".to_string()]);
        // The orphaned reply keeps its level.
        assert_eq!((c[2].depth, c[2].parent), (0, None));
    }

    #[tokio::test]
    async fn read_asks_algolia_for_the_item() {
        let mut server = mockito::Server::new_async().await;
        let m = server.mock("GET", "/api/v1/items/1").with_body(item().to_string()).create_async().await;
        let t = read(&server.url(), "1", FetchPolicy { allow_loopback: true }, &CancellationToken::new()).await.unwrap();
        m.assert_async().await;
        assert_eq!(t.author.as_deref(), Some("pg"));
    }
}
