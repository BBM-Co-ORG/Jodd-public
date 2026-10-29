//! YouTube comments through the Data API v3, `commentThreads.list`, with an
//! API key the user saved (spec 2026-10-28 §6.2, §11). Public comments need a
//! key only — no OAuth — at one quota unit per page.
//!
//! The thread's `snippet.channelId` is the video's channel, so a comment whose
//! `authorChannelId` equals it is the creator's own (`is_op`). The Data API
//! does not say which comment is pinned.
//!
//! The key travels in the query string: no URL from here is ever logged, and
//! `CommentsApi` has no `Debug` that could print it.

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{Comment, Discussion, MAX_COMMENTS_READ};
use crate::ingest::net::{read_capped, send_guarded, GuardedRequest};
use crate::ingest::FetchPolicy;

pub const API_BASE: &str = "https://www.googleapis.com/youtube/v3";
/// Threads per page (the API's maximum).
const PAGE_SIZE: &str = "100";
/// Pages read at most — one quota unit each.
const MAX_PAGES: usize = 2;
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone)]
pub struct CommentsApi {
    pub base: String,
    pub key: String,
}

impl std::fmt::Debug for CommentsApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommentsApi").field("base", &self.base).field("key", &"<redacted>").finish()
    }
}

impl CommentsApi {
    pub fn with_key(key: String) -> Self {
        CommentsApi { base: API_BASE.to_string(), key }
    }
}

fn snippet_comment(s: &Value, video_channel: &str, depth: u8, parent: Option<usize>) -> Option<Comment> {
    let text = s["textOriginal"].as_str().or_else(|| s["textDisplay"].as_str())?.to_string();
    let author = s["authorChannelId"]["value"].as_str().unwrap_or("");
    let links = super::text_links(&text);
    Some(Comment {
        is_op: !video_channel.is_empty() && author == video_channel,
        pinned: false,
        likes: s["likeCount"].as_u64(),
        depth,
        parent,
        text,
        links,
    })
}

/// One `commentThreads` page, appended to `out`. No network.
pub fn parse_page(page: &Value, out: &mut Vec<Comment>) {
    for item in page["items"].as_array().into_iter().flatten() {
        if out.len() >= MAX_COMMENTS_READ {
            return;
        }
        let channel = item["snippet"]["channelId"].as_str().unwrap_or("");
        let Some(top) = snippet_comment(&item["snippet"]["topLevelComment"]["snippet"], channel, 0, None) else { continue };
        out.push(top);
        let me = out.len() - 1;
        // The API embeds up to five replies; the creator's answers are usually among them.
        for r in item["replies"]["comments"].as_array().into_iter().flatten() {
            if out.len() >= MAX_COMMENTS_READ {
                return;
            }
            if let Some(c) = snippet_comment(&r["snippet"], channel, 1, Some(me)) {
                out.push(c);
            }
        }
    }
}

/// The user-facing reason in an API error body (`error.errors[0].reason`).
fn refusal(status: u16, body: &[u8]) -> String {
    let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    match v["error"]["errors"][0]["reason"].as_str().unwrap_or("") {
        "commentsDisabled" => "comments are turned off for this video".into(),
        "keyInvalid" | "badRequest" if status == 400 => "the YouTube API key was refused — check it in Settings › Links".into(),
        "quotaExceeded" | "dailyLimitExceeded" => "the YouTube API key's daily quota is used up".into(),
        "videoNotFound" => "YouTube did not find this video".into(),
        "accessNotConfigured" | "forbidden" => "the YouTube Data API is not enabled for this key".into(),
        _ => format!("YouTube refused the comments request (HTTP {status})"),
    }
}

