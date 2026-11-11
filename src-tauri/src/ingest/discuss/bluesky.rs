//! Bluesky: a post and its replies from the public AppView, no sign-in
//! (spec 2026-10-28 §6.2; `app.bsky.feed.getPostThread` is documented as
//! public at https://docs.bsky.app/docs/api/app-bsky-feed-get-post-thread).
//!
//! A post's text shows long links shortened ("example.com/very-lo…"); the full
//! URL is in the post's link facets, so the facets are what is read.

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{first_line, get_json, Comment, Discussion, Thread, MAX_COMMENTS_READ};
use crate::ingest::FetchPolicy;

pub const APPVIEW: &str = "https://public.api.bsky.app";
/// Reply levels asked for.
const DEPTH: &str = "6";
const THREAD_POST: &str = "app.bsky.feed.defs#threadViewPost";
const LINK_FACET: &str = "app.bsky.richtext.facet#link";
const EXTERNAL_EMBED: &str = "app.bsky.embed.external#view";

/// The post's text with each link facet's byte range replaced by its full
/// URL, and those URLs. A facet whose range is not on character boundaries is
/// skipped (the URL is still listed).
pub fn post_text(post: &Value) -> (String, Vec<String>) {
    let text = post["record"]["text"].as_str().unwrap_or("").to_string();
    let mut spans: Vec<(usize, usize, String)> = Vec::new();
    let mut links: Vec<String> = Vec::new();
    for facet in post["record"]["facets"].as_array().into_iter().flatten() {
        for feature in facet["features"].as_array().into_iter().flatten() {
            if feature["$type"] != LINK_FACET {
                continue;
            }
            let Some(uri) = feature["uri"].as_str().filter(|u| u.starts_with("https://") || u.starts_with("http://")) else { continue };
            if !links.iter().any(|l| l == uri) {
                links.push(uri.to_string());
            }
            let (start, end) = (facet["index"]["byteStart"].as_u64(), facet["index"]["byteEnd"].as_u64());
            if let (Some(s), Some(e)) = (start, end) {
                spans.push((s as usize, e as usize, uri.to_string()));
            }
        }
    }
    spans.sort_by(|a, b| b.0.cmp(&a.0));
    let mut out = text.clone();
    let mut last_start = usize::MAX;
    for (s, e, uri) in spans {
        // Overlapping or out-of-range facets are page-served nonsense: skip.
        if s >= e || e > out.len() || e > last_start || !out.is_char_boundary(s) || !out.is_char_boundary(e) {
            continue;
        }
        out.replace_range(s..e, &uri);
        last_start = s;
    }
    // A URL typed without a facet (a client that does not make them).
    for u in super::text_links(&out) {
        if !links.contains(&u) {
            links.push(u);
        }
    }
    (out, links)
}

fn author(post: &Value) -> Option<String> {
    let handle = post["author"]["handle"].as_str().filter(|h| !h.is_empty());
    let name = post["author"]["displayName"].as_str().map(str::trim).filter(|n| !n.is_empty());
    match (name, handle) {
        (Some(n), Some(h)) => Some(format!("{n} (@{h})")),
        (None, Some(h)) => Some(format!("@{h}")),
        (Some(n), None) => Some(n.to_string()),
        (None, None) => None,
    }
}

fn shared_link(post: &Value) -> Option<String> {
    (post["embed"]["$type"] == EXTERNAL_EMBED)
        .then(|| post["embed"]["external"]["uri"].as_str())
        .flatten()
        .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
        .map(str::to_string)
}

fn walk(node: &Value, depth: u8, parent: Option<usize>, root_did: &str, out: &mut Vec<Comment>) {
    for reply in node["replies"].as_array().into_iter().flatten() {
        if out.len() >= MAX_COMMENTS_READ {
            return;
        }
        // notFoundPost / blockedPost carry no text.
        if reply["$type"] != THREAD_POST {
            continue;
        }
        let post = &reply["post"];
        let (text, mut links) = post_text(post);
        if let Some(s) = shared_link(post) {
            if !links.contains(&s) {
                links.push(s);
            }
        }
        out.push(Comment {
            is_op: post["author"]["did"].as_str() == Some(root_did),
            pinned: false,
            likes: post["likeCount"].as_u64(),
            depth,
            parent,
            text,
            links,
        });
        let me = out.len() - 1;
        walk(reply, depth.saturating_add(1), Some(me), root_did, out);
    }
}

/// `getPostThread`'s `thread`, read. No network.
pub fn parse_thread(thread: &Value) -> Result<Thread, String> {
    if thread["$type"] != THREAD_POST {
        return Err("Bluesky did not return this post (it may be deleted or private)".into());
    }
    let post = &thread["post"];
    let root_did = post["author"]["did"].as_str().unwrap_or("");
    let (text, links) = post_text(post);
    let mut comments = Vec::new();
    walk(thread, 0, None, root_did, &mut comments);
    Ok(Thread {
        title: first_line(&text),
        author: author(post),
        text,
        links,
        shared: shared_link(post).into_iter().collect(),
        discussion: Discussion { total: post["replyCount"].as_u64(), comments },
        partial: None,
    })
}

pub async fn read(appview: &str, actor: &str, rkey: &str, policy: FetchPolicy, cancel: &CancellationToken) -> Result<Thread, String> {
    let did = if actor.starts_with("did:") {
        actor.to_string()
    } else {
        let url = reqwest::Url::parse_with_params(&format!("{appview}/xrpc/com.atproto.identity.resolveHandle"), &[("handle", actor)])
            .map_err(|_| "not a valid Bluesky handle".to_string())?;
        let v = get_json(url.as_str(), policy, cancel).await.map_err(|e| format!("Bluesky could not find @{actor} ({e})"))?;
        v["did"].as_str().filter(|d| d.starts_with("did:")).ok_or("Bluesky could not find that account")?.to_string()
    };
    let at = format!("at://{did}/app.bsky.feed.post/{rkey}");
    let url = reqwest::Url::parse_with_params(
        &format!("{appview}/xrpc/app.bsky.feed.getPostThread"),
        &[("uri", at.as_str()), ("depth", DEPTH), ("parentHeight", "0")],
    )
    .map_err(|_| "not a valid Bluesky post".to_string())?;
    let v = get_json(url.as_str(), policy, cancel).await.map_err(|e| format!("Bluesky did not return the post ({e})"))?;
    parse_thread(&v["thread"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn post(did: &str, text: &str, likes: u64) -> Value {
        json!({"author": {"did": did, "handle": "a.bsky.social", "displayName": "Ann"}, "record": {"text": text}, "likeCount": likes})
    }

    #[test]
    fn link_facets_restore_the_full_url_in_the_text() {
        let text = "อ่าน example.com/very-lo… ด้วย";
        let start = text.find("example").unwrap();
        let end = start + "example.com/very-lo…".len();
        let p = json!({"record": {"text": text, "facets": [
            {"index": {"byteStart": start, "byteEnd": end}, "features": [{"$type": LINK_FACET, "uri": "https://example.com/very-long-path"}]},
            {"index": {"byteStart": 1, "byteEnd": 2}, "features": [{"$type": LINK_FACET, "uri": "https://bad.example/"}]}
        ]}});
        let (out, links) = post_text(&p);
        assert_eq!(out, "อ่าน https://example.com/very-long-path ด้วย");
        assert_eq!(links, vec!["https://example.com/very-long-path".to_string(), "https://bad.example/".to_string()]);
    }

    #[test]
    fn a_thread_reads_the_post_its_card_and_its_replies_with_the_author_marked() {
        let mut root = post("did:plc:op", "The post text", 0);
        root["embed"] = json!({"$type": EXTERNAL_EMBED, "external": {"uri": "https://example.com/article", "title": "Article"}});
        root["replyCount"] = json!(3);
        let thread = json!({
            "$type": THREAD_POST, "post": root,
            "replies": [
                {"$type": THREAD_POST, "post": post("did:plc:other", "A reply from someone else with a point", 12),
                 "replies": [{"$type": THREAD_POST, "post": post("did:plc:op", "The author answers here: https://example.com/more", 3), "replies": []}]},
                {"$type": "app.bsky.feed.defs#blockedPost", "uri": "at://x"}
            ]
        });
        let t = parse_thread(&thread).unwrap();
        assert_eq!(t.author.as_deref(), Some("Ann (@a.bsky.social)"));
        assert_eq!(t.shared, vec!["https://example.com/article".to_string()]);
        assert_eq!(t.discussion.comments.len(), 2);
        let reply = &t.discussion.comments[1];
        assert!(reply.is_op && reply.depth == 1 && reply.parent == Some(0));
        assert_eq!(t.discussion.author_links(), vec!["https://example.com/more".to_string()]);
    }

    #[test]
    fn a_missing_post_says_so() {
        assert!(parse_thread(&json!({"$type": "app.bsky.feed.defs#notFoundPost"})).unwrap_err().contains("deleted or private"));
    }

    const LOOPBACK: FetchPolicy = FetchPolicy { allow_loopback: true };

    #[tokio::test]
    async fn read_resolves_the_handle_then_asks_for_the_thread() {
        let mut server = mockito::Server::new_async().await;
        let resolve = server
            .mock("GET", "/xrpc/com.atproto.identity.resolveHandle")
            .match_query(mockito::Matcher::UrlEncoded("handle".into(), "a.bsky.social".into()))
            .with_body(r#"{"did":"did:plc:op"}"#)
            .create_async()
            .await;
        let body = json!({"thread": {"$type": THREAD_POST, "post": post("did:plc:op", "Hello", 0), "replies": []}}).to_string();
        let thread = server
            .mock("GET", "/xrpc/app.bsky.feed.getPostThread")
            .match_query(mockito::Matcher::UrlEncoded("uri".into(), "at://did:plc:op/app.bsky.feed.post/3abc".into()))
            .with_body(body)
            .create_async()
            .await;
        let t = read(&server.url(), "a.bsky.social", "3abc", LOOPBACK, &CancellationToken::new()).await.unwrap();
        resolve.assert_async().await;
        thread.assert_async().await;
        assert_eq!(t.text, "Hello");
    }
}