/// Up to `MAX_PAGES` pages of the most relevant threads.
pub async fn read(api: &CommentsApi, video_id: &str, policy: FetchPolicy, cancel: &CancellationToken) -> Result<Discussion, String> {
    let mut comments = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut params: Vec<(&str, &str)> = vec![
            ("part", "snippet,replies"),
            ("videoId", video_id),
            ("order", "relevance"),
            ("maxResults", PAGE_SIZE),
            ("textFormat", "plainText"),
            ("key", api.key.as_str()),
        ];
        if let Some(t) = &token {
            params.push(("pageToken", t.as_str()));
        }
        let url = reqwest::Url::parse_with_params(&format!("{}/commentThreads", api.base), &params).map_err(|_| "not a valid video id".to_string())?;
        let resp = send_guarded(GuardedRequest::get(url.as_str()), policy, cancel).await?;
        let status = resp.status();
        let body = read_capped(resp, MAX_BODY_BYTES, cancel).await?;
        if !status.is_success() {
            return Err(refusal(status.as_u16(), &body));
        }
        let page: Value = serde_json::from_slice(&body).map_err(|_| "YouTube's answer was not JSON".to_string())?;
        parse_page(&page, &mut comments);
        token = page["nextPageToken"].as_str().map(str::to_string);
        if token.is_none() || comments.len() >= MAX_COMMENTS_READ {
            break;
        }
    }
    Ok(Discussion { total: None, comments })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn thread(channel: &str, author: &str, text: &str, likes: u64, replies: Vec<(&str, &str)>) -> Value {
        json!({
            "snippet": {"channelId": channel, "topLevelComment": {"snippet": {
                "textOriginal": text, "authorChannelId": {"value": author}, "likeCount": likes}}},
            "replies": {"comments": replies.iter().map(|(a, t)| json!({"snippet": {"textOriginal": t, "authorChannelId": {"value": a}, "likeCount": 0}})).collect::<Vec<_>>()}
        })
    }

    #[test]
    fn the_creator_is_the_videos_channel_and_replies_nest() {
        let page = json!({"items": [
            thread("UCcreator", "UCfan", "Where is the code for the second part?", 40, vec![("UCcreator", "Here: https://github.com/a/b")]),
            thread("UCcreator", "UCcreator", "Full version: https://example.com/full", 5, vec![]),
        ]});
        let mut out = Vec::new();
        parse_page(&page, &mut out);
        assert_eq!(out.len(), 3);
        assert!(!out[0].is_op && out[1].is_op && out[2].is_op);
        assert_eq!((out[1].depth, out[1].parent), (1, Some(0)));
        let d = Discussion { total: None, comments: out };
        assert_eq!(d.author_links(), vec!["https://github.com/a/b".to_string(), "https://example.com/full".to_string()]);
    }

    #[test]
    fn the_key_never_shows_in_debug_output() {
        let api = CommentsApi::with_key("AIzaSECRET".into());
        assert!(!format!("{api:?}").contains("SECRET"));
    }

    const LOOPBACK: FetchPolicy = FetchPolicy { allow_loopback: true };

    #[tokio::test]
    async fn reads_two_pages_at_most_and_names_a_refusal() {
        let mut server = mockito::Server::new_async().await;
        let page = |next: Option<&str>| {
            let mut v = json!({"items": [thread("UCc", "UCf", "A comment with enough words to keep", 1, vec![])]});
            if let Some(n) = next {
                v["nextPageToken"] = json!(n);
            }
            v.to_string()
        };
        let first = server
            .mock("GET", "/commentThreads")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("videoId".into(), "abcdefghijk".into()),
                mockito::Matcher::UrlEncoded("key".into(), "K".into()),
            ]))
            .with_body(page(Some("p2")))
            // mockito serves the first matching mock that still owes hits.
            .expect(1)
            .create_async()
            .await;
        let second = server
            .mock("GET", "/commentThreads")
            .match_query(mockito::Matcher::UrlEncoded("pageToken".into(), "p2".into()))
            .with_body(page(Some("p3")))
            .expect(1)
            .create_async()
            .await;
        let api = CommentsApi { base: server.url(), key: "K".into() };
        let d = read(&api, "abcdefghijk", LOOPBACK, &CancellationToken::new()).await.unwrap();
        first.assert_async().await;
        second.assert_async().await;
        assert_eq!(d.comments.len(), 2);

        let mut off = mockito::Server::new_async().await;
        let _m = off
            .mock("GET", "/commentThreads")
            .match_query(mockito::Matcher::Any)
            .with_status(403)
            .with_body(r#"{"error":{"errors":[{"reason":"commentsDisabled"}]}}"#)
            .create_async()
            .await;
        let api = CommentsApi { base: off.url(), key: "K".into() };
        assert_eq!(read(&api, "abcdefghijk", LOOPBACK, &CancellationToken::new()).await.unwrap_err(), "comments are turned off for this video");
    }
}
